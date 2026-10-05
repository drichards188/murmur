//! Shared fixtures: a Golden Gate world built from the real catalog, policy and area files.
#![allow(dead_code, clippy::unwrap_used)]

use std::path::PathBuf;

use murmur_decide::*;

pub const T0: Time = Time::from_secs(1_781_000_000);

pub fn sim_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../sim")
}

pub fn catalog() -> Catalog {
    Catalog::load(&sim_dir().join("platforms.toml")).unwrap()
}

pub fn policy() -> Policy {
    Policy::load(&sim_dir().join("policy.toml")).unwrap()
}

/// The sim policy with every human tier set to `submitter_authorizes = false`: a human's own
/// intent waits in the inbox like an agent's (how every run behaved before the switch, and
/// what a two-person tier does). Tests of the inbox mechanics use it.
pub fn waiting_policy() -> Policy {
    let mut p = policy();
    p.approval.submitter_authorizes = Some(false);
    for t in &mut p.tiers {
        t.submitter_authorizes = None;
    }
    p
}

pub fn areas() -> AreaSet {
    AreaSet::load_dir(&sim_dir().join("areas"), T0).unwrap()
}

/// Land south of 37.808 (San Francisco), water north of it, coverage over the scenario box.
pub fn terrain() -> PolygonTerrain {
    let b = |w: f64, s: f64, e: f64, n: f64| {
        Polygon::new(vec![
            LonLat::new(w, s),
            LonLat::new(e, s),
            LonLat::new(e, n),
            LonLat::new(w, n),
        ])
    };
    PolygonTerrain {
        coverage: b(-122.60, 37.70, -122.30, 37.95),
        land: vec![b(-122.60, 37.70, -122.30, 37.808)],
        water_depth_m: 20.0,
    }
}

pub const CRISSY: LonLat = LonLat::new(-122.4460, 37.8068);
pub const BAY: LonLat = LonLat::new(-122.4700, 37.8280);
pub const NEAR_CRISSY: LonLat = LonLat::new(-122.4500, 37.8040);
pub const ON_WATER: LonLat = LonLat::new(-122.4650, 37.8250);
pub const OUTSIDE_APPROVED: LonLat = LonLat::new(-122.60, 37.80);

/// A vehicle of catalog platform `platform`, healthy, with every fact fresh at `T0`.
pub fn vehicle(platform: &str, at: LonLat) -> VehicleFacts {
    let p = catalog().get(platform).cloned().unwrap();
    let ap = p.autopilot.unwrap_or(AutopilotKind::Drifter);
    VehicleFacts {
        platform: Some(p),
        autopilot: Some(Fact::new(ap, T0)),
        nav_ready: Some(Fact::new(true, T0)),
        position: Some(Fact::new(at, T0)),
        home: Some(Fact::new(at, T0)),
        battery_pct: Some(Fact::new(95.0, T0)),
        nav_source: None,
        report_interval_ms: None,
        velocity_mps: None,
        gps_fix: None,
        gps_glitch: None,
        gps_fix_from: None,
        circling: None,
        alt_msl_m: None,
        booted_t: None,
    }
}

/// sky-1 x10 copter, blade-1 switchblade, hawk-1 RQ-4 stand-in, boat-1 Sea Hunter stand-in,
/// mule-1 logistics UGV, track-1 antenna tracker, buoy-1 drifter, remus-1 AUV.
pub fn world() -> WorldSnapshot {
    let mut w = WorldSnapshot::new(T0);
    w.areas = Some(areas());
    for (uid, platform, at) in [
        ("sky-1", "x10", CRISSY),
        ("blade-1", "switchblade", CRISSY),
        ("hawk-1", "rq4", CRISSY),
        ("boat-1", "seahunter", BAY),
        ("mule-1", "ugv_logistics", CRISSY),
        ("track-1", "tracker", CRISSY),
        ("buoy-1", "buoy", BAY),
        ("remus-1", "remus", BAY),
    ] {
        w.vehicles.insert(Uid::new(uid), vehicle(platform, at));
    }
    w
}

pub fn uid(s: &str) -> Uid {
    Uid::new(s)
}

pub fn orbit(center: LonLat, radius_m: f64, uids: &[&str]) -> Proposal {
    Proposal::new(
        Task::Orbit { center, radius_m },
        uids.iter().map(|u| uid(u)).collect(),
    )
}

pub fn hold(uids: &[&str]) -> Proposal {
    Proposal::new(Task::Hold {}, uids.iter().map(|u| uid(u)).collect())
}

pub fn rtl(uids: &[&str]) -> Proposal {
    Proposal::new(Task::Rtl {}, uids.iter().map(|u| uid(u)).collect())
}

pub fn spec(source: Source, p: Proposal) -> IntentSpec {
    IntentSpec {
        id: IntentId(1),
        created_t: T0,
        source,
        task: p.task,
        uids: p.uids,
        constraints: p.constraints,
        rationale: p.rationale,
    }
}

pub fn human() -> Source {
    Source::Human {
        id: OperatorId::new("op-1"),
    }
}

pub fn agent() -> Source {
    Source::Agent {
        id: AgentId::new("planner"),
    }
}

/// The operators the tests act as (the bridge's `MURMUR_OPERATORS`), each also on the
/// engagement roster (`MURMUR_ENGAGE_OPERATORS`): a session carries engagement authority only
/// when opened with it.
pub fn roster() -> Roster {
    let ops: Vec<OperatorId> = ["op-1", "op-2", "op-3", "op-9", "op-late", "op", "me"]
        .map(OperatorId::new)
        .to_vec();
    Roster::new(ops.clone(), ops).unwrap()
}

pub fn engine(clock: &ManualClock) -> (Engine, MemorySink) {
    let log = MemorySink::new();
    let e = Engine::new(policy(), roster(), Box::new(clock.clone()), Box::new(log.clone())).unwrap();
    (e, log)
}

/// [`engine`] on [`waiting_policy`].
pub fn waiting_engine(clock: &ManualClock) -> (Engine, MemorySink) {
    let log = MemorySink::new();
    let e = Engine::new(
        waiting_policy(),
        roster(),
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .unwrap();
    (e, log)
}

pub fn rationale(s: &str) -> Rationale {
    Rationale::new(s).unwrap()
}

/// A forger who can write the file: parse the log, edit its events, renumber them and
/// recompute every hash, so the chain verifies and only recomputation can catch the edit.
pub fn forge(text: &str, edit: impl FnOnce(&mut Vec<Event>)) -> String {
    let mut events = parse_log(text).unwrap();
    edit(&mut events);
    let mut prev = murmur_decide::hash::GENESIS.to_string();
    let mut out = String::new();
    for (i, e) in events.iter_mut().enumerate() {
        e.seq = i as u64 + 1;
        e.prev = prev.clone();
        let line = serde_json::to_string(&*e).unwrap();
        prev = murmur_decide::hash::sha256_hex(line.as_bytes());
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Rules 9: a declaration is the engine's identification ledger, not a field of the picture
/// the bridge hands it (the engine fills every decision's tracks from the ledger). These
/// tests' hand-built pictures say who declared what (`declared_by`): declare each such track
/// through the engine, by its operator, in a session of their own (a password one in a
/// credentials run). Nothing before rules 9.
pub fn declare_shown(e: &mut Engine, world: &WorldSnapshot) {
    if !e.policy().rules.rule_id() {
        return;
    }
    for (id, t) in &world.tracks {
        let Some(by) = t.declared_by.clone() else { continue };
        if e.identification()
            .declaration(id)
            .is_some_and(|d| d.affiliation == t.affiliation.value)
        {
            continue;
        }
        let s = match e.auth_mode() {
            Some(AuthMode::Credentials) => e
                .open_authenticated_session(OperatorId::new(&by), Grants::default(), AuthMethod::Password)
                .unwrap(),
            _ => e.open_operator_session(OperatorId::new(&by)).unwrap(),
        };
        if e.identification().declaration(id).is_some() {
            e.revoke_declaration(s.authority(), id, None).unwrap();
        }
        e.declare_track(s.authority(), None, id, t.affiliation.value, None, None, world)
            .unwrap();
    }
}
