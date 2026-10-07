//! Engagement scenarios (Phase E exit: every invariant has a scenario that tries to break
//! it). A scenario is a TOML file (`sim/scenarios/engage/*.toml`) describing a small world
//! (our vehicles, perception tracks, operators with or without engagement authority, an
//! agent), a timeline of actions (declare, nominate, approve, abort, a track or a vehicle
//! changing, a link going silent, an agent trying to approve), and the designed outcome.
//! [`run`] plays it against a real [`Engine`] on a [`ManualClock`] with the repo's policy,
//! catalog and areas, steps engagements every tick as the bridge does, replays the audit
//! log, and, if the scenario names a `tamper`, attacks the log (with a recomputed hash chain,
//! the strongest attacker short of an external anchor) and checks that replay catches it.
//!
//! No physics and no truth process: tracks move along their velocity and are "detected"
//! every tick unless silenced; the engaging platform flies straight to the effect point at
//! its catalog cruise speed once dispatched, and follows its fallback when stopped (hold in
//! place, return home, or orbit where it is). After the effect the target track stops being
//! detected (`after_effect = "out_of_play"`, the default) or keeps being detected
//! (`"persists"`). It is a decision-layer harness: the live stack (bridge, stand-ins,
//! contacts) runs the same engine.
//!
//! Deterministic: same file, same options, same log. [`RunOptions::seed`] adds seeded
//! Gaussian noise to the track estimates (`track_noise_m`), for batch runs.
//!
//! The format is documented on [`Scenario`] and in `sim/scenarios/engage/README.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::areas::AreaSet;
use crate::audit::{EventBody, MemorySink, parse_log};
use crate::authority::{Grants, OperatorSession, Roster};
use crate::catalog::{Catalog, Domain};
use crate::engage::{self, EngageWindow, EngagementEvent, Fallback, offset};
use crate::engine::{Engine, Order, Routed};
use crate::geo::{LonLat, haversine_m};
use crate::hash::{GENESIS, sha256_hex};
use crate::ids::{AgentId, IntentId, OperatorId, Uid};
use crate::intent::{Proposal, Rationale, State, Task};
use crate::policy::Policy;
use crate::replay;
use crate::rules::Outcome;
use crate::terrain::PolygonTerrain;
use crate::time::{ManualClock, Time};
use crate::world::{Affiliation, Fact, TrackFacts, VehicleFacts, WorldSnapshot};

/// Engine time at scenario second 0.
pub const T0: Time = Time::from_secs(1_781_000_000);

/// A scenario file.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub title: String,
    /// The invariant it attacks (a rule id such as `keep_out_tracks`, or `two_person`,
    /// `agent_cannot_authorize`, `audit`).
    pub invariant: String,
    /// The attack, in a sentence.
    pub attack: String,
    /// The designed end of the engagement: `rejected`, `voided`, `aborted`, `done`,
    /// `pending_approval`, `expired`.
    pub expect: String,
    /// The check whose failure ends it (rejected, voided, aborted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_rule: Option<String>,
    /// `persists`, `ceased` or `unknown` (done).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_assessment: Option<String>,
    /// A refusal the timeline must have met (a substring of the engine's error), e.g.
    /// "may not approve it".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expect_refusals: Vec<String>,
    /// The signatures must have been dropped at least once (`approval.reset`): the frozen
    /// plan stopped holding.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expect_reset: bool,
    /// Rules 6: the platform must have followed its preloaded lost-link action on its own (a
    /// substring of what it did, e.g. "Loiter or Hold").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_lost_link: Option<String>,
    /// Rules 6: preload reports that must have been logged, in order (substrings: `set
    /// confirmed`, `set unconfirmed`, `restore confirmed`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expect_preload: Vec<String>,
    /// How the last nomination was stopped: `superseded` (another order stands, no fallback
    /// commanded), `system`, `picture`, `operator` or (rules 9) `revoked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_stop: Option<String>,
    /// Rules 9: identification reports that must have been logged, in order (substrings of
    /// the runner's `ident_reports`: `declared T-0001 by rule:hostile_act`, `flagged F-1
    /// attribution`, `revoked T-0001 by op-2`, ...).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expect_ident: Vec<String>,
    /// Rules 9: substrings no identification report may contain (`declared T-0003`: never
    /// declared, whatever happened).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expect_no_ident: Vec<String>,
    /// Rules 12: debts closed without a restore that replay must list, in order (substrings
    /// of its `restores_closed`: `expended by its own effect`, `written off by op-1`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expect_closed: Vec<String>,
    /// Rules 12: nothing may still be owed back when the log ends (replay's `unrestored`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expect_nothing_owed: bool,
    /// Rules 13: lost contacts retired as seen again, in order (substrings of
    /// `t=<s> T-0002 is T-0005 again`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expect_reacquired: Vec<String>,
    /// Rules 13: none may be retired (an ambiguous or missing match keeps a lost contact).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expect_none_reacquired: bool,
    /// After the run, attack the log and require replay to catch it:
    /// `proposer_approval`, `agent_approval`, `suppress_void`, `edit_snapshot`,
    /// `drop_signature`, `edit_effect`, `strip_approvers`, `drop_reset`, `agent_order_auto`,
    /// `drop_ack`, `loosen_policy`, and for rules 6 `drop_preload`, `forge_preload`,
    /// `drop_verify`, `drop_acceptance`, `acceptance_by_agent`, `acceptance_without_authority`,
    /// `hide_self_accepted`, and for rules 7 `drop_restore_owed`, and for rules 13
    /// `hide_lost_contact` (every decision's lost contacts cut), `drop_track_dropped` (the
    /// tracker's drops cut), `forge_lost_acceptance` (an acceptance of a lost contact re-dated
    /// to a later fix everywhere it appears).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tamper: Option<String>,
    /// Seconds to run (default: the last step plus 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_s: Option<u32>,
    /// What the target does after the effect: `out_of_play` (default: no more detections)
    /// or `persists`.
    #[serde(default = "out_of_play")]
    pub after_effect: String,
    /// Rules 6: how the engaging platform answers its lost-link preload at dispatch:
    /// `confirm` (default: read back as set, a second later), `no_answer` (it never answers
    /// the parameter protocol: the bridge reports the error), `mismatch` (it answers with
    /// another lost-link action than the order's), `never` (nothing is reported: the release
    /// waits for it until the engagement is late), `changed` (the set confirms, but by the
    /// re-read near the release another station has changed the action). A silent
    /// platform never answers. A confirmed platform is read again every 5 s from 30 s before
    /// the release window opens (`engage.preload` verify).
    #[serde(default = "confirm")]
    pub preload: String,
    /// Rules 6: the platform answers the restore of its own lost-link values only from this
    /// scenario second on (default: as soon as the engagement ended and it can hear us).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_after_s: Option<u32>,
    /// Paths relative to the repo root (defaults: sim/policy.toml, sim/platforms.toml,
    /// sim/areas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub areas: Option<String>,
    #[serde(default, rename = "operator")]
    pub operators: Vec<OperatorDef>,
    #[serde(default, rename = "agent")]
    pub agents: Vec<AgentDef>,
    #[serde(default, rename = "vehicle")]
    pub vehicles: Vec<VehicleDef>,
    #[serde(default, rename = "track")]
    pub tracks: Vec<TrackDef>,
    #[serde(default, rename = "step")]
    pub steps: Vec<Step>,
}

fn out_of_play() -> String {
    "out_of_play".into()
}

fn confirm() -> String {
    "confirm".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorDef {
    pub id: String,
    /// Engagement authority (`MURMUR_ENGAGE_OPERATORS` on the bridge).
    #[serde(default)]
    pub engage: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDef {
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VehicleDef {
    pub uid: String,
    /// Catalog id (`sim/platforms.toml`).
    pub platform: String,
    /// `[lon, lat]`.
    pub at: [f64; 2],
    /// East, north m/s.
    #[serde(default)]
    pub velocity: [f64; 2],
    #[serde(default = "battery")]
    pub battery: f64,
    #[serde(default = "interval")]
    pub report_interval_ms: u32,
    /// Rules 6: its altitude above mean sea level, reported with its position, m (absent:
    /// not reported, so a keep-out reads it in 2D).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_m: Option<f64>,
    /// Its own lost-link timer, s, as `sim/links.toml` sets it for its link (default 5): what
    /// a preload shortens and a restore puts back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost_link_s: Option<f64>,
    /// Lost before the run starts: silent from the start, its last report this many seconds
    /// before it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lost_for_s: Option<u32>,
}

fn battery() -> f64 {
    95.0
}

fn interval() -> u32 {
    1000
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrackDef {
    pub id: String,
    pub at: [f64; 2],
    #[serde(default)]
    pub velocity: [f64; 2],
    #[serde(default = "sigma")]
    pub sigma_m: f64,
    pub affiliation: Affiliation,
    /// How confidently the tracker holds that affiliation (default 0.8).
    #[serde(default = "aff_conf")]
    pub affiliation_confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default = "conf")]
    pub identity_confidence: f64,
    #[serde(default = "conf")]
    pub confidence: f64,
    #[serde(default = "surface")]
    pub domain: Domain,
    /// Seconds since the last detection at the start.
    #[serde(default)]
    pub age_s: f64,
    /// No detections at all (a track the sensors lost).
    #[serde(default)]
    pub silent: bool,
    #[serde(default)]
    pub sources: Vec<String>,
    /// Its estimated altitude (negative: depth), m (absent: unknown, as from an ESM bearing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_m: Option<f64>,
    /// The tracker has no velocity estimate for it yet (a new track: its velocity comes from
    /// fixes a baseline apart), however it moves.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_velocity_estimate: bool,
    /// Rules 6: its altitude was last measured this long before the start, s, and never
    /// again: the ranging sensors (radar, AIS) lost it while the others keep its position
    /// fresh (0: measured with every detection).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub alt_age_s: f64,
    /// Rules 9, for the evaluation's labels only (never shown to the engine): what the contact
    /// behind the track really is (default: hostile if `affiliation` is, else the estimate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth: Option<Affiliation>,
    /// Rules 9: the tracker saw it manoeuvre (its velocity changed by more than its noise)
    /// at the start, and the fastest it went since, m/s ([`TrackFacts::manoeuvre`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manoeuvre_speed_mps: Option<f64>,
    /// Rules 12: the tracker started it this long before the start, s
    /// ([`TrackFacts::first_seen_t`]); it has an average velocity (the velocity it holds)
    /// once it has been tracked [`SCENARIO_MEAN_MIN_S`] with a velocity estimate, as the
    /// bridge's tracker does. Absent: [`SCENARIO_TRACKED_S`] for a track the file lists (one
    /// the tracker followed before the scenario starts), 0 for one a `track` step makes (it
    /// appears then: young for its first [`SCENARIO_MEAN_MIN_S`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_s: Option<f64>,
    /// Rules 13: the AIS identity its contact broadcasts, as our receivers hear it
    /// ([`TrackFacts::ais`]): what a lost contact is matched again by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ais: Option<crate::world::AisIdentity>,
}

/// Rules 12, scenarios: how long a track is tracked with a velocity estimate before it has an
/// average velocity (the bridge tracker's `MEAN_MIN_S`).
pub const SCENARIO_MEAN_MIN_S: f64 = 45.0;

/// Rules 12, scenarios: how long a track the file lists was followed before the start, unless
/// it says (`tracked_s`): the scenarios' world begins with established tracks.
pub const SCENARIO_TRACKED_S: f64 = 600.0;

fn sigma() -> f64 {
    15.0
}

fn aff_conf() -> f64 {
    0.8
}

fn conf() -> f64 {
    0.9
}

fn surface() -> Domain {
    Domain::Surface
}

/// One timeline entry: at `at_s` seconds, do `action`.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Step {
    pub at_s: u32,
    #[serde(flatten)]
    pub action: Action,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// An operator declares a track's affiliation (`hostile` is never inferred).
    Declare {
        track: String,
        affiliation: Affiliation,
        by: String,
    },
    /// An operator (`by`) or an agent (`agent`, with a rationale) nominates an engagement.
    Nominate {
        #[serde(default)]
        by: Option<String>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
        platform: String,
        track: String,
        radius_m: f64,
        fallback: Fallback,
        #[serde(default)]
        window_s: Option<u32>,
        /// Rules 6: our vehicles the nominating operator accepts as lost (each since its last
        /// report, as the check shows it). Only an operator's nomination can carry them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        accept: Vec<String>,
        /// Rules 13: the lost contacts (track ids the tracker dropped) the nominating operator
        /// accepts, each since its last fix as the check shows it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        accept_lost: Vec<String>,
    },
    /// Rules 6: an operator with engagement authority accepts, for the last nomination while
    /// it waits with no signature, that one of our vehicles is lost since its last report
    /// (or since `since_s`, scenario seconds, to name another report: the engine refuses
    /// it). Without engagement authority it is refused.
    ///
    /// Rules 13: or a lost contact, `track` instead of `uid` (since its last fix, or `since_s`).
    Accept {
        by: String,
        #[serde(default)]
        uid: Option<String>,
        #[serde(default)]
        track: Option<String>,
        #[serde(default)]
        since_s: Option<f64>,
    },
    /// Rules 6: an operator withdraws an acceptance of a lost vehicle (rules 13: or of a lost
    /// contact, `track`) for the last nomination (pending or under way).
    Revoke {
        by: String,
        #[serde(default)]
        uid: Option<String>,
        #[serde(default)]
        track: Option<String>,
    },
    /// Rules 13: the tracker drops `track` (as the bridge's does after 180 s without a
    /// detection), handing the engine its facts now; the runner stops carrying it.
    DropTrack { track: String },
    /// An agent "tries" to accept a lost vehicle: there is no such method on its port, so it
    /// is recorded and nothing may change.
    AgentAccept { agent: String, uid: String },
    /// An operator with engagement authority asks for the picture of the last nomination
    /// (a picture token, as the console does before a signature); the next `approve` by them
    /// quotes it.
    Look { by: String },
    /// An operator signs the last nomination (with engagement authority if they have it;
    /// without it the engine refuses), quoting the picture token from their last `look`, or
    /// one asked for now (the console's look-then-sign in one step).
    Approve { by: String },
    /// An operator approves the last `task` (an order waiting in the inbox); with
    /// `acknowledge`, accepting that its dispatch stops an engagement in play.
    ApproveTask {
        by: String,
        #[serde(default)]
        acknowledge: bool,
    },
    /// Another order on some of our vehicles (hold, rtl, or an orbit around `center` of
    /// `radius_m`) from an operator (`by`) or an agent (`agent`, with a rationale). Once
    /// approved it goes as on the bridge: dispatched if the link reaches its vehicles, else
    /// queued (rules 5) and re-checked each second until it can go; either way it stands
    /// over any engagement on those vehicles (from rules 7 already when it is queued).
    Task {
        #[serde(default)]
        by: Option<String>,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        rationale: Option<String>,
        task: String,
        uids: Vec<String>,
        #[serde(default)]
        center: Option<[f64; 2]>,
        #[serde(default)]
        radius_m: Option<f64>,
    },
    /// The loop stalls: no engagement step for `seconds` (a bridge that stopped holding the
    /// picture).
    Stall { seconds: u32 },
    Reject { by: String },
    /// Any operator stops it.
    Abort { by: String },
    /// An agent tries to approve: there is no agent path to approval (the runner records
    /// that the agent's port has no such method, and the intent must not move).
    AgentApprove { agent: String },
    /// A track changes (or appears): any field given replaces the current one.
    Track {
        id: String,
        #[serde(default)]
        at: Option<[f64; 2]>,
        #[serde(default)]
        velocity: Option<[f64; 2]>,
        #[serde(default)]
        affiliation: Option<Affiliation>,
        #[serde(default)]
        declared_by: Option<String>,
        #[serde(default)]
        undeclare: bool,
        #[serde(default)]
        identity: Option<String>,
        #[serde(default)]
        identity_confidence: Option<f64>,
        #[serde(default)]
        sigma_m: Option<f64>,
        #[serde(default)]
        domain: Option<Domain>,
        /// Stop (true) or resume (false) detections.
        #[serde(default)]
        silent: Option<bool>,
        /// The tracker's velocity estimate is off by this much (east, north m/s) from the
        /// contact's real velocity, until set again: an estimate that wobbles, not a turn.
        #[serde(default)]
        velocity_error: Option<[f64; 2]>,
        /// Its estimated altitude, m (rules 6 read keep-outs in 3D where it is known).
        #[serde(default)]
        alt_m: Option<f64>,
        /// Rules 12: a track this step makes is one the tracker started this long ago, s
        /// ([`TrackDef::tracked_s`]: 0, a new track).
        #[serde(default)]
        tracked_s: Option<f64>,
        /// Rules 13: the AIS identity its contact broadcasts.
        #[serde(default)]
        ais: Option<crate::world::AisIdentity>,
    },
    /// One of our vehicles changes: position, velocity, link (silent: no reports; a report
    /// interval: the link class changed), navigation source (`gnss`, `dead_reckoning`).
    Vehicle {
        uid: String,
        #[serde(default)]
        at: Option<[f64; 2]>,
        #[serde(default)]
        velocity: Option<[f64; 2]>,
        #[serde(default)]
        silent: Option<bool>,
        #[serde(default)]
        report_interval_ms: Option<u32>,
        #[serde(default)]
        nav: Option<crate::world::NavSource>,
        /// The GPS receiver's fix type (3: a 3D fix).
        #[serde(default)]
        gps_fix: Option<u8>,
        /// The navigation filter flags the GPS as glitching.
        #[serde(default)]
        gps_glitch: Option<bool>,
        /// Fly a circle `[lon, lat, radius_m]` at the speed of its velocity (the bridge
        /// reports it as the vehicle's `circling` fact); a radius of 0 stops circling.
        #[serde(default)]
        circling: Option<[f64; 3]>,
        /// Report a circle `[lon, lat, radius_m]` the vehicle does not fly (a wrong or stale
        /// fact: it keeps its own position and velocity); a radius of 0 stops reporting it.
        #[serde(default)]
        reported_circle: Option<[f64; 3]>,
        /// Its altitude above mean sea level, m (reported with its position).
        #[serde(default)]
        alt_m: Option<f64>,
    },
    /// Rules 9: an abstract hostile act (no weapon of any kind: a time, a victim and where its
    /// author appeared to be). The contact behind track `by_track` (or an untracked contact at
    /// `at`) acts against our vehicle `against_vehicle` or the contact behind track
    /// `against_track`; our sensors `sensors` (`"<uid>:<sensor>"`) observe it and place its
    /// author with 1-sigma `sigma_m` (default 20 m), displaced by `offset` (east, north
    /// metres; default none) and, in a batch with act noise, by seeded Gaussian noise of that
    /// sigma. The engine attributes it and its rule declares, flags or records it.
    Act {
        #[serde(default)]
        by_track: Option<String>,
        #[serde(default)]
        at: Option<[f64; 2]>,
        #[serde(default)]
        against_vehicle: Option<String>,
        #[serde(default)]
        against_track: Option<String>,
        sensors: Vec<String>,
        #[serde(default = "act_sigma")]
        sigma_m: f64,
        #[serde(default)]
        offset: Option<[f64; 2]>,
    },
    /// Rules 9: an operator confirms the rule declaration in force on a track.
    ConfirmDeclaration { by: String, track: String },
    /// Rules 9: an operator revokes the declaration in force on a track (it stops the
    /// engagements in play on it, first).
    RevokeDeclaration {
        by: String,
        track: String,
        #[serde(default)]
        note: Option<String>,
    },
    /// Rules 9: an operator confirms or dismisses a review flag on a track: `flag` (its
    /// number), or the newest in force on it.
    ResolveFlag {
        by: String,
        track: String,
        #[serde(default)]
        flag: Option<u64>,
        resolution: crate::ident::FlagResolution,
    },
    /// Rules 9: an agent "tries" to declare a track hostile: there is no such method on its
    /// port, so it is recorded and nothing may change.
    AgentDeclare { agent: String, track: String },
    /// Rules 12: an operator writes off the lost-link values owed back to one of our vehicles
    /// (it is lost, spent, or not the vehicle they were preloaded on), saying why.
    WriteOff { by: String, uid: String, note: String },
    /// Check the last nomination now.
    Expect {
        #[serde(default)]
        state: Option<String>,
        #[serde(default)]
        rule: Option<String>,
        #[serde(default)]
        concurrences: Option<usize>,
        /// The state of the last `task` step's intent.
        #[serde(default)]
        task_state: Option<String>,
        /// A substring of the reason the last `task` step's intent is in its state.
        #[serde(default)]
        task_reason: Option<String>,
    },
}

/// Options for batch runs.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// Seed for the track estimate noise (none: no noise).
    pub seed: Option<u64>,
    /// 1-sigma Gaussian noise added to every track position estimate each tick, metres
    /// (independent around the truth).
    pub track_noise_m: f64,
    /// 1-sigma error of every track's velocity estimate, m/s, correlated over about 10 s
    /// (as a tracker's smoothed velocity wobbles: a first-order Gauss-Markov process).
    pub velocity_noise_mps: f64,
    /// Decide under these decision rules instead of the current ones (none: the current):
    /// to show what an older version did with the same timeline.
    pub rules_version: Option<u32>,
    /// Rules 9: each act's observed position also carries seeded Gaussian noise of its
    /// `sigma_m` (with `seed`): the identification evaluation's batches. Off: the scripted
    /// offset only, so a scenario's designed outcome does not depend on the seed.
    pub act_noise: bool,
    /// Rules 9: changes to the policy's `[identification]` (the evaluation's manual-versus-rule
    /// comparison on the same seeds): each key given replaces the file's, the rest stay.
    pub identification: IdentOverride,
    /// Rules 9, the evaluation: a scripted operator (the file's first operator) who reviews
    /// what the identification puts in the inbox, by its own procedure over what the console
    /// shows ([`ReviewProxy`]). A model, never human data.
    pub proxy: Option<ReviewProxy>,
    /// Rules 9, the evaluation: nominate every track declared hostile (the file's first armed
    /// vehicle, as the file's first operator, the effect radius 75 m, fallback return) as soon
    /// as it is declared and again every 10 s while refused, and have the first two operators
    /// with engagement authority look and sign 5 and 8 s after it waits for them. What an
    /// engagement on a declared track would come to, by the clock.
    pub nominator: bool,
    /// murmur-dup: a clock that steps back, as a wall clock does (this WSL host's, about
    /// 0.65 s every 32 s): the engine's clock and every time the run stamps (facts, the
    /// world's time, scripted times) read it, as the bridge's facts and engine both read the
    /// wall clock before it kept a clock of its own. None: the clock never steps.
    pub clock_step: Option<ClockStep>,
}

/// A clock that steps back by `back_ms` every `every_s` seconds of the run
/// ([`RunOptions::clock_step`]). A step longer than the run's one-second tick makes the clock
/// read earlier than it did the tick before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockStep {
    pub every_s: u32,
    pub back_ms: i64,
}

/// The run's clock `ms` milliseconds after [`T0`]: `T0` plus `ms`, less the steps taken by
/// then (none before `T0`).
fn stepped(ms: i64, step: Option<ClockStep>) -> Time {
    let back = match step {
        Some(c) if ms > 0 && c.every_s > 0 => c.back_ms * (ms / (i64::from(c.every_s) * 1000)),
        _ => 0,
    };
    T0.plus_ms(ms - back)
}

/// Rules 9: the identification keys a batch sets over the file's policy.
#[derive(Clone, Debug, Default)]
pub struct IdentOverride {
    pub rule_declaration: Option<bool>,
    pub attribution_threshold: Option<f64>,
    pub nominate_unconfirmed: Option<bool>,
    pub corroboration: Option<crate::ident::Corroboration>,
}

impl IdentOverride {
    pub fn is_empty(&self) -> bool {
        self.rule_declaration.is_none()
            && self.attribution_threshold.is_none()
            && self.nominate_unconfirmed.is_none()
            && self.corroboration.is_none()
    }
}

/// Rules 9, the evaluation's scripted operator (`RunOptions::proxy`): a model with stated
/// parameters, never human data, logged as the file's first operator (dev auth: asserted).
///
/// **When.** It looks at the inbox every `attend_s` (median; lognormal, sigma 0.4), an item
/// first seen at a uniformly random point of that interval, and spends `read_s` (median;
/// lognormal, sigma 0.3) on an item. At each look it reads the item with probability
/// `diligence`, else skims.
///
/// **What it reads** ([`console_pick`]): what the console shows for an act, the fix's
/// position with its 1-sigma, and the tracks there as shown (position, 1-sigma, affiliation
/// and its confidence, a declaration). Its procedure, its own and not the engine's rule:
/// the tracks within two combined sigmas of the fix (the victim excluded); if a declared
/// hostile is among them, nothing to add; if exactly one track is there and it is not shown
/// neutral or assumed friend at 0.5 or more, that is the author; anything else (none, several,
/// only a neutral) and it can't say. With probability `misread` a reading goes wrong: it takes
/// another track within three combined sigmas, or, reviewing a declaration, the other
/// answer.
///
/// **What it does.** An act the rule only recorded (rule declaration off): when it reads it,
/// it declares the author it picked (note `proxy: act A-n`); a skimmed act waits for its next
/// look (five at most). A rule declaration: when it reads it, it confirms it if it picks the
/// declared track, else revokes it; skimmed, it confirms (as the proxy operator approves a
/// card it skims). A flag from an act: when it reads it, it declares the author it picked,
/// answering the flag, else dismisses it; a skimmed flag waits. A proximity flag: dismissed
/// when read (keeping company is not an act).
#[derive(Clone, Debug)]
pub struct ReviewProxy {
    pub attend_s: f64,
    pub read_s: f64,
    pub diligence: f64,
    pub misread: f64,
}

impl Default for ReviewProxy {
    /// The proxy operator model's defaults (`eval/proxy.py`): a look every 20 s, 6 s to read,
    /// reads 80% of items; a 5% slip.
    fn default() -> Self {
        ReviewProxy {
            attend_s: 20.0,
            read_s: 6.0,
            diligence: 0.8,
            misread: 0.05,
        }
    }
}

/// The note the proxy's declarations carry (the evaluation links them to the act).
pub const PROXY_NOTE: &str = "proxy: act ";

/// What [`console_pick`] makes of an act.
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// The one track the console shows at the fix: its author.
    Author(String),
    /// A declared hostile is at the fix already: nothing to add.
    Declared(String),
    /// It can't say (no track there, several, or only a neutral or friend).
    CantSay(&'static str),
}

/// The evaluation proxy's procedure over what the console shows for an act (see
/// [`ReviewProxy`]): the tracks within two combined sigmas of the fix (its victim excluded),
/// on `tracks` as identified when the act came in.
pub fn console_pick(report: &crate::ident::ActReport, tracks: &BTreeMap<String, TrackFacts>) -> Pick {
    let victim = match &report.against {
        crate::ident::Against::Track { track } => Some(track.as_str()),
        _ => None,
    };
    let near: Vec<(&String, &TrackFacts)> = tracks
        .iter()
        .filter(|(id, _)| victim != Some(id.as_str()))
        .filter(|(_, t)| {
            let ring = 2.0 * (report.sigma_m.powi(2) + t.pos_sigma_m.powi(2)).sqrt();
            haversine_m(engage::track_at(t, report.observed_t), report.location) <= ring
        })
        .collect();
    if let Some((id, _)) = near
        .iter()
        .find(|(_, t)| t.affiliation.value == Affiliation::Hostile && t.declared_by.is_some())
    {
        return Pick::Declared((*id).clone());
    }
    match near.as_slice() {
        [] => Pick::CantSay("no track at the fix"),
        [(id, t)] => {
            let shown_protected = matches!(t.affiliation.value, Affiliation::Neutral | Affiliation::AssumedFriend)
                && t.affiliation_confidence >= 0.5;
            if shown_protected {
                Pick::CantSay("only a neutral or friend at the fix")
            } else {
                Pick::Author((*id).clone())
            }
        }
        _ => Pick::CantSay("several tracks at the fix"),
    }
}

/// A slip: another track within three combined sigmas of the fix (the victim excluded), if
/// any, drawn with `u` in [0, 1).
fn misread_pick(
    report: &crate::ident::ActReport,
    tracks: &BTreeMap<String, TrackFacts>,
    not: Option<&str>,
    u: f64,
) -> Option<String> {
    let victim = match &report.against {
        crate::ident::Against::Track { track } => Some(track.as_str()),
        _ => None,
    };
    let near: Vec<&String> = tracks
        .iter()
        .filter(|(id, _)| victim != Some(id.as_str()) && not != Some(id.as_str()))
        .filter(|(_, t)| {
            let ring = 3.0 * (report.sigma_m.powi(2) + t.pos_sigma_m.powi(2)).sqrt();
            haversine_m(engage::track_at(t, report.observed_t), report.location) <= ring
        })
        .map(|(id, _)| id)
        .collect();
    if near.is_empty() {
        return None;
    }
    let i = ((u * near.len() as f64) as usize).min(near.len() - 1);
    Some(near[i].clone())
}

/// One item the proxy has to look at.
#[derive(Clone, Debug)]
struct ProxyItem {
    due_s: u32,
    looks: u32,
    kind: ProxyKind,
}

#[derive(Clone, Debug)]
enum ProxyKind {
    /// An act the rule only recorded (its number).
    Act(String),
    /// A rule declaration (its track and number).
    Declaration(String, u64),
    /// A flag (its number).
    Flag(u64),
}

fn act_sigma() -> f64 {
    20.0
}

/// Rules 9: one identification event of a run with the truth behind it (the evaluation's
/// labels: was the track really hostile, was the act really its act). `t_s`: scenario seconds.
#[derive(Clone, Debug, Serialize)]
pub struct IdentRecord {
    /// `act`, `rule_declaration`, `operator_declaration`, `confirmed`, `revoked`, `flag`,
    /// `flag_resolved`.
    pub kind: String,
    pub t_s: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub act: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    /// Flag kind, verdict, or resolution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<f64>,
    /// Truth: the track's contact is hostile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truth_hostile: Option<bool>,
    /// Truth: the act was this track's contact's (for declarations and flags from an act).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truth_actor: Option<bool>,
    /// The act's true author (a track id, or `untracked`), and when it acted (scenario s).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub act_t_s: Option<f64>,
}

/// Rules 9, the evaluation: the review proxy's work in one run. Every item put before it ends
/// in exactly one decision once read (or skimmed, for a rule declaration: confirmed unread), so
/// `decisions` counts alike whether the item was an act (manual), a rule declaration or a flag.
#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq)]
pub struct ProxyStats {
    /// Items put before it (an act, a rule declaration, a flag).
    pub items: u32,
    /// Looks, a skimmed item looked at again counted each time.
    pub looks: u32,
    /// Items it read.
    pub reads: u32,
    /// Decisions, one per item it got to the end of (`declared + confirmed + revoked +
    /// dismissed + nothing`).
    pub decisions: u32,
    pub declared: u32,
    pub confirmed: u32,
    /// Of `confirmed`, skimmed unread (as a card skimmed is approved).
    pub confirmed_unread: u32,
    pub revoked: u32,
    pub dismissed: u32,
    /// Read, and decided nothing (an act it could not attribute, with the rule off).
    pub nothing: u32,
}

/// What a run produced.
#[derive(Clone, Debug, Serialize)]
pub struct ScenarioResult {
    pub file: String,
    pub title: String,
    pub invariant: String,
    pub attack: String,
    /// The designed outcome held, replay reproduced every decision, and a tamper (if any)
    /// was caught.
    pub ok: bool,
    pub failures: Vec<String>,
    pub expect: String,
    pub final_state: Option<String>,
    pub final_reason: Option<String>,
    /// The checks that failed at the end (submission checks for a rejection, the picture
    /// for a void or abort).
    pub failed_rules: Vec<String>,
    pub assessment: Option<String>,
    pub refusals: Vec<String>,
    pub fallback: Option<String>,
    /// Rules 6: what the platform did on its own after losing its link with the fallback
    /// preloaded as its lost-link action (`"<action> after <n> s without the GCS"`).
    pub lost_link_action: Option<String>,
    /// Rules 6: the preload reports logged (`set confirmed`, `set unconfirmed`, `restore
    /// confirmed`, ...), in order.
    pub preload_reports: Vec<String>,
    pub replay_ok: bool,
    pub replay_events: usize,
    pub replay_mismatches: Vec<String>,
    pub tamper: Option<String>,
    /// Replay caught the forged log (none: no tamper, or nothing in this run's log for it to
    /// edit, see `tamper_not_applicable`).
    pub tamper_detected: Option<bool>,
    /// Why the tamper had nothing to edit in this run's log (a seeded run can end
    /// differently from the designed one).
    pub tamper_not_applicable: Option<String>,
    pub tamper_mismatches: Vec<String>,
    /// How many times the signatures were dropped (`approval.reset`).
    pub resets: usize,
    /// Rules 13: the lost contacts retired as seen again (`t=<s> T-0002 is T-0005 again`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reacquired: Vec<String>,
    /// Rules 9: the identification events, in order (`t=<s> declared T-0001 by
    /// rule:hostile_act ...`), and the same with the truth behind each (evaluation labels).
    pub ident_reports: Vec<String>,
    pub ident: Vec<IdentRecord>,
    /// Rules 9, the evaluation: what the review proxy did, counted alike in every arm (none
    /// without the proxy).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<ProxyStats>,
    /// The audit log, for the harness to keep.
    #[serde(skip)]
    pub log: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ScenarioError {
    #[error("{0}: {1}")]
    Io(String, std::io::Error),
    #[error("{0}: {1}")]
    Parse(String, toml::de::Error),
    #[error("{0}: {1}")]
    Setup(String, String),
}

/// Load one scenario file.
pub fn load(path: &Path) -> Result<Scenario, ScenarioError> {
    let name = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|e| ScenarioError::Io(name.clone(), e))?;
    let sc: Scenario = toml::from_str(&text).map_err(|e| ScenarioError::Parse(name.clone(), e))?;
    if !matches!(sc.preload.as_str(), "confirm" | "no_answer" | "mismatch" | "never" | "changed") {
        return Err(ScenarioError::Parse(
            name,
            serde::de::Error::custom(format!("preload {:?}: confirm, no_answer, mismatch, never or changed", sc.preload)),
        ));
    }
    Ok(sc)
}

/// Every `*.toml` in a directory, in name order.
pub fn files(dir: &Path) -> Result<Vec<PathBuf>, ScenarioError> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| ScenarioError::Io(dir.display().to_string(), e))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    v.sort();
    Ok(v)
}

/// Load and run a file; `root` is the repo (for sim/policy.toml and friends).
pub fn run_file(path: &Path, root: &Path, opts: &RunOptions) -> Result<ScenarioResult, ScenarioError> {
    let sc = load(path)?;
    run(&sc, &path.display().to_string(), root, opts)
}

// ---- the run --------------------------------------------------------------------------------

struct VehicleSim {
    def: VehicleDef,
    nav: crate::world::NavSource,
    gps_fix: u8,
    gps_glitch: bool,
    /// the circle reported for it (center, radius m), and whether it flies it
    circling: Option<(LonLat, f64)>,
    circling_flown: bool,
    pos: LonLat,
    vel: [f64; 2],
    home: LonLat,
    silent: bool,
    interval: u32,
    last_t: Time,
    /// where the platform is flying (dispatched engagement, or fallback home)
    goal: Option<LonLat>,
    orbit: bool,
    /// its altitude MSL as reported (none: not reported)
    alt: Option<f64>,
    /// when its link was cut (it then hears no GCS either)
    silent_since: Option<u32>,
}

struct TrackSim {
    facts: TrackFacts,
    /// Where the contact really is and how it moves (the estimates are these plus the
    /// run's noise and any scripted velocity error).
    truth: LonLat,
    truth_vel: [f64; 2],
    vel_error: [f64; 2],
    /// The correlated part of the velocity estimate's error (`velocity_noise_mps`).
    vel_noise: [f64; 2],
    silent: bool,
    /// no velocity estimate (the tracker has none yet)
    no_velocity: bool,
    /// rules 6: its altitude is no longer measured (the ranging sensors lost it)
    alt_frozen: bool,
    /// rules 9, labels only: what its contact really is
    truth_aff: Affiliation,
}

fn is_zero(x: &f64) -> bool {
    *x == 0.0
}

/// Golden Gate land/water: land south of the Presidio shore and on Marin, water between.
fn terrain() -> PolygonTerrain {
    use crate::geo::Polygon;
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
        land: vec![b(-122.60, 37.70, -122.30, 37.800), b(-122.60, 37.832, -122.46, 37.95)],
        water_depth_m: 20.0,
    }
}

#[derive(Default)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gauss(&mut self) -> f64 {
        let u1 = self.next().max(1e-12);
        let u2 = self.next();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Play a scenario. `file` is only for the result.
pub fn run(
    sc: &Scenario,
    file: &str,
    root: &Path,
    opts: &RunOptions,
) -> Result<ScenarioResult, ScenarioError> {
    let setup = |e: String| ScenarioError::Setup(file.to_string(), e);
    let rel = |p: &Option<String>, d: &str| root.join(p.as_deref().unwrap_or(d));
    let mut policy = Policy::load(&rel(&sc.policy, "sim/policy.toml")).map_err(|e| setup(e.to_string()))?;
    if let Some(v) = opts.rules_version {
        policy.rules.rules_version = v;
        // older rules have no [identification]: no build of theirs logged one
        if v < crate::rules::RULE_ID_SINCE {
            policy.identification = None;
        }
    }
    // rules 9: the evaluation's identification setting, the same seeds either way
    let o = &opts.identification;
    if !o.is_empty() {
        let mut i = policy.identification.clone().unwrap_or_default();
        if let Some(on) = o.rule_declaration {
            i.rule_declaration = on;
        }
        if o.corroboration.is_some() {
            i.corroboration = o.corroboration;
        }
        if o.attribution_threshold.is_some() {
            i.attribution_threshold = o.attribution_threshold;
        }
        if o.nominate_unconfirmed.is_some() {
            i.nominate_unconfirmed = o.nominate_unconfirmed;
        }
        i.validate().map_err(setup)?;
        policy.identification = Some(i);
        policy.sync();
    }
    let catalog =
        Catalog::load(&rel(&sc.catalog, "sim/platforms.toml")).map_err(|e| setup(e.to_string()))?;
    let areas = AreaSet::load_dir(&rel(&sc.areas, "sim/areas"), T0).map_err(|e| setup(e.to_string()))?;
    let clock = ManualClock::new(T0);
    let log = MemorySink::new();
    // the operators the file lists, those with engagement authority on the engagement list
    // (the bridge's MURMUR_OPERATORS and MURMUR_ENGAGE_OPERATORS)
    let roster = Roster::new(
        sc.operators.iter().map(|o| OperatorId::new(&o.id)).collect(),
        sc.operators
            .iter()
            .filter(|o| o.engage)
            .map(|o| OperatorId::new(&o.id))
            .collect(),
    )
    .map_err(setup)?;
    let mut engine = Engine::with_areas(
        policy,
        Some(areas.clone()),
        roster,
        Box::new(clock.clone()),
        Box::new(log.clone()),
    )
    .map_err(|e| setup(e.to_string()))?;
    let terrain = terrain();

    let mut sessions: BTreeMap<String, OperatorSession> = BTreeMap::new();
    for o in &sc.operators {
        let s = engine
            .open_operator_session_with(OperatorId::new(&o.id), Grants { engage: o.engage })
            .map_err(|e| setup(e.to_string()))?;
        sessions.insert(o.id.clone(), s);
    }
    let mut vehicles: BTreeMap<String, VehicleSim> = BTreeMap::new();
    for v in &sc.vehicles {
        if catalog.get(&v.platform).is_none() {
            return Err(setup(format!("vehicle {}: platform {} not in the catalog", v.uid, v.platform)));
        }
        let at = LonLat::new(v.at[0], v.at[1]);
        vehicles.insert(
            v.uid.clone(),
            VehicleSim {
                def: v.clone(),
                nav: crate::world::NavSource::Gnss,
                gps_fix: 3,
                gps_glitch: false,
                circling: None,
                circling_flown: false,
                pos: at,
                vel: v.velocity,
                home: at,
                silent: v.lost_for_s.is_some(),
                interval: v.report_interval_ms,
                last_t: T0.plus_ms(-i64::from(v.lost_for_s.unwrap_or(0)) * 1000),
                goal: None,
                orbit: false,
                alt: v.alt_m,
                silent_since: v.lost_for_s.map(|_| 0),
            },
        );
    }
    let mut tracks: BTreeMap<String, TrackSim> = BTreeMap::new();
    for t in &sc.tracks {
        let mut sim = track_sim(t);
        // a tracker fed noisier detections says so in its position uncertainty
        if opts.seed.is_some() {
            sim.facts.pos_sigma_m = sim.facts.pos_sigma_m.max(opts.track_noise_m);
        }
        // rules 12: the bridge reports when it started a track only from rules 12 on
        if !engine.policy().rules.followups() {
            sim.facts.first_seen_t = None;
            sim.facts.mean_velocity_mps = None;
        }
        tracks.insert(t.id.clone(), sim);
    }
    // rules 9: a track the file lists as declared is declared through the engine's ledger,
    // by that operator, before the timeline starts (the engine fills it into every picture)
    let rule_id = engine.policy().rules.rule_id();
    if rule_id {
        for t in sc.tracks.iter().filter(|t| t.declared_by.is_some()) {
            let by = t.declared_by.clone().unwrap_or_default();
            let Some(sess) = sessions.get(&by) else {
                return Err(setup(format!("track {}: declared by {by}, not an operator of the file", t.id)));
            };
            if let Some(sim) = tracks.get_mut(&t.id) {
                sim.facts.declared_by = None;
            }
            let w = snapshot(&vehicles, &tracks, &catalog, &areas, T0);
            engine
                .declare_track(sess.authority(), None, &t.id, t.affiliation, None, None, &w)
                .map_err(|e| setup(e.to_string()))?;
        }
    }

    let end_s = sc
        .end_s
        .unwrap_or_else(|| sc.steps.iter().map(|s| s.at_s).max().unwrap_or(0) + 10);
    let mut rng = Rng(opts.seed.unwrap_or(0));
    let mut failures: Vec<String> = Vec::new();
    let mut refusals: Vec<String> = Vec::new();
    let mut last: Option<IntentId> = None;
    let mut target_out: Option<String> = None;
    let mut fallback_seen: Option<String> = None;
    let mut engaging: BTreeMap<IntentId, (String, LonLat)> = BTreeMap::new();
    let mut extra = Extra {
        // an act's own noise stream, so adding acts leaves the track noise of a seed unchanged
        act_noise: opts
            .seed
            .filter(|_| opts.act_noise)
            .map(|s| Rng(s ^ 0xAC75_0000_0000_0001)),
        proxy_rng: Rng(opts.seed.unwrap_or(1) ^ 0x3A27_0000_0000_0002),
        proxy: opts.proxy.clone().filter(|_| rule_id),
        nominator: opts.nominator && rule_id,
        clock_step: opts.clock_step,
        ..Extra::default()
    };

    for s in 0..=end_s {
        let now = stepped(i64::from(s) * 1000, opts.clock_step);
        clock.set(now);
        // kinematics: the contacts move along their velocity; a detection estimates where
        // one is (with independent noise around the truth each time), unless silent
        for (id, tr) in &mut tracks {
            let [e, n] = tr.truth_vel;
            if s > 0 {
                tr.truth = offset(tr.truth, e, n);
            }
            let silent = tr.silent || target_out.as_deref() == Some(id.as_str());
            if s > 0 && !silent {
                // the velocity estimate: the real one, plus a scripted error, plus a
                // wobble correlated over about 10 s
                if opts.velocity_noise_mps > 0.0 && opts.seed.is_some() {
                    let rho = (-0.1f64).exp();
                    let k = (1.0 - rho * rho).sqrt() * opts.velocity_noise_mps;
                    tr.vel_noise = [
                        rho * tr.vel_noise[0] + k * rng.gauss(),
                        rho * tr.vel_noise[1] + k * rng.gauss(),
                    ];
                }
                tr.facts.velocity_mps = (!tr.no_velocity).then_some([
                    tr.truth_vel[0] + tr.vel_error[0] + tr.vel_noise[0],
                    tr.truth_vel[1] + tr.vel_error[1] + tr.vel_noise[1],
                ]);
                let noisy = if opts.track_noise_m > 0.0 && opts.seed.is_some() {
                    offset(
                        tr.truth,
                        rng.gauss() * opts.track_noise_m,
                        rng.gauss() * opts.track_noise_m,
                    )
                } else {
                    tr.truth
                };
                tr.facts.position = Fact::new(noisy, now);
                tr.facts.last_seen_t = now;
                // rules 12: its average velocity once tracked long enough with a velocity
                if let (Some(first), Some(v)) = (tr.facts.first_seen_t, tr.facts.velocity_mps)
                    && tr.facts.mean_velocity_mps.is_none()
                    && now.ms_since(first) as f64 >= SCENARIO_MEAN_MIN_S * 1000.0
                {
                    tr.facts.mean_velocity_mps = Some(v);
                }
                // every source hit it; the ranging ones (and so the altitude) unless they lost it
                let frozen = tr.alt_frozen;
                for (src, st) in &mut tr.facts.source_t {
                    if !(frozen && ranging(src)) {
                        *st = now;
                    }
                }
                if !frozen && tr.facts.alt_m.is_some() {
                    tr.facts.alt_t = Some(now);
                }
            } else if s > 0 {
                // unobserved: the estimate stays where it was last seen
            }
        }
        // our vehicles
        for v in vehicles.values_mut() {
            let cruise = catalog
                .get(&v.def.platform)
                .and_then(|p| p.cruise_mps)
                .unwrap_or(0.0);
            if let Some(goal) = v.goal {
                let d = haversine_m(v.pos, goal);
                let step = cruise.min(d);
                if d > 1.0 && step > 0.0 {
                    let (ge, gn) = engage::enu(v.pos, goal);
                    let k = step / d;
                    v.vel = [ge / d * cruise, gn / d * cruise];
                    v.pos = offset(v.pos, ge * k, gn * k);
                } else {
                    v.vel = [0.0, 0.0];
                    if !v.orbit {
                        v.goal = None;
                    }
                }
            } else if let Some((c, radius)) = v.circling.filter(|_| s > 0 && v.circling_flown) {
                // a second around the circle, clockwise, at its speed
                let speed = v.vel[0].hypot(v.vel[1]);
                let (ce, cn) = engage::enu(c, v.pos);
                let a = ce.atan2(cn) + speed / radius.max(1.0);
                v.pos = offset(c, radius * a.sin(), radius * a.cos());
                v.vel = [speed * a.cos(), -speed * a.sin()];
            } else if s > 0 {
                v.pos = offset(v.pos, v.vel[0], v.vel[1]);
            }
            if !v.silent {
                v.last_t = now;
            }
        }
        // the timeline
        for step in sc.steps.iter().filter(|st| st.at_s == s) {
            act(
                &step.action,
                &mut engine,
                &sessions,
                &mut tracks,
                &mut vehicles,
                &catalog,
                &areas,
                now,
                &terrain,
                &mut last,
                &mut refusals,
                &mut fallback_seen,
                &mut failures,
                &mut engaging,
                &mut extra,
                s,
            );
        }
        // rules 6: the platforms answer their lost-link preloads (and restores), and one cut
        // off from the GCS long enough follows its preloaded action on its own
        preloads(
            &sc.preload,
            sc.restore_after_s.unwrap_or(0),
            &mut engine,
            &mut vehicles,
            &mut extra,
            &mut engaging,
            &mut fallback_seen,
            &mut failures,
            s,
        );
        // rules 9, the evaluation: the review proxy's items now due, then the nominator
        if extra.proxy.is_some() || extra.nominator {
            proxies(
                sc,
                &mut engine,
                &sessions,
                &mut tracks,
                &mut vehicles,
                &catalog,
                &areas,
                now,
                &terrain,
                &mut last,
                &mut refusals,
                &mut fallback_seen,
                &mut failures,
                &mut engaging,
                &mut extra,
                s,
            );
        }
        // the bridge's loop: expire, the weak-evidence rule (rules 9), then hold the picture /
        // release / observe
        let _ = engine.expire_due();
        let world = snapshot(&vehicles, &tracks, &catalog, &areas, now);
        if rule_id {
            match engine.step_identification(&world) {
                Ok(new) => {
                    if let Some(p) = extra.proxy.clone() {
                        for f in new {
                            let due = extra.first_look(&p, s);
                            extra.proxy_stats.items += 1;
                            extra.proxy_items.push(ProxyItem {
                                due_s: due,
                                looks: 1,
                                kind: ProxyKind::Flag(f),
                            });
                        }
                    }
                }
                Err(e) => failures.push(format!("t={s}: step_identification: {e}")),
            }
        }
        // rules 13: lost contacts seen again, unambiguously, are retired (as the bridge's loop)
        match engine.step_lost(&world) {
            Ok(found) => {
                for (lost, by) in found {
                    extra.reacquired.push(format!("t={s} {lost} is {by} again"));
                }
            }
            Err(e) => failures.push(format!("t={s}: step_lost: {e}")),
        }
        let outcome = if s < extra.stall_until {
            crate::engine::StepOutcome::default()
        } else {
            engine.step_engagements(&world, &terrain)
        };
        for (id, e) in &outcome.errors {
            failures.push(format!("t={s}: step_engagements {id}: {e}"));
        }
        {
            {
                for ev in outcome.events {
                    match ev {
                        EngagementEvent::Stopped(f) => {
                            fallback_seen = Some(f.fallback.as_str().into());
                            if let Some(v) = vehicles.get_mut(f.uid.as_str()) {
                                match f.fallback {
                                    Fallback::Abort => {
                                        v.goal = None;
                                        v.vel = [0.0, 0.0];
                                    }
                                    Fallback::Return => {
                                        v.goal = Some(v.home);
                                        v.orbit = false;
                                    }
                                    Fallback::Loiter => {
                                        v.goal = Some(v.pos);
                                        v.orbit = true;
                                    }
                                }
                            }
                            engaging.remove(&f.intent);
                        }
                        EngagementEvent::Effect(e) => {
                            if sc.after_effect != "persists" {
                                target_out = Some(e.track.clone());
                            }
                        }
                        EngagementEvent::Voided { .. } | EngagementEvent::Assessed { .. } => {}
                    }
                }
            }
        }
        // anything approved goes out at once, as on the bridge
        let world = snapshot(&vehicles, &tracks, &catalog, &areas, now);
        dispatch_approved(&mut engine, &world, &mut vehicles, &mut engaging, &mut failures, s);
        follow_queue(&mut engine, &world, &terrain, &mut vehicles, &mut engaging, &mut failures, s);
        for (id, (uid, _)) in &engaging {
            extra.preloads.entry(*id).or_insert_with(|| PreloadSim {
                uid: uid.clone(),
                due_s: s + 1,
                set: None,
                before: BTreeMap::new(),
                holds: BTreeMap::new(),
                timer_s: None,
                restored: false,
                acted: false,
                verified: 0,
            });
        }
    }

    // the outcome
    let intent = last.and_then(|id| engine.intent(id));
    let final_state = intent.map(|i| state_str(i.state()));
    let final_reason = intent.and_then(|i| i.history().last().map(|h| h.reason.clone()));
    let mut failed_rules: Vec<String> = Vec::new();
    if let Some(i) = intent {
        let from_watch = i
            .engagement()
            .and_then(|e| e.watch.as_ref())
            .filter(|w| !w.holds)
            .map(|w| w.results.clone());
        let results = from_watch.unwrap_or_else(|| i.checks().to_vec());
        for r in results {
            if r.outcome != Outcome::Pass && !failed_rules.contains(&r.rule.as_str().to_string()) {
                failed_rules.push(r.rule.as_str().into());
            }
        }
        // rules 9: stopped for a revoked declaration, before any picture could fail on it: the
        // target's (target_declared) or a bystander's its keep-out exempted (keep_out_tracks)
        if let Some(st) = i
            .engagement()
            .and_then(|e| e.stop.as_ref())
            .filter(|st| matches!(st.cause, engage::StopCause::Revoked { .. }))
        {
            let rule = if st.reason.starts_with("keep_out_tracks") {
                "keep_out_tracks"
            } else {
                "target_declared"
            };
            if !failed_rules.iter().any(|r| r == rule) {
                failed_rules.push(rule.into());
            }
        }
    }
    let assessment = intent
        .and_then(|i| i.engagement())
        .and_then(|e| e.assessment.as_ref())
        .map(|a| a.result.as_str().to_string());
    if final_state.as_deref() != Some(sc.expect.as_str()) {
        failures.push(format!(
            "expected the engagement to end {}, it is {} ({})",
            sc.expect,
            final_state.as_deref().unwrap_or("absent"),
            final_reason.as_deref().unwrap_or("")
        ));
    }
    if let Some(rule) = &sc.expect_rule
        && !failed_rules.contains(rule)
    {
        failures.push(format!(
            "expected check {rule} to fail, failed: [{}]",
            failed_rules.join(", ")
        ));
    }
    if let Some(a) = &sc.expect_assessment
        && assessment.as_deref() != Some(a.as_str())
    {
        failures.push(format!(
            "expected assessment {a}, got {}",
            assessment.as_deref().unwrap_or("none")
        ));
    }
    for want in &sc.expect_refusals {
        if !refusals.iter().any(|r| r.contains(want.as_str())) {
            failures.push(format!("expected a refusal containing {want:?}, got {refusals:?}"));
        }
    }
    if let Some(want) = &sc.expect_lost_link
        && !extra.lost_link_action.as_deref().is_some_and(|a| a.contains(want.as_str()))
    {
        failures.push(format!(
            "expected the platform to follow its preloaded lost-link action ({want}), it did {:?}",
            extra.lost_link_action
        ));
    }
    if let Some(want) = &sc.expect_stop {
        let stop = intent.and_then(|i| i.engagement()).and_then(|e| e.stop.as_ref());
        let got = stop.map(|st| st.cause.kind());
        if got != Some(want.as_str()) {
            failures.push(format!(
                "expected the engagement stopped as {want}, it was {} ({})",
                got.unwrap_or("not stopped"),
                stop.map(|st| st.reason.as_str()).unwrap_or("")
            ));
        }
        if want == "superseded" && stop.is_some_and(|st| st.fallback.is_some()) {
            failures.push("superseded, yet a fallback was commanded".into());
        }
    }
    let mut reports = extra.preload_reports.iter();
    for want in &sc.expect_preload {
        if !reports.any(|r| r.contains(want.as_str())) {
            failures.push(format!(
                "expected preload reports {:?} in order, got {:?}",
                sc.expect_preload, extra.preload_reports
            ));
            break;
        }
    }
    let text = log.text();
    // rules 9: what the identification ledger logged, with the truth behind it
    let (ident_reports, ident) = ident_records(&text, &tracks, &extra.acts);
    let mut reports_iter = ident_reports.iter();
    for want in &sc.expect_ident {
        if !reports_iter.any(|r| r.contains(want.as_str())) {
            failures.push(format!(
                "expected identification reports {:?} in order, got {:?}",
                sc.expect_ident, ident_reports
            ));
            break;
        }
    }
    for never in &sc.expect_no_ident {
        if let Some(r) = ident_reports.iter().find(|r| r.contains(never.as_str())) {
            failures.push(format!("an identification report must never say {never:?}: {r}"));
        }
    }
    let resets = text
        .lines()
        .filter(|l| l.contains("\"type\":\"approval.reset\""))
        .count();
    if sc.expect_reset && resets == 0 {
        failures.push("expected the signatures to be dropped (approval.reset), they never were".into());
    }
    // rules 13: lost contacts retired as seen again
    let mut retired = extra.reacquired.iter();
    for want in &sc.expect_reacquired {
        if !retired.any(|r| r.contains(want.as_str())) {
            failures.push(format!(
                "expected lost contacts retired {:?} in order, got {:?}",
                sc.expect_reacquired, extra.reacquired
            ));
            break;
        }
    }
    if sc.expect_none_reacquired && !extra.reacquired.is_empty() {
        failures.push(format!("expected no lost contact retired, got {:?}", extra.reacquired));
    }
    // replayed with the rules it was decided under (the current ones unless asked otherwise)
    let replay_opts = replay::ReplayOptions {
        rules: opts.rules_version.map_or_else(replay::RulesAccepted::default, replay::RulesAccepted::Exactly),
        ..replay::ReplayOptions::default()
    };
    let report = replay::replay_str_with(&text, &replay_opts).map_err(|e| setup(e.to_string()))?;
    let replay_mismatches: Vec<String> = report
        .mismatches
        .iter()
        .map(|m| format!("seq {}: {}", m.seq, m.what))
        .collect();
    if !report.ok() {
        failures.push(format!("replay: {}", replay_mismatches.join("; ")));
    }
    // rules 12: debts closed without a restore, and nothing left owed
    let mut closed = report.restores_closed.iter();
    for want in &sc.expect_closed {
        if !closed.any(|c| c.contains(want.as_str())) {
            failures.push(format!(
                "expected restores closed {:?} in order, replay lists {:?}",
                sc.expect_closed, report.restores_closed
            ));
            break;
        }
    }
    if sc.expect_nothing_owed && !report.unrestored.is_empty() {
        failures.push(format!("expected nothing owed back at the end, replay reports {:?}", report.unrestored));
    }
    let mut tamper_not_applicable = None;
    let (tamper_detected, tamper_mismatches) = match &sc.tamper {
        None => (None, Vec::new()),
        Some(kind) => match tamper(&text, kind) {
            Err(e) => {
                failures.push(format!("tamper {kind}: {e}"));
                (Some(false), Vec::new())
            }
            Ok(Tampered::NotApplicable(why)) => {
                tamper_not_applicable = Some(why);
                (None, Vec::new())
            }
            Ok(Tampered::Forged(forged)) => {
                let r = replay::replay_str_with(&forged, &replay_opts).map_err(|e| setup(e.to_string()))?;
                let ms: Vec<String> = r
                    .mismatches
                    .iter()
                    .map(|m| format!("seq {}: {}", m.seq, m.what))
                    .collect();
                if r.ok() {
                    failures.push(format!("tamper {kind}: replay did not catch it"));
                }
                (Some(!r.ok()), ms)
            }
        },
    };
    Ok(ScenarioResult {
        file: file.to_string(),
        title: sc.title.clone(),
        invariant: sc.invariant.clone(),
        attack: sc.attack.clone(),
        ok: failures.is_empty(),
        failures,
        expect: sc.expect.clone(),
        final_state,
        final_reason,
        failed_rules,
        assessment,
        refusals,
        fallback: fallback_seen,
        lost_link_action: extra.lost_link_action.clone(),
        preload_reports: extra.preload_reports.clone(),
        replay_ok: report.ok(),
        replay_events: report.events,
        replay_mismatches,
        tamper: sc.tamper.clone(),
        tamper_detected,
        tamper_not_applicable,
        tamper_mismatches,
        resets,
        reacquired: extra.reacquired.clone(),
        ident_reports,
        ident,
        proxy: extra.proxy.is_some().then_some(extra.proxy_stats),
        log: text,
    })
}

/// Rules 9: the identification events of a log as report lines (`t=<s> declared T-0001 by
/// rule:hostile_act ...`), and as records with the truth behind each: whether the track's
/// contact is really hostile, and whether the act was really its contact's (`acts`: each
/// act's true author and second, as the timeline scripted it).
fn ident_records(
    text: &str,
    tracks: &BTreeMap<String, TrackSim>,
    acts: &BTreeMap<String, (String, u32)>,
) -> (Vec<String>, Vec<IdentRecord>) {
    use crate::ident::{ActVerdict, Declarer};
    let Ok(events) = parse_log(text) else {
        return (Vec::new(), Vec::new());
    };
    let secs = |t: Time| t.ms_since(T0) as f64 / 1000.0;
    let hostile = |track: &str| tracks.get(track).map(|t| t.truth_aff == Affiliation::Hostile);
    let mut reports = Vec::new();
    let mut out = Vec::new();
    let base = |kind: &str, t: Time| IdentRecord {
        kind: kind.into(),
        t_s: secs(t),
        track: None,
        act: None,
        by: None,
        detail: None,
        attribution: None,
        truth_hostile: None,
        truth_actor: None,
        actor: None,
        act_t_s: None,
    };
    // the flag each act raised, for its tracks' labels
    for e in &events {
        let s = secs(e.t);
        match &e.body {
            EventBody::IdentAct {
                act, candidates, verdict, ..
            } => {
                let (actor, act_s) = acts.get(&act.act).cloned().unwrap_or_default();
                let best = candidates.first().map_or_else(
                    || "no track".to_string(),
                    |c| format!("{} at {:.2}", c.track, c.confidence),
                );
                let v = match verdict {
                    ActVerdict::Declare { track, .. } => format!("declare {track}"),
                    ActVerdict::Flag { kind, .. } => format!("flag {}", kind.as_str()),
                    ActVerdict::Recorded { why } => format!("recorded ({why})"),
                };
                reports.push(format!("t={s:.0} act {} against {} by {actor}: best {best}; {v}", act.act, act.against.describe()));
                out.push(IdentRecord {
                    track: candidates.first().map(|c| c.track.clone()),
                    act: Some(act.act.clone()),
                    detail: Some(v),
                    attribution: candidates.first().map(|c| c.confidence),
                    truth_hostile: (actor != "untracked").then(|| hostile(&actor)).flatten(),
                    truth_actor: candidates.first().map(|c| c.track == actor),
                    actor: Some(actor),
                    act_t_s: Some(f64::from(act_s)),
                    ..base("act", e.t)
                });
            }
            EventBody::IdentDeclared { declaration: d } => {
                let (kind, act, attribution) = match &d.by {
                    Declarer::Rule { evidence, .. } => ("rule_declaration", Some(evidence.act.clone()), Some(evidence.attribution)),
                    // the evaluation's proxy names the act it read
                    Declarer::Operator { note, .. } => (
                        "operator_declaration",
                        note.as_deref().and_then(|n| n.strip_prefix(PROXY_NOTE)).map(str::to_string),
                        None,
                    ),
                };
                let (actor, act_s) = act
                    .as_ref()
                    .and_then(|a| acts.get(a).cloned())
                    .map_or((None, None), |(a, s)| (Some(a), Some(f64::from(s))));
                reports.push(format!(
                    "t={s:.0} declared {} {} by {}{}",
                    d.track,
                    d.affiliation.as_str(),
                    d.label(),
                    attribution.map(|a| format!(" (attribution {a:.2})")).unwrap_or_default()
                ));
                out.push(IdentRecord {
                    track: Some(d.track.clone()),
                    act,
                    by: Some(d.label()),
                    detail: Some(d.affiliation.as_str().into()),
                    attribution,
                    truth_hostile: hostile(&d.track),
                    truth_actor: actor.as_ref().map(|a| *a == d.track),
                    actor,
                    act_t_s: act_s,
                    ..base(kind, e.t)
                });
            }
            EventBody::IdentConfirmed { track, by, .. } => {
                reports.push(format!("t={s:.0} confirmed {track} by {by}"));
                out.push(IdentRecord {
                    track: Some(track.clone()),
                    by: Some(by.to_string()),
                    truth_hostile: hostile(track),
                    ..base("confirmed", e.t)
                });
            }
            EventBody::IdentRevoked {
                track, by, engagements, ..
            } => {
                let stopped: Vec<String> = engagements.iter().map(ToString::to_string).collect();
                reports.push(format!("t={s:.0} revoked {track} by {by} (stopping [{}])", stopped.join(", ")));
                out.push(IdentRecord {
                    track: Some(track.clone()),
                    by: Some(by.to_string()),
                    detail: Some(format!("stopped {}", stopped.len())),
                    truth_hostile: hostile(track),
                    ..base("revoked", e.t)
                });
            }
            EventBody::IdentFlagged { flag, .. } => {
                let (actor, act_s) = flag
                    .act
                    .as_ref()
                    .and_then(|a| acts.get(a).cloned())
                    .map_or((None, None), |(a, s)| (Some(a), Some(f64::from(s))));
                let related = flag.related.as_ref().map(|r| format!(" with {r}")).unwrap_or_default();
                reports.push(format!(
                    "t={s:.0} flagged F-{} {} [{}]{related}",
                    flag.id,
                    flag.kind.as_str(),
                    flag.tracks.join(", ")
                ));
                // one record per track it names (none: one without)
                let named: Vec<Option<String>> = if flag.tracks.is_empty() {
                    vec![None]
                } else {
                    flag.tracks.iter().cloned().map(Some).collect()
                };
                for t in named {
                    out.push(IdentRecord {
                        truth_hostile: t.as_deref().and_then(hostile),
                        truth_actor: actor.as_ref().zip(t.as_ref()).map(|(a, t)| a == t),
                        track: t,
                        act: flag.act.clone(),
                        detail: Some(format!("F-{} {}", flag.id, flag.kind.as_str())),
                        actor: actor.clone(),
                        act_t_s: act_s,
                        ..base("flag", e.t)
                    });
                }
            }
            EventBody::IdentFlagResolved {
                flag, resolution, by, ..
            } => {
                let r = match resolution {
                    crate::ident::FlagResolution::Confirmed => "confirmed",
                    crate::ident::FlagResolution::Dismissed => "dismissed",
                };
                reports.push(format!("t={s:.0} resolved F-{flag} {r} by {by}"));
                out.push(IdentRecord {
                    by: Some(by.to_string()),
                    detail: Some(format!("F-{flag} {r}")),
                    ..base("flag_resolved", e.t)
                });
            }
            EventBody::EngageStopped {
                intent,
                cause: engage::StopCause::Revoked { track, .. },
                ..
            } => {
                reports.push(format!("t={s:.0} stopped {intent} for the revoked declaration of {track}"));
            }
            _ => {}
        }
    }
    (reports, out)
}

/// A source that ranges a contact in height (or puts it at sea level): radar, AIS.
fn ranging(source: &str) -> bool {
    matches!(source.rsplit(':').next(), Some("radar" | "ais"))
}

fn track_sim(t: &TrackDef) -> TrackSim {
    let seen = T0.plus_ms(-((t.age_s * 1000.0).round() as i64));
    let alt_seen = seen.plus_ms(-((t.alt_age_s * 1000.0).round() as i64));
    let speed = t.velocity[0].hypot(t.velocity[1]);
    TrackSim {
        truth: LonLat::new(t.at[0], t.at[1]),
        truth_vel: t.velocity,
        vel_error: [0.0, 0.0],
        vel_noise: [0.0, 0.0],
        facts: TrackFacts {
            position: Fact::new(LonLat::new(t.at[0], t.at[1]), seen),
            alt_m: t.alt_m,
            alt_t: t.alt_m.map(|_| alt_seen),
            velocity_mps: (!t.no_velocity_estimate).then_some(t.velocity),
            pos_sigma_m: t.sigma_m,
            sigma_growth_mps: Some((0.3 * speed).max(1.0)),
            confidence: t.confidence,
            affiliation: Fact::new(t.affiliation, seen),
            affiliation_confidence: t.affiliation_confidence,
            identity: t.identity.clone(),
            identity_confidence: t.identity_confidence,
            sources: t.sources.clone(),
            source_t: t
                .sources
                .iter()
                .map(|s| (s.clone(), if ranging(s) { alt_seen } else { seen }))
                .collect(),
            domain: t.domain,
            last_seen_t: seen,
            declared_by: t.declared_by.clone(),
            declaration: None,
            review_flags: Vec::new(),
            manoeuvre: t.manoeuvre_speed_mps.map(|v| crate::world::Manoeuvre {
                t: seen,
                change_mps: v,
                speed_mps: v,
            }),
            // rules 12: tracked long enough, a steady track's average velocity is its velocity
            mean_velocity_mps: Some(t.tracked_s.unwrap_or(SCENARIO_TRACKED_S))
                .filter(|s| *s >= SCENARIO_MEAN_MIN_S && !t.no_velocity_estimate)
                .map(|_| t.velocity),
            first_seen_t: Some(T0.plus_ms(-((t.tracked_s.unwrap_or(SCENARIO_TRACKED_S) * 1000.0).round() as i64))),
            ais: t.ais.clone(),
        },
        silent: t.silent,
        no_velocity: t.no_velocity_estimate,
        alt_frozen: t.alt_age_s > 0.0,
        truth_aff: t.truth.unwrap_or(t.affiliation),
    }
}

fn state_str(s: State) -> String {
    serde_json::to_value(s)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The world the engine reads at `now`.
fn snapshot(
    vehicles: &BTreeMap<String, VehicleSim>,
    tracks: &BTreeMap<String, TrackSim>,
    catalog: &Catalog,
    areas: &AreaSet,
    now: Time,
) -> WorldSnapshot {
    let mut w = WorldSnapshot::new(now);
    w.areas = Some(areas.clone());
    for (uid, v) in vehicles {
        let p = catalog.get(&v.def.platform).cloned();
        let ap = p.as_ref().and_then(|p| p.autopilot);
        let t = v.last_t;
        w.vehicles.insert(
            Uid::new(uid),
            VehicleFacts {
                autopilot: ap.map(|a| Fact::new(a, t)),
                nav_ready: Some(Fact::new(true, t)),
                position: Some(Fact::new(v.pos, t)),
                home: Some(Fact::new(v.home, T0)),
                battery_pct: Some(Fact::new(v.def.battery, t)),
                nav_source: Some(Fact::new(v.nav, t)),
                gps_fix: Some(Fact::new(v.gps_fix, t)),
                gps_glitch: Some(Fact::new(v.gps_glitch, t)),
                gps_fix_from: None,
                circling: v.circling.map(|(center, radius_m)| {
                    let source = Some(crate::world::CircleSource::OrbitTask);
                    let circle = crate::world::Circle {
                        center,
                        radius_m,
                        source,
                        center_t: None,
                    };
                    Fact::new(circle, t)
                }),
                report_interval_ms: Some(v.interval),
                velocity_mps: Some(Fact::new(v.vel, t)),
                alt_msl_m: v.alt.map(|a| Fact::new(a, t)),
                platform: p,
                booted_t: None,
            },
        );
    }
    for (id, tr) in tracks {
        let mut f = tr.facts.clone();
        // uncertainty and existence as the tracker carries them to now
        let age = now.ms_since(f.last_seen_t) as f64 / 1000.0;
        f.pos_sigma_m += engage::sigma_growth(&f) * age.max(0.0);
        f.confidence *= (-age.max(0.0) / 60.0).exp();
        w.tracks.insert(id.clone(), f);
    }
    w
}

#[allow(clippy::too_many_arguments)]
fn act(
    a: &Action,
    engine: &mut Engine,
    sessions: &BTreeMap<String, OperatorSession>,
    tracks: &mut BTreeMap<String, TrackSim>,
    vehicles: &mut BTreeMap<String, VehicleSim>,
    catalog: &Catalog,
    areas: &AreaSet,
    now: Time,
    terrain: &PolygonTerrain,
    last: &mut Option<IntentId>,
    refusals: &mut Vec<String>,
    fallback_seen: &mut Option<String>,
    failures: &mut Vec<String>,
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    extra: &mut Extra,
    s: u32,
) {
    let world = |tracks: &BTreeMap<String, TrackSim>, vehicles: &BTreeMap<String, VehicleSim>| {
        snapshot(vehicles, tracks, catalog, areas, now)
    };
    let session = |by: &str| sessions.get(by);
    match a {
        Action::Declare {
            track,
            affiliation,
            by,
        } => {
            let Some(sess) = session(by) else {
                return failures.push(format!("t={s}: unknown operator {by}"));
            };
            if !tracks.contains_key(track) {
                return failures.push(format!("t={s}: unknown track {track}"));
            }
            // rules 9: through the engine's identification ledger, which fills every picture
            if engine.policy().rules.rule_id() {
                let w = world(tracks, vehicles);
                match engine.declare_track(sess.authority(), None, track, *affiliation, None, None, &w) {
                    Ok(out) => follow_stops(vehicles, engaging, fallback_seen, &out.fallbacks),
                    Err(e) => refusals.push(e.to_string()),
                }
                return;
            }
            let Some(t) = tracks.get_mut(track) else { return };
            t.facts.affiliation = Fact::new(*affiliation, now);
            t.facts.declared_by = Some(by.clone());
            let _ = engine.record_operator_action(
                sess.authority(),
                "track.declare",
                &format!("track {track} declared {}", affiliation.as_str()),
            );
        }
        Action::Act {
            by_track,
            at,
            against_vehicle,
            against_track,
            sensors,
            sigma_m,
            offset: shift,
        } => {
            let n = extra.acts.len() + 1;
            let id = format!("A-{n}");
            let (author, actor) = match (by_track, at) {
                (Some(t), _) => match tracks.get(t) {
                    Some(sim) => (sim.truth, t.clone()),
                    None => return failures.push(format!("t={s}: act {id}: unknown track {t}")),
                },
                (None, Some(p)) => (LonLat::new(p[0], p[1]), "untracked".to_string()),
                (None, None) => return failures.push(format!("t={s}: act {id}: by_track or at")),
            };
            extra.acts.insert(id.clone(), (actor, s));
            if !engine.policy().rules.rule_id() {
                return refusals.push(format!("act {id}: these decision rules observe no hostile acts (rules 9)"));
            }
            let mut location = author;
            if let Some([e, n]) = shift {
                location = offset(location, *e, *n);
            }
            let base = location;
            if let Some(rng) = extra.act_noise.as_mut() {
                location = offset(base, rng.gauss() * sigma_m, rng.gauss() * sigma_m);
            }
            // each further observer (the sensors' uid prefixes, in order) its own fix, its own
            // noise
            let mut observers: Vec<&str> = Vec::new();
            for sensor in sensors {
                let o = sensor.split(':').next().unwrap_or(sensor);
                if !observers.contains(&o) {
                    observers.push(o);
                }
            }
            let fixes: Vec<crate::ident::Fix> = observers
                .iter()
                .skip(1)
                .map(|o| {
                    let loc = match extra.act_noise.as_mut() {
                        Some(rng) => offset(base, rng.gauss() * sigma_m, rng.gauss() * sigma_m),
                        None => base,
                    };
                    crate::ident::Fix {
                        observer: (*o).to_string(),
                        sensors: sensors.iter().filter(|x| x.split(':').next() == Some(*o)).cloned().collect(),
                        location: loc,
                        sigma_m: *sigma_m,
                    }
                })
                .collect();
            let against = match (against_vehicle, against_track) {
                (Some(u), _) => crate::ident::Against::Vehicle { uid: Uid::new(u) },
                (None, Some(t)) => crate::ident::Against::Track { track: t.clone() },
                (None, None) => crate::ident::Against::Unknown,
            };
            let report = crate::ident::ActReport {
                act: id.clone(),
                against,
                observed_t: now,
                sensors: sensors.clone(),
                location,
                sigma_m: *sigma_m,
                fixes,
                channel: Some("scenario".into()),
            };
            let w = world(tracks, vehicles);
            // what the console showed when the act came in (before anything it made)
            let shown = extra.proxy.is_some().then(|| engine.identified(&w).tracks.clone());
            match engine.record_act(report.clone(), &w) {
                Ok(out) => {
                    // the evaluation's review proxy: what the inbox now holds for it
                    if let (Some(p), Some(shown)) = (extra.proxy.clone(), shown) {
                        extra.act_pictures.insert(id.clone(), (report.clone(), shown));
                        let kind = match (&out.verdict, &out.declaration, &out.flag) {
                            (_, Some(d), _) => Some(ProxyKind::Declaration(d.track.clone(), d.id)),
                            (_, _, Some(f)) => Some(ProxyKind::Flag(f.id)),
                            (crate::ident::ActVerdict::Recorded { .. }, _, _)
                                if !engine.policy().identification().rule_declaration =>
                            {
                                Some(ProxyKind::Act(id.clone()))
                            }
                            _ => None,
                        };
                        if let Some(kind) = kind {
                            let due = extra.first_look(&p, s);
                            extra.proxy_stats.items += 1;
                            extra.proxy_items.push(ProxyItem { due_s: due, looks: 1, kind });
                        }
                    }
                }
                Err(e) => failures.push(format!("t={s}: act {id}: {e}")),
            }
        }
        Action::ConfirmDeclaration { by, track } => {
            let Some(sess) = session(by) else {
                return failures.push(format!("t={s}: unknown operator {by}"));
            };
            if let Err(e) = engine.confirm_declaration(sess.authority(), None, track) {
                refusals.push(e.to_string());
            }
        }
        Action::RevokeDeclaration { by, track, note } => {
            let Some(sess) = session(by) else {
                return failures.push(format!("t={s}: unknown operator {by}"));
            };
            match engine.revoke_declaration(sess.authority(), track, note.clone()) {
                Ok(out) => follow_stops(vehicles, engaging, fallback_seen, &out.fallbacks),
                Err(e) => refusals.push(e.to_string()),
            }
        }
        Action::ResolveFlag {
            by,
            track,
            flag,
            resolution,
        } => {
            let Some(sess) = session(by) else {
                return failures.push(format!("t={s}: unknown operator {by}"));
            };
            let flag = flag.or_else(|| engine.identification().flags_on(track).into_iter().max());
            let Some(flag) = flag else {
                return refusals.push(format!("no review flag in force on {track}"));
            };
            if let Err(e) = engine.resolve_flag(sess.authority(), flag, *resolution, None) {
                refusals.push(e.to_string());
            }
        }
        Action::WriteOff { by, uid, note } => {
            let Some(sess) = session(by) else {
                return failures.push(format!("t={s}: unknown operator {by}"));
            };
            if let Err(e) = engine.write_off_restore(sess.authority(), crate::engine::WriteOff::Platform(Uid::new(uid)), note) {
                refusals.push(e.to_string());
            }
        }
        Action::AgentDeclare { agent, track } => {
            // AgentPort has propose, preview, withdraw, record and read: nothing declares,
            // confirms or revokes. The attempt is recorded; the ledger must not move.
            let before = engine.identification().declaration(track).cloned();
            let _ = engine.agent_port(AgentId::new(agent)).record_action(
                "track.declare.attempt",
                &format!("track {track}: no agent path to declaring a track exists"),
            );
            refusals.push(format!(
                "agent {agent}: no declare method on AgentPort (declaring {track} hostile cannot be expressed)"
            ));
            if engine.identification().declaration(track).cloned() != before {
                failures.push(format!("t={s}: the identification moved on an agent's attempt to declare"));
            }
        }
        Action::Nominate {
            by,
            agent,
            rationale,
            platform,
            track,
            radius_m,
            fallback,
            window_s,
            accept,
            accept_lost,
        } => {
            let task = Task::Engage {
                track: track.clone(),
                effect_radius_m: *radius_m,
                window: window_s.map(|w| EngageWindow {
                    from: None,
                    until: now.plus_ms(i64::from(w) * 1000),
                }),
                fallback: *fallback,
            };
            let mut proposal = Proposal::new(task, vec![Uid::new(platform)]);
            let w = world(tracks, vehicles);
            // rules 6: each accepted lost vehicle since its last report, as the check shows it
            let accepted: Vec<(Uid, Option<Time>)> = accept
                .iter()
                .map(|u| {
                    let since = w.vehicles.get(&Uid::new(u)).and_then(|v| v.position.as_ref()).map(|p| p.t);
                    (Uid::new(u), since)
                })
                .collect();
            // rules 13: each accepted lost contact since its last fix, as the check shows it
            let lost_now = engine.lost_contacts();
            let contacts: Vec<(String, Time)> = accept_lost
                .iter()
                .map(|t| (t.clone(), lost_now.get(t).map_or(now, crate::lost::LostContact::since)))
                .collect();
            let out = match (by, agent) {
                (Some(by), _) => match session(by) {
                    Some(sess) => {
                        proposal.rationale = rationale.as_deref().map(Rationale::new).transpose().ok().flatten();
                        match (accepted.is_empty() && contacts.is_empty(), sess.engagement_authority()) {
                            (true, _) => engine.submit(sess.authority(), proposal, &w, terrain).map(|i| i.id()),
                            (false, Some(auth)) if contacts.is_empty() => engine
                                .submit_accepting(auth, proposal, &accepted, &w, terrain)
                                .map(|i| i.id()),
                            (false, Some(auth)) => engine
                                .submit_accepting_lost(auth, None, proposal, &accepted, &contacts, &w, terrain)
                                .map(|i| i.id()),
                            // the bridge refuses the whole nomination (403): so does the runner
                            (false, None) => {
                                return refusals.push(format!(
                                    "{by}: accepting a lost {} needs engagement authority (nomination refused)",
                                    if contacts.is_empty() { "vehicle" } else { "contact" }
                                ));
                            }
                        }
                    }
                    None => return failures.push(format!("t={s}: unknown operator {by}")),
                },
                (None, Some(agent)) => {
                    // an agent's port takes no acceptance: there is nothing to pass it to
                    if !accept.is_empty() || !accept_lost.is_empty() {
                        refusals.push(format!(
                            "agent {agent}: no acceptance of a lost vehicle on an agent's proposal (its port has no such parameter)"
                        ));
                    }
                    let Ok(r) = Rationale::new(rationale.clone().unwrap_or_default()) else {
                        refusals.push("an agent's proposal needs a rationale".into());
                        return;
                    };
                    engine
                        .agent_port(AgentId::new(agent))
                        .propose(proposal, r, &w, terrain)
                        .map(|i| i.id())
                }
                (None, None) => return failures.push(format!("t={s}: nominate needs by or agent")),
            };
            match out {
                Ok(id) => *last = Some(id),
                Err(e) => refusals.push(e.to_string()),
            }
        }
        Action::Look { by } => {
            let (Some(id), Some(sess)) = (*last, session(by)) else {
                return failures.push(format!("t={s}: look: no nomination or unknown operator {by}"));
            };
            let Some(auth) = sess.engagement_authority() else {
                refusals.push(format!("{by} has no engagement authority: no picture to sign"));
                return;
            };
            let w = world(tracks, vehicles);
            match engine.issue_picture(id, auth, &w) {
                Ok(t) => {
                    extra.looked.insert(by.clone(), t.id);
                }
                Err(e) => refusals.push(e.to_string()),
            }
        }
        Action::Approve { by } => {
            let (Some(id), Some(sess)) = (*last, session(by)) else {
                return failures.push(format!("t={s}: approve: no nomination or unknown operator {by}"));
            };
            let w = world(tracks, vehicles);
            let r = match sess.engagement_authority() {
                Some(auth) => {
                    // the picture from their last look, or one now
                    let token = match extra.looked.remove(by) {
                        Some(t) => Ok(t),
                        None => engine.issue_picture(id, auth, &w).map(|t| t.id),
                    };
                    token.and_then(|t| engine.approve_engagement(id, auth, None, t, &w, terrain).map(|_| ()))
                }
                None => engine.approve(id, sess.authority(), None, &w, terrain).map(|_| ()),
            };
            if let Err(e) = r {
                refusals.push(e.to_string());
            }
        }
        Action::ApproveTask { by, acknowledge } => {
            let (Some(id), Some(sess)) = (extra.last_task, session(by)) else {
                return failures.push(format!("t={s}: approve_task: no task or unknown operator {by}"));
            };
            let w = world(tracks, vehicles);
            let r = if *acknowledge {
                engine.approve_acknowledging(id, sess.authority(), None, &w, terrain)
            } else {
                engine.approve(id, sess.authority(), None, &w, terrain)
            };
            if let Err(e) = r {
                refusals.push(e.to_string());
            }
        }
        Action::Task {
            by,
            agent,
            rationale,
            task,
            uids,
            center,
            radius_m,
        } => {
            let t = match (task.as_str(), center) {
                ("hold", _) => Task::Hold {},
                ("rtl", _) => Task::Rtl {},
                ("orbit", Some(c)) => Task::Orbit {
                    center: LonLat::new(c[0], c[1]),
                    radius_m: radius_m.unwrap_or(200.0),
                },
                (other, _) => {
                    return failures.push(format!("t={s}: task {other}: hold, rtl, or orbit with a center"));
                }
            };
            let proposal = Proposal::new(t, uids.iter().map(Uid::new).collect());
            let w = world(tracks, vehicles);
            let out = match (by, agent) {
                (Some(by), _) => match session(by) {
                    Some(sess) => engine.submit(sess.authority(), proposal, &w, terrain).map(|i| i.id()),
                    None => return failures.push(format!("t={s}: unknown operator {by}")),
                },
                (None, Some(agent)) => {
                    let Ok(r) = Rationale::new(rationale.clone().unwrap_or_default()) else {
                        refusals.push("an agent's proposal needs a rationale".into());
                        return;
                    };
                    engine
                        .agent_port(AgentId::new(agent))
                        .propose(proposal, r, &w, terrain)
                        .map(|i| i.id())
                }
                (None, None) => return failures.push(format!("t={s}: task needs by or agent")),
            };
            match out {
                Ok(id) => extra.last_task = Some(id),
                Err(e) => refusals.push(e.to_string()),
            }
        }
        Action::Stall { seconds } => extra.stall_until = s + seconds,
        Action::Reject { by } => {
            if let (Some(id), Some(sess)) = (*last, session(by))
                && let Err(e) = engine.reject(id, sess.authority(), None)
            {
                refusals.push(e.to_string());
            }
        }
        Action::Abort { by } => {
            let (Some(id), Some(sess)) = (*last, session(by)) else {
                return failures.push(format!("t={s}: abort: no nomination or unknown operator {by}"));
            };
            match engine.abort_engagement(id, sess.authority(), "operator abort") {
                Ok(Some(f)) => {
                    *fallback_seen = Some(f.fallback.as_str().into());
                    if let Some(v) = vehicles.get_mut(f.uid.as_str()) {
                        match f.fallback {
                            Fallback::Abort => {
                                v.goal = None;
                                v.vel = [0.0, 0.0];
                            }
                            Fallback::Return => {
                                v.goal = Some(v.home);
                                v.orbit = false;
                            }
                            Fallback::Loiter => {
                                v.goal = Some(v.pos);
                                v.orbit = true;
                            }
                        }
                    }
                    engaging.remove(&id);
                }
                Ok(None) => {}
                Err(e) => refusals.push(e.to_string()),
            }
        }
        Action::Accept { by, uid, track, since_s } => {
            let (Some(id), Some(sess)) = (*last, session(by)) else {
                return failures.push(format!("t={s}: accept: no nomination or unknown operator {by}"));
            };
            let w = world(tracks, vehicles);
            // only engagement authority accepts (the bridge answers 403 otherwise)
            let Some(auth) = sess.engagement_authority() else {
                return refusals.push(format!(
                    "{by}: accepting a lost {} needs engagement authority",
                    if track.is_some() { "contact" } else { "vehicle" }
                ));
            };
            let scripted = since_s.map(|x| stepped((x * 1000.0).round() as i64, extra.clock_step));
            let r = match (uid, track) {
                (Some(uid), None) => {
                    let since = scripted.or_else(|| w.vehicles.get(&Uid::new(uid)).and_then(|v| v.position.as_ref()).map(|p| p.t));
                    engine.accept_unknown_friendly(id, auth, Uid::new(uid), since, &w).map(|_| ())
                }
                // rules 13: a lost contact, since its last fix as the check shows it
                (None, Some(t)) => {
                    let since = scripted
                        .or_else(|| engine.lost_contacts().get(t).map(crate::lost::LostContact::since))
                        .unwrap_or(now);
                    engine.accept_lost_contact(id, auth, None, t, since, &w).map(|_| ())
                }
                _ => return failures.push(format!("t={s}: accept: uid or track, one of them")),
            };
            if let Err(e) = r {
                refusals.push(e.to_string());
            }
        }
        Action::Revoke { by, uid, track } => {
            let (Some(id), Some(sess)) = (*last, session(by)) else {
                return failures.push(format!("t={s}: revoke: no nomination or unknown operator {by}"));
            };
            let r = match (uid, track) {
                (Some(uid), None) => engine.revoke_unknown_friendly(id, sess.authority(), Uid::new(uid)).map(|_| ()),
                (None, Some(t)) => engine.revoke_lost_contact(id, sess.authority(), t).map(|_| ()),
                _ => return failures.push(format!("t={s}: revoke: uid or track, one of them")),
            };
            if let Err(e) = r {
                refusals.push(e.to_string());
            }
        }
        Action::DropTrack { track } => {
            // the tracker's drop: its facts now, as the bridge hands them (the estimate only)
            let w = world(tracks, vehicles);
            let Some(facts) = w.tracks.get(track).cloned() else {
                return failures.push(format!("t={s}: drop_track: unknown track {track}"));
            };
            tracks.remove(track);
            if let Err(e) = engine.record_track_drops(vec![(track.clone(), facts)]) {
                failures.push(format!("t={s}: drop_track {track}: {e}"));
            }
        }
        Action::AgentAccept { agent, uid } => {
            // AgentPort has propose, preview, withdraw, record and read: nothing accepts a lost
            // vehicle. The attempt is recorded; the intent and its acceptances must not move.
            let before = last.and_then(|id| engine.intent(id)).map(|i| {
                (i.state(), i.engagement().map(|e| e.accepted.clone()).unwrap_or_default())
            });
            let _ = engine.agent_port(AgentId::new(agent)).record_action(
                "engage.accept.attempt",
                &format!("vehicle {uid}: no agent path to accepting a lost vehicle exists"),
            );
            refusals.push(format!(
                "agent {agent}: no accept method on AgentPort (accepting vehicle {uid} as lost cannot be expressed)"
            ));
            let after = last.and_then(|id| engine.intent(id)).map(|i| {
                (i.state(), i.engagement().map(|e| e.accepted.clone()).unwrap_or_default())
            });
            if before != after {
                failures.push(format!("t={s}: the intent moved on an agent's attempt to accept"));
            }
        }
        Action::AgentApprove { agent } => {
            // The agent's whole surface is AgentPort: propose, preview, withdraw, record,
            // read. None of them approves; the attempt is recorded and the intent must not
            // move.
            let before = last.and_then(|id| engine.intent(id)).map(|i| i.state());
            let _ = engine
                .agent_port(AgentId::new(agent))
                .record_action("intent.approve.attempt", "no agent path to approval exists");
            refusals.push(format!(
                "agent {agent}: no approve method on AgentPort (the attempt cannot be expressed)"
            ));
            let after = last.and_then(|id| engine.intent(id)).map(|i| i.state());
            if before != after {
                failures.push(format!("t={s}: the intent moved on an agent's attempt"));
            }
        }
        Action::Track {
            id,
            at,
            velocity,
            affiliation,
            declared_by,
            undeclare,
            identity,
            identity_confidence,
            sigma_m,
            domain,
            silent,
            velocity_error,
            alt_m,
            tracked_s,
            ais,
        } => {
            let new = !tracks.contains_key(id);
            let t = tracks.entry(id.clone()).or_insert_with(|| {
                track_sim(&TrackDef {
                    id: id.clone(),
                    at: at.unwrap_or([0.0, 0.0]),
                    velocity: velocity.unwrap_or([0.0, 0.0]),
                    sigma_m: sigma_m.unwrap_or(15.0),
                    affiliation: affiliation.unwrap_or(Affiliation::Unknown),
                    affiliation_confidence: 0.8,
                    declared_by: None,
                    identity: identity.clone(),
                    identity_confidence: identity_confidence.unwrap_or(0.9),
                    confidence: 0.9,
                    domain: domain.unwrap_or(Domain::Surface),
                    age_s: 0.0,
                    silent: false,
                    sources: vec!["sensor".into()],
                    alt_m: *alt_m,
                    no_velocity_estimate: false,
                    alt_age_s: 0.0,
                    truth: None,
                    manoeuvre_speed_mps: None,
                    tracked_s: None,
                    ais: None,
                })
            });
            if let Some(a) = ais {
                t.facts.ais = Some(a.clone());
            }
            // rules 12: a track a step makes appears now (or `tracked_s` ago): young, its
            // average velocity comes once tracked long enough
            if new && engine.policy().rules.followups() {
                let s = tracked_s.unwrap_or(0.0);
                t.facts.first_seen_t = Some(now.plus_ms(-((s * 1000.0).round() as i64)));
                t.facts.mean_velocity_mps = (s >= SCENARIO_MEAN_MIN_S && t.facts.velocity_mps.is_some())
                    .then_some(t.truth_vel);
            } else if new {
                t.facts.first_seen_t = None;
                t.facts.mean_velocity_mps = None;
            }
            if let Some(a) = alt_m {
                t.facts.alt_m = Some(*a);
                t.facts.alt_t = Some(now);
                t.alt_frozen = false;
            }
            if let Some(p) = at {
                t.truth = LonLat::new(p[0], p[1]);
                t.facts.position = Fact::new(t.truth, now);
                t.facts.last_seen_t = now;
            }
            if let Some(v) = velocity {
                t.truth_vel = *v;
                t.facts.velocity_mps = Some(*v);
                t.facts.sigma_growth_mps = Some((0.3 * v[0].hypot(v[1])).max(1.0));
            }
            if let Some(err) = velocity_error {
                t.vel_error = *err;
                t.facts.velocity_mps = Some([
                    t.truth_vel[0] + err[0] + t.vel_noise[0],
                    t.truth_vel[1] + err[1] + t.vel_noise[1],
                ]);
            }
            if let Some(a) = affiliation {
                t.facts.affiliation = Fact::new(*a, now);
            }
            // rules 9: declarations are the engine's ledger (below), not the picture's own
            let rule_id = engine.policy().rules.rule_id();
            if let Some(b) = declared_by
                && !rule_id
            {
                t.facts.declared_by = Some(b.clone());
            }
            if *undeclare {
                t.facts.declared_by = None;
                t.facts.affiliation = Fact::new(Affiliation::Suspect, now);
            }
            if let Some(k) = identity {
                t.facts.identity = Some(k.clone());
            }
            if let Some(c) = identity_confidence {
                t.facts.identity_confidence = *c;
            }
            if let Some(sg) = sigma_m {
                t.facts.pos_sigma_m = *sg;
            }
            if let Some(d) = domain {
                t.facts.domain = *d;
            }
            if let Some(si) = silent {
                t.silent = *si;
            }
            if rule_id {
                // a declaration given here is made by that operator; a withdrawn one is
                // revoked by the first operator of the file (scenario 06: the track falls
                // back to its estimate)
                let first = sessions.keys().next().cloned();
                let w = world(tracks, vehicles);
                if let Some(b) = declared_by {
                    let aff = affiliation.unwrap_or(Affiliation::Hostile);
                    match session(b) {
                        Some(sess) => match engine.declare_track(sess.authority(), None, id, aff, None, None, &w) {
                            Ok(out) => follow_stops(vehicles, engaging, fallback_seen, &out.fallbacks),
                            Err(e) => refusals.push(e.to_string()),
                        },
                        None => failures.push(format!("t={s}: unknown operator {b}")),
                    }
                }
                if *undeclare {
                    match first.as_deref().and_then(session) {
                        Some(sess) => match engine.revoke_declaration(sess.authority(), id, Some("the declaration is withdrawn".into())) {
                            Ok(out) => follow_stops(vehicles, engaging, fallback_seen, &out.fallbacks),
                            Err(e) => refusals.push(e.to_string()),
                        },
                        None => failures.push(format!("t={s}: undeclare: no operator")),
                    }
                }
            }
        }
        Action::Vehicle {
            uid,
            at,
            velocity,
            silent,
            report_interval_ms,
            nav,
            gps_fix,
            gps_glitch,
            circling,
            reported_circle,
            alt_m,
        } => {
            let Some(v) = vehicles.get_mut(uid) else {
                return failures.push(format!("t={s}: unknown vehicle {uid}"));
            };
            if let Some(a) = alt_m {
                v.alt = Some(*a);
            }
            if let Some(p) = at {
                v.pos = LonLat::new(p[0], p[1]);
            }
            if let Some(vel) = velocity {
                v.vel = *vel;
            }
            if let Some(si) = silent {
                if *si && !v.silent {
                    v.silent_since = Some(s);
                }
                if !*si {
                    v.silent_since = None;
                }
                v.silent = *si;
            }
            if let Some(i) = report_interval_ms {
                v.interval = *i;
            }
            if let Some(n) = nav {
                v.nav = *n;
            }
            if let Some(f) = gps_fix {
                v.gps_fix = *f;
            }
            if let Some(g) = gps_glitch {
                v.gps_glitch = *g;
            }
            if let Some([lon, lat, radius]) = circling {
                v.circling = (*radius > 0.0).then(|| (LonLat::new(*lon, *lat), *radius));
                v.circling_flown = true;
            }
            if let Some([lon, lat, radius]) = reported_circle {
                v.circling = (*radius > 0.0).then(|| (LonLat::new(*lon, *lat), *radius));
                v.circling_flown = false;
            }
        }
        Action::Expect {
            state,
            rule,
            concurrences,
            task_state,
            task_reason,
        } => {
            if let Some(want) = task_reason {
                let got = extra
                    .last_task
                    .and_then(|id| engine.intent(id))
                    .and_then(|i| i.history().last().map(|h| h.reason.clone()))
                    .unwrap_or_default();
                if !got.contains(want.as_str()) {
                    failures.push(format!("t={s}: expected the task's reason to say {want:?}, it says {got:?}"));
                }
            }
            if let Some(want) = task_state {
                let got = extra.last_task.and_then(|id| engine.intent(id)).map(|i| state_str(i.state()));
                if got.as_deref() != Some(want.as_str()) {
                    failures.push(format!(
                        "t={s}: expected the task {want}, got {}",
                        got.as_deref().unwrap_or("none")
                    ));
                }
            }
            let i = last.and_then(|id| engine.intent(id));
            let got = i.map(|i| state_str(i.state()));
            if let Some(want) = state
                && got.as_deref() != Some(want.as_str())
            {
                failures.push(format!(
                    "t={s}: expected {want}, got {} ({})",
                    got.as_deref().unwrap_or("none"),
                    i.and_then(|i| i.history().last().map(|h| h.reason.clone()))
                        .unwrap_or_default()
                ));
            }
            if let Some(n) = concurrences
                && i.map(|i| i.concurrences().len()) != Some(*n)
            {
                failures.push(format!(
                    "t={s}: expected {n} concurrences, got {:?}",
                    i.map(|i| i.concurrences().len())
                ));
            }
            if let Some(r) = rule {
                let failed = i.is_some_and(|i| {
                    i.checks()
                        .iter()
                        .chain(
                            i.engagement()
                                .and_then(|e| e.watch.as_ref())
                                .map(|w| w.results.as_slice())
                                .unwrap_or(&[]),
                        )
                        .any(|c| c.rule.as_str() == r && c.outcome != Outcome::Pass)
                });
                if !failed {
                    failures.push(format!("t={s}: expected check {r} to fail"));
                }
            }
        }
    }
}

/// Runner state the timeline changes besides the world.
#[derive(Default)]
struct Extra {
    /// No engagement step before this second.
    stall_until: u32,
    /// The last `task` step's intent.
    last_task: Option<IntentId>,
    /// The picture token each operator's last `look` got.
    looked: BTreeMap<String, u64>,
    /// Rules 6: each dispatched engagement's lost-link preload as its platform answers it.
    preloads: BTreeMap<IntentId, PreloadSim>,
    lost_link_action: Option<String>,
    preload_reports: Vec<String>,
    /// Rules 9: the acts scripted so far (their number, true author, scenario second), and
    /// the seeded noise on their observed positions (a batch with act noise).
    acts: BTreeMap<String, (String, u32)>,
    act_noise: Option<Rng>,
    /// Rules 9, the evaluation: the review proxy, its draws, the items it has to look at, and
    /// each act's report with the picture it came in on (what the console showed).
    proxy: Option<ReviewProxy>,
    proxy_rng: Rng,
    proxy_items: Vec<ProxyItem>,
    proxy_stats: ProxyStats,
    act_pictures: BTreeMap<String, (crate::ident::ActReport, BTreeMap<String, TrackFacts>)>,
    /// Rules 9, the evaluation: the nominator, when each declared track is next nominated,
    /// and the signatures due (second, intent, operator).
    nominator: bool,
    nominate_next: BTreeMap<String, (u32, u32)>,
    sign_due: Vec<(u32, IntentId, String)>,
    /// murmur-dup: the run's clock steps ([`RunOptions::clock_step`]).
    clock_step: Option<ClockStep>,
    /// Rules 13: the lost contacts retired, as `t=<s> <lost> is <track> again`.
    reacquired: Vec<String>,
}

impl Extra {
    /// When the proxy first gets to an item that came in at `s`: the rest of its look
    /// interval, then the reading.
    fn first_look(&mut self, p: &ReviewProxy, s: u32) -> u32 {
        let attend = p.attend_s * (0.4 * self.proxy_rng.gauss()).exp();
        let read = p.read_s * (0.3 * self.proxy_rng.gauss()).exp();
        s + (self.proxy_rng.next() * attend + read).round().max(1.0) as u32
    }

    /// Its next look at an item it skimmed.
    fn next_look(&mut self, p: &ReviewProxy, s: u32) -> u32 {
        let attend = p.attend_s * (0.4 * self.proxy_rng.gauss()).exp();
        let read = p.read_s * (0.3 * self.proxy_rng.gauss()).exp();
        s + (attend + read).round().max(1.0) as u32
    }
}

/// Rules 9, the evaluation, once a second: the review proxy's items now due, then the
/// nominator and the signatures it set going. Each goes through the engine as the console's
/// requests would.
#[allow(clippy::too_many_arguments)]
fn proxies(
    sc: &Scenario,
    engine: &mut Engine,
    sessions: &BTreeMap<String, OperatorSession>,
    tracks: &mut BTreeMap<String, TrackSim>,
    vehicles: &mut BTreeMap<String, VehicleSim>,
    catalog: &Catalog,
    areas: &AreaSet,
    now: Time,
    terrain: &PolygonTerrain,
    last: &mut Option<IntentId>,
    refusals: &mut Vec<String>,
    fallback_seen: &mut Option<String>,
    failures: &mut Vec<String>,
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    extra: &mut Extra,
    s: u32,
) {
    let Some(op) = sc.operators.first().map(|o| o.id.clone()) else {
        return;
    };
    if let Some(p) = extra.proxy.clone() {
        let (due, later): (Vec<ProxyItem>, Vec<ProxyItem>) = std::mem::take(&mut extra.proxy_items)
            .into_iter()
            .partition(|i| i.due_s <= s);
        extra.proxy_items = later;
        for mut item in due {
            extra.proxy_stats.looks += 1;
            let reads = extra.proxy_rng.next() < p.diligence;
            let slip = extra.proxy_rng.next() < p.misread;
            let u = extra.proxy_rng.next();
            let Some(sess) = sessions.get(&op) else { return };
            let w = snapshot(vehicles, tracks, catalog, areas, now);
            // what the console shows for the item's act, as it came in
            let act_of = |k: &ProxyKind, engine: &Engine| -> Option<String> {
                match k {
                    ProxyKind::Act(a) => Some(a.clone()),
                    ProxyKind::Declaration(track, id) => engine.identification().declaration(track).and_then(|d| match &d.by {
                        crate::ident::Declarer::Rule { evidence, .. } if d.id == *id => Some(evidence.act.clone()),
                        _ => None,
                    }),
                    ProxyKind::Flag(f) => engine.identification().flag(*f).and_then(|(fl, _)| fl.act.clone()),
                }
            };
            let act = act_of(&item.kind, engine);
            let shown = act.as_ref().and_then(|a| extra.act_pictures.get(a).cloned());
            let pick = shown.as_ref().map(|(r, t)| {
                let honest = console_pick(r, t);
                if slip {
                    let not = match &honest {
                        Pick::Author(a) => Some(a.as_str()),
                        _ => None,
                    };
                    match misread_pick(r, t, not, u) {
                        Some(other) => Pick::Author(other),
                        None => honest,
                    }
                } else {
                    honest
                }
            });
            match item.kind.clone() {
                ProxyKind::Act(a) => {
                    if !reads {
                        if item.looks < 5 {
                            item.looks += 1;
                            item.due_s = extra.next_look(&p, s);
                            extra.proxy_items.push(item);
                        }
                        continue;
                    }
                    extra.proxy_stats.reads += 1;
                    extra.proxy_stats.decisions += 1;
                    let declared = match pick {
                        Some(Pick::Author(track)) => match engine.declare_track(
                            sess.authority(),
                            None,
                            &track,
                            Affiliation::Hostile,
                            Some(format!("{PROXY_NOTE}{a}")),
                            None,
                            &w,
                        ) {
                            Ok(_) => true,
                            Err(e) => {
                                refusals.push(format!("proxy: {e}"));
                                false
                            }
                        },
                        _ => false,
                    };
                    if declared {
                        extra.proxy_stats.declared += 1;
                    } else {
                        extra.proxy_stats.nothing += 1;
                    }
                }
                ProxyKind::Declaration(track, id) => {
                    // still the declaration it saw, not yet confirmed
                    if !engine
                        .identification()
                        .declaration(&track)
                        .is_some_and(|d| d.id == id && d.unconfirmed_rule())
                    {
                        continue;
                    }
                    let agrees = matches!(&pick, Some(Pick::Author(t)) if *t == track);
                    // skimmed: confirmed, as a card skimmed is approved; a slip says the other
                    let confirm = if !reads { true } else if slip { !agrees } else { agrees };
                    extra.proxy_stats.reads += u32::from(reads);
                    extra.proxy_stats.decisions += 1;
                    if confirm {
                        extra.proxy_stats.confirmed += 1;
                        extra.proxy_stats.confirmed_unread += u32::from(!reads);
                    } else {
                        extra.proxy_stats.revoked += 1;
                    }
                    let action = if confirm {
                        Action::ConfirmDeclaration {
                            by: op.clone(),
                            track: track.clone(),
                        }
                    } else {
                        Action::RevokeDeclaration {
                            by: op.clone(),
                            track: track.clone(),
                            note: Some(format!(
                                "proxy: {}",
                                match &pick {
                                    Some(Pick::Author(t)) => format!("the act looks like {t}'s"),
                                    Some(Pick::Declared(t)) => format!("{t}, declared, is at the fix"),
                                    Some(Pick::CantSay(why)) => format!("can't say: {why}"),
                                    None => "nothing to go on".into(),
                                }
                            )),
                        }
                    };
                    act_proxy(
                        &action, engine, sessions, tracks, vehicles, catalog, areas, now, terrain, last, refusals,
                        fallback_seen, failures, engaging, extra, s,
                    );
                }
                ProxyKind::Flag(f) => {
                    if !engine.identification().flag(f).is_some_and(|(_, st)| st.in_force()) {
                        continue;
                    }
                    if !reads {
                        if item.looks < 5 {
                            item.looks += 1;
                            item.due_s = extra.next_look(&p, s);
                            extra.proxy_items.push(item);
                        }
                        continue;
                    }
                    extra.proxy_stats.reads += 1;
                    extra.proxy_stats.decisions += 1;
                    let declared = match (&pick, &act) {
                        (Some(Pick::Author(track)), Some(a)) => engine
                            .declare_track(
                                sess.authority(),
                                None,
                                track,
                                Affiliation::Hostile,
                                Some(format!("{PROXY_NOTE}{a}")),
                                Some(f),
                                &w,
                            )
                            .is_ok(),
                        _ => false,
                    };
                    if declared {
                        extra.proxy_stats.declared += 1;
                    } else {
                        extra.proxy_stats.dismissed += 1;
                        if let Err(e) = engine.resolve_flag(
                            sess.authority(),
                            f,
                            crate::ident::FlagResolution::Dismissed,
                            Some("proxy: can't say who did it".into()),
                        ) {
                            refusals.push(format!("proxy: {e}"));
                        }
                    }
                }
            }
        }
    }
    if !extra.nominator {
        return;
    }
    // the signatures due
    type Signature = (u32, IntentId, String);
    let (due, later): (Vec<Signature>, Vec<Signature>) =
        std::mem::take(&mut extra.sign_due).into_iter().partition(|d| d.0 <= s);
    extra.sign_due = later;
    for (_, id, by) in due {
        if engine.intent(id).is_some_and(|i| i.state() == State::PendingApproval) {
            *last = Some(id);
            act_proxy(
                &Action::Approve { by }, engine, sessions, tracks, vehicles, catalog, areas, now, terrain, last, refusals,
                fallback_seen, failures, engaging, extra, s,
            );
        }
    }
    // a nomination still waiting whose signer's signature does not stand (refused, or dropped
    // with a plan that stopped holding): that signer looks and signs again 10 s on
    let signers_all: Vec<String> = sc.operators.iter().filter(|o| o.engage).map(|o| o.id.clone()).take(2).collect();
    let waiting: Vec<(IntentId, Vec<String>)> = engine
        .intents()
        .filter(|i| matches!(i.spec().task, Task::Engage { .. }) && i.state() == State::PendingApproval)
        .map(|i| (i.id(), i.engagement().map_or_else(Vec::new, |e| e.signed.iter().map(|sp| sp.by.to_string()).collect())))
        .collect();
    for (id, signed) in waiting {
        for by in &signers_all {
            if !signed.contains(by) && !extra.sign_due.iter().any(|d| d.1 == id && d.2 == *by) {
                extra.sign_due.push((s + 10, id, by.clone()));
            }
        }
    }
    // every track declared hostile, nominated as soon as it may be
    let Some(platform) = sc
        .vehicles
        .iter()
        .find(|v| catalog.get(&v.platform).is_some_and(|p| p.armed))
        .map(|v| v.uid.clone())
    else {
        return;
    };
    let signers: Vec<String> = sc.operators.iter().filter(|o| o.engage).map(|o| o.id.clone()).take(2).collect();
    let declared: Vec<String> = engine
        .identification()
        .declarations()
        .filter(|d| d.affiliation == Affiliation::Hostile)
        .map(|d| d.track.clone())
        .collect();
    for track in declared {
        let busy = engine.intents().any(|i| {
            matches!(i.spec().task, Task::Engage { .. })
                && matches!(
                    i.state(),
                    State::PendingApproval | State::Approved | State::Dispatched | State::Effected | State::Done
                )
                && crate::engage::params(i.spec()).is_some_and(|p| p.0 == track)
        });
        if busy {
            continue;
        }
        let (at, tries) = *extra.nominate_next.entry(track.clone()).or_insert((s + 5, 0));
        if s < at || tries >= 12 {
            continue;
        }
        let before = *last;
        act_proxy(
            &Action::Nominate {
                by: Some(op.clone()),
                agent: None,
                rationale: Some(format!("{track} declared hostile")),
                platform: platform.clone(),
                track: track.clone(),
                radius_m: 75.0,
                fallback: Fallback::Return,
                window_s: Some(600),
                accept: Vec::new(),
                accept_lost: Vec::new(),
            },
            engine, sessions, tracks, vehicles, catalog, areas, now, terrain, last, refusals, fallback_seen, failures,
            engaging, extra, s,
        );
        let pending = (*last != before)
            .then_some(*last)
            .flatten()
            .filter(|id| engine.intent(*id).is_some_and(|i| i.state() == State::PendingApproval));
        match pending {
            Some(id) => {
                for (k, by) in signers.iter().enumerate() {
                    extra.sign_due.push((s + 5 + 3 * k as u32, id, by.clone()));
                }
                extra.nominate_next.insert(track, (u32::MAX, tries + 1));
            }
            None => {
                extra.nominate_next.insert(track, (s + 10, tries + 1));
            }
        }
    }
}

/// An action the evaluation's proxies take, through the runner's own path.
#[allow(clippy::too_many_arguments)]
fn act_proxy(
    a: &Action,
    engine: &mut Engine,
    sessions: &BTreeMap<String, OperatorSession>,
    tracks: &mut BTreeMap<String, TrackSim>,
    vehicles: &mut BTreeMap<String, VehicleSim>,
    catalog: &Catalog,
    areas: &AreaSet,
    now: Time,
    terrain: &PolygonTerrain,
    last: &mut Option<IntentId>,
    refusals: &mut Vec<String>,
    fallback_seen: &mut Option<String>,
    failures: &mut Vec<String>,
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    extra: &mut Extra,
    s: u32,
) {
    act(
        a, engine, sessions, tracks, vehicles, catalog, areas, now, terrain, last, refusals, fallback_seen, failures,
        engaging, extra, s,
    );
}

/// Rules 6: a dispatched engagement's lost-link preload, as the runner's platform answers it.
struct PreloadSim {
    uid: String,
    /// when it answers the preload (a second after the dispatch)
    due_s: u32,
    /// the `set` reported, and whether it confirmed
    set: Option<bool>,
    /// its own values, read before the preload
    before: BTreeMap<String, f64>,
    /// what it holds once preloaded (what a re-read shows)
    holds: BTreeMap<String, f64>,
    /// the timer it carries while preloaded, s
    timer_s: Option<f64>,
    restored: bool,
    /// it already followed its lost-link action
    acted: bool,
    /// re-reads near the release so far
    verified: u32,
}

/// What a stand-in of each firmware carries before a preload (`crates/murmur-standins`
/// `params.rs` with the LOS link's 5 s timer): the platform's own values a restore puts back.
fn own_value(name: &str) -> f64 {
    match name {
        "FS_OPTIONS" => 16.0,
        "FS_LONG_TIMEOUT" | "FS_GCS_TIMEOUT" => 5.0,
        _ => 1.0,
    }
}

/// Rules 6: each dispatched engagement's platform answers its lost-link preload (as
/// `scenario.preload` says), puts its own values back once the engagement has ended, and,
/// cut off from the GCS for longer than its preloaded timer, follows the preloaded action.
#[allow(clippy::too_many_arguments)]
fn preloads(
    mode: &str,
    restore_after_s: u32,
    engine: &mut Engine,
    vehicles: &mut BTreeMap<String, VehicleSim>,
    extra: &mut Extra,
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    fallback_seen: &mut Option<String>,
    failures: &mut Vec<String>,
    s: u32,
) {
    use crate::engage::PreloadPhase;
    use crate::engine::PreloadReport;
    let ids: Vec<IntentId> = extra.preloads.keys().copied().collect();
    for id in ids {
        let Some(intent) = engine.intent(id) else { continue };
        let state = intent.state();
        let Some(preload) = intent.engagement().and_then(|e| e.preload.clone()) else {
            continue;
        };
        let Some(ps) = extra.preloads.get_mut(&id) else { continue };
        let silent = vehicles.get(&ps.uid).is_none_or(|v| v.silent);
        let own_timer = vehicles
            .get(&ps.uid)
            .and_then(|v| v.def.lost_link_s)
            .unwrap_or(5.0);
        let own = |n: &str| if n == preload.timer { own_timer } else { own_value(n) };
        // the set, a second after the dispatch
        if ps.set.is_none() && s >= ps.due_s && mode != "never" {
            let names: Vec<String> = preload.names().iter().map(|n| (*n).to_string()).collect();
            let before: BTreeMap<String, f64> =
                names.iter().map(|n| (n.clone(), own(n))).collect();
            let set_values = || {
                let mut r = before.clone();
                for p in &preload.set {
                    r.insert(p.name.clone(), p.value);
                }
                for b in &preload.clear_bits {
                    let v = r.get(&b.name).copied().unwrap_or(0.0).round() as i64;
                    r.insert(b.name.clone(), (v & !i64::from(b.clear)) as f64);
                }
                let timer = own_timer.min(preload.timer_max_s);
                r.insert(preload.timer.clone(), timer);
                r
            };
            let report = if silent || mode == "no_answer" {
                PreloadReport {
                    uid: Uid::new(&ps.uid),
                    phase: PreloadPhase::Set,
                    before: BTreeMap::new(),
                    readback: BTreeMap::new(),
                    error: Some(format!(
                        "no answer from the vehicle to PARAM_REQUEST_READ {} after 3 attempts",
                        names[0]
                    )),
                    vehicle: None,
                }
            } else if mode == "mismatch" {
                // it answers with another action than the order's (the first set parameter
                // one off): a write it did not take
                let mut r = set_values();
                if let Some(p) = preload.set.last() {
                    r.insert(p.name.clone(), p.value + 1.0);
                }
                PreloadReport {
                    uid: Uid::new(&ps.uid),
                    phase: PreloadPhase::Set,
                    before: before.clone(),
                    readback: r,
                    error: None,
                    vehicle: None,
                }
            } else {
                PreloadReport {
                    uid: Uid::new(&ps.uid),
                    phase: PreloadPhase::Set,
                    before: before.clone(),
                    readback: set_values(),
                    error: None,
                    vehicle: None,
                }
            };
            ps.before = report.before.clone();
            ps.holds = report.readback.clone();
            ps.timer_s = report.readback.get(&preload.timer).copied();
            // the write-ahead: its own values logged before anything is written
            if !report.before.is_empty() {
                let identity = vehicles.get(&ps.uid).map(|v| crate::engage::VehicleIdentity {
                    platform: Some(v.def.platform.clone()),
                    callsign: Some(ps.uid.clone()),
                    sysid: None,
                    home: Some(v.home),
                });
                let intent_report = PreloadReport {
                    uid: Uid::new(&ps.uid),
                    phase: PreloadPhase::Intent,
                    before: report.before.clone(),
                    readback: BTreeMap::new(),
                    error: None,
                    vehicle: identity,
                };
                if let Err(e) = engine.record_preload(id, intent_report) {
                    failures.push(format!("t={s}: preload intent {id}: {e}"));
                }
            }
            match engine.record_preload(id, report) {
                Ok(stop) => {
                    let confirmed = engine
                        .intent(id)
                        .and_then(|i| i.engagement())
                        .and_then(|e| e.preloaded.as_ref())
                        .is_some_and(|r| r.confirmed);
                    ps.set = Some(confirmed);
                    extra.preload_reports.push(format!(
                        "t={s} set {}",
                        if confirmed { "confirmed" } else { "unconfirmed" }
                    ));
                    if let Some(f) = stop {
                        *fallback_seen = Some(f.fallback.as_str().into());
                        if let Some(v) = vehicles.get_mut(f.uid.as_str()) {
                            follow(v, f.fallback);
                        }
                        engaging.remove(&f.intent);
                    }
                }
                Err(e) => failures.push(format!("t={s}: preload {id}: {e}")),
            }
            continue;
        }
        let Some(ps) = extra.preloads.get_mut(&id) else { continue };
        // read again near the release; with `changed`, another station changed its lost-link
        // action after the set (the last parameter the preload sets, one off)
        let due = intent
            .engagement()
            .is_some_and(|e| crate::engage::preload_verify_due(e, stepped(i64::from(s) * 1000, extra.clock_step), &engine.policy().rules));
        if ps.set == Some(true) && state == State::Dispatched && !silent && due {
            let mut readback = ps.holds.clone();
            if mode == "changed"
                && let Some(p) = preload.set.last()
            {
                readback.insert(p.name.clone(), p.value + 1.0);
            }
            let report = PreloadReport {
                uid: Uid::new(&ps.uid),
                phase: PreloadPhase::Verify,
                before: BTreeMap::new(),
                readback,
                error: None,
                vehicle: None,
            };
            match engine.record_preload(id, report) {
                Ok(stop) => {
                    let confirmed = engine
                        .intent(id)
                        .and_then(|i| i.engagement())
                        .and_then(|e| e.verified.as_ref())
                        .is_some_and(|r| r.confirmed);
                    if !confirmed || ps.verified == 0 {
                        extra.preload_reports.push(format!(
                            "t={s} verify {}",
                            if confirmed { "confirmed" } else { "changed" }
                        ));
                    }
                    ps.verified += 1;
                    if let Some(f) = stop {
                        *fallback_seen = Some(f.fallback.as_str().into());
                        if let Some(v) = vehicles.get_mut(f.uid.as_str()) {
                            follow(v, f.fallback);
                        }
                        engaging.remove(&f.intent);
                    }
                }
                Err(e) => failures.push(format!("t={s}: verify {id}: {e}")),
            }
            continue;
        }
        // cut off from the GCS for longer than its preloaded timer: its own failsafe
        if ps.set == Some(true)
            && !ps.restored
            && !ps.acted
            && let Some(v) = vehicles.get_mut(&ps.uid)
            && let (Some(since), Some(timer)) = (v.silent_since, ps.timer_s)
            && f64::from(s.saturating_sub(since)) > timer
        {
            ps.acted = true;
            follow(v, preload.fallback);
            extra.lost_link_action = Some(format!(
                "{} ({} {}) after {} s without the GCS",
                preload.action,
                preload.firmware,
                preload.fallback.as_str(),
                s - since
            ));
        }
        // its own values back once the engagement has ended (when it can hear us, and from
        // `restore_after_s` on)
        // (rules 12: nothing if no longer owed: the platform was spent by its effect, or its
        // values were written off)
        if ps.set.is_some()
            && !ps.restored
            && !ps.before.is_empty()
            && state != State::Dispatched
            && !silent
            && s >= restore_after_s
            && engine.restore_owed_to(&Uid::new(&ps.uid))
        {
            let report = PreloadReport {
                uid: Uid::new(&ps.uid),
                phase: PreloadPhase::Restore,
                before: BTreeMap::new(),
                readback: ps.before.clone(),
                error: None,
                vehicle: None,
            };
            match engine.record_preload(id, report) {
                Ok(_) => {
                    ps.restored = true;
                    extra.preload_reports.push(format!("t={s} restore confirmed"));
                }
                Err(e) => failures.push(format!("t={s}: restore {id}: {e}")),
            }
        }
    }
}

/// Rules 9: the engagements a revocation stopped: their platforms fly the fallbacks.
fn follow_stops(
    vehicles: &mut BTreeMap<String, VehicleSim>,
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    fallback_seen: &mut Option<String>,
    fallbacks: &[engage::FallbackOrder],
) {
    for f in fallbacks {
        *fallback_seen = Some(f.fallback.as_str().into());
        if let Some(v) = vehicles.get_mut(f.uid.as_str()) {
            follow(v, f.fallback);
        }
        engaging.remove(&f.intent);
    }
}

/// The runner's platform flies a fallback: hold where it is, home, or orbit where it is.
fn follow(v: &mut VehicleSim, f: Fallback) {
    match f {
        Fallback::Abort => {
            v.goal = None;
            v.vel = [0.0, 0.0];
        }
        Fallback::Return => {
            v.goal = Some(v.home);
            v.orbit = false;
        }
        Fallback::Loiter => {
            v.goal = Some(v.pos);
            v.orbit = true;
        }
    }
}

fn dispatch_approved(
    engine: &mut Engine,
    world: &WorldSnapshot,
    vehicles: &mut BTreeMap<String, VehicleSim>,
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    failures: &mut Vec<String>,
    s: u32,
) {
    let ready: Vec<(IntentId, bool, Vec<Uid>)> = engine
        .intents()
        .filter(|i| matches!(i.state(), State::Approved | State::AutoApproved))
        .map(|i| (i.id(), matches!(i.spec().task, Task::Engage { .. }), i.spec().uids.clone()))
        .collect();
    for (id, is_engage, uids) in ready {
        if !is_engage {
            // another order on these vehicles: as on the bridge, dispatched if the link
            // reaches them, else queued; either way it stands and supersedes any engagement
            // on them (no fallback commanded; a queued one from rules 7)
            match engine.dispatch_or_queue(id, world) {
                Ok(Routed::Dispatched(Order::Vehicles(o))) => {
                    supersede_engagements(engine, id, &uids, engaging, failures, s);
                    fly(vehicles, &o);
                }
                Ok(Routed::Dispatched(_)) => failures.push(format!("t={s}: {id} dispatched as something else")),
                Ok(Routed::Queued { .. }) => {
                    if engine.policy().rules.rules_version >= crate::rules::QUEUE_FOLLOWUPS_SINCE {
                        supersede_engagements(engine, id, &uids, engaging, failures, s);
                    }
                }
                // rules 7: refused (no queue for its kind, the platform owed its values), but
                // it stands over an engagement on its vehicles all the same (as on the bridge)
                Ok(Routed::Voided { .. }) => {
                    supersede_engagements(engine, id, &uids, engaging, failures, s);
                }
                Err(e) => failures.push(format!("t={s}: dispatch {id}: {e}")),
            }
            continue;
        }
        match engine.dispatch_order(id) {
            Ok(Order::Engage(o)) => {
                if let Some(v) = vehicles.get_mut(o.uid.as_str()) {
                    v.goal = Some(o.effect_point);
                    v.orbit = true;
                }
                // as the bridge does, any other engagement in play on it first, in the dispatch's step (none can be:
                // one engagement per platform, the platform_free check)
                let others: Vec<IntentId> = engine
                    .intents()
                    .filter(|i| {
                        i.id() != id
                            && matches!(i.spec().task, Task::Engage { .. })
                            && matches!(i.state(), State::PendingApproval | State::Approved | State::Dispatched)
                            && i.spec().uids.contains(&o.uid)
                    })
                    .map(|i| i.id())
                    .collect();
                for other in others {
                    failures.push(format!("t={s}: engagement {other} was in play on {} when {id} was dispatched", o.uid));
                    if let Err(e) = engine.supersede_engagement(other, id) {
                        failures.push(format!("t={s}: supersede {other}: {e}"));
                    }
                    engaging.remove(&other);
                }
                // then, as on the bridge, older orders still waiting for this vehicle are void
                let stale: Vec<IntentId> = engine
                    .intents()
                    .filter(|i| {
                        i.id() != id
                            && !matches!(i.spec().task, Task::Engage { .. })
                            && matches!(i.state(), State::PendingApproval | State::Approved)
                            && i.spec().uids.contains(&o.uid)
                    })
                    .map(|i| i.id())
                    .collect();
                for other in stale {
                    if let Err(e) = engine.void(other, &format!("superseded: {id} was dispatched to vehicle {}", o.uid)) {
                        failures.push(format!("t={s}: void {other}: {e}"));
                    }
                }
                engaging.insert(id, (o.uid.to_string(), o.effect_point));
            }
            Ok(_) => failures.push(format!("t={s}: {id} dispatched as something else")),
            Err(e) => failures.push(format!("t={s}: dispatch {id}: {e}")),
        }
    }
}

/// An order taking `uids` (dispatched, or from rules 7 queued) stops every engagement in play
/// on them, superseded by it, with no fallback commanded: the newest order stands.
fn supersede_engagements(
    engine: &mut Engine,
    id: IntentId,
    uids: &[Uid],
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    failures: &mut Vec<String>,
    s: u32,
) {
    let hit: Vec<IntentId> = engine
        .intents()
        .filter(|i| {
            i.id() != id
                && matches!(i.spec().task, Task::Engage { .. })
                && matches!(
                    i.state(),
                    State::PendingApproval | State::Approved | State::Dispatched
                )
                && i.spec().uids.iter().any(|u| uids.contains(u))
        })
        .map(|i| i.id())
        .collect();
    for e in hit {
        if let Err(err) = engine.supersede_engagement(e, id) {
            failures.push(format!("t={s}: supersede {e}: {err}"));
        }
        engaging.remove(&e);
    }
}

/// The runner's vehicles fly an order: home for an rtl, around the center for an orbit, held
/// where they are for a hold.
fn fly(vehicles: &mut BTreeMap<String, VehicleSim>, o: &crate::engine::DispatchOrder) {
    for u in &o.uids {
        if let Some(v) = vehicles.get_mut(u.as_str()) {
            match &o.task {
                Task::Rtl {} => {
                    v.goal = Some(v.home);
                    v.orbit = false;
                }
                Task::Orbit { center, .. } => {
                    v.goal = Some(*center);
                    v.orbit = true;
                }
                _ => {
                    v.goal = None;
                    v.vel = [0.0, 0.0];
                }
            }
        }
    }
}

/// Rules 5, the bridge's queue: every queued order is offered again on the world now (still
/// unreachable: nothing happens; else re-checked and dispatched, sent back to the inbox, or
/// voided).
fn follow_queue(
    engine: &mut Engine,
    world: &WorldSnapshot,
    terrain: &PolygonTerrain,
    vehicles: &mut BTreeMap<String, VehicleSim>,
    engaging: &mut BTreeMap<IntentId, (String, LonLat)>,
    failures: &mut Vec<String>,
    s: u32,
) {
    let queued: Vec<IntentId> = engine
        .intents()
        .filter(|i| i.state() == State::Queued)
        .map(|i| i.id())
        .collect();
    for id in queued {
        if engine.intent(id).map(|i| i.state()) != Some(State::Queued) {
            continue;
        }
        match engine.dequeue(id, world, terrain) {
            Ok(crate::engine::Dequeued::Dispatched(Order::Vehicles(o))) => {
                let uids = o.uids.clone();
                supersede_engagements(engine, id, &uids, engaging, failures, s);
                fly(vehicles, &o);
            }
            Ok(crate::engine::Dequeued::Dispatched(_)) => {
                failures.push(format!("t={s}: queued {id} dispatched as something else"));
            }
            Ok(_) => {}
            Err(e) => failures.push(format!("t={s}: dequeue {id}: {e}")),
        }
    }
}

// ---- log attacks ------------------------------------------------------------------------

/// Rules 13: every decision's inputs an event logs.
fn decision_inputs(body: &mut EventBody) -> Vec<&mut crate::world::LoggedInputs> {
    match body {
        EventBody::CheckResult { inputs, .. }
        | EventBody::PolicyDecision { inputs, .. }
        | EventBody::PictureIssued { inputs, .. }
        | EventBody::EngagePicture { inputs, .. }
        | EventBody::EngageObserve { inputs, .. }
        | EventBody::IntentQueued { inputs, .. } => vec![inputs],
        EventBody::ApprovalReset { inputs: Some(i), .. } | EventBody::IntentDispatched { inputs: Some(i), .. } => vec![i],
        _ => Vec::new(),
    }
}

/// Rules 9: every track picture an event logs (a decision's inputs, an act's or a flag's).
fn picture_tracks(body: &mut EventBody) -> Vec<&mut BTreeMap<String, TrackFacts>> {
    match body {
        EventBody::CheckResult { inputs, .. }
        | EventBody::PolicyDecision { inputs, .. }
        | EventBody::PictureIssued { inputs, .. }
        | EventBody::EngagePicture { inputs, .. }
        | EventBody::EngageObserve { inputs, .. }
        | EventBody::IntentQueued { inputs, .. } => vec![&mut inputs.tracks],
        EventBody::ApprovalReset { inputs: Some(i), .. } | EventBody::IntentDispatched { inputs: Some(i), .. } => {
            vec![&mut i.tracks]
        }
        EventBody::IdentAct { inputs, .. } | EventBody::IdentFlagged { inputs: Some(inputs), .. } => {
            vec![&mut inputs.tracks]
        }
        _ => Vec::new(),
    }
}

/// A log attack's result.
#[derive(Clone, Debug, PartialEq)]
pub enum Tampered {
    /// The forged log, its chain recomputed.
    Forged(String),
    /// The log has nothing this attack edits (no reset to hide, no signature to drop...).
    NotApplicable(String),
}

/// Attack a log the way a tamperer who can write the file would, recomputing `seq` and the
/// hash chain so the chain itself verifies: only recomputation can catch it. `Err`: an
/// unknown attack.
pub fn tamper(text: &str, kind: &str) -> Result<Tampered, String> {
    match forge(text, kind) {
        Ok(forged) => Ok(Tampered::Forged(forged)),
        Err(e) if e.starts_with("unknown tamper") || e.starts_with("log:") => Err(e),
        Err(e) => Ok(Tampered::NotApplicable(e)),
    }
}

fn forge(text: &str, kind: &str) -> Result<String, String> {
    let mut events = parse_log(text).map_err(|e| format!("log: {e}"))?;
    let engage_intent = events.iter().find_map(|e| match &e.body {
        EventBody::IntentCreated { intent, .. } if matches!(intent.task, Task::Engage { .. }) => {
            Some(intent.clone())
        }
        _ => None,
    });
    let spec = engage_intent.ok_or("no engagement in the log")?;
    match kind {
        "proposer_approval" | "agent_approval" => {
            let who = match (kind, &spec.source) {
                ("proposer_approval", crate::intent::Source::Human { id }) => id.clone(),
                ("proposer_approval", _) => return Err("the engagement was not proposed by a human".into()),
                _ => OperatorId::new("agent-planner"),
            };
            let mut done = false;
            for e in &mut events {
                if let EventBody::ApprovalGranted { intent, by, .. } = &mut e.body
                    && *intent == spec.id
                {
                    *by = who.clone();
                    done = true;
                }
            }
            if !done {
                return Err("no approval.granted to forge".into());
            }
        }
        "strip_approvers" => {
            // a log that says one approver was enough: the policy's engagement tier (if the
            // file had one) and the decision without `approvers`, and the first signature
            // gone
            let mut decided = false;
            for e in &mut events {
                match &mut e.body {
                    EventBody::RunStarted { policy, .. } => {
                        for t in &mut policy.tiers {
                            if t.name == "engagement" {
                                t.approvers = None;
                            }
                        }
                    }
                    EventBody::PolicyDecision {
                        intent, decision, ..
                    } if *intent == spec.id => {
                        decision.approvers = 1;
                        decided = true;
                    }
                    _ => {}
                }
            }
            let before = events.len();
            events.retain(|e| {
                !matches!(&e.body, EventBody::ApprovalConcurred { intent, .. } if *intent == spec.id)
            });
            if !decided || events.len() == before {
                return Err("no engagement decision and signature to strip".into());
            }
        }
        "agent_order_auto" => {
            // an agent's order on the engaging platform made to look auto-approved: the
            // decision says the hold/rtl tier and the engagement-in-play facts are gone
            let order = events
                .iter()
                .find_map(|e| match &e.body {
                    EventBody::IntentCreated { intent, .. }
                        if intent.source.is_agent()
                            && matches!(intent.task, Task::Hold {} | Task::Rtl {}) =>
                    {
                        Some(intent.id)
                    }
                    _ => None,
                })
                .ok_or("no agent order in the log")?;
            let mut done = false;
            for e in &mut events {
                if let EventBody::PolicyDecision {
                    intent,
                    inputs,
                    decision,
                } = &mut e.body
                    && *intent == order
                {
                    inputs.engaged.clear();
                    decision.tier = crate::policy::Tier::Auto;
                    decision.rule = "low-risk".into();
                    done = true;
                }
            }
            if !done {
                return Err("no decision on the agent's order".into());
            }
        }
        "drop_restore_owed" => {
            // rules 7: a mission's decisions made to look as if its platform's own lost-link
            // values had been back (the fact stripped from every input that logged it)
            let mut done = false;
            for e in &mut events {
                let inputs = match &mut e.body {
                    EventBody::CheckResult { inputs, .. }
                    | EventBody::PolicyDecision { inputs, .. }
                    | EventBody::IntentQueued { inputs, .. } => Some(inputs),
                    EventBody::IntentDispatched { inputs, .. } => inputs.as_mut(),
                    _ => None,
                };
                if let Some(inputs) = inputs
                    && !inputs.restore_owed.is_empty()
                {
                    inputs.restore_owed.clear();
                    done = true;
                }
            }
            if !done {
                return Err("no lost-link values owed in any decision's inputs".into());
            }
        }
        "drop_ack" => {
            // an approval that stopped an engagement, without saying so
            let mut done = false;
            for e in &mut events {
                match &mut e.body {
                    EventBody::ApprovalGranted { supersedes, .. }
                    | EventBody::ApprovalConcurred { supersedes, .. }
                        if !supersedes.is_empty() =>
                    {
                        supersedes.clear();
                        done = true;
                    }
                    _ => {}
                }
            }
            if !done {
                return Err("no acknowledged approval to strip".into());
            }
        }
        "loosen_policy" => {
            // a log whose policy says picture tokens are good for a day
            let mut done = false;
            for e in &mut events {
                if let EventBody::RunStarted { policy, .. } = &mut e.body {
                    let mut r = policy.rules.engage_rules();
                    r.token_ttl_s = Some(86_400.0);
                    r.seen_max_age_s = None;
                    policy.rules.engage = Some(r);
                    done = true;
                }
            }
            if !done {
                return Err("no run.started".into());
            }
        }
        "drop_reset" => {
            // hide that the picture changed between signatures
            let before = events.len();
            events.retain(|e| {
                !matches!(&e.body, EventBody::ApprovalReset { intent, .. } if *intent == spec.id)
            });
            if events.len() == before {
                return Err("no approval.reset to drop".into());
            }
        }
        "drop_signature" => {
            let before = events.len();
            events.retain(|e| {
                !matches!(&e.body, EventBody::ApprovalConcurred { intent, .. } if *intent == spec.id)
            });
            if events.len() == before {
                return Err("no approval.concurred to drop".into());
            }
        }
        "suppress_void" => {
            // drop the stop and its transition, and make the failed picture look clean
            let stop = events
                .iter()
                .position(|e| matches!(&e.body, EventBody::EngageStopped { intent, .. } if *intent == spec.id))
                .ok_or("no engage.stopped to suppress")?;
            let tr = events[stop..]
                .iter()
                .position(|e| matches!(&e.body, EventBody::IntentState { intent, .. } if *intent == spec.id))
                .map(|i| i + stop)
                .ok_or("no transition after the stop")?;
            events.remove(tr);
            events.remove(stop);
            for e in &mut events {
                if let EventBody::EngagePicture { intent, holds, .. } = &mut e.body
                    && *intent == spec.id
                {
                    *holds = true;
                }
            }
        }
        "edit_snapshot" | "edit_effect" => {
            let mut done = false;
            for e in &mut events {
                match &mut e.body {
                    EventBody::ApprovalGranted {
                        intent,
                        snapshot: Some(snap),
                        ..
                    } if *intent == spec.id && kind == "edit_snapshot" => {
                        // move the predicted effect point 300 m north (as if the approver had
                        // seen it there), consistently in the dispatch order too
                        snap.preview.effect_point = offset(snap.preview.effect_point, 0.0, 300.0);
                        done = true;
                    }
                    EventBody::IntentDispatched {
                        intent,
                        engage: Some(o),
                        ..
                    } if *intent == spec.id && kind == "edit_snapshot" => {
                        o.effect_point = offset(o.effect_point, 0.0, 300.0);
                    }
                    EventBody::EngageEffect { intent, point, .. }
                        if *intent == spec.id && kind == "edit_effect" =>
                    {
                        *point = offset(*point, 0.0, 300.0);
                        done = true;
                    }
                    _ => {}
                }
            }
            if !done {
                return Err(format!("nothing to {kind}"));
            }
        }
        "drop_preload" => {
            // rules 6: hide that the platform confirmed (or failed) its lost-link preload
            let before = events.len();
            events.retain(|e| {
                !matches!(
                    &e.body,
                    EventBody::EngagePreload { intent, phase: crate::engage::PreloadPhase::Set, .. }
                        if *intent == spec.id
                )
            });
            if events.len() == before {
                return Err("no engage.preload set to drop".into());
            }
        }
        "drop_verify" => {
            // rules 6: hide the re-read near the release that found the preload changed
            let before = events.len();
            events.retain(|e| {
                !matches!(
                    &e.body,
                    EventBody::EngagePreload { intent, phase: crate::engage::PreloadPhase::Verify, confirmed: false, .. }
                        if *intent == spec.id
                )
            });
            if events.len() == before {
                return Err("no unconfirmed engage.preload verify to drop".into());
            }
        }
        "forge_preload" => {
            // rules 6: a preload the platform never confirmed made to look confirmed
            let mut done = false;
            for e in &mut events {
                if let EventBody::EngagePreload {
                    intent,
                    confirmed,
                    error,
                    ..
                } = &mut e.body
                    && *intent == spec.id
                    && !*confirmed
                {
                    *confirmed = true;
                    *error = None;
                    done = true;
                }
            }
            if !done {
                return Err("no unconfirmed engage.preload to forge".into());
            }
        }
        "drop_acceptance" => {
            // rules 6: hide that an operator accepted a lost vehicle (the decisions still
            // read it)
            let before = events.len();
            events.retain(|e| !matches!(&e.body, EventBody::EngageAccepted { intent, .. } if *intent == spec.id));
            if events.len() == before {
                return Err("no engage.accepted to drop".into());
            }
        }
        "acceptance_by_agent" => {
            // rules 6: an acceptance credited to an agent (in the operator's session)
            let mut done = false;
            for e in &mut events {
                if let EventBody::EngageAccepted { intent, by, .. } = &mut e.body
                    && *intent == spec.id
                {
                    *by = OperatorId::new("agent-planner");
                    done = true;
                }
            }
            if !done {
                return Err("no engage.accepted to forge".into());
            }
        }
        "hide_self_accepted" => {
            // rules 6: a signer who accepted a lost vehicle recorded as if they had not
            let mut done = false;
            for e in &mut events {
                match &mut e.body {
                    EventBody::ApprovalGranted { intent, self_accepted, .. }
                    | EventBody::ApprovalConcurred { intent, self_accepted, .. }
                        if *intent == spec.id && *self_accepted =>
                    {
                        *self_accepted = false;
                        done = true;
                    }
                    _ => {}
                }
            }
            if !done {
                return Err("no self-accepted signature to hide".into());
            }
        }
        "acceptance_without_authority" => {
            // rules 6: an acceptance re-credited to an operator whose session has no
            // engagement authority (that session, as opened in the log)
            let plain = events.iter().find_map(|e| match &e.body {
                EventBody::OperatorAction {
                    operator,
                    session,
                    action,
                    grants,
                    ..
                } if action == "session.opened" && !grants.is_some_and(|g| g.engage) => {
                    Some((operator.clone(), *session))
                }
                _ => None,
            });
            let Some((op, sess)) = plain else {
                return Err("no session without engagement authority in the log".into());
            };
            let mut done = false;
            for e in &mut events {
                if let EventBody::EngageAccepted {
                    intent, by, session, ..
                } = &mut e.body
                    && *intent == spec.id
                {
                    *by = op.clone();
                    *session = sess;
                    done = true;
                }
            }
            if !done {
                return Err("no engage.accepted to forge".into());
            }
        }
        "hide_lost_contact" => {
            // rules 13: every decision on it shown without the lost contacts it read
            let mut done = false;
            for e in &mut events {
                for i in decision_inputs(&mut e.body) {
                    if !i.lost_contacts.is_empty() {
                        i.lost_contacts.clear();
                        done = true;
                    }
                }
            }
            if !done {
                return Err("no decision read a lost contact".into());
            }
        }
        "drop_track_dropped" => {
            // rules 13: the tracker's drops cut from the log (the contacts vanish from the
            // picture as if they had never been dropped)
            let before = events.len();
            events.retain(|e| !matches!(&e.body, EventBody::TrackDropped { .. }));
            if events.len() == before {
                return Err("no track.dropped to cut".into());
            }
        }
        "forge_lost_acceptance" => {
            // rules 13: the acceptance of a lost contact re-dated, as if the contact had been seen
            // a minute later than it was (a smaller region it accepted), everywhere it appears:
            // the acceptance, every decision's inputs and every picture token
            let mut forged: BTreeMap<String, Time> = BTreeMap::new();
            for e in &mut events {
                if let EventBody::EngageAccepted {
                    intent,
                    contact: Some(c),
                    since: Some(t),
                    ..
                } = &mut e.body
                    && *intent == spec.id
                {
                    *t = t.plus_ms(60_000);
                    forged.insert(c.clone(), *t);
                }
            }
            if forged.is_empty() {
                return Err("no acceptance of a lost contact to forge".into());
            }
            for e in &mut events {
                for i in decision_inputs(&mut e.body) {
                    for (c, a) in i.lost_accepted.iter_mut() {
                        if let Some(t) = forged.get(c) {
                            a.since = Some(*t);
                        }
                    }
                }
                let tokens: Vec<&mut engage::PictureToken> = match &mut e.body {
                    EventBody::PictureIssued { token, .. } => vec![&mut **token],
                    EventBody::ApprovalConcurred { token: Some(t), .. } | EventBody::ApprovalGranted { token: Some(t), .. } => {
                        vec![&mut **t]
                    }
                    _ => Vec::new(),
                };
                for tok in tokens {
                    for (c, a) in tok.lost_accepted.iter_mut() {
                        if let Some(t) = forged.get(c) {
                            a.since = Some(*t);
                        }
                    }
                }
            }
        }
        "drop_act" => {
            // rules 9: a rule declaration left without the act behind it
            let at = events
                .iter()
                .position(|e| matches!(&e.body, EventBody::IdentAct { .. }))
                .ok_or("no ident.act to drop")?;
            events.remove(at);
        }
        "declare_flagged_act" => {
            // rules 9: an act attributed below the threshold (flagged) made to look declared:
            // its verdict and its flag replaced by a rule declaration of its best candidate
            let at = events
                .iter()
                .position(|e| {
                    matches!(&e.body, EventBody::IdentAct { verdict: crate::ident::ActVerdict::Flag { .. }, .. })
                })
                .ok_or("no flagged act to forge")?;
            let declared = events[..at]
                .iter()
                .filter(|e| matches!(e.body, EventBody::IdentDeclared { .. }))
                .count() as u64;
            let (report, best) = match &mut events[at].body {
                EventBody::IdentAct {
                    act,
                    candidates,
                    verdict,
                    ..
                } => {
                    let best = candidates.first().cloned().ok_or("the act has no candidate")?;
                    *verdict = crate::ident::ActVerdict::Declare {
                        track: best.track.clone(),
                        attribution: best.confidence,
                        corroborated_by: None,
                    };
                    (act.clone(), best)
                }
                _ => return Err("not an act".into()),
            };
            let next = events.get(at + 1).ok_or("nothing after the act")?;
            let t = next.t;
            let cfg = crate::ident::IdentConfig {
                rule_declaration: true,
                ..crate::ident::IdentConfig::default()
            };
            let d = crate::ident::rule_declaration(declared + 1, t, &report, &best.track, best.confidence, None, &cfg);
            events[at + 1].body = EventBody::IdentDeclared {
                declaration: Box::new(d),
            };
        }
        "declaration_by_another_operator" => {
            // rules 9: an operator's declaration credited to another operator (in the first
            // one's session), the decisions' pictures made to agree
            let (track, from, to) = events
                .iter()
                .find_map(|e| match &e.body {
                    EventBody::IdentDeclared { declaration } => match &declaration.by {
                        crate::ident::Declarer::Operator { operator, .. } => {
                            Some((declaration.track.clone(), operator.clone()))
                        }
                        crate::ident::Declarer::Rule { .. } => None,
                    },
                    _ => None,
                })
                .and_then(|(t, op)| {
                    let other = events.iter().find_map(|e| match &e.body {
                        EventBody::OperatorAction { operator, action, .. }
                            if action == "session.opened" && *operator != op =>
                        {
                            Some(operator.clone())
                        }
                        _ => None,
                    })?;
                    Some((t, op, other))
                })
                .ok_or("no operator's declaration and another operator to credit it to")?;
            let swap = |d: &mut crate::ident::Declaration| {
                if let crate::ident::Declarer::Operator { operator, .. } = &mut d.by
                    && *operator == from
                {
                    *operator = to.clone();
                }
            };
            for e in &mut events {
                if let EventBody::IdentDeclared { declaration } = &mut e.body
                    && declaration.track == track
                {
                    swap(declaration);
                }
                for tracks in picture_tracks(&mut e.body) {
                    if let Some(t) = tracks.get_mut(&track)
                        && t.declared_by.as_deref() == Some(from.as_str())
                    {
                        t.declared_by = Some(to.to_string());
                        if let Some(d) = t.declaration.as_mut() {
                            swap(d);
                        }
                    }
                }
            }
        }
        "drop_revocation_stop" => {
            // rules 9: the engagement a revocation stopped made to go on (its stop and the
            // transition after it cut)
            let at = events
                .iter()
                .position(|e| {
                    matches!(&e.body, EventBody::EngageStopped { cause: engage::StopCause::Revoked { .. }, .. })
                })
                .ok_or("no engagement stopped for a revocation")?;
            events.remove(at);
            if matches!(events.get(at).map(|e| &e.body), Some(EventBody::IntentState { .. })) {
                events.remove(at);
            }
        }
        "confirm_in_inputs" => {
            // rules 9: a rule declaration nobody confirmed shown confirmed in the decisions'
            // pictures (so it is exempt in another engagement's keep-out)
            let mut done = false;
            for e in &mut events {
                for tracks in picture_tracks(&mut e.body) {
                    for t in tracks.values_mut() {
                        if let Some(d) = t.declaration.as_mut()
                            && d.unconfirmed_rule()
                        {
                            d.confirmed = Some(crate::ident::Confirmation {
                                by: OperatorId::new("op-2"),
                                session: crate::ids::SessionId(2),
                                t: d.t,
                            });
                            done = true;
                        }
                    }
                }
            }
            if !done {
                return Err("no unconfirmed rule declaration in any picture".into());
            }
        }
        "drop_expended_fact" => {
            // rules 12: a platform's closure as expended kept, the catalog fact that justifies
            // it taken out of every picture (as if the platform could be restored)
            if !events.iter().any(|e| matches!(&e.body, EventBody::RestoreClosed { closed_by: crate::audit::RestoreClosure::Expended { .. }, .. })) {
                return Err("no restore closed as expended".into());
            }
            let mut out = Vec::new();
            for e in &events {
                let line = serde_json::to_string(e).map_err(|err| err.to_string())?;
                let line = line.replace(",\"expended_by_effect\":true", "");
                out.push(serde_json::from_str::<crate::audit::Event>(&line).map_err(|err| err.to_string())?);
            }
            events = out;
        }
        "write_off_by_another" => {
            // rules 12: a write-off credited to another operator than the session's
            let mut done = false;
            for e in &mut events {
                if let EventBody::RestoreClosed {
                    closed_by: crate::audit::RestoreClosure::WrittenOff { operator, .. },
                    ..
                } = &mut e.body
                {
                    *operator = OperatorId::new(if operator.as_str() == "op-2" { "op-3" } else { "op-2" });
                    done = true;
                }
            }
            if !done {
                return Err("no write-off".into());
            }
        }
        "drop_standing_clause" => {
            // rules 12: a refusal's reason cut of the orders it leaves standing
            let mut done = false;
            for e in &mut events {
                if let EventBody::IntentState { reason, .. } = &mut e.body
                    && let Some(i) = reason.find("; this refusal replaces no order")
                {
                    reason.truncate(i);
                    done = true;
                }
            }
            if !done {
                return Err("no refusal naming an order still standing".into());
            }
        }
        "drop_first_seen" => {
            // rules 12: every track made to look as if the tracker had not said when it
            // started it (a young track's keep-out region would not be there)
            let mut done = false;
            for e in &mut events {
                let inputs = match &mut e.body {
                    EventBody::CheckResult { inputs, .. }
                    | EventBody::PolicyDecision { inputs, .. }
                    | EventBody::EngagePicture { inputs, .. }
                    | EventBody::EngageObserve { inputs, .. } => Some(inputs),
                    _ => None,
                };
                for t in inputs.into_iter().flat_map(|i| i.tracks.values_mut()) {
                    if t.first_seen_t.take().is_some() {
                        done = true;
                    }
                }
            }
            if !done {
                return Err("no track's first detection in any decision's inputs".into());
            }
        }
        other => return Err(format!("unknown tamper {other:?}")),
    }
    // renumber and re-chain
    let mut prev = GENESIS.to_string();
    let mut out = String::new();
    for (i, e) in events.iter_mut().enumerate() {
        e.seq = i as u64 + 1;
        e.prev = prev.clone();
        let line = serde_json::to_string(&*e).map_err(|err| err.to_string())?;
        prev = sha256_hex(line.as_bytes());
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}
