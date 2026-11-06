//! Engagement effects (Phase E) on the truth side. The bridge hands an engagement's abstract
//! effect back on the simulation channel, in its answer to `POST /api/sim/contacts`: a point,
//! a radius, a time, the contact its target track was associated with, and where our
//! vehicles reported themselves. Here every contact inside the radius is flagged with the
//! time; one whose scenario says `on_effect = "out_of_play"` (the default) leaves the
//! scenario, so our sensors stop detecting it (that is what the assessment reads). The
//! record of what was inside, whether the intended contact was, and any collateral (another
//! contact, or one of our vehicles, inside the radius; one of ours with no reported position
//! is listed as unknown) goes to the evaluation truth log only: the C2 never sees it. The
//! engaging platform is named apart, never counted as collateral. An effect whose weapon lists
//! no hit or damage figures (an abstract effect, every one before the damage model) does nothing
//! more: a flag and a time.
//!
//! The damage model (bd murmur-lbw8.2, [`crate::damage`]): an effect that comes with its weapon
//! (`weapon`, the catalog's facts of the loadout weapon released) and figures for it is a strike.
//! The intended contact, if it is inside the radius at the strike, is a shot: a hit draw against
//! its kind in the conditions, then a damage draw (destroyed: out of play; its propulsion lost:
//! it stops or drifts; its weapons out: its acts launch nothing; or undamaged). Every other
//! contact, and every vehicle of ours, within the warhead's blast radius of the point draws for
//! collateral damage. Only a destroyed contact leaves the scenario; one inside the radius but
//! unhurt goes on, flagged. What happened to ours is handed back ([`EffectApplied::ours_damaged`])
//! for the vehicle simulation to act on. Each draw is seeded by the run's seed and the intent
//! ([`DamageCtx`]), so a run repeats. An intended contact whose scenario pins the outcome
//! (`strike_outcome`, a training aid: the tutorial's boat) draws nothing: the pinned outcome
//! lands on it, inside the radius or not, ahead of an evaluation's hit chance, recorded as pinned
//! ([`damage::pinned`]); the collateral around it is drawn as ever.
//!
//! Rules 20 (a munition in flight): an effect may come with its impact time (`impact_t`, on this
//! process's scenario clock); it is held ([`InFlight`]) and applied at the effect point once
//! that time has come, to whatever is inside the radius then. Without one it applies at once.

use murmur_standins::geo::distance_m;
use serde::{Deserialize, Serialize};

use crate::damage::{self, Conditions, DamageRecord, DamageState, Target, WeaponDamage};
use crate::motion::Mover;
use crate::scenario::{self, Domain, OnEffect};

/// One of our vehicles as the bridge last heard it (its reported position; none if it has
/// never reported one), and what the damage model reads of it: its height, its catalog platform,
/// domain and whether it is armed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ours {
    pub uid: String,
    #[serde(default)]
    pub lon: Option<f64>,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub armed: bool,
}

/// Rules 20: the munition's flight as the bridge hands it over (the engine's `MunitionFlight`):
/// where it was launched from and where it strikes, which the damage model's range rule reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Flight {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_point: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_alt_m: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_point: Option<[f64; 2]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_alt_m: Option<f64>,
}

impl Flight {
    /// The distance over the ground from the launch to the strike, m, when both are known: what
    /// the damage model's range rule reads ([`damage::hit_chance`]).
    pub fn ground_m(&self) -> Option<f64> {
        let (a, b) = (self.launch_point?, self.impact_point?);
        Some(distance_m(a[1], a[0], b[1], b[0]))
    }
}

/// What the damage model needs beside the effect: the run's seed (each draw's stream is the
/// intent's, [`damage::uniform`]) and the scenario's conditions; and, for an evaluation only (the
/// headless episode's `effects.p_hit`), a hit chance that replaces the target's (none live).
#[derive(Debug, Clone, Copy, Default)]
pub struct DamageCtx {
    pub seed: u64,
    pub conditions: Option<scenario::Conditions>,
    pub hit_override: Option<f64>,
}

/// One of ours the damage model hurt, for the vehicle simulation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OursDamaged {
    pub uid: String,
    pub state: DamageState,
    /// What did it, for the record.
    pub cause: String,
}

/// The effect as the bridge sends it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectNotice {
    pub intent: u64,
    pub track: String,
    #[serde(default)]
    pub target_contact: Option<String>,
    /// Bridge time, unix seconds.
    pub t: f64,
    /// `[lon, lat]`.
    pub point: [f64; 2],
    pub radius_m: f64,
    /// The engaging platform's uid (not in `ours`).
    #[serde(default)]
    pub platform: Option<String>,
    /// Our other vehicles.
    #[serde(default)]
    pub ours: Vec<Ours>,
    /// Rules 20: when the munition carrying it strikes, scenario time (unix s): the effect is
    /// held until then ([`InFlight`]). None: at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_t: Option<f64>,
    /// Rules 20: the munition's flight (its launch and impact points), as the bridge hands it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub munition: Option<Flight>,
    /// The loadout weapon released (the catalog's facts), whose figures make it a strike for the
    /// damage model. None, or a weapon without figures: an abstract effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon: Option<WeaponDamage>,
    /// Rules 26: a gun's burst number (1: its release): each burst is drawn on its own stream
    /// (`effect:<intent>:<burst>`). None for any other weapon (`effect:<intent>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<u32>,
}

impl EffectNotice {
    /// The stream its draws are named by: the intent's, and a gun's burst's (rules 26).
    pub fn stream(&self) -> String {
        match self.burst {
            Some(b) => format!("effect:{}:{b}", self.intent),
            None => format!("effect:{}", self.intent),
        }
    }
}

/// Rules 20: the effects whose munition is still in flight, held until their impact time and
/// then handed back to be applied ([`apply`]), in the order they came; one without an impact
/// time is due at once. The contacts process and the headless simulator share it.
#[derive(Debug, Default)]
pub struct InFlight {
    held: Vec<EffectNotice>,
}

impl InFlight {
    /// Take an effect from the bridge.
    pub fn hold(&mut self, e: EffectNotice) {
        self.held.push(e);
    }

    /// The effects due by scenario time `now` (unix s), in the order they came; the rest stay.
    pub fn due(&mut self, now: f64) -> Vec<EffectNotice> {
        let (due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|e| e.impact_t.is_none_or(|t| t <= now));
        self.held = wait;
        due
    }

    /// How many are still in flight.
    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

/// Collateral: anything inside the radius that was not the intended contact.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Collateral {
    pub contacts: Vec<String>,
    pub ours: Vec<String>,
    /// Ours with no reported position: not known to be clear.
    #[serde(default)]
    pub ours_unknown: Vec<String>,
}

/// What the effect did, for the evaluation truth log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectApplied {
    pub intent: u64,
    pub track: String,
    pub t: f64,
    /// Scenario time it was applied (unix s).
    pub applied_t: f64,
    pub point: [f64; 2],
    pub radius_m: f64,
    pub target_contact: Option<String>,
    /// The engaging platform.
    pub platform: Option<String>,
    /// The intended contact was inside the radius when the effect was applied.
    pub target_inside: bool,
    /// Every contact inside the radius, with its distance from the point.
    pub inside: Vec<(String, f64)>,
    /// Contacts taken out of the scenario.
    pub out_of_play: Vec<String>,
    pub collateral: Collateral,
    /// Rules 20: the munition's impact time it was held for (scenario unix s; none: applied on
    /// receipt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact_t: Option<f64>,
    /// The damage model: the weapon that struck, and every draw it made (the target's shot, each
    /// collateral within the blast radius). Absent for an abstract effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weapon: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub damage: Vec<DamageRecord>,
    /// Ours the damage model hurt, for the vehicle simulation (and the record).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ours_damaged: Vec<OursDamaged>,
    /// Rules 26: a gun's burst it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<u32>,
}

/// Apply one effect at scenario time `now` (unix s), abstractly: every contact inside the radius
/// flagged, out of play unless its scenario says `persists`.
pub fn apply(e: &EffectNotice, movers: &mut [Mover], now: f64) -> EffectApplied {
    apply_with(e, movers, now, None)
}

/// [`apply`], and with `ctx` an effect whose weapon lists figures is a strike for the damage
/// model (the module doc).
pub fn apply_with(e: &EffectNotice, movers: &mut [Mover], now: f64, ctx: Option<&DamageCtx>) -> EffectApplied {
    if let (Some(ctx), Some(w)) = (ctx, e.weapon.as_ref().filter(|w| w.modelled())) {
        return strike(e, w, movers, now, ctx);
    }
    let [lon, lat] = e.point;
    let mut inside = Vec::new();
    let mut out = Vec::new();
    let mut collateral = Collateral::default();
    for m in movers.iter_mut().filter(|m| m.st.exists) {
        let d = distance_m(lat, lon, m.st.lat, m.st.lon);
        if d > e.radius_m {
            continue;
        }
        inside.push((m.contact.id.clone(), (d * 10.0).round() / 10.0));
        m.effect_t = Some(now);
        if m.contact.on_effect == OnEffect::OutOfPlay {
            m.out_of_play = true;
            m.st.exists = false;
            out.push(m.contact.id.clone());
        }
        if e.target_contact.as_deref() != Some(m.contact.id.as_str()) {
            collateral.contacts.push(m.contact.id.clone());
        }
    }
    for o in e.ours.iter().filter(|o| e.platform.as_ref() != Some(&o.uid)) {
        match o.lat.zip(o.lon) {
            Some((olat, olon)) if distance_m(lat, lon, olat, olon) <= e.radius_m => {
                collateral.ours.push(o.uid.clone());
            }
            Some(_) => {}
            None => collateral.ours_unknown.push(o.uid.clone()),
        }
    }
    let target_inside = e
        .target_contact
        .as_ref()
        .is_some_and(|t| inside.iter().any(|(id, _)| id == t));
    EffectApplied {
        intent: e.intent,
        track: e.track.clone(),
        t: e.t,
        applied_t: now,
        point: e.point,
        radius_m: e.radius_m,
        target_contact: e.target_contact.clone(),
        platform: e.platform.clone(),
        target_inside,
        inside,
        out_of_play: out,
        collateral,
        impact_t: e.impact_t,
        weapon: None,
        damage: Vec::new(),
        ours_damaged: Vec::new(),
        burst: e.burst,
    }
}

/// The ours of `e` inside its radius (2D) as collateral, and those with no position as unknown
/// (the platform itself never).
fn ours_inside(e: &EffectNotice) -> Collateral {
    let [lon, lat] = e.point;
    let mut c = Collateral::default();
    for o in e.ours.iter().filter(|o| e.platform.as_ref() != Some(&o.uid)) {
        match o.lat.zip(o.lon) {
            Some((olat, olon)) if distance_m(lat, lon, olat, olon) <= e.radius_m => c.ours.push(o.uid.clone()),
            Some(_) => {}
            None => c.ours_unknown.push(o.uid.clone()),
        }
    }
    c
}

/// A contact's facts as the damage model reads them: its kind as our sensors see it, and armed
/// when its catalog kind is (sim/contacts.toml) or its scenario gives it a weapon.
pub(crate) fn target_of(m: &Mover) -> Target<'_> {
    Target {
        kind: &m.contact.kind,
        speed_mps: m.st.speed_mps,
        armed: m.contact.weapon.is_some() || m.contact.armed_kind(),
        surface: m.contact.domain == Domain::Surface,
    }
}

/// Put a draw's state on a contact (unless its scenario keeps it in play), saying so in the record.
pub(crate) fn put(m: &mut Mover, r: &mut DamageRecord, now: f64) {
    if r.state == DamageState::Undamaged {
        return;
    }
    if m.contact.on_effect == OnEffect::Persists {
        r.note = Some("its scenario says persists: kept in play, unchanged".into());
        return;
    }
    r.applied = m.damage(r.state, now);
}

fn tenth(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// The damage model's strike (the module doc).
fn strike(e: &EffectNotice, w: &WeaponDamage, movers: &mut [Mover], now: f64, ctx: &DamageCtx) -> EffectApplied {
    let [lon, lat] = e.point;
    // rules 26: each of a gun's bursts on its own stream
    let stream = e.stream();
    let mut inside = Vec::new();
    let mut collateral = ours_inside(e);
    for m in movers.iter_mut().filter(|m| m.st.exists) {
        let d = distance_m(lat, lon, m.st.lat, m.st.lon);
        if d > e.radius_m {
            continue;
        }
        inside.push((m.contact.id.clone(), tenth(d)));
        m.effect_t = Some(now);
        if e.target_contact.as_deref() != Some(m.contact.id.as_str()) {
            collateral.contacts.push(m.contact.id.clone());
        }
    }
    let c = Conditions::at(ctx.conditions.as_ref(), now, lat, lon);
    let out = e.munition.as_ref().and_then(Flight::ground_m);
    let mut records = Vec::new();
    // the shot at the intended contact, if it is inside the radius
    let target = e
        .target_contact
        .as_ref()
        .and_then(|t| movers.iter().position(|m| m.st.exists && m.contact.id == *t));
    let target_inside = target.is_some_and(|i| inside.iter().any(|(id, _)| *id == movers[i].contact.id));
    let mut z0 = e.munition.as_ref().and_then(|f| f.impact_alt_m).unwrap_or(0.0);
    if let Some(i) = target {
        let m = &mut movers[i];
        let d = tenth(distance_m(lat, lon, m.st.lat, m.st.lon));
        if let Some(pin) = m.contact.strike_outcome {
            // its scenario pins every strike on it (a training aid): no draw, wherever it is
            if target_inside {
                z0 = m.st.alt_m;
            }
            let mut r = damage::pinned(&m.contact.id, w, &target_of(m), pin);
            r.distance_m = Some(d);
            if !target_inside {
                r.note = Some(format!(
                    "its scenario pins every strike on it (a training aid): no hit or damage draw, though not inside the {:.0} m radius at the strike",
                    e.radius_m
                ));
            }
            put(m, &mut r, now);
            records.push(r);
        } else if target_inside {
            z0 = m.st.alt_m;
            let mut r = damage::shot_with(ctx.seed, &format!("{stream}:target"), &m.contact.id, w, &target_of(m), &c, out, ctx.hit_override);
            r.distance_m = Some(d);
            put(m, &mut r, now);
            records.push(r);
        } else {
            records.push(DamageRecord {
                who: m.contact.id.clone(),
                role: "target".into(),
                kind: m.contact.kind.clone(),
                figures: None,
                distance_m: Some(d),
                p_hit: 0.0,
                factors: Vec::new(),
                u_hit: 1.0,
                hit: false,
                u_damage: None,
                state: DamageState::Undamaged,
                applied: false,
                pinned: false,
                note: Some(format!("not inside the {:.0} m radius at the strike: missed", e.radius_m)),
            });
        }
    }
    // collateral within the blast radius: every other contact, and ours
    for (i, m) in movers.iter_mut().enumerate() {
        if !m.st.exists || Some(i) == target {
            continue;
        }
        let d = distance_m(lat, lon, m.st.lat, m.st.lon).hypot(m.st.alt_m - z0);
        let t = target_of(m);
        let stream = format!("{stream}:contact:{}", m.contact.id);
        let Some(mut r) = damage::near(ctx.seed, &stream, &m.contact.id, w, t.kind, t.armed, d) else {
            continue;
        };
        put(m, &mut r, now);
        records.push(r);
    }
    let mut ours_damaged = Vec::new();
    for o in e.ours.iter().filter(|o| e.platform.as_ref() != Some(&o.uid)) {
        let Some((olat, olon)) = o.lat.zip(o.lon) else { continue };
        let d = distance_m(lat, lon, olat, olon).hypot(o.alt_m.unwrap_or(0.0) - z0);
        let kind = damage::ours_kind(o.platform.as_deref(), o.domain.as_deref());
        let who = format!("ours:{}", o.uid);
        let Some(mut r) = damage::near(ctx.seed, &format!("{stream}:{who}"), &who, w, kind, o.armed, d) else {
            continue;
        };
        if r.state != DamageState::Undamaged {
            r.applied = true;
            ours_damaged.push(OursDamaged {
                uid: o.uid.clone(),
                state: r.state,
                cause: match e.burst {
                    Some(b) => format!("caught in burst {b} of intent {} ({})", e.intent, w.name),
                    None => format!("caught in the effect of intent {} ({})", e.intent, w.name),
                },
            });
        }
        records.push(r);
    }
    let out = records
        .iter()
        .filter(|r| r.applied && r.state == DamageState::Destroyed && !r.who.starts_with("ours:"))
        .map(|r| r.who.clone())
        .collect();
    EffectApplied {
        intent: e.intent,
        track: e.track.clone(),
        t: e.t,
        applied_t: now,
        point: e.point,
        radius_m: e.radius_m,
        target_contact: e.target_contact.clone(),
        platform: e.platform.clone(),
        target_inside,
        inside,
        out_of_play: out,
        collateral,
        impact_t: e.impact_t,
        weapon: Some(w.name.clone()),
        damage: records,
        ours_damaged,
        burst: e.burst,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::Scenario;

    fn ours(uid: &str, lon: Option<f64>, lat: Option<f64>) -> Ours {
        Ours { uid: uid.into(), lon, lat, alt_m: None, platform: None, domain: None, armed: false }
    }

    #[test]
    fn contacts_inside_the_radius_are_flagged_and_the_rest_untouched() {
        let sc = Scenario::from_toml(
            r#"
            title = "t"
            [[contact]]
            id = "target"
            affiliation = "hostile"
            domain = "surface"
            kind = "small_fast_boat"
            motion = "station"
            at = [-122.4950, 37.8120]
            [[contact]]
            id = "near"
            affiliation = "neutral"
            domain = "surface"
            kind = "sailboat"
            motion = "station"
            at = [-122.4950, 37.8124]
            on_effect = "persists"
            [[contact]]
            id = "far"
            affiliation = "neutral"
            domain = "surface"
            kind = "ferry"
            motion = "station"
            at = [-122.4800, 37.8200]
            "#,
        )
        .expect("scenario");
        let mut movers: Vec<Mover> = sc
            .contacts()
            .expect("contacts")
            .into_iter()
            .enumerate()
            .map(|(i, c)| Mover::new(c, 1, i as u64))
            .collect();
        for m in &mut movers {
            m.step(1.0, 1.0, None);
        }
        let e = EffectNotice {
            intent: 7,
            track: "T-0001".into(),
            target_contact: Some("target".into()),
            t: 100.0,
            point: [-122.4950, 37.8120],
            radius_m: 75.0,
            platform: Some("9".into()),
            ours: vec![
                ours("1", Some(-122.4951), Some(37.8121)),
                ours("2", Some(-122.4700), Some(37.8160)),
                ours("3", None, None),
                // the platform itself, if a bridge sent it: never collateral
                ours("9", Some(-122.4950), Some(37.8120)),
            ],
            impact_t: None,
            munition: None,
            weapon: None,
            burst: None,
        };
        let r = apply(&e, &mut movers, 101.0);
        assert!(r.target_inside);
        assert_eq!(r.out_of_play, vec!["target".to_string()]);
        assert_eq!(r.collateral.contacts, vec!["near".to_string()]);
        assert_eq!(r.collateral.ours, vec!["1".to_string()]);
        assert_eq!(r.collateral.ours_unknown, vec!["3".to_string()]);
        assert_eq!(r.platform.as_deref(), Some("9"));
        assert_eq!(r.inside.len(), 2);
        // the target leaves the scenario; the sailboat persists, flagged; the ferry untouched
        for m in &mut movers {
            m.step(2.0, 1.0, None);
        }
        assert!(!movers[0].st.exists);
        assert!(movers[1].st.exists && movers[1].effect_t == Some(101.0));
        assert!(movers[2].st.exists && movers[2].effect_t.is_none());
    }

    fn notice(intent: u64, impact_t: Option<f64>) -> EffectNotice {
        EffectNotice {
            intent,
            track: "T-0001".into(),
            target_contact: None,
            t: 100.0,
            point: [-122.4950, 37.8120],
            radius_m: 75.0,
            platform: None,
            ours: Vec::new(),
            impact_t,
            munition: None,
            weapon: None,
            burst: None,
        }
    }

    /// Rules 20: an effect without an impact time is due at once; one with is held until its
    /// impact, then handed back once, in the order they came; the truth record names the impact.
    #[test]
    fn a_munitions_effect_is_held_until_its_impact() {
        let mut f = InFlight::default();
        f.hold(notice(1, Some(118.4)));
        f.hold(notice(2, None));
        f.hold(notice(3, Some(110.0)));
        let ids = |v: Vec<EffectNotice>| v.into_iter().map(|e| e.intent).collect::<Vec<_>>();
        assert_eq!(ids(f.due(101.0)), vec![2]);
        assert_eq!(f.len(), 2);
        assert_eq!(ids(f.due(110.0)), vec![3]);
        assert!(f.due(118.0).is_empty());
        assert_eq!(ids(f.due(119.0)), vec![1]);
        assert!(f.is_empty() && f.due(1e9).is_empty());
        // the notice's JSON from the bridge, with and without the field
        let e: EffectNotice = serde_json::from_value(serde_json::json!({
            "intent": 4, "track": "T-0001", "t": 100.0, "point": [-122.495, 37.812], "radius_m": 75.0,
            "impact_t": 118.4, "munition": { "name": "Hellfire-class missile" },
        }))
        .expect("notice");
        assert_eq!(e.impact_t, Some(118.4));
        let mut movers: Vec<Mover> = Vec::new();
        let r = apply(&e, &mut movers, 118.5);
        assert_eq!(r.impact_t, Some(118.4));
        assert!(serde_json::to_string(&r).expect("json").contains("\"impact_t\":118.4"));
        let r = apply(&notice(5, None), &mut movers, 101.0);
        assert!(!serde_json::to_string(&r).expect("json").contains("impact_t"));
    }

    // ---- the damage model (bd murmur-lbw8.2) ------------------------------------------------

    const SCENE: &str = r#"
        title = "t"
        [[contact]]
        id = "boat"
        affiliation = "hostile"
        domain = "surface"
        kind = "small_fast_boat"
        motion = "route"
        speed_mps = 8
        route = [[-122.4950, 37.8120], [-122.4700, 37.8120]]
        weapon = { name = "ATGM-class missile", kind = "atgm", speed_mps = 250, range_m = 5000 }
        [[contact]]
        id = "skiff"
        affiliation = "neutral"
        domain = "surface"
        kind = "sailboat"
        motion = "station"
        at = [-122.49502, 37.81201]
        [[contact]]
        id = "yacht"
        affiliation = "neutral"
        domain = "surface"
        kind = "sailboat"
        motion = "station"
        at = [-122.4950, 37.8124]
    "#;

    /// The scene a second in: the boat under way at the point, a skiff about 2 m from it, a
    /// yacht 44 m off (inside the 75 m radius, outside a 9 kg warhead's blast).
    fn scene() -> Vec<Mover> {
        let sc = Scenario::from_toml(SCENE).expect("scenario");
        let mut movers: Vec<Mover> = sc
            .contacts()
            .expect("contacts")
            .into_iter()
            .enumerate()
            .map(|(i, c)| Mover::new(c, 1, i as u64))
            .collect();
        for m in &mut movers {
            m.step(1.0, 1.0, None);
        }
        movers
    }

    /// A weapon whose figures against a small fast boat are `boat` (a 9 kg warhead).
    fn weapon(boat: serde_json::Value) -> WeaponDamage {
        serde_json::from_value(serde_json::json!({
            "name": "AGM-114R Hellfire II", "kind": "missile", "guidance": "semi_active_laser",
            "warhead": { "class": "multi_function", "mass_kg": 9.0 }, "max_range_m": 8000.0,
            "effects": { "small_fast_boat": boat, "sailboat": { "p_hit": 0.85, "p_kill": 0.65, "p_mobility": 0.30 } },
        }))
        .expect("weapon")
    }

    fn strike_notice(intent: u64, w: WeaponDamage, ours: Vec<Ours>) -> EffectNotice {
        EffectNotice {
            weapon: Some(w),
            ours,
            target_contact: Some("boat".into()),
            platform: Some("9".into()),
            ..notice(intent, None)
        }
    }

    const CTX: DamageCtx = DamageCtx { seed: 17, conditions: None, hit_override: None };

    fn target(r: &EffectApplied) -> &DamageRecord {
        r.damage.iter().find(|d| d.role == "target").expect("the target's draw")
    }

    /// A certain hit destroys, stops or disarms by the figures; a certain miss leaves the boat as
    /// it was. Each lands on the boat as the model says: destroyed out of play, stopped still on
    /// the water (still detected), disarmed holding its fire.
    #[test]
    fn a_hit_or_a_miss_and_each_damage_state() {
        let cases = [
            (serde_json::json!({ "p_hit": 1.0, "p_kill": 1.0 }), true, DamageState::Destroyed),
            (serde_json::json!({ "p_hit": 1.0, "p_mobility": 1.0 }), true, DamageState::Mobility),
            (serde_json::json!({ "p_hit": 1.0, "p_firepower": 1.0 }), true, DamageState::Firepower),
            (serde_json::json!({ "p_hit": 1.0 }), true, DamageState::Undamaged),
            (serde_json::json!({ "p_hit": 0.0, "p_kill": 1.0 }), false, DamageState::Undamaged),
        ];
        for (figs, hit, state) in cases {
            let mut movers = scene();
            let r = apply_with(&strike_notice(7, weapon(figs), Vec::new()), &mut movers, 101.0, Some(&CTX));
            let t = target(&r);
            assert_eq!((t.hit, t.state), (hit, state), "{t:?}");
            assert_eq!(r.weapon.as_deref(), Some("AGM-114R Hellfire II"));
            let boat = &mut movers[0];
            for s in 2..10 {
                boat.step(f64::from(s), 1.0, None);
            }
            match state {
                DamageState::Destroyed => {
                    assert!(!boat.st.exists && r.out_of_play == vec!["boat".to_string()]);
                }
                DamageState::Mobility => {
                    assert!(boat.st.exists, "a stopped boat is still on the water, still detected");
                    assert!(boat.st.speed_mps < 0.1, "{}", boat.st.speed_mps);
                    assert!(matches!(boat.contact.motion, crate::scenario::Motion::Drift { .. }));
                    assert!(r.out_of_play.is_empty());
                }
                DamageState::Firepower => assert!(boat.st.exists && boat.disarmed && boat.st.speed_mps > 1.0),
                DamageState::Undamaged => assert!(boat.st.exists && boat.damage.is_none() && boat.st.speed_mps > 1.0),
            }
            // only the destroyed leave: the yacht inside the radius, outside the blast, goes on
            assert!(movers[2].st.exists && movers[2].effect_t == Some(101.0) && movers[2].damage.is_none());
        }
    }

    /// Rules 26: each of a gun's bursts is a shot of its own, drawn on its own stream (the release,
    /// burst 1, as much as any later one): two bursts of a 50-50 gun do not repeat one draw, the
    /// same burst reruns the same, and the record names the burst. A gun loses 20 points a 5 m/s
    /// band past 15 m/s and fires at 0.6 in fog through its stabilised sight (10.5).
    #[test]
    fn each_burst_of_a_gun_draws_on_its_own_stream() {
        let gun: WeaponDamage = serde_json::from_value(serde_json::json!({
            "name": "M2 .50 HMG", "kind": "gun", "guidance": "stabilised_sight",
            "warhead": { "class": "ball_ap_12_7mm", "mass_kg": 0.043 }, "max_range_m": 1800.0,
            "effects": { "small_fast_boat": { "p_hit": 0.5, "p_mobility": 1.0 } },
        }))
        .expect("gun");
        let burst = |b: u32| {
            let mut movers = scene();
            let e = EffectNotice { burst: Some(b), ..strike_notice(7, gun.clone(), Vec::new()) };
            apply_with(&e, &mut movers, 101.0, Some(&CTX))
        };
        let us: Vec<f64> = (1..=8).map(|b| target(&burst(b)).u_hit).collect();
        assert!(us.windows(2).any(|w| w[0] != w[1]), "{us:?}");
        assert_eq!(target(&burst(3)).u_hit, us[2], "a burst reruns the same");
        assert_eq!(burst(3).burst, Some(3));
        // no burst: the intent's own stream, as every effect before rules 26
        let mut movers = scene();
        let plain = apply_with(&strike_notice(7, gun.clone(), Vec::new()), &mut movers, 101.0, Some(&CTX));
        assert_eq!(target(&plain).u_hit, damage::uniform(17, "effect:7:target:hit"));
        assert_eq!(target(&burst(2)).u_hit, damage::uniform(17, "effect:7:2:target:hit"));
        // the gun's factors
        let fast = Target { kind: "small_fast_boat", speed_mps: 22.0, armed: false, surface: true };
        let clear = Conditions { visibility_m: 20_000.0, sea_state: 2.0, sun_elevation_deg: 40.0 };
        let (_, _, _, p) = damage::hit_chance(&gun, &fast, &clear, None);
        assert!((p - 0.1).abs() < 1e-9, "two bands at 20 points: {p}");
        let fog = Conditions { visibility_m: 500.0, ..clear };
        let slow = Target { speed_mps: 3.0, ..fast };
        let (_, _, _, p) = damage::hit_chance(&gun, &slow, &fog, None);
        assert!((p - 0.3).abs() < 1e-9, "x0.6 in fog: {p}");
    }

    /// Collateral: a skiff 2 m from the point draws (its chance near 1), the yacht 44 m off does
    /// not; one of ours on the point draws too and is handed back, the platform itself never, one
    /// of ours 3 km up never (3D distance).
    #[test]
    fn collateral_within_the_blast_draws_contacts_and_ours() {
        let mut movers = scene();
        let ours = vec![
            Ours { uid: "4".into(), lon: Some(-122.4950), lat: Some(37.8120), alt_m: Some(0.5), platform: Some("ugv_armed".into()), domain: Some("ground".into()), armed: true },
            Ours { uid: "5".into(), lon: Some(-122.4950), lat: Some(37.8120), alt_m: Some(3000.0), platform: Some("mq9b_armed".into()), domain: Some("air".into()), armed: true },
            Ours { uid: "9".into(), lon: Some(-122.4950), lat: Some(37.8120), alt_m: None, platform: None, domain: None, armed: false },
        ];
        let r = apply_with(&strike_notice(7, weapon(serde_json::json!({ "p_hit": 0.0 })), ours), &mut movers, 101.0, Some(&CTX));
        let who: Vec<&str> = r.damage.iter().map(|d| d.who.as_str()).collect();
        assert_eq!(who, vec!["boat", "skiff", "ours:4"], "{:#?}", r.damage);
        let skiff = &r.damage[1];
        assert_eq!(skiff.role, "collateral");
        assert!(skiff.p_hit > 0.6 && skiff.distance_m.is_some_and(|d| d < 3.0), "{skiff:?}");
        let ours4 = &r.damage[2];
        assert_eq!((ours4.kind.as_str(), ours4.figures.as_deref()), ("vehicle", None));
        if ours4.state != DamageState::Undamaged {
            assert_eq!(r.ours_damaged, vec![OursDamaged { uid: "4".into(), state: ours4.state, cause: "caught in the effect of intent 7 (AGM-114R Hellfire II)".into() }]);
        }
        // a hit on the skiff lands on it (a sailboat's figures: destroyed or stopped, or nothing)
        assert_eq!(movers[1].damage.map(|(s, _)| s), (skiff.state != DamageState::Undamaged).then_some(skiff.state));
    }

    /// The same scenario, seed and intent draw the same, every time; another intent draws on its
    /// own stream. Across 400 intents a 0.75 figure hits about three times in four.
    #[test]
    fn the_seeded_draw_reruns_identically() {
        let w = weapon(serde_json::json!({ "p_hit": 0.75, "p_kill": 0.85, "p_mobility": 0.10, "p_firepower": 0.05 }));
        let run = |intent: u64| {
            let mut movers = scene();
            let r = apply_with(&strike_notice(intent, w.clone(), Vec::new()), &mut movers, 101.0, Some(&CTX));
            serde_json::to_string(&r).expect("json")
        };
        assert_eq!(run(7), run(7));
        let hits = (0..400u64)
            .filter(|i| {
                let mut movers = scene();
                let r = apply_with(&strike_notice(*i, w.clone(), Vec::new()), &mut movers, 101.0, Some(&CTX));
                target(&r).hit
            })
            .count();
        assert!((270..=330).contains(&hits), "{hits} of 400");
    }

    /// REAPER 1's weapons as the catalog lists them (sim/platforms.toml, `mq9b_armed`'s loadout).
    fn reaper_weapons() -> Vec<WeaponDamage> {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../sim/platforms.toml")).expect("the catalog");
        let cat: toml::Value = toml::from_str(&text).expect("toml");
        cat["mq9b_armed"]["loadout"]
            .as_array()
            .expect("a loadout")
            .iter()
            .map(|w| serde_json::from_value(serde_json::to_value(w).expect("json")).expect("a weapon"))
            .collect()
    }

    /// [`scene`] with the boat's `strike_outcome` pinned to `o` (`[training]`).
    fn pinned_scene(o: &str) -> Vec<Mover> {
        let text = format!("{SCENE}\n[training]\nstrike_outcome = {{ boat = \"{o}\" }}\n");
        let mut movers: Vec<Mover> = Scenario::from_toml(&text)
            .expect("scenario")
            .contacts()
            .expect("contacts")
            .into_iter()
            .enumerate()
            .map(|(i, c)| Mover::new(c, 1, i as u64))
            .collect();
        for m in &mut movers {
            m.step(1.0, 1.0, None);
        }
        movers
    }

    /// A target whose scenario pins `strike_outcome = "destroyed"` (the tutorial's boat) is
    /// destroyed by every strike of either of REAPER 1's weapons, whatever the seed, the intent or
    /// the burst (the same strikes unpinned miss or only damage it now and then), ahead of an
    /// evaluation's hit chance and outside the radius too; its record says pinned and draws
    /// nothing, and the collateral around it is drawn exactly as without the pin.
    #[test]
    fn a_pinned_target_takes_its_outcome_whatever_the_draw() {
        let ws = reaper_weapons();
        let names: Vec<&str> = ws.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["AGM-114R Hellfire II", "GBU-12 Paveway II"]);
        for w in &ws {
            assert!(w.modelled(), "{}", w.name);
            let mut drawn = std::collections::BTreeSet::new();
            for seed in [1, 17, 4242] {
                for intent in 1..=30u64 {
                    let ctx = DamageCtx { seed, ..CTX };
                    let e = EffectNotice { burst: (intent % 3 == 0).then_some(2), ..strike_notice(intent, w.clone(), Vec::new()) };
                    let mut movers = pinned_scene("destroyed");
                    let r = apply_with(&e, &mut movers, 101.0, Some(&ctx));
                    let t = target(&r);
                    assert!(t.pinned && t.hit && t.applied && t.u_damage.is_none(), "{t:?}");
                    assert_eq!((t.state, t.p_hit), (DamageState::Destroyed, 1.0));
                    assert!(t.factors[0].what.contains("pinned by the scenario"), "{t:?}");
                    assert_eq!(r.out_of_play.first().map(String::as_str), Some("boat"));
                    assert!(!movers[0].st.exists);
                    let mut plain = scene();
                    let rp = apply_with(&e, &mut plain, 101.0, Some(&ctx));
                    assert_eq!(r.damage[1..], rp.damage[1..], "collateral as without the pin");
                    let tp = target(&rp);
                    assert!(!tp.pinned);
                    drawn.insert((tp.hit, tp.state));
                }
            }
            assert!(drawn.len() > 1 && drawn.iter().any(|d| *d != (true, DamageState::Destroyed)), "{}: {drawn:?}", w.name);
        }
        let hellfire = ws[0].clone();
        // an evaluation's hit chance does not move it; a strike outside the radius lands on it too
        let mut movers = pinned_scene("destroyed");
        let r = apply_with(&strike_notice(3, hellfire.clone(), Vec::new()), &mut movers, 101.0, Some(&DamageCtx { hit_override: Some(0.0), ..CTX }));
        assert_eq!(target(&r).state, DamageState::Destroyed);
        let mut movers = pinned_scene("destroyed");
        let off = EffectNotice { point: [-122.4850, 37.8120], ..strike_notice(3, hellfire.clone(), Vec::new()) };
        let r = apply_with(&off, &mut movers, 101.0, Some(&CTX));
        assert!(!r.target_inside && target(&r).state == DamageState::Destroyed && !movers[0].st.exists);
        assert!(target(&r).note.as_deref().is_some_and(|n| n.contains("not inside")));
        // the other outcomes land as pinned
        for (o, hit, state) in [("mobility", true, DamageState::Mobility), ("undamaged", true, DamageState::Undamaged), ("miss", false, DamageState::Undamaged)] {
            let mut movers = pinned_scene(o);
            let r = apply_with(&strike_notice(3, hellfire.clone(), Vec::new()), &mut movers, 101.0, Some(&CTX));
            let t = target(&r);
            assert_eq!((t.hit, t.state, t.pinned), (hit, state, true), "{o}");
            assert!(movers[0].st.exists, "{o}");
        }
        // the truth log's record says so; an unpinned one has no such key
        let mut movers = pinned_scene("destroyed");
        let json = serde_json::to_string(&apply_with(&strike_notice(3, hellfire.clone(), Vec::new()), &mut movers, 101.0, Some(&CTX))).expect("json");
        assert!(json.contains("\"pinned\":true"), "{json}");
        let mut movers = scene();
        let json = serde_json::to_string(&apply_with(&strike_notice(3, hellfire, Vec::new()), &mut movers, 101.0, Some(&CTX))).expect("json");
        assert!(!json.contains("pinned"), "{json}");
    }

    /// Without figures (no weapon, or one listing none) the effect is the abstract one exactly:
    /// everything inside the radius out of play, the same record bytes as before.
    #[test]
    fn without_figures_the_effect_is_abstract_as_before() {
        let mut a = scene();
        let mut b = scene();
        let plain = EffectNotice { target_contact: Some("boat".into()), ..notice(7, None) };
        let bare = EffectNotice {
            weapon: Some(WeaponDamage { effects: Default::default(), ..weapon(serde_json::json!({})) }),
            ..plain.clone()
        };
        let ra = apply(&plain, &mut a, 101.0);
        let rb = apply_with(&bare, &mut b, 101.0, Some(&CTX));
        assert_eq!(serde_json::to_string(&ra).expect("json"), serde_json::to_string(&rb).expect("json"));
        assert_eq!(ra.out_of_play, vec!["boat".to_string(), "skiff".to_string(), "yacht".to_string()]);
        let json = serde_json::to_string(&ra).expect("json");
        for k in ["damage", "weapon", "ours_damaged"] {
            assert!(!json.contains(k), "{k} in {json}");
        }
    }
}
