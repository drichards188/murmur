//! Lost contacts (rules 13, [`crate::rules::LOST_CONTACT_SINCE`], murmur-533): a contact the
//! tracker drops is not forgotten by the decision core.
//!
//! The bridge's tracker drops a track it has not detected for `MURMUR_TRACK_DROP_S` (180 s).
//! Before rules 13 the keep-out stopped counting it from then on, so a civilian boat that went
//! out of sensor view near a target stopped protecting anything once its track aged out, and
//! an effect could be released where it might still be. Now:
//!
//! - **The drop is a logged fact.** The bridge hands the engine every track it drops with its
//!   facts at the drop ([`crate::Engine::record_track_drops`], `track.dropped`), and keeps the
//!   track in the picture until the engine has it, so no decision falls between the two. The
//!   engine holds each as a [`LostContact`] in its ledger ([`LostLedger`]), as replay does from
//!   the log.
//! - **It still protects.** Every engagement's keep-out on tracks counts a lost contact
//!   anywhere it could have reached since its last fix, by the reach model the keep-out reads
//!   for a live track ([`crate::engage::track_reach`] on its facts at the drop: along its
//!   velocity with its predicted uncertainty, along its average velocity, anywhere at the
//!   fastest it went if it manoeuvred, anywhere at its domain's top speed without a velocity
//!   or an average velocity), growing without a cap: fail closed, as a lost vehicle of ours. It
//!   is exempt where a live track would be (a declared hostile, by the identification ledger),
//!   and never the target (a target out of the picture fails its own checks).
//! - **An operator may accept it, for one engagement.** As a lost vehicle of ours (DECISIONS.md
//!   "A lost friendly blocks until an operator accepts it, explicitly and logged"): an operator
//!   with engagement authority, at the nomination or while it waits with no signature, logged
//!   as `engage.accepted` naming the contact (`contact`, `since` its last fix) under the
//!   operator's session with its facts and the world's time; every check of that engagement
//!   reads it, every signer's picture token carries it, a reset of the signatures clears it,
//!   any operator may revoke it, never an agent. It covers that loss only.
//! - **Seen again, unambiguously, it is retired.** The tracker starts a new track (a new id)
//!   for a contact it sees again after a drop: association across a drop is by identity, and
//!   the only identity here that names one contact is its AIS (MMSI). The engine retires a lost
//!   contact ([`reacquisitions`], `track.reacquired`, with the picture it judged) only when
//!   exactly one live track carries its MMSI and no other lost contact does, that track is
//!   undeclared and its tracker sees nothing that contradicts its AIS (its estimate is not
//!   suspect), it is in the same domain, was first seen after the lost contact's last fix, and
//!   is where the lost contact could have reached. Anything less leaves the lost contact in
//!   force beside the new track (fail closed): a contact without AIS is never matched.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::engage::{self, EngageRules};
use crate::geo::haversine_m;
use crate::time::Time;
use crate::world::{Affiliation, TrackFacts};

/// A contact the tracker dropped (`track.dropped`), as the keep-out counts it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LostContact {
    /// When the engine recorded the drop (the `track.dropped` event's time).
    pub dropped_t: Time,
    /// Its facts as the tracker handed them at the drop: its last fix (`facts.position`, at
    /// `facts.position.t`), its velocity, uncertainty, identity and affiliation estimate then.
    /// In a decision's inputs its identification is filled from the ledger, as a live track's.
    pub facts: TrackFacts,
}

impl LostContact {
    /// Its last fix: what it has been out of view since, and what an acceptance names.
    pub fn since(&self) -> Time {
        self.facts.position.t
    }
}

/// The lost contacts in force, by the track id each was dropped under.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LostLedger {
    contacts: BTreeMap<String, LostContact>,
}

impl LostLedger {
    /// Track `id` dropped at `t` with `facts`. A second drop of the same id (never on the
    /// bridge, whose ids are not reused) replaces the first: it is a newer loss.
    pub fn record(&mut self, id: &str, facts: TrackFacts, t: Time) {
        self.contacts.insert(id.to_string(), LostContact { dropped_t: t, facts });
    }

    /// Retire lost contact `id` (seen again): what it was.
    pub fn retire(&mut self, id: &str) -> Option<LostContact> {
        self.contacts.remove(id)
    }

    pub fn get(&self, id: &str) -> Option<&LostContact> {
        self.contacts.get(id)
    }

    pub fn contacts(&self) -> &BTreeMap<String, LostContact> {
        &self.contacts
    }

    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.contacts.len()
    }

    /// The lost contacts as a decision reads them: each one's identification filled from the
    /// ledger `ident` (a declaration in force on its id exempts it, as a live track's).
    pub fn filled(&self, ident: &crate::ident::Ledger, rule_id: bool) -> BTreeMap<String, LostContact> {
        if !rule_id {
            return self.contacts.clone();
        }
        let mut facts: BTreeMap<String, TrackFacts> =
            self.contacts.iter().map(|(id, l)| (id.clone(), l.facts.clone())).collect();
        ident.fill(&mut facts);
        self.contacts
            .iter()
            .map(|(id, l)| {
                let f = facts.remove(id).unwrap_or_else(|| l.facts.clone());
                (id.clone(), LostContact { dropped_t: l.dropped_t, facts: f })
            })
            .collect()
    }
}

/// A lost contact seen again ([`reacquisitions`]): which, as which live track, and why that
/// is unambiguous.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reacquired {
    pub track: String,
    pub by_track: String,
    pub evidence: String,
}

/// Every lost contact of `ledger` that a live track of `tracks` (the picture, its
/// identification filled) is, unambiguously (see the module), in id order. A live track
/// matches at most one (its MMSI is unique among the lost contacts by the rule itself).
pub fn reacquisitions(ledger: &LostLedger, tracks: &BTreeMap<String, TrackFacts>, ec: &EngageRules) -> Vec<Reacquired> {
    ledger
        .contacts
        .iter()
        .filter_map(|(id, lost)| {
            match_one(id, lost, ledger, tracks, ec).ok().map(|(by_track, evidence)| Reacquired {
                track: id.clone(),
                by_track,
                evidence,
            })
        })
        .collect()
}

/// Whether one live track of `tracks` is lost contact `id` again: `Ok((its id, the evidence))`,
/// or `Err(why not)`.
pub fn match_one(
    id: &str,
    lost: &LostContact,
    ledger: &LostLedger,
    tracks: &BTreeMap<String, TrackFacts>,
    ec: &EngageRules,
) -> Result<(String, String), String> {
    let Some(ais) = &lost.facts.ais else {
        return Err("it had no AIS identity: nothing names it uniquely".into());
    };
    let others_lost = ledger
        .contacts
        .iter()
        .filter(|(k, l)| k.as_str() != id && l.facts.ais.as_ref().is_some_and(|a| a.mmsi == ais.mmsi))
        .count();
    if others_lost > 0 {
        return Err(format!("another lost contact also carried MMSI {}", ais.mmsi));
    }
    let claimants: Vec<(&String, &TrackFacts)> = tracks
        .iter()
        .filter(|(k, t)| k.as_str() != id && t.ais.as_ref().is_some_and(|a| a.mmsi == ais.mmsi))
        .collect();
    let (tid, t) = match claimants.as_slice() {
        [] => return Err(format!("no track carries MMSI {}", ais.mmsi)),
        [one] => *one,
        many => {
            return Err(format!(
                "{} tracks carry MMSI {}: ambiguous",
                many.len(),
                ais.mmsi
            ));
        }
    };
    if t.declaration.is_some() || t.declared_by.is_some() {
        return Err(format!("{tid} is declared: not taken for a protected contact"));
    }
    if matches!(t.affiliation.value, Affiliation::Suspect | Affiliation::Hostile) {
        return Err(format!(
            "{tid} is {}: what our sensors see does not fit its AIS",
            t.affiliation.value.as_str()
        ));
    }
    if t.domain != lost.facts.domain {
        return Err(format!("{tid} is {:?}, not {:?}", t.domain, lost.facts.domain));
    }
    let first = t.first_seen_t.unwrap_or(t.position.t);
    if first <= lost.since() {
        return Err(format!("{tid} was first seen before {id}'s last fix"));
    }
    // where it is, against where the lost contact could have got to by then
    let at = t.position.t;
    let regions = engage::track_reach(&lost.facts, at, ec);
    let slack = ec.keep_out_sigma * t.pos_sigma_m;
    let (gap, _) = regions
        .iter()
        .map(|r| (haversine_m(r.center, t.position.value) - r.radius_m - slack, r.kind))
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .ok_or("no region to judge it by")?;
    if gap > 0.0 {
        return Err(format!(
            "{tid} is {gap:.0} m beyond where {id} could have reached since its last fix"
        ));
    }
    Ok((
        tid.clone(),
        format!(
            "{tid} is the only track carrying AIS MMSI {} ({}), undeclared, {} and consistent with it, first seen {:.0} s after {id}'s last fix, within where {id} could have reached; no other lost contact carries it",
            ais.mmsi,
            ais.name,
            t.affiliation.value.as_str(),
            first.ms_since(lost.since()) as f64 / 1000.0
        ),
    ))
}
