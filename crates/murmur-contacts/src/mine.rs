//! Rules 27 (bd murmur-lbw8.4): our minefields on the truth side. The bridge hands each mine the
//! decision engine laid (`mine.laid`) back on the simulation channel (`mines`, in its answer to
//! `POST /api/sim/contacts`): its place, its field, the mine's catalog facts (what sets it off and
//! how near, its hit and damage figures, a torpedo's speed and run), and when it arms and goes
//! inert on this process's clock. Here every live mine waits for its trigger, and a contact, or
//! one of our vehicles, that meets it sets it off; the damage model (stage 2, [`crate::damage`])
//! draws what that does, on the mine's own stream. The C2 never reads any of it: the record goes
//! to the evaluation truth log (`mine_events`), a contact destroyed leaves the picture through
//! the sensors, and one of ours struck reports it itself (an `impacts` entry, which the bridge
//! turns into the vehicle's struck report).
//!
//! - **Pressure** (a TM-62M): a ground contact or vehicle of ours of a kind the mine lists figures
//!   for (a person is never one: 150 kg minimum) whose way in the last second passed within the
//!   mine's trigger reach plus its half-width ([`HALF_WIDTH_M`]) of it makes a pass; each pass
//!   draws the mine's hit chance (a wheel or track over it, docs/research/weapons.md 10.3) on
//!   `mine:<field>:<n>:<who>:<pass>`, and only a hit sets it off. Then the damage given a hit, and
//!   anything else within the warhead's blast radius draws for collateral damage.
//! - **Acoustic** (a Hammerhead): a subsurface contact or vehicle of ours of a kind it lists
//!   figures for within its trigger reach launches its torpedo at it (the mine is spent): the
//!   torpedo chases it at its speed ([`Run`], pure pursuit, its acoustic homing [12]) and strikes
//!   once within [`STRIKE_M`] of it, where the hit and damage are drawn on
//!   `mine:<field>:<n>:run`; a torpedo that runs its range out, or whose target leaves the water,
//!   goes inert (Hague VIII Article 1: a torpedo that misses becomes harmless).
//! - A mine is set off only between its arming and its inert time; past its inert time it is
//!   inert and dropped (`inert`).
//!
//! The contacts process and the headless simulator share it.

use std::collections::{BTreeMap, BTreeSet};

use murmur_standins::geo::distance_m;
use serde::{Deserialize, Serialize};

use crate::damage::{self, Conditions, DamageRecord, DamageState, Target, WeaponDamage};
use crate::effect::{DamageCtx, OursDamaged};
use crate::motion::Mover;
use crate::scenario::Domain;
use crate::truth::ImpactTruth;
use crate::weapon::OursAt;

/// The half-width of a vehicle passing over a pressure mine, m [E]: a car's or a light truck's
/// track is about 1.5 to 2 m, so a vehicle passes over a mine within about 1 m of its centreline.
pub const HALF_WIDTH_M: f64 = 1.0;

/// How near its target a mine's torpedo strikes, m [E]: its homing's last metres.
pub const STRIKE_M: f64 = 10.0;

/// One mine as the bridge hands it over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MineNotice {
    /// Its field (the lay order's intent) and its number in it.
    pub field: u64,
    pub n: u32,
    /// `[lon, lat]`.
    pub point: [f64; 2],
    /// Our vehicle that laid it (its uid as the bridge names it).
    #[serde(default)]
    pub layer: Option<String>,
    pub weapon: WeaponDamage,
    #[serde(default)]
    pub trigger: Option<String>,
    /// How near a contact of its domains sets it off, m.
    pub trigger_m: f64,
    /// A torpedo's speed, m/s (0 for a mine that is its own charge).
    #[serde(default)]
    pub speed_mps: f64,
    /// The domains it is set off by (`ground`, `subsurface`).
    pub targets: Vec<String>,
    /// When it arms and goes inert, this process's scenario time (unix s).
    pub arms_t: f64,
    pub inert_t: f64,
}

impl MineNotice {
    /// Its id in the record: `MF-<field>/<n>`.
    pub fn id(&self) -> String {
        format!("MF-{}/{}", self.field, self.n)
    }

    fn acoustic(&self) -> bool {
        self.targets.iter().any(|d| d == "subsurface")
    }

    fn sets_off(&self, domain: &str) -> bool {
        self.targets.iter().any(|d| d == domain)
    }
}

/// One of a mine's torpedoes running at its target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub mine: MineNotice,
    /// Where it is, `[lon, lat]`, how far it has run and what it chases.
    pub at: [f64; 2],
    pub run_m: f64,
    pub target: Victim,
}

/// What a mine is set off by: a contact (by id) or one of ours (by uid).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Victim {
    Contact(String),
    Ours(u64),
}

impl Victim {
    fn who(&self) -> String {
        match self {
            Victim::Contact(id) => id.clone(),
            Victim::Ours(u) => format!("ours:{u}"),
        }
    }
}

/// What a mine did, for the evaluation truth log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MineEvent {
    pub t: f64,
    pub mine: String,
    /// `pass` (a pressure mine's pass drawn, not set off), `detonated`, `launched` (an acoustic
    /// mine's torpedo), `struck` (the torpedo's hit draw), `ran_out` (the torpedo inert) or
    /// `inert` (the mine past its period).
    pub what: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub who: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub damage: Vec<DamageRecord>,
}

/// One second of the minefields: what happened, our vehicles hurt (for the vehicle simulation)
/// and the strikes on ours (for their struck reports).
#[derive(Debug, Default)]
pub struct MineStep {
    pub events: Vec<MineEvent>,
    pub ours_damaged: Vec<OursDamaged>,
    pub impacts: Vec<ImpactTruth>,
}

/// The live mines and the torpedoes they launched.
#[derive(Debug, Default)]
pub struct Minefields {
    mines: Vec<MineNotice>,
    runs: Vec<Run>,
    /// Where each contact and vehicle of ours was at the last step (a pass is swept over a second).
    last: BTreeMap<Victim, (f64, f64)>,
    /// Victims within a mine's zone at the last step (a new pass starts only on entering it), and
    /// each mine's passes so far by victim (each its own draw).
    inside: BTreeSet<(String, Victim)>,
    passes: BTreeMap<(String, Victim), u32>,
}

/// How far `p` is from the segment `a`-`b` (lat, lon), m, on a local plane.
fn segment_distance(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let k = 111_320.0;
    let c = p.0.to_radians().cos();
    let xy = |q: (f64, f64)| ((q.1 - p.1) * k * c, (q.0 - p.0) * k);
    let (ax, ay) = xy(a);
    let (bx, by) = xy(b);
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let f = if len2 > 0.0 { (-(ax * dx + ay * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (ax + dx * f).hypot(ay + dy * f)
}

fn domain_str(d: Domain) -> &'static str {
    match d {
        Domain::Air => "air",
        Domain::Surface => "surface",
        Domain::Subsurface => "subsurface",
        Domain::Ground => "ground",
    }
}

impl Minefields {
    /// Take a mine from the bridge (once; a second notice of the same mine is ignored).
    pub fn lay(&mut self, m: MineNotice) {
        if !self.mines.iter().any(|x| x.field == m.field && x.n == m.n) {
            self.mines.push(m);
        }
    }

    /// The domains the live mines are set off by at `now` (what the truth message asks the bridge
    /// for our vehicles in), and whether anything is held at all.
    pub fn live_domains(&self, now: f64) -> Vec<String> {
        let mut d: Vec<String> = self
            .mines
            .iter()
            .filter(|m| m.inert_t > now)
            .flat_map(|m| m.targets.iter().cloned())
            .chain(self.runs.iter().flat_map(|r| r.mine.targets.iter().cloned()))
            .collect();
        d.sort();
        d.dedup();
        d
    }

    pub fn is_empty(&self) -> bool {
        self.mines.is_empty() && self.runs.is_empty()
    }

    /// One step at scenario time `now` over `dt` seconds (the module doc): the mines past their
    /// period inert, the torpedoes run, then every armed mine against every contact and vehicle of
    /// ours of its domains.
    pub fn step(&mut self, now: f64, dt: f64, movers: &mut [Mover], ours: &BTreeMap<u64, OursAt>, ctx: &DamageCtx) -> MineStep {
        let mut out = MineStep::default();
        // past their period: inert
        let (gone, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut self.mines).into_iter().partition(|m| m.inert_t <= now);
        self.mines = keep;
        for m in gone {
            out.events.push(MineEvent { t: now, mine: m.id(), what: "inert".into(), who: None, damage: Vec::new() });
        }
        self.run_torpedoes(now, dt, movers, ours, ctx, &mut out);
        // where each victim is now, and was a second ago
        let mut now_at: BTreeMap<Victim, (f64, f64, &'static str)> = BTreeMap::new();
        for m in movers.iter().filter(|m| m.st.exists) {
            now_at.insert(Victim::Contact(m.contact.id.clone()), (m.st.lat, m.st.lon, domain_str(m.contact.domain)));
        }
        for o in ours.values() {
            let d = match o.domain.as_deref() {
                Some("ground") => "ground",
                Some("subsurface") => "subsurface",
                _ => continue,
            };
            now_at.insert(Victim::Ours(o.uid), (o.lat, o.lon, d));
        }
        let mut spent: Vec<usize> = Vec::new();
        for (k, m) in self.mines.iter().enumerate() {
            if now < m.arms_t {
                continue;
            }
            let [mlon, mlat] = m.point;
            for (v, (lat, lon, d)) in &now_at {
                if !m.sets_off(d) {
                    continue;
                }
                let (kind, target) = match v {
                    Victim::Contact(id) => {
                        let Some(mv) = movers.iter().find(|x| x.contact.id == *id) else { continue };
                        (mv.contact.kind.clone(), crate::effect::target_of(mv).speed_mps)
                    }
                    Victim::Ours(u) => {
                        let o = &ours[u];
                        (damage::ours_kind(o.platform.as_deref(), o.domain.as_deref()).to_string(), o.ve.hypot(o.vn))
                    }
                };
                // only a kind the mine lists figures for sets it off (a person never sets off a TM-62)
                if m.weapon.figures_for(&kind).is_none() {
                    continue;
                }
                let prev = self.last.get(v).copied().unwrap_or((*lat, *lon));
                let key = (m.id(), v.clone());
                if m.acoustic() {
                    if distance_m(mlat, mlon, *lat, *lon) <= m.trigger_m {
                        out.events.push(MineEvent { t: now, mine: m.id(), what: "launched".into(), who: Some(v.who()), damage: Vec::new() });
                        self.runs.push(Run { mine: m.clone(), at: m.point, run_m: 0.0, target: v.clone() });
                        spent.push(k);
                        break;
                    }
                    continue;
                }
                let near = segment_distance((mlat, mlon), prev, (*lat, *lon)) <= m.trigger_m + HALF_WIDTH_M;
                let entered = near && !self.inside.contains(&key);
                if near {
                    self.inside.insert(key.clone());
                } else {
                    self.inside.remove(&key);
                }
                if !entered {
                    continue;
                }
                let pass = self.passes.entry(key).or_insert(0);
                *pass += 1;
                let stream = format!("mine:{}:{}:{}:{}", m.field, m.n, v.who(), pass);
                let c = Conditions::at(ctx.conditions.as_ref(), now, mlat, mlon);
                let (r, hurt) = shoot(m, v, &kind, target, &stream, movers, ours, &c, ctx, now);
                let hit = r.hit;
                out.events.push(MineEvent {
                    t: now,
                    mine: m.id(),
                    what: if hit { "detonated" } else { "pass" }.into(),
                    who: Some(v.who()),
                    damage: vec![r],
                });
                if hit {
                    out.ours_damaged.extend(hurt);
                    if let Victim::Ours(u) = v {
                        out.impacts.push(impact(m, *u, now, *lat, *lon));
                    }
                    let mut more = collateral(m, v, &stream, movers, ours, ctx, now);
                    if let Some(e) = out.events.last_mut() {
                        e.damage.append(&mut more.0);
                    }
                    out.ours_damaged.append(&mut more.1);
                    spent.push(k);
                    break;
                }
            }
        }
        spent.sort_unstable();
        spent.dedup();
        for k in spent.into_iter().rev() {
            self.mines.remove(k);
        }
        self.last = now_at.into_iter().map(|(v, (lat, lon, _))| (v, (lat, lon))).collect();
        out
    }

    /// Each torpedo runs `dt` seconds at its speed toward its target's position now; it strikes
    /// within [`STRIKE_M`], and goes inert past its range or with its target gone.
    fn run_torpedoes(&mut self, now: f64, dt: f64, movers: &mut [Mover], ours: &BTreeMap<u64, OursAt>, ctx: &DamageCtx, out: &mut MineStep) {
        let runs = std::mem::take(&mut self.runs);
        for mut r in runs {
            let target = match &r.target {
                Victim::Contact(id) => movers.iter().find(|m| m.contact.id == *id && m.st.exists).map(|m| (m.st.lat, m.st.lon, m.contact.kind.clone(), m.st.speed_mps)),
                Victim::Ours(u) => ours.get(u).map(|o| (o.lat, o.lon, damage::ours_kind(o.platform.as_deref(), o.domain.as_deref()).to_string(), o.ve.hypot(o.vn))),
            };
            let Some((tlat, tlon, kind, speed)) = target else {
                out.events.push(MineEvent { t: now, mine: r.mine.id(), what: "ran_out".into(), who: Some(r.target.who()), damage: Vec::new() });
                continue;
            };
            let [lon, lat] = r.at;
            let d = distance_m(lat, lon, tlat, tlon);
            let step = r.mine.speed_mps.max(0.0) * dt;
            if d <= STRIKE_M + step {
                let stream = format!("mine:{}:{}:run", r.mine.field, r.mine.n);
                let c = Conditions::at(ctx.conditions.as_ref(), now, tlat, tlon);
                let (rec, hurt) = shoot(&r.mine, &r.target, &kind, speed, &stream, movers, ours, &c, ctx, now);
                if rec.hit {
                    out.ours_damaged.extend(hurt);
                    if let Victim::Ours(u) = &r.target {
                        out.impacts.push(impact(&r.mine, *u, now, tlat, tlon));
                    }
                }
                out.events.push(MineEvent { t: now, mine: r.mine.id(), what: "struck".into(), who: Some(r.target.who()), damage: vec![rec] });
                continue;
            }
            r.run_m += step;
            if r.run_m > r.mine.weapon.max_range_m.unwrap_or(f64::INFINITY) {
                out.events.push(MineEvent { t: now, mine: r.mine.id(), what: "ran_out".into(), who: Some(r.target.who()), damage: Vec::new() });
                continue;
            }
            let f = step / d;
            r.at = [lon + (tlon - lon) * f, lat + (tlat - lat) * f];
            self.runs.push(r);
        }
    }
}

/// The shot at the victim a mine (or its torpedo) caught: the hit draw against its kind, then the
/// damage; a contact takes its state, one of ours is returned for the vehicle simulation.
#[allow(clippy::too_many_arguments)]
fn shoot(
    m: &MineNotice,
    v: &Victim,
    kind: &str,
    speed: f64,
    stream: &str,
    movers: &mut [Mover],
    ours: &BTreeMap<u64, OursAt>,
    c: &Conditions,
    ctx: &DamageCtx,
    now: f64,
) -> (DamageRecord, Vec<OursDamaged>) {
    let mut hurt = Vec::new();
    match v {
        Victim::Contact(id) => {
            let Some(mv) = movers.iter_mut().find(|x| x.contact.id == *id) else {
                let t = Target { kind, speed_mps: speed, armed: false, surface: false };
                return (damage::shot_with(ctx.seed, stream, id, &m.weapon, &t, c, None, ctx.hit_override), hurt);
            };
            let t = crate::effect::target_of(mv);
            let mut r = damage::shot_with(ctx.seed, stream, id, &m.weapon, &t, c, None, ctx.hit_override);
            crate::effect::put(mv, &mut r, now);
            (r, hurt)
        }
        Victim::Ours(u) => {
            let o = ours.get(u);
            let t = Target {
                kind,
                speed_mps: speed,
                armed: o.is_some_and(|o| o.armed),
                surface: false,
            };
            let mut r = damage::shot_with(ctx.seed, stream, &v.who(), &m.weapon, &t, c, None, ctx.hit_override);
            if r.state != DamageState::Undamaged {
                r.applied = true;
                hurt.push(OursDamaged { uid: u.to_string(), state: r.state, cause: format!("set off {} ({})", m.id(), m.weapon.name) });
            }
            (r, hurt)
        }
    }
}

/// Everything else within the warhead's blast radius of a detonated mine draws for collateral
/// damage (as an effect's strike does, [`damage::near`]).
fn collateral(m: &MineNotice, v: &Victim, stream: &str, movers: &mut [Mover], ours: &BTreeMap<u64, OursAt>, ctx: &DamageCtx, now: f64) -> (Vec<DamageRecord>, Vec<OursDamaged>) {
    let [lon, lat] = m.point;
    let mut records = Vec::new();
    let mut hurt = Vec::new();
    for mv in movers.iter_mut().filter(|x| x.st.exists && Victim::Contact(x.contact.id.clone()) != *v) {
        let d = distance_m(lat, lon, mv.st.lat, mv.st.lon);
        let t = crate::effect::target_of(mv);
        let (kind, armed) = (t.kind.to_string(), t.armed);
        let Some(mut r) = damage::near(ctx.seed, &format!("{stream}:contact:{}", mv.contact.id), &mv.contact.id, &m.weapon, &kind, armed, d) else {
            continue;
        };
        crate::effect::put(mv, &mut r, now);
        records.push(r);
    }
    for o in ours.values().filter(|o| Victim::Ours(o.uid) != *v) {
        let d = distance_m(lat, lon, o.lat, o.lon);
        let kind = damage::ours_kind(o.platform.as_deref(), o.domain.as_deref());
        let who = format!("ours:{}", o.uid);
        let Some(mut r) = damage::near(ctx.seed, &format!("{stream}:{who}"), &who, &m.weapon, kind, o.armed, d) else {
            continue;
        };
        if r.state != DamageState::Undamaged {
            r.applied = true;
            hurt.push(OursDamaged { uid: o.uid.to_string(), state: r.state, cause: format!("caught by {} ({})", m.id(), m.weapon.name) });
        }
        records.push(r);
    }
    (records, hurt)
}

/// One of ours struck by a mine (a hit: only a hit is recorded so), as an opponent weapon's
/// strike is recorded (`impacts`): the bridge turns it into the vehicle's own struck report.
fn impact(m: &MineNotice, uid: u64, t: f64, lat: f64, lon: f64) -> ImpactTruth {
    ImpactTruth {
        weapon: m.id(),
        act: format!("mine {}", m.id()),
        t,
        lat,
        lon,
        against_vehicle: Some(uid),
        against_contact: None,
        hit: Some(true),
        damage: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::scenario::Scenario;

    fn mine(field: u64, point: [f64; 2], weapon: &str, targets: &[&str], trigger_m: f64, speed: f64) -> MineNotice {
        MineNotice {
            field,
            n: 1,
            point,
            layer: Some("7".into()),
            weapon: serde_json::from_str(weapon).unwrap(),
            trigger: None,
            trigger_m,
            speed_mps: speed,
            targets: targets.iter().map(|s| s.to_string()).collect(),
            arms_t: 1000.0,
            inert_t: 5000.0,
        }
    }

    const TM62: &str = r#"{"name":"TM-62M mine","kind":"mine","guidance":"pressure_fuze","warhead":{"class":"blast","mass_kg":9.5},"max_range_m":1,
        "effects":{"vehicle":{"p_hit":0.25,"p_kill":0.95,"p_mobility":0.05},"truck":{"p_hit":0.30,"p_kill":0.60,"p_mobility":0.40}}}"#;
    const HAMMERHEAD: &str = r#"{"name":"Hammerhead (encapsulated Mk 54)","kind":"mine","guidance":"acoustic_homing","warhead":{"class":"blast","mass_kg":43.9},"max_range_m":9100,
        "effects":{"uuv":{"p_hit":0.50,"p_kill":0.95,"p_mobility":0.05}}}"#;

    fn movers(toml: &str) -> Vec<Mover> {
        let s = Scenario::from_toml(toml).unwrap();
        s.contacts().unwrap().into_iter().enumerate().map(|(i, c)| Mover::new(c, 1, i as u64)).collect()
    }

    fn steps(f: &mut Minefields, ms: &mut [Mover], from: f64, n: usize, seed: u64) -> Vec<MineEvent> {
        let ctx = DamageCtx { seed, conditions: None, hit_override: None };
        let mut out = Vec::new();
        for k in 0..n {
            let t = from + k as f64;
            for m in ms.iter_mut() {
                m.step(t - 1000.0, 1.0, None);
            }
            out.extend(f.step(t, 1.0, ms, &BTreeMap::new(), &ctx).events);
        }
        out
    }

    fn car(name: &str, kind: &str) -> String {
        format!(
            "title = \"t\"\n[[contact]]\nid = \"{name}\"\naffiliation = \"neutral\"\ndomain = \"ground\"\nkind = \"{kind}\"\nspeed_mps = 10\nmotion = \"transit\"\nroute = [[-122.4590, 37.7940], [-122.4590, 37.7960]]\n"
        )
    }

    /// A car driving over a TM-62M makes a pass, which draws the mine's hit chance on the mine's
    /// own stream: the same seed draws the same, a hit sets it off and destroys or stops the car,
    /// a miss leaves the mine live; a person walking over it never sets it off. Before it arms and
    /// after its period it catches nothing.
    #[test]
    fn a_tm62_is_set_off_by_a_car_on_a_seeded_draw() {
        let toml = car("car", "vehicle");
        let at = [-122.4590, 37.7950];
        // over the seeds, about a quarter of the passes set it off (p_hit 0.25), each seed the same twice
        let mut hits = 0;
        for seed in 0..200u64 {
            let mut a = Minefields::default();
            a.lay(mine(3, at, TM62, &["ground"], 1.0, 0.0));
            let mut ms = movers(&toml);
            let ev = steps(&mut a, &mut ms, 1000.0, 30, seed);
            let mut b = Minefields::default();
            b.lay(mine(3, at, TM62, &["ground"], 1.0, 0.0));
            let mut ms2 = movers(&toml);
            assert_eq!(ev, steps(&mut b, &mut ms2, 1000.0, 30, seed), "seed {seed} draws the same");
            let pass = ev.iter().find(|e| e.mine == "MF-3/1").unwrap();
            assert_eq!(pass.who.as_deref(), Some("car"));
            assert_eq!(pass.damage[0].p_hit, 0.25);
            if pass.what == "detonated" {
                hits += 1;
                assert!(ms[0].damage.is_some(), "the car took its damage");
                assert!(a.is_empty(), "the mine is spent");
            } else {
                assert_eq!(pass.what, "pass");
                assert!(!a.is_empty(), "a miss leaves it live");
            }
        }
        assert!((30..=70).contains(&hits), "{hits} of 200");
        // a person: no figures, never set off
        let mut f = Minefields::default();
        f.lay(mine(3, at, TM62, &["ground"], 1.0, 0.0));
        let mut ms = movers(&car("walker", "person"));
        assert!(steps(&mut f, &mut ms, 1000.0, 30, 1).is_empty());
        // before it arms nothing; once inert, dropped
        let mut f = Minefields::default();
        f.lay(mine(3, at, TM62, &["ground"], 1.0, 0.0));
        let mut ms = movers(&toml);
        assert!(steps(&mut f, &mut ms, 900.0, 30, 1).is_empty());
        let ev = steps(&mut f, &mut movers(&toml), 5000.0, 2, 1);
        assert_eq!(ev[0].what, "inert");
        assert!(f.is_empty());
    }

    /// A submarine within a Hammerhead's hearing launches its torpedo, which chases it at its
    /// speed and strikes once beside it (the hit drawn on the mine's run stream); a surface ship
    /// passing over it sets nothing off.
    #[test]
    fn a_hammerhead_homes_on_a_submarine() {
        let sub = "title = \"t\"\n[[contact]]\nid = \"sub\"\naffiliation = \"hostile\"\ndomain = \"subsurface\"\nkind = \"submarine\"\nspeed_mps = 4\nalt_m = -40\nmotion = \"transit\"\nroute = [[-122.5100, 37.8050], [-122.4900, 37.8150]]\n\
            [[contact]]\nid = \"ship\"\naffiliation = \"neutral\"\ndomain = \"surface\"\nkind = \"cargo_ship\"\nspeed_mps = 6\nmotion = \"transit\"\nroute = [[-122.5000, 37.8000], [-122.5000, 37.8200]]\n";
        let mut f = Minefields::default();
        f.lay(mine(5, [-122.5000, 37.8100], HAMMERHEAD, &["subsurface"], 1000.0, 20.0));
        let mut ms = movers(sub);
        let ev = steps(&mut f, &mut ms, 1000.0, 400, 3);
        let launched = ev.iter().position(|e| e.what == "launched").unwrap();
        assert_eq!(ev[launched].who.as_deref(), Some("sub"));
        let struck = ev.iter().position(|e| e.what == "struck").unwrap();
        assert!(struck > launched);
        let s = &ev[struck];
        assert_eq!((s.who.as_deref(), s.damage.len(), s.damage[0].p_hit), (Some("sub"), 1, 0.5));
        assert!(ev.iter().all(|e| e.who.as_deref() != Some("ship")));
        assert!(f.is_empty(), "the mine is spent and its torpedo done");
    }
}
