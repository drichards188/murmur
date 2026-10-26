//! Predicted link coverage (bd murmur-hei, docs/LINK-COVERAGE.md): what the emulator's own
//! deterministic geometry says a link would be at a point, an altitude and (SATCOM) a time,
//! before any vehicle goes there, and along a route.
//!
//! The predictor reads the same links file and computes with the same functions as the
//! emulator's packet path (`link.rs`, `Link::set_position` and `Link::cond`): the profile a
//! vehicle starts on (`Config::profile_for`), the depth gate and the gated profile that carries
//! the traffic while it is closed, the range curve ([`range_loss`]), the terrain obstruction from
//! a [`Terrain`] (with `terrain::TidewaterTerrain`, the emulator's own cells, altitude bands and
//! profile sampling) and what the profile's terrain mode makes of it ([`terrain_effect`]),
//! SATCOM visibility from a `sat::SpaceSource` answer (down while no satellite of the class is
//! above the mask), and the profile's own long-run loss ([`Params::mean_loss`]). It draws
//! nothing: the emulator's random draws (each packet's loss) decide what happens; this says what
//! they are drawn against.
//!
//! Not predicted: scripted events (cuts, jammers, extra loss: a scenario's, not known ahead), the
//! brief SATCOM handover outages, queueing and latency, and a landed aircraft's at-rest terrain
//! check (`Link::at_rest`).
//!
//! States: up below [`DEGRADED_LOSS`], degraded from it, down from [`DOWN_LOSS`] or wherever the
//! emulator would block every packet (terrain in `block` mode, no satellite, the depth gate with
//! no gated profile). The bridge's live link badges use the same two thresholds
//! (crates/murmur-bridge/src/emu.rs).
//!
//! A route ([`route_answer`]): its legs sampled every [`ROUTE_STEP_M`], and the vehicle's
//! lost-link failsafe walked heartbeat by heartbeat at the vehicle's speed ([`Walk`]): it trips
//! when no GCS heartbeat arrives for `lost_link_s` (ArduPilot's rule, the stand-ins'), each
//! heartbeat lost with the predicted loss where the vehicle then is, independently of the
//! others (as the emulator draws range and terrain loss for each packet). Exact for that model;
//! the channel's own short bursts (`burst_len` packets) are left out.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::config::{Config, DownWhen, VehicleSpec};
use crate::geo::{GeoPoint, LosParams, Terrain, haversine_m, slant_m};
use crate::model::{Params, range_loss, terrain_effect};
use crate::sat::SatObs;

/// Predicted loss at or above this is degraded (the bridge's live badge uses the same).
pub const DEGRADED_LOSS: f64 = 0.2;
/// Predicted loss at or above this is down, as is a link the emulator would block.
pub const DOWN_LOSS: f64 = 0.95;
/// A route's legs are predicted at points this far apart, m (the emulator's terrain answers are
/// per 250 m cell: five points a cell).
pub const ROUTE_STEP_M: f64 = 50.0;
/// A route answer's time on station: the failsafe's chance is over the way there and this long
/// on the task path (its laps flown again), s.
pub const ON_STATION_S: f64 = 300.0;
/// A route holds when its failsafe's chance (on the way and [`ON_STATION_S`] on station) is at
/// most this.
pub const HOLDS_P: f64 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Up,
    Degraded,
    Down,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Up => "up",
            State::Degraded => "degraded",
            State::Down => "down",
        }
    }

    /// One letter a cell in a coverage grid: `u`, `d`, `x`.
    pub fn letter(self) -> char {
        match self {
            State::Up => 'u',
            State::Degraded => 'd',
            State::Down => 'x',
        }
    }

    pub fn of_loss(p_loss: f64, blocked: bool) -> State {
        if blocked || p_loss >= DOWN_LOSS {
            State::Down
        } else if p_loss >= DEGRADED_LOSS {
            State::Degraded
        } else {
            State::Up
        }
    }
}

/// Why a link is degraded or down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// The profile's own channel loss.
    Loss,
    /// Range from the ground station.
    Range,
    /// Terrain between the vehicle and the ground station.
    Terrain,
    /// SATCOM: no satellite of the profile's class above the terminal's mask.
    NoSat,
    /// The depth gate: a link that is down while the vehicle is submerged (SATCOM, radio).
    Submerged,
    /// The depth gate: a link that is down while the vehicle is at the surface (an acoustic
    /// modem with no radio to carry it).
    Surfaced,
}

impl Reason {
    pub fn name(self) -> &'static str {
        match self {
            Reason::Loss => "loss",
            Reason::Range => "range",
            Reason::Terrain => "terrain",
            Reason::NoSat => "no_sat",
            Reason::Submerged => "submerged",
            Reason::Surfaced => "surfaced",
        }
    }

    /// One letter a cell in a coverage grid (`-` for none): `l`, `r`, `t`, `s`, `w`, `a`.
    pub fn letter(self) -> char {
        match self {
            Reason::Loss => 'l',
            Reason::Range => 'r',
            Reason::Terrain => 't',
            Reason::NoSat => 's',
            Reason::Submerged => 'w',
            Reason::Surfaced => 'a',
        }
    }

    pub fn words(self) -> &'static str {
        match self {
            Reason::Loss => "its link's own loss",
            Reason::Range => "out of its radio's range",
            Reason::Terrain => "behind terrain from the ground station",
            Reason::NoSat => "no satellite of its class in view",
            Reason::Submerged => "submerged",
            Reason::Surfaced => "at the surface",
        }
    }
}

/// What the SATCOM part of a prediction rests on.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SatNote {
    pub class: String,
    /// Following the constellation (an answer from the satellites); false: the profile's static
    /// numbers, as the emulator keeps without element sets, and why in `note`.
    pub live: bool,
    /// Satellites of the class above the mask.
    pub visible: Option<usize>,
    pub note: Option<String>,
}

/// The predicted link at one point.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Prediction {
    pub state: State,
    /// The share of packets the emulator would lose there: 1 - (1 - own)(1 - range)(1 -
    /// terrain), or 1 where it blocks every packet.
    pub p_loss: f64,
    pub reason: Option<Reason>,
    /// The profile carrying the traffic there (its gated profile while the depth gate closes
    /// its own).
    pub profile: String,
    pub base_loss: f64,
    pub range_loss: f64,
    pub terrain_loss: f64,
    /// The emulator would drop every packet there.
    pub blocked: bool,
    /// Slant range to the ground station, m.
    pub range_m: Option<f64>,
    /// How far terrain stands above the ray to the ground station, m (negative: clear by that
    /// much); none where the profile does no terrain check.
    pub obstruction_m: Option<f64>,
    /// The terrain check had no answer for the point yet: counted open, as the emulator counts
    /// it until its answer comes.
    pub terrain_unknown: bool,
    pub satcom: Option<SatNote>,
}

/// One resolved profile.
#[derive(Clone, Debug)]
pub struct Profile {
    pub name: String,
    pub params: Params,
    /// The profile that carries the traffic while the depth gate closes this one.
    pub gated: Option<(String, Params)>,
    /// The vehicle's lost-link timer, s (`lost_link_s`; a gated profile's is not used).
    pub lost_link_s: Option<f64>,
}

/// Every profile of a links file, ready to predict with.
pub struct Predictor {
    cfg: Config,
    gs: Option<GeoPoint>,
    submerged_depth_m: f64,
    profiles: BTreeMap<String, Profile>,
}

impl Predictor {
    /// From a links file's configuration, its ground station altitude already settled
    /// (`Config::resolve_ground_alt`, as the emulator settles it before building its links).
    pub fn new(cfg: &Config) -> Result<Predictor, String> {
        let mut profiles = BTreeMap::new();
        for name in cfg.class.keys() {
            let raw = cfg.resolve(name)?;
            let params = Params::from_profile(&raw).map_err(|e| format!("profile {name}: {e}"))?;
            let gated = match &raw.gated_profile {
                Some(g) => {
                    let graw = cfg.resolve(g).map_err(|e| format!("profile {name}: {e}"))?;
                    Some((
                        g.clone(),
                        Params::from_profile(&graw).map_err(|e| format!("profile {g}: {e}"))?,
                    ))
                }
                None => None,
            };
            profiles.insert(
                name.clone(),
                Profile {
                    name: name.clone(),
                    params,
                    gated,
                    lost_link_s: raw.lost_link_s,
                },
            );
        }
        Ok(Predictor {
            cfg: cfg.clone(),
            gs: cfg.ground_station.as_ref().map(|g| GeoPoint {
                lat: g.lat,
                lon: g.lon,
                alt_m: g.alt_m.metres(),
            }),
            submerged_depth_m: cfg.submerged_depth_m,
            profiles,
        })
    }

    pub fn profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn profiles(&self) -> impl Iterator<Item = &Profile> {
        self.profiles.values()
    }

    /// The profile a vehicle starts on, as the emulator chooses it.
    pub fn profile_for(&self, v: &VehicleSpec) -> String {
        self.cfg.profile_for(v)
    }

    pub fn ground_station(&self) -> Option<GeoPoint> {
        self.gs
    }

    /// Submerged as the emulator reads it: deeper than `submerged_depth_m`.
    pub fn submerged(&self, p: &GeoPoint) -> bool {
        -p.alt_m > self.submerged_depth_m
    }

    fn gate_closed(&self, own: &Params, p: &GeoPoint) -> bool {
        match own.down_when {
            Some(DownWhen::Submerged) => self.submerged(p),
            Some(DownWhen::Surfaced) => !self.submerged(p),
            None => false,
        }
    }

    /// The SATCOM class whose satellites decide the link at `p`: none for a profile without
    /// one, or where its depth gate closes it.
    pub fn sat_class(&self, profile: &str, p: &GeoPoint) -> Option<&str> {
        let own = &self.profiles.get(profile)?.params;
        if self.gate_closed(own, p) {
            return None;
        }
        own.space_class.as_deref()
    }

    /// The line-of-sight parameters the terrain check of `profile` asks with (none: no check),
    /// for a caller that fetches the terrain before predicting.
    pub fn los(&self, profile: &str) -> Option<LosParams> {
        self.gs?;
        self.profiles.get(profile)?.params.terrain.map(|t| t.los)
    }

    /// The link of a vehicle on `profile` at `p` (altitude above mean sea level; negative is
    /// depth). `terrain` answers the obstruction (asked as the emulator's link asks it);
    /// `sat` is the constellation's answer at `p` for [`Predictor::sat_class`], if any.
    pub fn predict(
        &self,
        profile: &str,
        p: &GeoPoint,
        terrain: &dyn Terrain,
        sat: Option<&Result<SatObs, String>>,
    ) -> Result<Prediction, String> {
        let prof = self
            .profiles
            .get(profile)
            .ok_or_else(|| format!("unknown link profile {profile:?}"))?;
        let own = &prof.params;
        let submerged = self.submerged(p);
        let gate = self.gate_closed(own, p);
        let (active_name, active) = match (&prof.gated, gate) {
            (Some((n, g)), true) => (n.clone(), g),
            _ => (prof.name.clone(), own),
        };
        // the order of `Link::cond`: the gate, then the satellites, then the terrain
        let mut blocked: Option<Reason> = None;
        if gate && prof.gated.is_none() {
            blocked = Some(if submerged {
                Reason::Submerged
            } else {
                Reason::Surfaced
            });
        }
        let mut satcom = None;
        if !gate && let Some(class) = &own.space_class {
            satcom = Some(match sat {
                None => SatNote {
                    class: class.clone(),
                    live: false,
                    visible: None,
                    note: Some("no satellite data: the profile's static numbers, as the emulator keeps them".into()),
                },
                Some(Err(e)) => SatNote {
                    class: class.clone(),
                    live: false,
                    visible: None,
                    note: Some(format!("{e}: the profile's static numbers")),
                },
                Some(Ok(o)) => {
                    if o.best.is_none() {
                        blocked.get_or_insert(Reason::NoSat);
                    }
                    SatNote {
                        class: class.clone(),
                        live: true,
                        visible: Some(o.visible),
                        note: None,
                    }
                }
            });
        }
        let range_m = self.gs.map(|g| slant_m(p, &g));
        let rl = match (active.range, range_m) {
            (Some(r), Some(d)) => range_loss(r, d),
            _ => 0.0,
        };
        // the obstruction is asked with the vehicle's own profile's line of sight (as
        // `Link::set_position` asks it), its effect is the profile carrying the traffic's
        let mut obstruction_m = None;
        let mut terrain_unknown = false;
        if let (Some(t), Some(g)) = (&own.terrain, self.gs)
            && terrain.enabled()
        {
            match terrain.obstruction_m(p, &g, &t.los) {
                Some(o) => obstruction_m = Some(o),
                None => terrain_unknown = true,
            }
        }
        let mut tl = 0.0;
        if let (Some(t), Some(o)) = (&active.terrain, obstruction_m) {
            let (b, l) = terrain_effect(t, o);
            if b {
                blocked.get_or_insert(Reason::Terrain);
            }
            tl = l;
        }
        let base = active.mean_loss();
        let p_loss = if blocked.is_some() {
            1.0
        } else {
            1.0 - (1.0 - base) * (1.0 - rl) * (1.0 - tl)
        };
        let state = State::of_loss(p_loss, blocked.is_some());
        let reason = blocked.or(if state == State::Up {
            None
        } else {
            [(rl, Reason::Range), (tl, Reason::Terrain), (base, Reason::Loss)]
                .into_iter()
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, r)| r)
        });
        Ok(Prediction {
            state,
            p_loss,
            reason,
            profile: active_name,
            base_loss: base,
            range_loss: rl,
            terrain_loss: tl,
            blocked: blocked.is_some(),
            range_m,
            obstruction_m,
            terrain_unknown,
            satcom,
        })
    }
}

// ---- routes -----------------------------------------------------------------------------------

/// Points every `step_m` along a straight leg, both ends included, each with how far along the
/// leg it is (m); latitude, longitude and altitude linear between the ends.
pub fn along(from: &GeoPoint, to: &GeoPoint, step_m: f64) -> Vec<(GeoPoint, f64)> {
    let len = haversine_m(from, to);
    if !len.is_finite() {
        return vec![(*from, 0.0)];
    }
    let n = (len / step_m.max(1.0)).ceil().max(1.0) as usize;
    (0..=n)
        .map(|i| {
            let f = i as f64 / n as f64;
            (
                GeoPoint {
                    lat: from.lat + (to.lat - from.lat) * f,
                    lon: from.lon + (to.lon - from.lon) * f,
                    alt_m: from.alt_m + (to.alt_m - from.alt_m) * f,
                },
                len * f,
            )
        })
        .collect()
}

/// Which part of an order's route a leg is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Part {
    /// From where the vehicle is to the task's first point.
    Transit,
    /// The task path itself (an orbit's ring, a patrol's loop: one lap).
    Task,
    /// From where the task ends to home (an rtl's whole route).
    Return,
    /// No route: the vehicle stays where it is (hold, track).
    Here,
}

impl Part {
    pub fn words(self) -> &'static str {
        match self {
            Part::Transit => "the transit leg",
            Part::Task => "the task path",
            Part::Return => "the return leg",
            Part::Here => "where it is",
        }
    }
}

/// One leg of a route, predicted at its points (in travel order, `along_m` rising from 0).
#[derive(Clone, Debug)]
pub struct LegSamples {
    pub part: Part,
    pub name: String,
    /// The task path is flown again and again (an orbit, a patrol).
    pub looped: bool,
    pub length_m: f64,
    pub samples: Vec<(f64, GeoPoint, Prediction)>,
}

/// A stretch of a leg in one state.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Stretch {
    pub state: State,
    pub from_m: f64,
    pub to_m: f64,
    pub seconds: f64,
    pub reason: Option<Reason>,
    pub max_p_loss: f64,
}

/// Where a leg first goes down, and for how long.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FirstDown {
    pub along_m: f64,
    /// [lon, lat]
    pub at: [f64; 2],
    pub alt_m: f64,
    pub reason: Option<Reason>,
    /// How long it stays down from there at the vehicle's speed, s.
    pub seconds: f64,
    /// That is at least the failsafe's time: the failsafe trips there.
    pub trips: bool,
}

/// One leg's predicted link.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LegAnswer {
    pub part: Part,
    pub name: String,
    pub looped: bool,
    pub length_m: f64,
    pub duration_s: f64,
    pub max_p_loss: f64,
    pub stretches: Vec<Stretch>,
    pub first_down: Option<FirstDown>,
    /// The first stretch down at least as long as the failsafe's time (where it trips).
    pub trip_down: Option<FirstDown>,
    /// Seconds down on the leg (a lap of a loop).
    pub down_s: f64,
    /// The chance the failsafe trips on this leg flown alone from a fresh heartbeat (a lap of
    /// a loop).
    pub p_failsafe: Option<f64>,
    /// Points whose terrain had no answer (counted open).
    pub terrain_unknown: usize,
}

/// A vehicle's lost-link failsafe: its timer and the GCS heartbeat period on its link.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Failsafe {
    pub lost_link_s: f64,
    pub heartbeat_s: f64,
}

impl Failsafe {
    /// Heartbeats lost in a row that trip it: the gap after `k` lost is (k + 1) periods, and the
    /// failsafe acts once the gap exceeds its timer.
    pub fn losses_to_trip(&self) -> usize {
        let h = self.heartbeat_s.max(1e-3);
        ((self.lost_link_s / h).floor() as usize).max(1)
    }
}

/// The failsafe walked heartbeat by heartbeat: the chance of each run of lost heartbeats, and of
/// having tripped.
#[derive(Clone, Debug)]
pub struct Walk {
    run: Vec<f64>,
    tripped: f64,
}

impl Walk {
    pub fn new(f: &Failsafe) -> Walk {
        let mut run = vec![0.0; f.losses_to_trip()];
        run[0] = 1.0;
        Walk { run, tripped: 0.0 }
    }

    /// One heartbeat, lost with chance `q`.
    pub fn step(&mut self, q: f64) {
        let q = q.clamp(0.0, 1.0);
        let k = self.run.len();
        let mut next = vec![0.0; k];
        let mut alive = 0.0;
        for (i, p) in self.run.iter().enumerate() {
            alive += p * (1.0 - q);
            if i + 1 < k {
                next[i + 1] += p * q;
            } else {
                self.tripped += p * q;
            }
        }
        next[0] += alive;
        self.run = next;
    }

    pub fn tripped(&self) -> f64 {
        self.tripped.min(1.0)
    }
}

/// The loss at `along_m` on a leg: its nearest predicted point's.
fn loss_at(leg: &LegSamples, along_m: f64) -> f64 {
    let s = &leg.samples;
    if s.is_empty() {
        return 0.0;
    }
    let i = s.partition_point(|x| x.0 < along_m);
    let best = [i.saturating_sub(1), i.min(s.len() - 1)]
        .into_iter()
        .min_by(|a, b| (s[*a].0 - along_m).abs().total_cmp(&(s[*b].0 - along_m).abs()))
        .unwrap_or(0);
    s[best].2.p_loss
}

/// Walk `seconds` of a leg (from its start, again and again for a loop) at `speed_mps`.
/// Returns where on the leg (m along, the last lap's for a loop) the chance of having tripped
/// first reached one half, if it did on this walk.
fn walk_leg(w: &mut Walk, leg: &LegSamples, speed_mps: f64, f: &Failsafe, seconds: f64) -> Option<f64> {
    let h = f.heartbeat_s.max(1e-3);
    let steps = (seconds / h).floor() as usize;
    let mut half = None;
    for j in 0..steps {
        let mut d = j as f64 * h * speed_mps;
        if leg.length_m > 0.0 {
            if leg.looped {
                d %= leg.length_m;
            } else {
                d = d.min(leg.length_m);
            }
        }
        let before = w.tripped();
        w.step(loss_at(leg, d));
        if half.is_none() && before < 0.5 && w.tripped() >= 0.5 {
            half = Some(d);
        }
    }
    half
}

/// One leg's answer at `speed_mps`.
pub fn leg_answer(leg: &LegSamples, speed_mps: f64, f: Option<&Failsafe>) -> LegAnswer {
    let v = speed_mps.max(0.1);
    let s = &leg.samples;
    // each point stands for the stretch halfway to its neighbours
    let bound = |i: usize| -> (f64, f64) {
        let a = if i == 0 { 0.0 } else { 0.5 * (s[i - 1].0 + s[i].0) };
        let b = if i + 1 >= s.len() {
            leg.length_m.max(s[i].0)
        } else {
            0.5 * (s[i].0 + s[i + 1].0)
        };
        (a, b)
    };
    let mut stretches: Vec<Stretch> = Vec::new();
    for (i, (_, _, p)) in s.iter().enumerate() {
        let (a, b) = bound(i);
        match stretches.last_mut() {
            Some(last) if last.state == p.state && (p.state == State::Up || last.reason == p.reason) => {
                last.to_m = b;
                last.max_p_loss = last.max_p_loss.max(p.p_loss);
            }
            _ => stretches.push(Stretch {
                state: p.state,
                from_m: a,
                to_m: b,
                seconds: 0.0,
                reason: p.reason,
                max_p_loss: p.p_loss,
            }),
        }
    }
    // a down stretch is one however its reason changes (terrain, then range)
    let mut merged: Vec<Stretch> = Vec::new();
    for st in stretches {
        match merged.last_mut() {
            Some(last) if last.state == State::Down && st.state == State::Down => {
                last.to_m = st.to_m;
                last.max_p_loss = last.max_p_loss.max(st.max_p_loss);
            }
            _ => merged.push(st),
        }
    }
    for st in &mut merged {
        st.seconds = (st.to_m - st.from_m) / v;
        st.max_p_loss = round_to(st.max_p_loss, 3);
        st.from_m = round_to(st.from_m, 0);
        st.to_m = round_to(st.to_m, 0);
        st.seconds = round_to(st.seconds, 1);
    }
    // where a down stretch starts: its first point down at or after the stretch's start
    let down_at = |st: &Stretch| -> FirstDown {
        let i = s
            .iter()
            .position(|x| x.2.state == State::Down && x.0 + ROUTE_STEP_M >= st.from_m)
            .unwrap_or(0);
        let (along, p, pred) = &s[i];
        FirstDown {
            along_m: round_to(*along, 0),
            at: [round_to(p.lon, 6), round_to(p.lat, 6)],
            alt_m: round_to(p.alt_m, 1),
            reason: pred.reason,
            seconds: st.seconds,
            trips: f.is_some_and(|f| st.seconds >= f.lost_link_s),
        }
    };
    let first_down = merged.iter().find(|st| st.state == State::Down).map(down_at);
    let trip_down = merged
        .iter()
        .find(|st| st.state == State::Down && f.is_some_and(|f| st.seconds >= f.lost_link_s))
        .map(down_at);
    let down_s = merged
        .iter()
        .filter(|st| st.state == State::Down)
        .map(|st| st.seconds)
        .sum::<f64>();
    let p_failsafe = f.map(|f| {
        let mut w = Walk::new(f);
        let _ = walk_leg(&mut w, leg, v, f, leg.length_m / v);
        round_to(w.tripped(), 3)
    });
    LegAnswer {
        part: leg.part,
        name: leg.name.clone(),
        looped: leg.looped,
        length_m: round_to(leg.length_m, 0),
        duration_s: round_to(leg.length_m / v, 1),
        max_p_loss: round_to(s.iter().map(|x| x.2.p_loss).fold(0.0, f64::max), 3),
        stretches: merged,
        first_down,
        trip_down,
        down_s: round_to(down_s, 1),
        p_failsafe,
        terrain_unknown: s.iter().filter(|x| x.2.terrain_unknown).count(),
    }
}

/// A route's predicted link.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RouteAnswer {
    pub legs: Vec<LegAnswer>,
    pub speed_mps: f64,
    pub failsafe_s: Option<f64>,
    pub heartbeat_s: Option<f64>,
    /// The chance the failsafe trips on the order's way (its transit, then [`ON_STATION_S`] on
    /// its task path; an rtl's return leg; [`ON_STATION_S`] where a hold leaves it).
    pub p_failsafe: Option<f64>,
    /// `p_failsafe` is at most [`HOLDS_P`] (with no failsafe timer: nothing on the way is down).
    pub holds: bool,
    /// Where the link first goes down on the way.
    pub first_down: Option<RouteDown>,
    /// Where it first goes down for at least the failsafe's time on the way: the failsafe trips
    /// there.
    pub trip_down: Option<RouteDown>,
    /// Where on the way the failsafe has more likely tripped than not (the walk's median), if it
    /// does.
    pub likely_trip: Option<TripAt>,
}

/// Where on a route the failsafe has more likely tripped than not.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TripAt {
    pub part: Part,
    pub leg: String,
    /// Along the leg (a loop's lap), m.
    pub along_m: f64,
}

/// A place on a route where the link goes down: which part, and where on it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RouteDown {
    pub part: Part,
    pub leg: String,
    #[serde(flatten)]
    pub down: FirstDown,
}

/// The legs an order's way is judged on: the transit and the task path; the return only when it
/// is the whole order (an rtl); the point where a hold leaves it.
fn on_the_way(legs: &[LegSamples]) -> Vec<&LegSamples> {
    let own: Vec<&LegSamples> = legs.iter().filter(|l| l.part != Part::Return).collect();
    if own.is_empty() {
        legs.iter().collect()
    } else {
        own
    }
}

/// The answer for a route's legs (in travel order) at `speed_mps`, with the vehicle's failsafe.
pub fn route_answer(legs: &[LegSamples], speed_mps: f64, f: Option<&Failsafe>) -> RouteAnswer {
    let v = speed_mps.max(0.1);
    let answers: Vec<LegAnswer> = legs.iter().map(|l| leg_answer(l, v, f)).collect();
    let way = on_the_way(legs);
    let mut likely_trip = None;
    let p_failsafe = f.map(|f| {
        let mut w = Walk::new(f);
        for leg in &way {
            let seconds = match leg.part {
                Part::Task | Part::Here => ON_STATION_S,
                _ => leg.length_m / v,
            };
            if let Some(d) = walk_leg(&mut w, leg, v, f, seconds)
                && likely_trip.is_none()
            {
                likely_trip = Some(TripAt {
                    part: leg.part,
                    leg: leg.name.clone(),
                    along_m: round_to(d, 0),
                });
            }
        }
        round_to(w.tripped(), 3)
    });
    let on_way = |pick: fn(&LegAnswer) -> Option<FirstDown>| {
        way.iter().find_map(|leg| {
            answers
                .iter()
                .find(|a| a.name == leg.name && a.part == leg.part)
                .and_then(|a| {
                    pick(a).map(|down| RouteDown {
                        part: a.part,
                        leg: a.name.clone(),
                        down,
                    })
                })
        })
    };
    let first_down = on_way(|a| a.first_down.clone());
    let trip_down = on_way(|a| a.trip_down.clone());
    let holds = match p_failsafe {
        Some(p) => p <= HOLDS_P,
        None => first_down.is_none(),
    };
    RouteAnswer {
        legs: answers,
        speed_mps: round_to(v, 1),
        failsafe_s: f.map(|f| f.lost_link_s),
        heartbeat_s: f.map(|f| f.heartbeat_s),
        p_failsafe,
        holds,
        first_down,
        trip_down,
        likely_trip,
    }
}

/// "1.2 km", "300 m".
pub fn distance_words(m: f64) -> String {
    if m >= 1000.0 {
        format!("{:.1} km", m / 1000.0)
    } else {
        format!("{:.0} m", (m / 10.0).round() * 10.0)
    }
}

fn seconds_words(s: f64) -> String {
    if s >= 120.0 {
        format!("about {:.0} min", s / 60.0)
    } else if s >= 10.0 {
        format!("about {:.0} s", (s / 5.0).round() * 5.0)
    } else {
        format!("about {:.0} s", s.max(1.0))
    }
}

/// Where on a part a place is: "1.2 km along the transit leg", "at the start of the transit leg",
/// "300 m into the task path's lap".
fn place_words(part: Part, along_m: f64) -> String {
    match part {
        Part::Task if along_m < 1.0 => "on the task path".to_string(),
        Part::Task => format!("{} into a lap of the task path", distance_words(along_m)),
        Part::Here => "where it is".to_string(),
        _ if along_m < 1.0 => format!("at the start of {}", part.words()),
        _ => format!("{} along {}", distance_words(along_m), part.words()),
    }
}

/// The plain sentence for an answer about vehicle `who`: where on the way it would lose its link
/// for at least its failsafe's time (else where it first goes down), for how long, and against
/// its failsafe's time; or how degraded the link would be; or that it holds. Then the failsafe's
/// chance when it does not hold, a lap of the task path down, and the way home.
pub fn sentence(who: &str, a: &RouteAnswer) -> String {
    let fs = a.failsafe_s;
    let fs_words = |secs: f64| match fs {
        Some(t) if secs >= t => format!("; its failsafe trips after {t:.0} s"),
        Some(t) => format!(", shorter than its failsafe's {t:.0} s"),
        None => String::new(),
    };
    let lead = a.trip_down.as_ref().or(a.first_down.as_ref());
    let mut out = if let Some(rd) = lead {
        let d = &rd.down;
        let why = d.reason.map(|r| format!(" ({})", r.words())).unwrap_or_default();
        match rd.part {
            Part::Here => format!(
                "{who} would have no link where it is{why}{}.",
                fs.map(|t| format!("; its failsafe trips after {t:.0} s")).unwrap_or_default()
            ),
            part => format!(
                "{who} would lose its link {}{why}, for {}{}{}.",
                place_words(part, d.along_m),
                seconds_words(d.seconds),
                if part == Part::Task { " a lap" } else { "" },
                fs_words(d.seconds)
            ),
        }
    } else {
        let way: Vec<&LegAnswer> = a
            .legs
            .iter()
            .filter(|l| a.legs.iter().all(|x| x.part == Part::Return) || l.part != Part::Return)
            .collect();
        let worst = way.iter().map(|l| l.max_p_loss).fold(0.0, f64::max);
        let parts: Vec<&str> = way.iter().map(|l| l.part.words()).collect();
        if worst >= DEGRADED_LOSS {
            format!(
                "{who}'s link would be degraded on {} (loss up to {:.0} %).",
                parts.join(" and "),
                100.0 * worst
            )
        } else {
            format!(
                "{who}'s link is predicted to hold on {} (loss at most {:.0} %).",
                parts.join(" and "),
                100.0 * worst
            )
        }
    };
    if let Some(at) = &a.likely_trip
        && lead.is_none_or(|rd| rd.part != at.part || (rd.down.along_m - at.along_m).abs() > 2.0 * ROUTE_STEP_M)
    {
        out.push_str(&format!(" Its failsafe most likely trips {}.", place_words(at.part, at.along_m)));
    }
    if !a.holds
        && let (Some(p), Some(t)) = (a.p_failsafe, fs)
    {
        out.push_str(&format!(
            " Chance its failsafe ({t:.0} s) trips on the way and in {:.0} minutes on station: {:.0} %.",
            ON_STATION_S / 60.0,
            100.0 * p
        ));
    }
    // a lap of the task path down, when the lead was on the way there
    if lead.is_some_and(|rd| rd.part == Part::Transit)
        && let Some(task) = a.legs.iter().find(|l| l.part == Part::Task)
        && let Some(d) = task.trip_down.as_ref().or(task.first_down.as_ref())
    {
        if d.seconds + 1.0 >= task.duration_s {
            out.push_str(" On station it would have no link the whole lap.");
        } else {
            out.push_str(&format!(
                " On station it would be down {}, for {} a lap.",
                place_words(Part::Task, d.along_m),
                seconds_words(d.seconds)
            ));
        }
    }
    // the way home, when it is not the order's own way
    if a.legs.iter().any(|l| l.part != Part::Return)
        && let Some(r) = a.legs.iter().find(|l| l.part == Part::Return)
        && let Some(d) = r.trip_down.as_ref().or(r.first_down.as_ref())
    {
        out.push_str(&format!(
            " On the way home it would lose it {}, for {}.",
            place_words(Part::Return, d.along_m),
            seconds_words(d.seconds)
        ));
    }
    out
}

fn round_to(x: f64, n: i32) -> f64 {
    let f = 10f64.powi(n);
    (x * f).round() / f
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::TerrainCfg;
    use crate::link::{Env, Link, LinkMeta};
    use crate::mavpos::position_frame;
    use crate::model::Dir;
    use crate::sat::BestSat;
    use crate::terrain::{SampleFuture, Sampler, TidewaterTerrain};
    use std::sync::Arc;

    /// A DEM flat at sea level but for a north-south ridge at one longitude.
    struct Ridge {
        lon: f64,
        height: f64,
    }

    impl Sampler for Ridge {
        fn sample<'a>(&'a self, _t: f64, points: &'a [(f64, f64)]) -> SampleFuture<'a> {
            let out = points
                .iter()
                .map(|(_, lon)| Some(if (lon - self.lon).abs() < 0.0015 { self.height } else { 0.0 }))
                .collect();
            Box::pin(async move { Ok(out) })
        }
    }

    const LINKS: &str = r#"
        submerged_depth_m = 1.0
        [ground_station]
        lat = 37.8068
        lon = -122.446
        alt_m = 5.0
        [class.los]
        loss = 0.005
        burst_len = 3
        range_ok_km = 15
        range_max_km = 25
        range_exp = 2
        gs_antenna_m = 10
        terrain = "ramp"
        terrain_ramp_m = 20
        lost_link_s = 5
        [class.blocky]
        range_ok_km = 5
        range_max_km = 12
        terrain = "block"
        lost_link_s = 10
        [class.satcom]
        space_class = "wgs"
        loss = 0.01
        down_when = "submerged"
        lost_link_s = 120
        [class.acoustic]
        loss = 0.1
        down_when = "surfaced"
        gated_profile = "acoustic_surface"
        lost_link_s = 120
        [class.acoustic_surface]
        loss = 0.02
        [class.modem_only]
        down_when = "surfaced"
    "#;

    fn cfg() -> Config {
        Config::parse(LINKS).expect("links")
    }

    fn pt(lon: f64, lat: f64, alt_m: f64) -> GeoPoint {
        GeoPoint { lat, lon, alt_m }
    }

    /// A point on a line east-north-east from the ground station.
    fn on_line(lon: f64, alt_m: f64) -> GeoPoint {
        pt(lon, 37.8068 + (lon + 122.446) * 0.3, alt_m)
    }

    fn terrain() -> TidewaterTerrain {
        TidewaterTerrain::new(TerrainCfg {
            cell_m: 250.0,
            sample_spacing_m: 50.0,
            ..TerrainCfg::default()
        })
    }

    /// Answers every cell the points need (as the bridge does before it predicts).
    async fn warm(t: &TidewaterTerrain, s: &dyn Sampler, pred: &Predictor, profile: &str, pts: &[GeoPoint]) {
        let (Some(gs), Some(los)) = (pred.ground_station(), pred.los(profile)) else { return };
        for p in pts {
            let _ = t.obstruction_m(p, &gs, &los);
        }
        while t.sizes().0 > 0 {
            t.tick(s, 0.0).await;
        }
    }

    /// The prediction agrees with the emulator's own link at sampled points: range, the ridge's
    /// shadow and clear air, block and ramp modes. The two keep separate terrain caches over the
    /// same DEM, as the bridge's predictor and the emulator do.
    #[tokio::test]
    async fn agrees_with_the_emulators_link_at_sampled_points() {
        let c = cfg();
        let pred = Predictor::new(&c).expect("predictor");
        let ridge = Ridge { lon: -122.43, height: 120.0 };
        let mine = terrain();
        let theirs = Arc::new(terrain());
        let env = Arc::new(Env::new(&c, theirs.clone(), 1).expect("env"));
        let mut checked = 0;
        for profile in ["los", "blocky"] {
            let params = Params::from_profile(&c.resolve(profile).expect("p")).expect("params");
            let meta = LinkMeta {
                uid: 1,
                sysid: 1,
                port: 0,
                class: profile.into(),
                profile: profile.into(),
                domain: "air".into(),
                platform: "x".into(),
            };
            let mut link = Link::new(meta, params, env.clone());
            // far west, between the station and the ridge (clear), behind it low and high, far out
            // east on the range curve
            let mut pts = Vec::new();
            for lon in [-122.75, -122.44, -122.42, -122.40, -122.30, -122.20, -122.10] {
                for alt in [5.0, 60.0, 150.0, 400.0] {
                    pts.push(on_line(lon, alt));
                }
            }
            warm(&mine, &ridge, &pred, profile, &pts).await;
            for p in &pts {
                link.observe_up(&position_frame(0, p.lat, p.lon, p.alt_m));
            }
            while theirs.sizes().0 > 0 {
                theirs.tick(&ridge, 0.0).await;
            }
            for p in &pts {
                link.observe_up(&position_frame(0, p.lat, p.lon, p.alt_m));
                let cond = link.cond(Dir::Up, 0);
                let got = pred.predict(profile, p, &mine, None).expect("predict");
                assert!(!got.terrain_unknown);
                assert_eq!(got.blocked, cond.blocked.is_some(), "{profile} {p:?}");
                assert!((got.range_loss - cond.range_loss).abs() < 1e-12, "{profile} {p:?}");
                assert!((got.terrain_loss - cond.terrain_loss).abs() < 1e-12, "{profile} {p:?}");
                let want = if cond.blocked.is_some() {
                    1.0
                } else {
                    1.0 - (1.0 - link.params().mean_loss()) * (1.0 - cond.range_loss) * (1.0 - cond.terrain_loss)
                };
                assert!((got.p_loss - want).abs() < 1e-12);
                checked += 1;
            }
        }
        assert_eq!(checked, 56);
        // and the cases differ: the ridge's shadow down low, clear high above it, range far out
        // west (the ridge is east of the station)
        let low = pred.predict("los", &on_line(-122.40, 5.0), &mine, None).expect("p");
        assert_eq!((low.state, low.reason), (State::Down, Some(Reason::Terrain)), "{low:?}");
        let high = pred.predict("los", &on_line(-122.40, 400.0), &mine, None).expect("p");
        assert_eq!(high.state, State::Up, "{high:?}");
        let far = pred.predict("blocky", &on_line(-122.75, 400.0), &mine, None).expect("p");
        assert_eq!((far.state, far.reason), (State::Down, Some(Reason::Range)), "{far:?}");
        let blocked = pred.predict("blocky", &on_line(-122.40, 5.0), &mine, None).expect("p");
        assert!(blocked.blocked && blocked.reason == Some(Reason::Terrain), "{blocked:?}");
    }

    #[test]
    fn the_depth_gate_and_the_gated_profile() {
        let pred = Predictor::new(&cfg()).expect("predictor");
        let none = crate::geo::NoTerrain;
        let under = pt(-122.5, 37.8, -20.0);
        let surface = pt(-122.5, 37.8, 0.0);
        // SATCOM down while submerged
        let s = pred.predict("satcom", &under, &none, None).expect("p");
        assert_eq!((s.state, s.reason), (State::Down, Some(Reason::Submerged)));
        assert!(s.satcom.is_none(), "the gate closes it before the satellites");
        // the modem works under water; at the surface its radio carries the traffic
        let a = pred.predict("acoustic", &under, &none, None).expect("p");
        assert_eq!((a.state, a.profile.as_str()), (State::Up, "acoustic"));
        assert!((a.p_loss - 0.1).abs() < 1e-9, "{a:?}");
        let r = pred.predict("acoustic", &surface, &none, None).expect("p");
        assert_eq!((r.state, r.profile.as_str()), (State::Up, "acoustic_surface"));
        // a modem with no radio is down at the surface
        let m = pred.predict("modem_only", &surface, &none, None).expect("p");
        assert_eq!((m.state, m.reason), (State::Down, Some(Reason::Surfaced)));
        assert_eq!(pred.sat_class("satcom", &surface), Some("wgs"));
        assert_eq!(pred.sat_class("satcom", &under), None);
        assert!(pred.predict("nope", &surface, &none, None).is_err());
    }

    #[test]
    fn satcom_is_down_with_no_satellite_and_static_without_data() {
        let pred = Predictor::new(&cfg()).expect("predictor");
        let none = crate::geo::NoTerrain;
        let p = pt(-122.5, 37.8, 18_000.0);
        let up = Ok(SatObs {
            visible: 2,
            best: Some(BestSat {
                norad_id: 1,
                name: "A".into(),
                elevation_deg: 30.0,
                range_km: 38_000.0,
            }),
            latency_ms: None,
        });
        let got = pred.predict("satcom", &p, &none, Some(&up)).expect("p");
        assert_eq!(got.state, State::Up);
        assert_eq!(got.satcom.as_ref().and_then(|s| s.visible), Some(2));
        let down = Ok(SatObs {
            visible: 0,
            best: None,
            latency_ms: None,
        });
        let got = pred.predict("satcom", &p, &none, Some(&down)).expect("p");
        assert_eq!((got.state, got.reason), (State::Down, Some(Reason::NoSat)));
        let got = pred.predict("satcom", &p, &none, None).expect("p");
        assert_eq!(got.state, State::Up);
        assert!(got.satcom.as_ref().is_some_and(|s| !s.live && s.note.is_some()));
    }

    #[test]
    fn the_failsafe_walk_is_exact_at_its_ends() {
        let f = Failsafe {
            lost_link_s: 5.0,
            heartbeat_s: 1.0,
        };
        assert_eq!(f.losses_to_trip(), 5);
        // four heartbeats lost in a row: the gap is 5 s, not more: no trip
        let mut w = Walk::new(&f);
        for q in [0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0] {
            w.step(q);
        }
        assert_eq!(w.tripped(), 0.0);
        let mut w = Walk::new(&f);
        for _ in 0..5 {
            w.step(1.0);
        }
        assert!((w.tripped() - 1.0).abs() < 1e-12);
        // independent losses at one half: P(5 in a row within 5) = 1/32
        let mut w = Walk::new(&f);
        for _ in 0..5 {
            w.step(0.5);
        }
        assert!((w.tripped() - 1.0 / 32.0).abs() < 1e-12);
        // a clean link never trips
        let mut w = Walk::new(&f);
        for _ in 0..1000 {
            w.step(0.0);
        }
        assert_eq!(w.tripped(), 0.0);
    }

    fn up_or_down(along: f64, down: bool) -> (f64, GeoPoint, Prediction) {
        (
            along,
            pt(-122.4 + along * 1e-5, 37.8, 80.0),
            Prediction {
                state: if down { State::Down } else { State::Up },
                p_loss: if down { 1.0 } else { 0.0 },
                reason: down.then_some(Reason::Terrain),
                profile: "los".into(),
                base_loss: 0.0,
                range_loss: 0.0,
                terrain_loss: if down { 1.0 } else { 0.0 },
                blocked: false,
                range_m: None,
                obstruction_m: None,
                terrain_unknown: false,
                satcom: None,
            },
        )
    }

    /// A transit of 4 km down from 1.2 km to 2.6 km: at 20 m/s that is 70 s down, against a 5 s
    /// failsafe; the sentence says so.
    #[test]
    fn a_route_says_where_it_goes_down_for_how_long_and_against_the_failsafe() {
        let transit = LegSamples {
            part: Part::Transit,
            name: "transit leg (position to waypoint 1)".into(),
            looped: false,
            length_m: 4000.0,
            samples: (0..=80)
                .map(|i| {
                    let d = f64::from(i) * 50.0;
                    up_or_down(d, (1225.0..2625.0).contains(&d))
                })
                .collect(),
        };
        let lap = LegSamples {
            part: Part::Task,
            name: "task path".into(),
            looped: true,
            length_m: 1257.0,
            samples: (0..=25).map(|i| up_or_down(f64::from(i) * 50.0, false)).collect(),
        };
        let f = Failsafe {
            lost_link_s: 5.0,
            heartbeat_s: 1.0,
        };
        let a = route_answer(&[transit, lap], 20.0, Some(&f));
        let rd = a.first_down.clone().expect("down");
        assert_eq!(rd.part, Part::Transit);
        assert_eq!(a.trip_down.as_ref(), Some(&rd), "the first down is long enough to trip it");
        let d = rd.down;
        assert_eq!(d.along_m, 1250.0);
        assert!((d.seconds - 70.0).abs() < 0.01, "{d:?}");
        assert!(d.trips);
        assert_eq!(a.p_failsafe, Some(1.0));
        assert!(!a.holds);
        // five heartbeats lost at 20 m/s: it trips 100 m into the shadow
        let at = a.likely_trip.clone().expect("trips");
        assert_eq!(at.part, Part::Transit);
        assert!((1250.0..=1360.0).contains(&at.along_m), "{at:?}");
        assert_eq!(a.legs[1].p_failsafe, Some(0.0));
        let s = sentence("PUMA 2", &a);
        assert_eq!(
            s,
            "PUMA 2 would lose its link 1.2 km along the transit leg (behind terrain from the ground station), \
             for about 70 s; its failsafe trips after 5 s. Chance its failsafe (5 s) trips on the way and in 5 \
             minutes on station: 100 %."
        );
        // a down stretch shorter than the failsafe: down, said, but the failsafe holds
        let short = LegSamples {
            part: Part::Transit,
            name: "transit".into(),
            looped: false,
            length_m: 1000.0,
            samples: (0..=20)
                .map(|i| {
                    let d = f64::from(i) * 50.0;
                    up_or_down(d, (475.0..525.0).contains(&d))
                })
                .collect(),
        };
        let a = route_answer(std::slice::from_ref(&short), 20.0, Some(&f));
        assert!(a.holds && a.p_failsafe == Some(0.0), "{a:?}");
        assert!(a.trip_down.is_none());
        assert!(sentence("PUMA 1", &a).contains("shorter than its failsafe's 5 s"));
        // down briefly from the start, then for good at the far end, and a lap of the task path
        // down: the sentence leads with where the failsafe trips, and says the rest
        let start = LegSamples {
            part: Part::Transit,
            name: "transit".into(),
            looped: false,
            length_m: 3000.0,
            samples: (0..=60)
                .map(|i| {
                    let d = f64::from(i) * 50.0;
                    up_or_down(d, d < 25.0 || d > 1975.0)
                })
                .collect(),
        };
        let shadowed = LegSamples {
            part: Part::Task,
            name: "task path (one lap)".into(),
            looped: true,
            length_m: 1000.0,
            samples: (0..=20).map(|i| up_or_down(f64::from(i) * 50.0, true)).collect(),
        };
        let home = LegSamples {
            part: Part::Return,
            name: "return".into(),
            ..start.clone()
        };
        let a = route_answer(&[start, shadowed, home], 20.0, Some(&f));
        assert_eq!(a.first_down.as_ref().map(|d| d.down.along_m), Some(0.0));
        assert_eq!(a.trip_down.as_ref().map(|d| d.down.along_m), Some(2000.0));
        assert_eq!(
            sentence("PUMA 3", &a),
            "PUMA 3 would lose its link 2.0 km along the transit leg (behind terrain from the ground station), \
             for about 50 s; its failsafe trips after 5 s. Chance its failsafe (5 s) trips on the way and in 5 \
             minutes on station: 100 %. On station it would have no link the whole lap. On \
             the way home it would lose it 2.0 km along the return leg, for about 50 s."
        );
        // nothing down: it holds, and says so
        let clear = LegSamples {
            part: Part::Task,
            name: "task path".into(),
            looped: true,
            length_m: 1000.0,
            samples: (0..=20).map(|i| up_or_down(f64::from(i) * 50.0, false)).collect(),
        };
        let a = route_answer(&[clear], 20.0, Some(&f));
        assert!(a.holds);
        assert_eq!(sentence("PUMA 1", &a), "PUMA 1's link is predicted to hold on the task path (loss at most 0 %).");
    }

    #[test]
    fn points_along_a_leg_include_both_ends() {
        let a = pt(-122.45, 37.80, 10.0);
        let b = pt(-122.44, 37.80, 110.0);
        let pts = along(&a, &b, 50.0);
        let len = haversine_m(&a, &b);
        assert_eq!(pts.len(), (len / 50.0).ceil() as usize + 1);
        assert_eq!(pts[0].1, 0.0);
        assert!((pts.last().expect("end").1 - len).abs() < 1e-6);
        assert!((pts.last().expect("end").0.alt_m - 110.0).abs() < 1e-9);
    }
}
