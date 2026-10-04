//! Engagement effects (Phase E) on the truth side. The bridge hands an engagement's abstract
//! effect back on the simulation channel, in its answer to `POST /api/sim/contacts`: a point,
//! a radius, a time, the contact its target track was associated with, and where our
//! vehicles reported themselves. Here every contact inside the radius is flagged with the
//! time; one whose scenario says `on_effect = "out_of_play"` (the default) leaves the
//! scenario, so our sensors stop detecting it (that is what the assessment reads). The
//! record of what was inside, whether the intended contact was, and any collateral (another
//! contact, or one of our vehicles, inside the radius) goes to the evaluation truth log
//! only: the C2 never sees it. No damage, blast or lethality is modelled: a flag and a time.

use murmur_standins::geo::distance_m;
use serde::{Deserialize, Serialize};

use crate::motion::Mover;
use crate::scenario::OnEffect;

/// One of our vehicles as the bridge last heard it (its reported position).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ours {
    pub uid: String,
    pub lon: f64,
    pub lat: f64,
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
    #[serde(default)]
    pub ours: Vec<Ours>,
}

/// Collateral: anything inside the radius that was not the intended contact.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Collateral {
    pub contacts: Vec<String>,
    pub ours: Vec<String>,
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
    /// The intended contact was inside the radius when the effect was applied.
    pub target_inside: bool,
    /// Every contact inside the radius, with its distance from the point.
    pub inside: Vec<(String, f64)>,
    /// Contacts taken out of the scenario.
    pub out_of_play: Vec<String>,
    pub collateral: Collateral,
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
    for o in &e.ours {
        if distance_m(lat, lon, o.lat, o.lon) <= e.radius_m {
            collateral.ours.push(o.uid.clone());
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
        target_inside,
        inside,
        out_of_play: out,
        collateral,
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
            ours: vec![
                Ours { uid: "1".into(), lon: -122.4951, lat: 37.8121 },
                Ours { uid: "2".into(), lon: -122.4700, lat: 37.8160 },
            ],
        };
        let r = apply(&e, &mut movers, 101.0);
        assert!(r.target_inside);
        assert_eq!(r.out_of_play, vec!["target".to_string()]);
        assert_eq!(r.collateral.contacts, vec!["near".to_string()]);
        assert_eq!(r.collateral.ours, vec!["1".to_string()]);
        assert_eq!(r.inside.len(), 2);
        // the target leaves the scenario; the sailboat persists, flagged; the ferry untouched
        for m in &mut movers {
            m.step(2.0, 1.0, None);
        }
        assert!(!movers[0].st.exists);
        assert!(movers[1].st.exists && movers[1].effect_t == Some(101.0));
        assert!(movers[2].st.exists && movers[2].effect_t.is_none());
    }
}
