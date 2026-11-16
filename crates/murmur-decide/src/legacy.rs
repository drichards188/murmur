//! What decision rules below 33 decided by: the constants that rules 33
//! ([`crate::rules::SCENARIO_SETTINGS_SINCE`], docs/SCENARIO-SETTINGS.md) hand to the scenario's
//! policy. Each one here is read only where a run decides under older rules (or as the standing
//! value a file writes out, [`crate::watch::WatchSettings::default`], [`crate::silent::SilentDrift`]),
//! never in a decision of rules 33, which reads [`crate::rules::RulesConfig::watch`] and
//! [`crate::rules::RulesConfig::silent_drift`] instead. They are kept in one place so that a guard
//! can tell a copy of a setting from the record of what older rules meant, and so that the old
//! rules decide, and replay recomputes them, byte for byte as they always did.

/// Time on station before an identify order gives up, after the time to get there, s [E]: a camera
/// takes seconds a look (docs/SENSING.md: 3.4 s to detect, 1 s more to identify, a second to
/// settle after each slew), so five minutes on station is many looks. Rules 33: the policy's
/// `[watch] identify_on_station_s`.
pub const IDENTIFY_ON_STATION_S: f64 = 300.0;

/// Rules 16: a track is out of view after this long without a detection, s, and from then on
/// a keep-out counts it anywhere it could have got to since its last fix
/// ([`crate::engage::out_of_view`]). The bridge's tracker calls a track stale at the same age
/// (`tracks::model::STALE_S`: the console fades it) and drops it at 180 s (then a lost contact,
/// [`crate::lost`]). Why 10 s: the sensing model draws a detection every second for every sensor
/// in reach, with a probability of at least 0.47 within a sensor's nominal range and 0.94 within
/// half of it, so ten misses in a row (at most 0.16%, and 10^-12 at half range) mean the sensors
/// are not seeing the contact, not a gap between fixes; an AIS report comes every 2 to 10 s
/// underway. Below it a contact that turned back unseen is at most about twice its speed times
/// the gap off its course (100 m at 5 m/s), which the regions along its course already allow for
/// (their acceleration-noise term is that size 40 s ahead); above it nothing bounds where it went
/// but its speed. Until the drop the track is in the picture and the tracker would say nothing of
/// a turn it has not seen (the review of murmur-533: a ferry steaming away, unseen 170 s, let a
/// nomination through that the same ferry unseen 182 s, lost, blocked). Rules 33: the policy's
/// `[watch] stale_s`.
pub const OUT_OF_VIEW_S: f64 = 10.0;

/// Rules 30: how fast the water carries a vessel that makes no way, m/s: the Gate's spring current
/// (GUESS; tidewater's field at the Gate reaches 2.3 m/s). Rules 33: the policy's `[rules.silent]
/// drift_mps.surface`.
pub const SURFACE_DRIFT_MPS: f64 = 2.5;

/// Rules 30: how fast the water carries a submerged vehicle that makes no way, m/s: the surface's
/// (the spec's 1.0 GUESS was too little for the headless check: the simulator's water runs the
/// surface current at every depth, docs/HEADLESS.md 17.4, and a vehicle near the surface meets
/// it). Rules 33: `[rules.silent] drift_mps.subsurface`.
pub const SUBSURFACE_DRIFT_MPS: f64 = SURFACE_DRIFT_MPS;

/// Rules 30: how fast the wind carries an airship that makes no way, m/s (GUESS; the Gate's world
/// draws 3 to 8 m/s at the bridge). Rules 33: `[rules.silent] drift_mps.air`.
pub const AIR_DRIFT_MPS: f64 = 10.0;
