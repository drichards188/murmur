//! Engagement effects (Phase E) on the truth side. The bridge hands an engagement's abstract
//! effect back on the simulation channel, in its answer to `POST /api/sim/contacts`: a point,
//! a radius, a time, the contact its target track was associated with, and where our
//! vehicles reported themselves. Here every contact inside the radius is flagged with the
//! time; one whose scenario says `on_effect = "out_of_play"` (the default) leaves the
//! scenario, so our sensors stop detecting it (that is what the assessment reads). The
//! record of what was inside, whether the intended contact was, and any collateral (another
//! contact, or one of our vehicles, inside the radius; one of ours with no reported position
//! is listed as unknown) goes to the evaluation truth log only: the C2 never sees it. The
//! engaging platform is named apart, never counted as collateral. No damage, blast or
//! lethality is modelled: a flag and a time.
//!
//! Rules 20 (a munition in flight): an effect may come with its impact time (`impact_t`, on this
//! process's scenario clock); it is held ([`InFlight`]) and applied at the effect point once
//! that time has come, to whatever is inside the radius then. Without one it applies at once.

use murmur_standins::geo::distance_m;
use serde::{Deserialize, Serialize};

use crate::motion::Mover;
use crate::scenario::OnEffect;

/// One of our vehicles as the bridge last heard it (its reported position; none if it has
/// never reported one).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ours {
    pub uid: String,
    #[serde(default)]
    pub lon: Option<f64>,
    #[serde(default)]
    pub lat: Option<f64>,
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
}

/// Apply one effect at scenario time `now` (unix s).
pub fn apply(e: &EffectNotice, movers: &mut [Mover], now: f64) -> EffectApplied {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::Scenario;

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
                Ours { uid: "1".into(), lon: Some(-122.4951), lat: Some(37.8121) },
                Ours { uid: "2".into(), lon: Some(-122.4700), lat: Some(37.8160) },
                Ours { uid: "3".into(), lon: None, lat: None },
                // the platform itself, if a bridge sent it: never collateral
                Ours { uid: "9".into(), lon: Some(-122.4950), lat: Some(37.8120) },
            ],
            impact_t: None,
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
}
