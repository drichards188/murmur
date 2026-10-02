//! Checks terrain line of sight against a running `tidewater serve` without any vehicles:
//!
//! losprobe --tidewater http://127.0.0.1:8772 --gs 37.8105,-122.4770,5 [--gs-antenna 10]
//!          [--antenna 0] [--fresnel-mhz 900] [--at 2026-06-14T10:00:00Z]
//!          lat,lon,alt  lat,lon,alt ...
//!
//! Prints, per vehicle position, whether the path from the ground station is clear or
//! obstructed and by how many metres (negative: clear by that much).

use murmur_linkemu::config::TerrainCfg;
use murmur_linkemu::geo::{GeoPoint, LosParams, Terrain, haversine_m};
use murmur_linkemu::terrain::{HttpSampler, TidewaterTerrain};
use murmur_linkemu::timeline::{Timeline, parse_iso_utc};

fn triple(s: &str) -> Result<GeoPoint, String> {
    let v: Vec<f64> = s
        .split(',')
        .map(|x| {
            x.trim()
                .parse::<f64>()
                .map_err(|_| format!("bad number in {s:?}"))
        })
        .collect::<Result<_, _>>()?;
    match v.as_slice() {
        [lat, lon, alt] => Ok(GeoPoint {
            lat: *lat,
            lon: *lon,
            alt_m: *alt,
        }),
        [lat, lon] => Ok(GeoPoint {
            lat: *lat,
            lon: *lon,
            alt_m: 0.0,
        }),
        _ => Err(format!("want lat,lon[,alt]: {s:?}")),
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("losprobe: {e}");
        std::process::exit(2);
    }
}

async fn run() -> Result<(), String> {
    let mut url = String::from("http://127.0.0.1:8772");
    let mut gs: Option<GeoPoint> = None;
    let mut p = LosParams::default();
    let mut at = None;
    let mut vehicles = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut val = || it.next().ok_or_else(|| format!("{a} needs a value"));
        let num = |s: String| s.parse::<f64>().map_err(|_| format!("bad number {s:?}"));
        match a.as_str() {
            "--tidewater" => url = val()?,
            "--gs" => gs = Some(triple(&val()?)?),
            "--gs-antenna" => p.gs_antenna_m = num(val()?)?,
            "--antenna" => p.antenna_m = num(val()?)?,
            "--fresnel-mhz" => p.fresnel_mhz = num(val()?)?,
            "--k" => p.k = num(val()?)?,
            "--at" => at = Some(val()?),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => vehicles.push(triple(s)?),
        }
    }
    let gs = gs.ok_or("--gs lat,lon,alt is required")?;
    let tl = Timeline::new(at.as_deref(), None)?;
    let t = tl
        .at_unix
        .unwrap_or_else(|| parse_iso_utc("2026-06-14T10:00:00Z").unwrap_or(0.0));
    let cfg = TerrainCfg::default();
    let terrain = TidewaterTerrain::new(cfg.clone());
    let sampler = HttpSampler::new(&url, cfg.timeout_s)?;
    for v in &vehicles {
        terrain.obstruction_m(v, &gs, &p);
    }
    let started = std::time::Instant::now();
    let done = terrain.tick(&sampler, t).await;
    eprintln!(
        "{done} paths in {:.0} ms ({} requests)",
        started.elapsed().as_secs_f64() * 1000.0,
        terrain
            .stats()
            .map_or(0, |s| s["requests"].as_u64().unwrap_or(0))
    );
    for v in &vehicles {
        let d = haversine_m(&gs, v) / 1000.0;
        match terrain.obstruction_m(v, &gs, &p) {
            Some(o) if o > 0.0 => println!(
                "{:.4},{:.4} alt {:>5.0} m  {d:5.1} km  OBSTRUCTED by {o:.0} m",
                v.lat, v.lon, v.alt_m
            ),
            Some(o) => println!(
                "{:.4},{:.4} alt {:>5.0} m  {d:5.1} km  clear by {:.0} m",
                v.lat, v.lon, v.alt_m, -o
            ),
            None => println!("{:.4},{:.4}  no answer", v.lat, v.lon),
        }
    }
    Ok(())
}
