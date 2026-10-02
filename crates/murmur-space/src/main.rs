//! `murmur-space`: fetch element sets, list access windows, tasking options, link windows and
//! GNSS visibility. Human-readable tables by default, `--json` for machines.

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveDateTime, Utc};
use clap::{Args, Parser, Subcommand};
use murmur_space::area::{Area, load_areas};
use murmur_space::cache::{Cache, FetchOutcome};
use murmur_space::catalog::{Capability, Catalog};
use murmur_space::geo::Geodetic;
use murmur_space::space::{AccessQuery, GnssQuery, LinkQuery, Space};
use murmur_space::tasking::{NoCloudModel, Rank, TaskRequest};
use murmur_space::timemap::{EpochMode, TimeMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "murmur-space",
    about = "Murmur space segment: orbits, access, tasking options, links, GNSS"
)]
struct Cli {
    /// Constellation catalog (default: sim/space/constellations.toml in the repo).
    #[arg(long, global = true)]
    catalog: Option<PathBuf>,
    /// Element-set cache directory (default: sim/space/cache).
    #[arg(long, global = true)]
    cache: Option<PathBuf>,
    /// Named areas file (default: sim/space/areas.toml).
    #[arg(long, global = true)]
    areas: Option<PathBuf>,
    /// How scenario time maps to propagation time (see README: epoch honesty).
    #[arg(long, global = true, value_enum, default_value_t = EpochMode::Auto)]
    epoch: EpochMode,
    /// Print JSON instead of tables.
    #[arg(long, global = true)]
    json: bool,
    /// Hours from UTC for the local-time column (default: the area's, else 0).
    #[arg(long, global = true, allow_hyphen_values = true)]
    utc_offset: Option<f64>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download and cache element sets from CelesTrak (at most once per 2-hour update cycle).
    Fetch {
        /// Refetch anything older than 2 hours (CelesTrak's update cycle; never sooner).
        #[arg(long)]
        force: bool,
        /// Refetch caches older than this many hours.
        #[arg(long, default_value_t = 12.0)]
        max_age_h: f64,
    },
    /// Store element sets obtained elsewhere (e.g. historical GP JSON from Space-Track) under
    /// a catalog query, replacing the CelesTrak cache for it.
    Import {
        /// Catalog query the file stands for, e.g. GROUP=starlink.
        #[arg(long)]
        query: String,
        file: PathBuf,
    },
    /// Classes, satellite counts and element ages.
    Catalog,
    /// Imaging access windows over an area.
    Passes {
        #[command(flatten)]
        target: TargetArgs,
        #[command(flatten)]
        window: WindowArgs,
        /// Class ids (repeatable; default: every imaging class).
        #[arg(long = "class")]
        classes: Vec<String>,
        /// Keep optical windows in darkness.
        #[arg(long)]
        include_night: bool,
        /// Rows per class in the table.
        #[arg(long, default_value_t = 12)]
        limit: usize,
    },
    /// Ranked tasking options for a collection request, with exclusions.
    Task {
        #[command(flatten)]
        target: TargetArgs,
        #[command(flatten)]
        window: WindowArgs,
        /// Request time (default: window start).
        #[arg(long)]
        now: Option<String>,
        /// Acceptable capabilities, comma separated: sar,optical.
        #[arg(long, default_value = "sar,optical", value_delimiter = ',')]
        capability: Vec<String>,
        /// Product must be in hand by this time.
        #[arg(long)]
        deadline: Option<String>,
        /// Coarsest acceptable resolution, metres.
        #[arg(long)]
        max_res: Option<f64>,
        #[arg(long, value_enum, default_value_t = Rank::EarliestProduct)]
        rank: Rank,
        #[arg(long = "class")]
        classes: Vec<String>,
        /// Leave out routine collects from untaskable sensors.
        #[arg(long)]
        taskable_only: bool,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        /// Exclusion rows to list (all are counted).
        #[arg(long, default_value_t = 8)]
        exclusions: usize,
    },
    /// SATCOM link windows, coverage and latency from a terminal position.
    Link {
        #[arg(long, allow_hyphen_values = true)]
        lat: f64,
        #[arg(long, allow_hyphen_values = true)]
        lon: f64,
        #[arg(long, default_value_t = 0.0)]
        alt_m: f64,
        #[command(flatten)]
        window: WindowArgs,
        #[arg(long = "class")]
        classes: Vec<String>,
        /// Sampling step for visible count and latency, seconds.
        #[arg(long, default_value_t = 60.0)]
        step: f64,
        /// Include the per-sample timeline in JSON.
        #[arg(long)]
        samples: bool,
        #[arg(long, default_value_t = 8)]
        limit: usize,
    },
    /// GNSS visible satellites and DOP over time at a point.
    Gnss {
        #[arg(long, allow_hyphen_values = true)]
        lat: f64,
        #[arg(long, allow_hyphen_values = true)]
        lon: f64,
        #[arg(long, default_value_t = 0.0)]
        alt_m: f64,
        #[command(flatten)]
        window: WindowArgs,
        #[arg(long, default_value_t = 1800.0)]
        step: f64,
        #[arg(long)]
        mask: Option<f64>,
        #[arg(long = "class", default_value = "gps")]
        class: String,
    },
    /// Satellite positions (and footprints) of a class at an instant, for maps.
    Positions {
        #[arg(long = "class")]
        class: String,
        #[arg(long)]
        at: String,
        #[arg(long)]
        footprints: bool,
    },
}

#[derive(Args)]
struct TargetArgs {
    /// Named area from the areas file (e.g. golden-gate).
    #[arg(long)]
    area: Option<String>,
    /// Or a point: lat,lon[,radius_km].
    #[arg(long, allow_hyphen_values = true)]
    point: Option<String>,
}

#[derive(Args)]
struct WindowArgs {
    /// Window start, scenario time (RFC 3339, "2026-06-14T10:00Z" or "2026-06-14").
    #[arg(long)]
    from: String,
    /// Window end (default: start + 24 h).
    #[arg(long)]
    to: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("murmur-space: {e}");
            ExitCode::FAILURE
        }
    }
}

fn repo_root() -> PathBuf {
    if let Ok(r) = std::env::var("MURMUR_ROOT") {
        return PathBuf::from(r);
    }
    if let Ok(mut d) = std::env::current_dir() {
        loop {
            if d.join("sim/space/constellations.toml").exists() {
                return d;
            }
            if !d.pop() {
                break;
            }
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn parse_time(s: &str) -> Result<DateTime<Utc>, String> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.with_timezone(&Utc));
    }
    for f in ["%Y-%m-%dT%H:%M%:z", "%Y-%m-%dT%H:%M%z"] {
        if let Ok(t) = DateTime::parse_from_str(s, f) {
            return Ok(t.with_timezone(&Utc));
        }
    }
    let z = s.trim_end_matches('Z');
    for f in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(n) = NaiveDateTime::parse_from_str(z, f) {
            return Ok(n.and_utc());
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc());
    }
    Err(format!(
        "cannot parse time {s:?} (RFC 3339, e.g. 2026-06-14T16:00-07:00 or 2026-06-14T23:00Z)"
    ))
}

fn window(w: &WindowArgs) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    let from = parse_time(&w.from)?;
    let to = match &w.to {
        Some(t) => parse_time(t)?,
        None => from + Duration::hours(24),
    };
    if to <= from {
        return Err("--to must be after --from".into());
    }
    Ok((from, to))
}

struct Ctx {
    catalog: PathBuf,
    cache: PathBuf,
    areas: PathBuf,
}

impl Ctx {
    fn space(&self) -> Result<Space, String> {
        Space::load(&self.catalog, &self.cache).map_err(|e| e.to_string())
    }

    fn target(&self, t: &TargetArgs) -> Result<(Area, String, Option<f64>), String> {
        match (&t.area, &t.point) {
            (Some(name), None) => {
                let areas = load_areas(&self.areas)?;
                let a = areas
                    .into_iter()
                    .find(|a| &a.name == name)
                    .ok_or_else(|| format!("no area {name:?} in {}", self.areas.display()))?;
                Ok((a.area, a.title.unwrap_or(a.name), Some(a.utc_offset_h)))
            }
            (None, Some(p)) => {
                let v: Vec<f64> = p
                    .split(',')
                    .map(|x| x.trim().parse::<f64>())
                    .collect::<Result<_, _>>()
                    .map_err(|e| format!("--point: {e}"))?;
                if v.len() < 2 {
                    return Err("--point needs lat,lon[,radius_km]".into());
                }
                Ok((
                    Area::point(v[0], v[1], v.get(2).copied().unwrap_or(1.0)),
                    format!("{:.4},{:.4}", v[0], v[1]),
                    None,
                ))
            }
            _ => Err("give exactly one of --area or --point".into()),
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let root = repo_root();
    let ctx = Ctx {
        catalog: cli
            .catalog
            .clone()
            .unwrap_or_else(|| root.join("sim/space/constellations.toml")),
        cache: cli
            .cache
            .clone()
            .unwrap_or_else(|| root.join("sim/space/cache")),
        areas: cli
            .areas
            .clone()
            .unwrap_or_else(|| root.join("sim/space/areas.toml")),
    };
    let json = cli.json;
    let offset_for = |area: Option<f64>| cli.utc_offset.or(area).unwrap_or(0.0);
    match cli.cmd {
        Cmd::Fetch { force, max_age_h } => {
            let catalog = Catalog::load(&ctx.catalog).map_err(|e| e.to_string())?;
            let cache = Cache::new(&ctx.cache);
            let out = cache.fetch_all(
                &catalog.queries(),
                Duration::minutes((max_age_h * 60.0) as i64),
                force,
                Utc::now(),
            );
            if json {
                return print_json(&out);
            }
            let mut failed = false;
            for o in &out {
                match o {
                    FetchOutcome::Fetched(m) => println!(
                        "fetched  {:<22} {:>6} objects  {:>8} bytes",
                        m.query, m.objects, m.bytes
                    ),
                    FetchOutcome::Fresh(m) => println!(
                        "cached   {:<22} {:>6} objects  fetched {} ({:.1} h ago)",
                        m.query,
                        m.objects,
                        m.fetched_at.format("%Y-%m-%d %H:%MZ"),
                        (Utc::now() - m.fetched_at).num_minutes() as f64 / 60.0
                    ),
                    FetchOutcome::Failed { query, error } => {
                        failed = true;
                        println!("FAILED   {query:<22} {error}");
                    }
                    FetchOutcome::Skipped { query, reason } => {
                        println!("skipped  {query:<22} {reason}")
                    }
                }
            }
            println!("cache: {}", ctx.cache.display());
            if failed {
                return Err("some fetches failed".into());
            }
            Ok(())
        }
        Cmd::Import { query, file } => {
            let text =
                std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let m = Cache::new(&ctx.cache)
                .import(
                    &query,
                    &text,
                    Utc::now(),
                    &format!("import:{}", file.display()),
                )
                .map_err(|e| e.to_string())?;
            println!("imported {} objects as {}", m.objects, m.query);
            Ok(())
        }
        Cmd::Catalog => {
            let space = ctx.space()?;
            let now = space.reference;
            if json {
                #[derive(serde::Serialize)]
                struct Row<'a> {
                    class: &'a murmur_space::SatClass,
                    satellites: usize,
                    init_errors: usize,
                    epoch_oldest: Option<DateTime<Utc>>,
                    epoch_newest: Option<DateTime<Utc>>,
                }
                let rows: Vec<Row> = space
                    .classes
                    .iter()
                    .map(|c| Row {
                        class: &c.class,
                        satellites: c.sats.len(),
                        init_errors: c.init_errors,
                        epoch_oldest: c.sats.iter().map(|s| s.epoch).min(),
                        epoch_newest: c.sats.iter().map(|s| s.epoch).max(),
                    })
                    .collect();
                return print_json(
                    &serde_json::json!({"reference_epoch": now, "classes": rows, "cache": space.cache_meta}),
                );
            }
            println!(
                "element reference epoch (median): {}",
                now.format("%Y-%m-%d %H:%MZ")
            );
            println!(
                "{:<10} {:<9} {:>5}  {:<17} {:<17} name",
                "class", "cap", "sats", "oldest epoch", "newest epoch"
            );
            for c in &space.classes {
                let old = c.sats.iter().map(|s| s.epoch).min();
                let new = c.sats.iter().map(|s| s.epoch).max();
                println!(
                    "{:<10} {:<9} {:>5}  {:<17} {:<17} {}",
                    c.class.id,
                    c.class.capability.to_string(),
                    c.sats.len(),
                    old.map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default(),
                    new.map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default(),
                    c.class.name
                );
            }
            for m in &space.cache_meta {
                println!(
                    "cache {:<22} fetched {}",
                    m.query,
                    m.fetched_at.format("%Y-%m-%d %H:%MZ")
                );
            }
            Ok(())
        }
        Cmd::Passes {
            target,
            window: w,
            classes,
            include_night,
            limit,
        } => {
            let (area, title, area_off) = ctx.target(&target)?;
            let (from, to) = window(&w)?;
            let space = ctx.space()?;
            let rep = space
                .imaging_access(&AccessQuery {
                    area,
                    from,
                    to,
                    classes,
                    epoch_mode: cli.epoch,
                    include_night,
                })
                .map_err(|e| e.to_string())?;
            if json {
                return print_json(&rep);
            }
            let off = offset_for(area_off);
            println!(
                "Imaging access over {title}, {} to {} (scenario time)",
                fmt_t(from, off),
                fmt_t(to, off)
            );
            print_time_map(&rep.time_map, from, to);
            println!(
                "target {:.4},{:.4}  area radius {:.1} km  compute {} ms\n",
                rep.area_center.lat_deg,
                rep.area_center.lon_deg,
                rep.area_radius_km,
                rep.compute_ms
            );
            println!(
                "{:<10} {:<9} {:>5} {:>6} {:>9} {:>8} {:>7}",
                "class", "cap", "sats", "stale", "launched>", "windows", "night"
            );
            for c in &rep.classes {
                println!(
                    "{:<10} {:<9} {:>5} {:>6} {:>9} {:>8} {:>7}",
                    c.class_id,
                    c.capability.to_string(),
                    c.satellites,
                    c.stale_skipped,
                    c.not_yet_launched,
                    c.windows,
                    c.night_excluded
                );
            }
            for c in &rep.classes {
                let rows: Vec<_> = rep
                    .windows
                    .iter()
                    .filter(|x| x.class_id == c.class_id)
                    .collect();
                if rows.is_empty() {
                    continue;
                }
                println!(
                    "\n{} ({} windows{})",
                    c.name,
                    rows.len(),
                    if rows.len() > limit {
                        format!(", first {limit}")
                    } else {
                        String::new()
                    }
                );
                println!(
                    "  {:<24} {:<19} {:>5} {:>6} {:>6} {:>6} {:>6} {:>5}",
                    "satellite", "best (local)", "dur s", "offndr", "incid", "el", "res m", "sun"
                );
                for x in rows.iter().take(limit) {
                    println!(
                        "  {:<24} {:<19} {:>5.0} {:>6.1} {:>6.1} {:>6.1} {:>6} {:>5.0}{}",
                        trunc(&x.satellite, 24),
                        fmt_t(x.best_time, off),
                        x.duration_s,
                        x.best.off_nadir_deg,
                        x.best.incidence_deg,
                        x.best.elevation_deg,
                        x.resolution_m
                            .map(|r| format!("{r:.2}"))
                            .unwrap_or_else(|| "-".into()),
                        x.sun_elevation_deg,
                        if x.daylight_ok == Some(false) {
                            " night"
                        } else {
                            ""
                        }
                    );
                }
            }
            Ok(())
        }
        Cmd::Task {
            target,
            window: w,
            now,
            capability,
            deadline,
            max_res,
            rank,
            classes,
            taskable_only,
            limit,
            exclusions,
        } => {
            let (area, title, area_off) = ctx.target(&target)?;
            let (from, to) = window(&w)?;
            let caps: Vec<Capability> = capability
                .iter()
                .map(|c| c.parse())
                .collect::<Result<_, _>>()?;
            let req = TaskRequest {
                capabilities: caps,
                area,
                from,
                to,
                now: now.as_deref().map(parse_time).transpose()?.unwrap_or(from),
                product_deadline: deadline.as_deref().map(parse_time).transpose()?,
                max_resolution_m: max_res,
                rank,
                classes,
                include_untaskable: !taskable_only,
                epoch_mode: cli.epoch,
            };
            let space = ctx.space()?;
            let res = space
                .tasking_options(&req, &NoCloudModel)
                .map_err(|e| e.to_string())?;
            if json {
                return print_json(&res);
            }
            let off = offset_for(area_off);
            let caps_s: Vec<String> = req.capabilities.iter().map(ToString::to_string).collect();
            println!(
                "Tasking request: {} over {title}; collect {} to {}; requested {}{}{}",
                caps_s.join(" or "),
                fmt_t(from, off),
                fmt_t(to, off),
                fmt_t(req.now, off),
                req.product_deadline
                    .map(|d| format!("; product by {}", fmt_t(d, off)))
                    .unwrap_or_default(),
                req.max_resolution_m
                    .map(|r| format!("; resolution <= {r} m"))
                    .unwrap_or_default(),
            );
            print_time_map(&res.time_map, from, to);
            println!(
                "ranked by {:?}; cloud model: {}; compute {} ms\n",
                req.rank, res.cloud_model, res.compute_ms
            );
            println!(
                "{} feasible options{}",
                res.options.len(),
                if res.options.len() > limit {
                    format!(" (first {limit})")
                } else {
                    String::new()
                }
            );
            println!(
                " #  {:<9} {:<22} {:<7} {:<19} {:>6} {:>6} {:>6} {:<19} notes",
                "class",
                "satellite",
                "type",
                "collect (local)",
                "offndr",
                "incid",
                "res m",
                "product (local)"
            );
            for o in res.options.iter().take(limit) {
                println!(
                    "{:>2}  {:<9} {:<22} {:<7} {:<19} {:>6.1} {:>6.1} {:>6} {:<19} {}",
                    o.rank,
                    o.class_id,
                    trunc(&o.satellite, 22),
                    o.capability.to_string(),
                    fmt_t(o.collect_time, off),
                    o.geometry.off_nadir_deg,
                    o.geometry.incidence_deg,
                    o.resolution_m
                        .map(|r| format!("{r:.2}"))
                        .unwrap_or_else(|| "-".into()),
                    o.product_time
                        .map(|p| fmt_t(p, off))
                        .unwrap_or_else(|| "unknown".into()),
                    short_notes(&o.notes, o.taskable, o.covers_area),
                );
            }
            println!("\n{} passes excluded:", res.exclusions.len());
            for c in &res.exclusion_summary {
                println!(
                    "  {:<10} {:<22} {:>4}",
                    c.class_id,
                    format!("{:?}", c.reason),
                    c.passes
                );
            }
            if !res.classes_without_passes.is_empty() {
                println!(
                    "  no candidate pass at all: {}",
                    res.classes_without_passes.join(", ")
                );
            }
            if exclusions > 0 && !res.exclusions.is_empty() {
                println!("\nexamples:");
                let mut shown = std::collections::BTreeSet::new();
                // One example per (class, reason) first, then fill in time order.
                let mut picked: Vec<&murmur_space::tasking::Exclusion> = Vec::new();
                for e in &res.exclusions {
                    if shown.insert((e.class_id.clone(), e.reason)) {
                        picked.push(e);
                    }
                }
                picked.sort_by_key(|e| e.pass_time);
                for e in picked.iter().take(exclusions) {
                    println!(
                        "  {:<9} {:<22} {:<19} {:<16} {}",
                        e.class_id,
                        trunc(&e.satellite, 22),
                        fmt_t(e.pass_time, off),
                        format!("{:?}", e.reason),
                        e.detail
                    );
                }
            }
            Ok(())
        }
        Cmd::Link {
            lat,
            lon,
            alt_m,
            window: w,
            classes,
            step,
            samples,
            limit,
        } => {
            let (from, to) = window(&w)?;
            let space = ctx.space()?;
            let point = Geodetic::new(lat, lon, alt_m / 1000.0);
            let rep = space
                .links(&LinkQuery {
                    point,
                    from,
                    to,
                    classes,
                    epoch_mode: cli.epoch,
                    sample_step_s: step,
                    include_samples: samples,
                })
                .map_err(|e| e.to_string())?;
            if json {
                return print_json(&rep);
            }
            let off = offset_for(None);
            println!(
                "SATCOM links from {lat:.4},{lon:.4} (alt {alt_m:.0} m), {} to {}",
                fmt_t(from, off),
                fmt_t(to, off)
            );
            print_time_map(&rep.time_map, from, to);
            println!("compute {} ms\n", rep.compute_ms);
            println!(
                "{:<9} {:>11} {:>6} {:>8} {:>8} {:>11} {:>13} {:>16}",
                "class",
                "sats used",
                "mask",
                "windows",
                "cover %",
                "max gap s",
                "visible",
                "latency ms"
            );
            for c in &rep.classes {
                println!(
                    "{:<9} {:>11} {:>6.1} {:>8} {:>8.1} {:>11.0} {:>13} {:>16}",
                    c.class_id,
                    format!(
                        "{}/{}",
                        c.satellites - c.stale_skipped - c.not_yet_launched,
                        c.satellites
                    ),
                    c.mask_deg,
                    c.windows.len(),
                    c.coverage_fraction * 100.0,
                    c.longest_outage_s,
                    format!(
                        "{}-{} ({:.1})",
                        c.visible_min, c.visible_max, c.visible_mean
                    ),
                    match (c.latency_ms_min, c.latency_ms_median, c.latency_ms_max) {
                        (Some(a), Some(m), Some(b)) => format!("{a:.0}/{m:.0}/{b:.0}"),
                        _ => "unknown".into(),
                    }
                );
            }
            println!(
                "(visible: min-max (mean) sampled every {step:.0} s; latency: one-way min/median/max to the best satellite)"
            );
            for c in &rep.classes {
                println!(
                    "\n{}: {} windows{}",
                    c.name,
                    c.windows.len(),
                    if c.windows.len() > limit {
                        format!(", first {limit}")
                    } else {
                        String::new()
                    }
                );
                println!(
                    "  {:<26} {:<19} {:<19} {:>7} {:>6} {:>6} {:>8} {:>8}",
                    "satellite",
                    "start (local)",
                    "end",
                    "dur s",
                    "max el",
                    "az",
                    "range km",
                    "lat ms"
                );
                for x in c.windows.iter().take(limit) {
                    println!(
                        "  {:<26} {:<19} {:<19} {:>7.0} {:>6.1} {:>6.0} {:>8.0} {:>8}",
                        trunc(&x.satellite, 26),
                        fmt_t(x.start, off),
                        fmt_t(x.end, off),
                        x.duration_s,
                        x.peak_elevation_deg,
                        x.peak_azimuth_deg,
                        x.min_range_km,
                        x.latency_ms_at_peak
                            .map(|l| format!("{l:.0}"))
                            .unwrap_or_else(|| "-".into())
                    );
                }
                if !c.outages.is_empty() {
                    let shown: Vec<String> = c
                        .outages
                        .iter()
                        .take(6)
                        .map(|g| format!("{}+{:.0}s", fmt_t(g.start, off), g.duration_s()))
                        .collect();
                    println!(
                        "  outages ({}): {}{}",
                        c.outages.len(),
                        shown.join(", "),
                        if c.outages.len() > 6 { ", ..." } else { "" }
                    );
                }
            }
            Ok(())
        }
        Cmd::Gnss {
            lat,
            lon,
            alt_m,
            window: w,
            step,
            mask,
            class,
        } => {
            let (from, to) = window(&w)?;
            let space = ctx.space()?;
            let rep = space
                .gnss(&GnssQuery {
                    point: Geodetic::new(lat, lon, alt_m / 1000.0),
                    from,
                    to,
                    step_s: step,
                    class,
                    mask_deg: mask,
                    epoch_mode: cli.epoch,
                })
                .map_err(|e| e.to_string())?;
            if json {
                return print_json(&rep);
            }
            let off = offset_for(None);
            println!(
                "GNSS ({}) at {lat:.4},{lon:.4}, mask {:.0} deg, {} satellites ({} stale, {} launched later: skipped)",
                rep.class_id, rep.mask_deg, rep.satellites, rep.stale_skipped, rep.not_yet_launched
            );
            print_time_map(&rep.time_map, from, to);
            println!(
                "visible {}-{}, PDOP mean {} max {}; compute {} ms\n",
                rep.visible_min,
                rep.visible_max,
                rep.pdop_mean
                    .map(|p| format!("{p:.2}"))
                    .unwrap_or_else(|| "-".into()),
                rep.pdop_max
                    .map(|p| format!("{p:.2}"))
                    .unwrap_or_else(|| "-".into()),
                rep.compute_ms
            );
            println!(
                "{:<19} {:>4} {:>6} {:>6} {:>6} {:>6}",
                "time (local)", "vis", "GDOP", "PDOP", "HDOP", "VDOP"
            );
            for s in &rep.samples {
                match s.dop {
                    Some(d) => println!(
                        "{:<19} {:>4} {:>6.2} {:>6.2} {:>6.2} {:>6.2}",
                        fmt_t(s.t, off),
                        s.visible,
                        d.gdop,
                        d.pdop,
                        d.hdop,
                        d.vdop
                    ),
                    None => println!(
                        "{:<19} {:>4}      -      -      -      -",
                        fmt_t(s.t, off),
                        s.visible
                    ),
                }
            }
            Ok(())
        }
        Cmd::Positions {
            class,
            at,
            footprints,
        } => {
            let t = parse_time(&at)?;
            let space = ctx.space()?;
            let tm = space.time_map(cli.epoch, t, t);
            let pos = space
                .positions(&class, t, &tm, footprints)
                .map_err(|e| e.to_string())?;
            if json {
                return print_json(&serde_json::json!({"time_map": tm, "positions": pos}));
            }
            print_time_map(&tm, t, t);
            for p in &pos {
                println!(
                    "{:<26} {:>8.3} {:>9.3} {:>9.1} km",
                    trunc(&p.satellite, 26),
                    p.position.lat_deg,
                    p.position.lon_deg,
                    p.position.alt_km
                );
            }
            Ok(())
        }
    }
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<(), String> {
    let s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    println!("{s}");
    Ok(())
}

fn fmt_t(t: DateTime<Utc>, offset_h: f64) -> String {
    match FixedOffset::east_opt((offset_h * 3600.0) as i32) {
        Some(o) if offset_h != 0.0 => {
            t.with_timezone(&o).format("%m-%d %H:%M:%S").to_string() + &format!("{offset_h:+.0}")
        }
        _ => t.format("%m-%d %H:%M:%SZ").to_string(),
    }
}

fn print_time_map(tm: &TimeMap, from: DateTime<Utc>, to: DateTime<Utc>) {
    if tm.shift_days == 0 {
        println!(
            "epoch: direct propagation; elements reference {}; furthest instant {:.1} d from it",
            tm.reference.format("%Y-%m-%d %H:%MZ"),
            tm.max_offset_days(from, to)
        );
    } else {
        println!(
            "epoch: TIME-SHIFTED {:+} d (orbits propagated at {} to {} UTC, same time of day; sun at scenario time); elements reference {}",
            tm.shift_days,
            tm.to_prop(from).format("%Y-%m-%d %H:%M"),
            tm.to_prop(to).format("%Y-%m-%d %H:%M"),
            tm.reference.format("%Y-%m-%d %H:%MZ"),
        );
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "~"
    }
}

fn short_notes(notes: &[String], taskable: bool, covers: Option<bool>) -> String {
    let mut v = Vec::new();
    if !taskable {
        v.push("routine (not taskable)");
    }
    if notes.iter().any(|n| n.contains("estimate")) {
        v.push("timing est.");
    }
    if notes.iter().any(|n| n.contains("cloud")) {
        v.push("no cloud model");
    }
    if covers == Some(false) {
        v.push("partial area");
    }
    v.join(", ")
}
