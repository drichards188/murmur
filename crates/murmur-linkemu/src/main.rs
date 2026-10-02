use murmur_linkemu::config::{Config, load_fleet};
use murmur_linkemu::geo::NoTerrain;
use murmur_linkemu::link::Env;
use murmur_linkemu::proxy::{Options, build_links, start, write_bridge_manifest};
use murmur_linkemu::stats::{cpu_seconds, totals};
use murmur_linkemu::{http, model::CAUSES};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

const USAGE: &str =
    "murmur-linkemu: degrade each vehicle's UDP link between vehicles and the bridge

USAGE: murmur-linkemu --bridge-base PORT [options]

  --fleet FILE         fleet manifest (default sim/run/fleet.json)
  --config FILE        link profiles and events (default sim/links.toml)
  --scenario FILE      extra [[event]] entries, merged with the config's
  --bridge-base PORT   the bridge's base port: vehicle i's traffic goes to PORT + i
  --vehicle-base PORT  the base port the vehicles were launched with (default: the lowest
                       port in the fleet manifest)
  --bridge-host HOST   default 127.0.0.1
  --listen-host HOST   default 127.0.0.1
  --manifest-out FILE  write a copy of the fleet manifest with bridge-side ports, for the
                       bridge to read (MURMUR_FLEET_MANIFEST); required for the Python bridge
  --http ADDR          stats endpoint (default 127.0.0.1:8766; 'off' disables)
  --seed N             overrides the config's seed
  --log-interval SEC   periodic log line (default 5; 0 disables)
  --threads N          tokio worker threads (default 1)
";

struct Args {
    fleet: PathBuf,
    config: PathBuf,
    scenario: Option<PathBuf>,
    manifest_out: Option<PathBuf>,
    bridge_base: u16,
    vehicle_base: Option<u16>,
    bridge_host: String,
    listen_host: String,
    http: String,
    seed: Option<u64>,
    log_interval: u64,
    threads: usize,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        fleet: "sim/run/fleet.json".into(),
        config: "sim/links.toml".into(),
        scenario: None,
        manifest_out: None,
        bridge_base: 0,
        vehicle_base: None,
        bridge_host: "127.0.0.1".into(),
        listen_host: "127.0.0.1".into(),
        http: "127.0.0.1:8766".into(),
        seed: None,
        log_interval: 5,
        threads: 1,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        if flag == "-h" || flag == "--help" {
            print!("{USAGE}");
            std::process::exit(0);
        }
        let mut val = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        let num = |s: String| {
            s.parse::<u64>()
                .map_err(|_| format!("{flag}: bad number {s:?}"))
        };
        match flag.as_str() {
            "--fleet" => a.fleet = val()?.into(),
            "--config" => a.config = val()?.into(),
            "--scenario" => a.scenario = Some(val()?.into()),
            "--manifest-out" => a.manifest_out = Some(val()?.into()),
            "--bridge-base" => a.bridge_base = num(val()?)? as u16,
            "--vehicle-base" => a.vehicle_base = Some(num(val()?)? as u16),
            "--bridge-host" => a.bridge_host = val()?,
            "--listen-host" => a.listen_host = val()?,
            "--http" => a.http = val()?,
            "--seed" => a.seed = Some(num(val()?)?),
            "--log-interval" => a.log_interval = num(val()?)?,
            "--threads" => a.threads = num(val()?)? as usize,
            _ => return Err(format!("unknown option {flag}\n\n{USAGE}")),
        }
    }
    if a.bridge_base == 0 {
        return Err(format!("--bridge-base is required\n\n{USAGE}"));
    }
    Ok(a)
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("murmur-linkemu: {e}");
        std::process::exit(2);
    }
}

fn real_main() -> Result<(), String> {
    let args = parse_args()?;
    let mut cfg = Config::load(&args.config)?;
    if let Some(s) = &args.scenario {
        cfg.merge_scenario(s)?;
    }
    let fleet = load_fleet(&args.fleet)?;
    if fleet.is_empty() {
        return Err("fleet manifest has no vehicles".into());
    }
    let vehicle_base = args
        .vehicle_base
        .unwrap_or_else(|| fleet.iter().map(|v| v.port).min().unwrap_or(0));
    if let Some(out) = &args.manifest_out {
        write_bridge_manifest(&args.fleet, out, vehicle_base, args.bridge_base)?;
    }
    let seed = args.seed.or(cfg.seed).unwrap_or(1);
    let env = Arc::new(Env::new(&cfg, Arc::new(NoTerrain), seed));
    let links = build_links(&fleet, &cfg, &env)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(args.threads.max(1))
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let t0 = Instant::now();
        let opts = Options {
            listen_host: args.listen_host.clone(),
            bridge_host: args.bridge_host.clone(),
            bridge_base: args.bridge_base,
            vehicle_base,
        };
        let stats = Arc::new(start(links, &opts, t0).await?);
        if args.http != "off" {
            http::serve(&args.http, stats.clone(), t0).await?;
        }
        eprintln!(
            "linkemu: {} links bound in {:.1}s, seed {seed}, {} events, vehicles on {}+ -> bridge {}:{}+, stats http://{}/stats",
            stats.len(),
            t0.elapsed().as_secs_f64(),
            env.schedule.names().len(),
            vehicle_base,
            args.bridge_host,
            args.bridge_base,
            args.http
        );
        for (name, s, e) in env.schedule.names() {
            let end = if e == u64::MAX { "end".to_string() } else { format!("{:.0}s", e as f64 / 1e9) };
            eprintln!("linkemu: event {name:?} from {:.0}s to {end}", s as f64 / 1e9);
        }

        let mut prev = (0u64, 0u64, 0u64, [0u64; CAUSES.len()], cpu_seconds(), Instant::now());
        let interval = if args.log_interval == 0 { 3600 } else { args.log_interval };
        loop {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(interval)) => {}
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("linkemu: stopping");
                    return Ok::<(), String>(());
                }
            }
            if args.log_interval == 0 {
                continue;
            }
            let t = totals(&stats);
            let cpu = cpu_seconds();
            let dt = prev.5.elapsed().as_secs_f64();
            let drops: Vec<String> = CAUSES
                .iter()
                .filter(|c| t.by_cause[c.idx()] > prev.3[c.idx()])
                .map(|c| format!("{}={}", c.name(), t.by_cause[c.idx()] - prev.3[c.idx()]))
                .collect();
            eprintln!(
                "linkemu t={:.0}s in={:.0}/s out={:.0}/s dropped={:.0}/s [{}] cpu={:.1}%",
                t0.elapsed().as_secs_f64(),
                (t.in_pkts - prev.0) as f64 / dt,
                (t.out_pkts - prev.1) as f64 / dt,
                (t.dropped - prev.2) as f64 / dt,
                drops.join(" "),
                (cpu - prev.4) / dt * 100.0
            );
            prev = (t.in_pkts, t.out_pkts, t.dropped, t.by_cause, cpu, Instant::now());
        }
    })
}
