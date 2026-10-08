//! Lost contacts (rules 16, [`crate::rules::LOST_CONTACT_SINCE`], murmur-533): a contact the
//! tracker drops is not forgotten by the decision core.
//!
//! The bridge's tracker drops a track it has not detected for `MURMUR_TRACK_DROP_S` (180 s).
//! Before rules 16 the keep-out stopped counting it from then on, so a civilian boat that went
//! out of sensor view near a target stopped protecting anything once its track aged out, and
//! an effect could be released where it might still be. Now:
//!
//! - **The drop is a logged fact.** The bridge hands the engine every track it drops with its
//!   facts at the drop ([`crate::Engine::record_track_drops`], `track.dropped`), and keeps the
//!   track in the picture until the engine has it, so no decision falls between the two. The
//!   engine holds each as a [`LostContact`] in its ledger ([`LostLedger`]), as replay does from
//!   the log. A track id is dropped once (the bridge never reuses one): a second drop of an id
//!   the ledger holds is refused, and replay flags one.
//! - **It still protects.** Every engagement's keep-out on tracks counts a lost contact
//!   anywhere it could have reached since its last fix ([`crate::engage::lost_reach`] on its
//!   facts at the drop): the regions the keep-out reads for a live track and, since nothing
//!   sees it turn or speed up, anywhere at its domain's top speed (or its own bound, if faster),
//!   growing without a cap: fail closed, as a lost vehicle of ours. It is exempt where a live
//!   track would be (a declared hostile, by the identification ledger), and never the target
//!   (a target out of the picture fails its own checks). A live track out of view counts the
//!   same disc ([`crate::engage::OUT_OF_VIEW_S`]).
//! - **An operator may accept it, for one engagement.** As a lost vehicle of ours (DECISIONS.md
//!   "A lost friendly blocks until an operator accepts it, explicitly and logged"): an operator
//!   with engagement authority, at the nomination or while it waits with no signature, logged
//!   as `engage.accepted` naming the contact (`contact`, `since` its last fix) under the
//!   operator's session with its facts and the world's time; every check of that engagement
//!   reads it, every signer's picture token carries it, a reset of the signatures clears it,
//!   any operator may revoke it, never an agent. It covers that loss only.
//! - **Seen again, an operator confirms it.** The tracker starts a new track (a new id) for a
//!   contact it sees again after a drop; association across a drop is by identity, and the only
//!   identity here that names one contact is its AIS (MMSI), which a contact may broadcast for
//!   another (spoofed) and the tracker finds suspect only once EO/IR has looked long enough. So
//!   a live track carrying a lost contact's MMSI is only a claim ([`claims`], `lost.claimed`,
//!   logged once with the picture and the evidence: names and ship types compared, how far it
//!   is against how far the lost contact could have got, what our sensors say of its kind),
//!   shown for review. Any operator may confirm one under their own session
//!   ([`crate::Engine::confirm_lost_claim`], `track.reacquired`, replay-checked; never an
//!   agent), and only while nothing contradicts it: the only track carrying the MMSI and no
//!   other lost contact carrying it, undeclared, not suspect (what our sensors see fits its
//!   AIS), in the same domain, first seen after the lost contact's last fix, where the lost
//!   contact could have reached. Until then the lost contact keeps blocking beside the new
//!   track (fail closed); a contact without AIS is never claimed.
//! - **A confirmation lasts while its claimant stands.** If the claimant later turns suspect,
//!   is declared or leaves the picture (dropped), the confirmation is void and the lost contact
//!   counts again ([`void_reason`]): every decision reads it so on its own picture
//!   ([`LostLedger::in_force_for`]), and the loop logs it (`lost.reinstated`) and keeps it so.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::engage::{self, EngageRules};
use crate::geo::haversine_m;
use crate::ids::OperatorId;
use crate::time::Time;
use crate::world::{Affiliation, AisIdentity, TrackFacts};

/// A contact the tracker dropped (`track.dropped`), as the keep-out counts it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LostContact {
    /// When the engine recorded the drop (the `track.dropped` event's time).
    pub dropped_t: Time,
    /// Its facts as the tracker handed them at the drop: its last fix (`facts.position`, at
    /// `facts.position.t`), its velocity, uncertainty, identity and affiliation estimate then.
    /// In a decision's inputs its identification is filled from the ledger, as a live track's.
    pub facts: TrackFacts,
    /// The AIS identity its track carried at the drop (the world's `track_ais`), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ais: Option<AisIdentity>,
}

impl LostContact {
    /// Its last fix: what it has been out of view since, and what an acceptance names.
    pub fn since(&self) -> Time {
        self.facts.position.t
    }
}

/// A lost contact an operator confirmed as a live track (`track.reacquired`): retired from the
/// keep-outs while that track stands ([`void_reason`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Confirmed {
    pub contact: LostContact,
    /// The live track it was confirmed as.
    pub by_track: String,
    pub by: OperatorId,
    pub t: Time,
}

/// The lost contacts, by the track id each was dropped under: those in force, those an
/// operator confirmed as a live track, and the claims raised.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LostLedger {
    contacts: BTreeMap<String, LostContact>,
    confirmed: BTreeMap<String, Confirmed>,
    /// (lost contact, claimant) pairs already raised (`lost.claimed`), each raised once.
    claimed: BTreeSet<(String, String)>,
}

impl LostLedger {
    /// Track `id` dropped at `t` with `facts` and its AIS identity then. Refused for an id the
    /// ledger already holds (in force or confirmed): the tracker drops an id once.
    pub fn record(&mut self, id: &str, facts: TrackFacts, ais: Option<AisIdentity>, t: Time) -> Result<(), String> {
        if self.contacts.contains_key(id) || self.confirmed.contains_key(id) {
            return Err(format!("{id} was dropped already: a track id is dropped once"));
        }
        self.contacts.insert(id.to_string(), LostContact { dropped_t: t, facts, ais });
        Ok(())
    }

    /// Whether `id` is a lost contact, in force or confirmed.
    pub fn holds(&self, id: &str) -> bool {
        self.contacts.contains_key(id) || self.confirmed.contains_key(id)
    }

    /// Lost contact `id` in force.
    pub fn get(&self, id: &str) -> Option<&LostContact> {
        self.contacts.get(id)
    }

    /// The lost contacts in force.
    pub fn contacts(&self) -> &BTreeMap<String, LostContact> {
        &self.contacts
    }

    /// The lost contacts an operator confirmed as a live track.
    pub fn confirmed(&self) -> &BTreeMap<String, Confirmed> {
        &self.confirmed
    }

    /// Nothing lost: none in force and none confirmed.
    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty() && self.confirmed.is_empty()
    }

    /// How many are in force.
    pub fn len(&self) -> usize {
        self.contacts.len()
    }

    /// Retire lost contact `id` as live track `by_track`, confirmed by `by` at `t`.
    pub fn confirm(&mut self, id: &str, by_track: &str, by: &OperatorId, t: Time) -> Result<(), String> {
        let contact = self.contacts.remove(id).ok_or_else(|| format!("{id} is not a lost contact in force"))?;
        self.confirmed.insert(
            id.to_string(),
            Confirmed {
                contact,
                by_track: by_track.to_string(),
                by: by.clone(),
                t,
            },
        );
        Ok(())
    }

    /// A confirmation void: lost contact `id` counts again. What it was confirmed as.
    pub fn reinstate(&mut self, id: &str) -> Option<Confirmed> {
        let c = self.confirmed.remove(id)?;
        self.contacts.insert(id.to_string(), c.contact.clone());
        Some(c)
    }

    /// Whether the claim (`lost`, `track`) was raised already.
    pub fn claimed(&self, lost: &str, track: &str) -> bool {
        self.claimed.contains(&(lost.to_string(), track.to_string()))
    }

    /// Record the claim (`lost`, `track`) as raised.
    pub fn claim(&mut self, lost: &str, track: &str) {
        self.claimed.insert((lost.to_string(), track.to_string()));
    }

    /// The confirmations `picture` voids: each confirmed contact whose claimant fails in it,
    /// with why ([`void_reason`]), in id order.
    pub fn void_in(&self, picture: &BTreeMap<String, TrackFacts>) -> Vec<(String, String, String)> {
        self.confirmed
            .iter()
            .filter_map(|(id, c)| void_reason(&c.by_track, picture).map(|why| (id.clone(), c.by_track.clone(), why)))
            .collect()
    }

    /// The lost contacts in force as a decision reads them: each one's identification filled
    /// from the ledger `ident` (a declaration in force on its id exempts it, as a live track's).
    pub fn filled(&self, ident: &crate::ident::Ledger, rule_id: bool) -> BTreeMap<String, LostContact> {
        fill(&self.contacts, ident, rule_id)
    }

    /// The lost contacts a decision on `picture` (its identification filled) reads: those in
    /// force and those whose confirmation that picture voids (the claimant gone, suspect or
    /// declared in it), whether or not the loop has logged it yet, each filled from `ident`.
    pub fn in_force_for(
        &self,
        picture: &BTreeMap<String, TrackFacts>,
        ident: &crate::ident::Ledger,
        rule_id: bool,
    ) -> BTreeMap<String, LostContact> {
        let mut all = self.contacts.clone();
        for (id, _, _) in self.void_in(picture) {
            if let Some(c) = self.confirmed.get(&id) {
                all.insert(id, c.contact.clone());
            }
        }
        fill(&all, ident, rule_id)
    }
}

fn fill(contacts: &BTreeMap<String, LostContact>, ident: &crate::ident::Ledger, rule_id: bool) -> BTreeMap<String, LostContact> {
    if !rule_id {
        return contacts.clone();
    }
    let mut facts: BTreeMap<String, TrackFacts> = contacts.iter().map(|(id, l)| (id.clone(), l.facts.clone())).collect();
    ident.fill(&mut facts);
    contacts
        .iter()
        .map(|(id, l)| {
            let f = facts.remove(id).unwrap_or_else(|| l.facts.clone());
            (
                id.clone(),
                LostContact {
                    dropped_t: l.dropped_t,
                    facts: f,
                    ais: l.ais.clone(),
                },
            )
        })
        .collect()
}

/// Why a confirmation as `by_track` is void on `picture` (its identification filled), if it
/// is: the claimant is no longer in the picture (dropped), is suspect or hostile (what our
/// sensors see no longer fits its AIS), or is declared.
pub fn void_reason(by_track: &str, picture: &BTreeMap<String, TrackFacts>) -> Option<String> {
    let Some(t) = picture.get(by_track) else {
        return Some(format!("{by_track} is no longer in the picture (dropped)"));
    };
    if t.declaration.is_some() || t.declared_by.is_some() {
        return Some(format!("{by_track} is declared"));
    }
    if matches!(t.affiliation.value, Affiliation::Suspect | Affiliation::Hostile) {
        return Some(format!(
            "{by_track} is {}: what our sensors see no longer fits its AIS",
            t.affiliation.value.as_str()
        ));
    }
    None
}

/// A live track carrying a lost contact's AIS identity ([`claims`]): which, the evidence an
/// operator weighs, and what keeps it from being confirmed now (empty: it may be).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Claim {
    /// The lost contact.
    pub lost: String,
    /// The live track claiming to be it.
    pub track: String,
    pub mmsi: String,
    /// The claimant's distance from the lost contact's last fix, m.
    pub distance_m: f64,
    /// How far the lost contact could have got from its last fix by the claimant's fix, m
    /// (its farthest region's edge, [`engage::lost_reach`]).
    pub reach_m: f64,
    pub evidence: String,
    /// What keeps it from being confirmed (empty: an operator may confirm it).
    pub blockers: Vec<String>,
}

impl Claim {
    /// Whether an operator may confirm it now.
    pub fn confirmable(&self) -> bool {
        self.blockers.is_empty()
    }
}

/// Every claim on `ledger`'s lost contacts in force on `tracks` (the picture, its
/// identification filled) with the AIS identities heard for it (`track_ais`): each live track
/// carrying a lost contact's MMSI, in (lost contact, track) order.
pub fn claims(
    ledger: &LostLedger,
    tracks: &BTreeMap<String, TrackFacts>,
    track_ais: &BTreeMap<String, AisIdentity>,
    ec: &EngageRules,
) -> Vec<Claim> {
    let mut out = Vec::new();
    for (id, lost) in &ledger.contacts {
        let Some(ais) = &lost.ais else { continue };
        for (tid, a) in track_ais {
            if tid != id
                && a.mmsi == ais.mmsi
                && tracks.contains_key(tid)
                && let Some(c) = claim_of(id, lost, ledger, tid, tracks, track_ais, ec)
            {
                out.push(c);
            }
        }
    }
    out
}

/// The claim of live track `tid` of `tracks` to be lost contact `id` (`lost`): None unless both
/// carry the same MMSI.
pub fn claim_of(
    id: &str,
    lost: &LostContact,
    ledger: &LostLedger,
    tid: &str,
    tracks: &BTreeMap<String, TrackFacts>,
    track_ais: &BTreeMap<String, AisIdentity>,
    ec: &EngageRules,
) -> Option<Claim> {
    let ais = lost.ais.as_ref()?;
    let (t, theirs) = (tracks.get(tid)?, track_ais.get(tid)?);
    if theirs.mmsi != ais.mmsi || tid == id {
        return None;
    }
    let mut blockers = Vec::new();
    let mut said = Vec::new();
    // names and ship types compared (shown: an AIS identity is what the contact says it is)
    let same = |a: &str, b: &str| if a == b { "the same".to_string() } else { format!("{id} gave {b}") };
    said.push(format!(
        "{tid} carries AIS MMSI {} as {id} did: name {} ({}), ship type {} ({})",
        ais.mmsi,
        theirs.name,
        same(&theirs.name, &ais.name),
        or_none(&theirs.ship_type),
        same(&theirs.ship_type, &ais.ship_type)
    ));
    // unambiguous: no other live track or lost contact carries it
    let others: Vec<&String> = track_ais
        .iter()
        .filter(|(k, a)| k.as_str() != tid && k.as_str() != id && a.mmsi == ais.mmsi && tracks.contains_key(k.as_str()))
        .map(|(k, _)| k)
        .collect();
    let others_lost: Vec<&String> = ledger
        .contacts
        .iter()
        .chain(ledger.confirmed.iter().map(|(k, c)| (k, &c.contact)))
        .filter(|(k, l)| k.as_str() != id && l.ais.as_ref().is_some_and(|a| a.mmsi == ais.mmsi))
        .map(|(k, _)| k)
        .collect();
    if !others.is_empty() {
        blockers.push(format!("{} also carr{} MMSI {}: ambiguous", list(&others), if others.len() == 1 { "ies" } else { "y" }, ais.mmsi));
    }
    if !others_lost.is_empty() {
        blockers.push(format!("lost contact {} also carried MMSI {}: ambiguous", list(&others_lost), ais.mmsi));
    }
    // where it is against where the lost contact could have got to by its fix
    let at = t.position.t;
    let regions = engage::lost_reach(&lost.facts, at, ec);
    let slack = ec.keep_out_sigma * t.pos_sigma_m;
    let distance_m = haversine_m(lost.facts.position.value, t.position.value);
    let gap = regions
        .iter()
        .map(|r| haversine_m(r.center, t.position.value) - r.radius_m - slack)
        .fold(f64::INFINITY, f64::min);
    let reach_m = regions
        .iter()
        .map(|r| haversine_m(r.center, lost.facts.position.value) + r.radius_m)
        .fold(0.0, f64::max);
    let since = at.ms_since(lost.since()) as f64 / 1000.0;
    said.push(format!(
        "{} from {id}'s last fix, which {id} could have got {} from in the {since:.0} s since",
        km(distance_m),
        km(reach_m)
    ));
    if gap > 0.0 {
        blockers.push(format!("{tid} is {} beyond where {id} could have reached since its last fix", km(gap)));
    }
    // what our sensors say of its kind (EO/IR or sonar classify; radar gives a size class)
    let mut classified: Vec<&str> = t
        .sources
        .iter()
        .filter_map(|s| s.split(':').nth(1))
        .filter(|s| matches!(*s, "eo_ir" | "sonar"))
        .collect();
    // one name per kind of sensor, however many of ours carry one
    classified.sort_unstable();
    classified.dedup();
    let by = if classified.is_empty() {
        "no classifying sensor of ours (its kind is from its AIS or a radar's size class)".to_string()
    } else {
        format!("by {}", classified.join(", "))
    };
    said.push(match (&t.identity, &lost.facts.identity) {
        (Some(k), Some(was)) if k == was => format!("our sensors call {tid} {k} ({:.2}, {by}), as {id} was: the kinds agree", t.identity_confidence),
        (Some(k), Some(was)) => format!("our sensors call {tid} {k} ({:.2}, {by}), {id} was {was}: the kinds differ", t.identity_confidence),
        (Some(k), None) => format!("our sensors call {tid} {k} ({:.2}, {by}); {id} was not classified", t.identity_confidence),
        (None, _) => format!("our sensors have not classified {tid}"),
    });
    // what nothing may contradict
    let declared = t.declaration.is_some() || t.declared_by.is_some();
    if declared {
        blockers.push(format!("{tid} is declared: not taken for a protected contact"));
    }
    if matches!(t.affiliation.value, Affiliation::Suspect | Affiliation::Hostile) {
        blockers.push(format!(
            "{tid} is {}: what our sensors see does not fit its AIS",
            t.affiliation.value.as_str()
        ));
    }
    if t.domain != lost.facts.domain {
        blockers.push(format!("{tid} is {:?}, {id} was {:?}", t.domain, lost.facts.domain));
    }
    let first = t.first_seen_t.unwrap_or(t.position.t);
    if first <= lost.since() {
        blockers.push(format!("{tid} was first seen before {id}'s last fix: not a new sighting of it"));
    }
    said.push(format!(
        "{tid} {}, {}, first seen {:.0} s after {id}'s last fix",
        t.affiliation.value.as_str(),
        if declared { "declared" } else { "undeclared" },
        first.ms_since(lost.since()) as f64 / 1000.0
    ));
    if others.is_empty() && others_lost.is_empty() {
        said.push(format!("no other track or lost contact carries MMSI {}", ais.mmsi));
    }
    Some(Claim {
        lost: id.to_string(),
        track: tid.to_string(),
        mmsi: ais.mmsi.clone(),
        distance_m: (distance_m * 10.0).round() / 10.0,
        reach_m: (reach_m * 10.0).round() / 10.0,
        evidence: said.join("; "),
        blockers,
    })
}

fn or_none(s: &str) -> &str {
    if s.is_empty() { "none given" } else { s }
}

fn list(ids: &[&String]) -> String {
    ids.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
}

/// "640 m", "1.9 km".
fn km(m: f64) -> String {
    if m.abs() < 1_000.0 { format!("{m:.0} m") } else { format!("{:.1} km", m / 1000.0) }
}
