# Engagement scenarios (Phase E)

Every invariant of the engagement decision chain has a scenario here that tries to break
it. Each file describes a small world, a timeline of actions and the designed outcome; the
runner plays it against the real decision engine (`crates/murmur-decide`, with the repo's
`sim/policy.toml`, `sim/platforms.toml` and `sim/areas/`), steps engagements every second
as the bridge does, replays the audit log, and, when the file names a `tamper`, attacks the
log the way someone with write access would (editing events and recomputing the whole hash
chain) and requires replay to catch it.

```
cargo run -p murmur-decide --bin murmur-scenarios -- sim/scenarios/engage            # table
cargo run -p murmur-decide --bin murmur-scenarios -- --json sim/scenarios/engage     # one JSON object per line
cargo run -p murmur-decide --bin murmur-scenarios -- --seed 7 --noise 3 --log-dir logs/engage sim/scenarios/engage
cargo run -p murmur-decide --bin murmur-scenarios -- --rules 6 sim/scenarios/engage   # what rules 6 did
cargo run -p murmur-decide --bin murmur-scenarios -- --reaches physical sim/scenarios/engage   # the physical model's sensor reaches
```

The assessment's coverage reads each sensor's `range_m` (the legacy sensing model's reach, and
every designed end's) unless `--reaches physical` (`RunOptions::reaches`) gives the facts the
catalog's `physical_reach_m`, as the bridge's facts carry them in a run on the physical model
(bd murmur-1u4, docs/SENSING.md "The decision engine's reaches"). A scenario written for the
physical reaches says so itself (`reaches = "physical"`, rules 31's identify scenarios: a camera's
reaches by size class are the physical model's), whatever the option. From rules 32 (bd
murmur-ahss) the engine reads a sensor with a physical reach at its reach under the conditions now,
the vehicle fact `reach_now` the bridge computes from the physical model where the vehicle is: a
scenario on the physical reaches states it for its vehicles (`[[vehicle]]`'s `reach_now`, a
`vehicle` step's to change it; the figures from `cargo test --release -p murmur-bridge --bin
murmur-bridge scenario_reach_figures -- --ignored --nocapture`). Without one the engine knows no
such reach (an identify order cannot be evaluated, an assessment counts no cover from it), so
`--reaches physical` on scenarios that state none shows the clear-day reaches with `--rules 31`.

Exit status 0 when every scenario ended as designed (and replayed, and any tamper was
caught), 1 otherwise, 2 on a load error. `cargo test -p murmur-decide --test engage` runs
them all too.

The effect is an abstract event: a point, a radius, a time. Nothing here models a weapon.

## For a harness

- Library: `murmur_decide::scenario::{load, files, run, run_file, RunOptions, ScenarioResult,
  tamper}`. `run_file(path, repo_root, &RunOptions { seed, track_noise_m })` returns a
  `ScenarioResult` (serializable; the audit log text is in `log`, not serialized).
- Batch: the run is deterministic for a file and options. `RunOptions::seed` with
  `track_noise_m > 0` adds seeded Gaussian noise (metres, 1-sigma) to every track estimate
  each second, so one file becomes a family of seeded runs.
- Result fields: `file`, `title`, `invariant`, `attack`, `ok`, `failures`, `expect`,
  `final_state`, `final_reason`, `failed_rules` (the checks that failed at the end: the
  submission's for a rejection, the picture watch's for a void or abort), `assessment`,
  `refusals` (engine refusals met along the way), `fallback` (the one commanded),
  `replay_ok`, `replay_events`, `replay_mismatches`, `tamper`, `tamper_detected`,
  `tamper_mismatches`.
- No physics and no truth process: contacts move along their velocity and are detected every
  second unless silenced (with `--noise`, each detection is the truth plus independent
  Gaussian noise, not a random walk; with `--vnoise`, each velocity estimate carries an
  error correlated over about 10 s, as a tracker's smoothed velocity wobbles); the engaging platform flies straight to the effect point at its
  catalog cruise speed once dispatched and follows its fallback when stopped. After the
  effect the target stops being detected (`after_effect = "out_of_play"`) or not
  (`"persists"`). The live stack runs the same engine against stand-ins and the contacts
  process.
- Rules 6: a dispatched platform answers its lost-link preload a second after the dispatch
  (`preload`, below), as the bridge reports it; with it confirmed and its link cut for longer
  than the preloaded timer it follows the preloaded action on its own
  (`lost_link_action`); once the engagement ended and it can hear us, its own values go back
  (a restore; from `restore_after_s` on, when the file sets one). Result fields:
  `lost_link_action`, `preload_reports`.
- Rules 5 and 7: an approved `task` goes as on the bridge: dispatched if the link reaches its
  vehicles, else queued and offered again every second (re-checked, then dispatched, sent to
  the inbox or voided; expired at its deadline). From rules 7 a queued one supersedes an
  engagement on its vehicles as a dispatched one does, and a mission (an orbit) waits for its
  platform's own lost-link values to be back. `RunOptions::rules_version` (`--rules N`) runs a
  file under older rules (its log replayed with them) to show what they did.
- Rules 9: an `act` step is a hostile act our sensors observed (its author a track, or a
  point we hold no track of; its observed position the author's, plus a scripted `offset`,
  plus with `act_noise` seeded noise of its sigma). The engine attributes it and declares by
  rule or flags; every second the runner also calls `Engine::step_identification` (the
  proximity flag) before stepping engagements. `RunOptions::identification` changes the
  policy's `[identification]` keys it names (`--ident on|off`, `--threshold X`,
  `--nominate-unconfirmed`, `--corroboration none|second_observation`);
  `RunOptions::proxy` (`--proxy`) is the evaluation's scripted reviewer, with its own
  procedure over what the console shows (`scenario::console_pick`), and
  `RunOptions::nominator` (`--nominator`) nominates every declared track and has two signers
  sign it. Result fields: `ident_reports`
  (one line per identification event), `ident` (each with the truth behind it: the track's
  `truth`, the act's real author), as `eval/ident.py` reads them.
- Rules 30: a dispatched order's vehicles say they started it, as the bridge's do (`op.state`
  starting, logged ahead, then active), so that what may be running on one that falls silent
  (a `vehicle` step's `silent`, or `lost_for_s`) reads it (`murmur_decide::silent`); under older
  rules the runner reports no op, as before. A `[[vehicle]]`'s `lost_link` is its own failsafe as
  the bridge would read it.
- Rules 16: a `drop_track` step is the tracker's drop (as the bridge's after 180 s without a
  detection; the run logs that age, `run.started`'s `track_drop_s`, and on a clock that steps
  back a step that comes sooner by the run's clock waits until it has passed; one still
  waiting at the end, a track that never went silent or not long enough, fails the run): the engine is
  handed the track's facts then and its AIS (`track.dropped`) and the runner stops carrying it; every second the runner also calls `Engine::step_lost` (a
  track carrying a lost contact's MMSI raises a claim, `lost.claimed`; a confirmation the
  picture no longer bears out is reinstated, `lost.reinstated`), as the bridge's loop does; a
  `confirm_claim` step is an operator's confirmation (`track.reacquired`). A track whose
  `silent` step is 10 s or more old counts at its domain's top speed from its last fix
  (`engage::OUT_OF_VIEW_S`). Result fields: `claimed` (`t=<s> T-0005 claims to be T-0002`,
  with what blocks it), `reacquired` (`t=<s> T-0002 is T-0005 again, confirmed by op-2`),
  `reinstated` (`t=<s> T-0002 counts again: why`).

- Rules 34 (being built: `min_rules = 34`; docs/SWARMS.md, bd murmur-4nik): an order to more
  than one vehicle is a group order (its `task` step's optional `group`: `spread`, `min_members`,
  `alt_m`, `wait_s`, `reform`, `lanes`, `lane_spacing_m`). Once approved the runner sends it out as
  the bridge will (`Engine::dispatch_group`): its member orders, each dispatched or queued on its
  own and flown as any order; a `finish` step ends the newest order under way on one vehicle, done
  or failed, as the bridge reports a tasking's end, and a group ends once each member order it
  sent has. Each type's group-order figures are on its facts (`Catalog::formed`); a file's
  `water_depth_m` sets the runner's water (20 m) for a sub's `depth` rule. Tampers
  `move_member_slot`, `member_without_group`, `admit_raised_member`, `move_member_station`.
  Stage 7, the kind `transit` (`route`, one point or more): flown along its route once and held at
  its end; the runner flies a transit's vehicle along its route and ends the order done once it
  stops at the end (the bridge's op ends there, the vehicle holding by its type); a group transit
  spreads `stations` (the default) or `block`, and never re-forms.

## Format

```toml
title = "A neutral boat enters the keep-out after approval"
invariant = "keep_out_tracks"    # what it attacks: a rule id, or two_person,
                                 # agent_cannot_authorize, signed_picture,
                                 # engagement_in_play, abort, assessment, audit
attack = "a neutral boat closes on the effect point after approval"
expect = "aborted"               # the designed end of the last nomination: rejected, voided,
                                 # aborted, done, pending_approval, expired
expect_rule = "keep_out_tracks"  # optional: a check that must have failed at the end
expect_assessment = "ceased"     # optional (done): persists, ceased, unknown
expect_refusals = ["may not approve it"]   # optional: substrings of engine refusals met
expect_reset = true              # optional: the signatures must have been dropped at least once
tamper = "suppress_void"         # optional log attack replay must catch: proposer_approval,
                                 # agent_approval, suppress_void, edit_snapshot,
                                 # drop_signature, edit_effect, strip_approvers,
                                 # drop_reset, agent_order_auto, drop_ack, loosen_policy,
                                 # and rules 6: drop_preload, forge_preload,
                                 # drop_verify, drop_acceptance, acceptance_by_agent,
                                 # acceptance_without_authority, hide_self_accepted,
                                 # and rules 7: drop_restore_owed,
                                 # and rules 9: drop_act, declare_flagged_act,
                                 # declaration_by_another_operator, drop_revocation_stop,
                                 # confirm_in_inputs,
                                 # and rules 12: drop_first_seen, drop_expended_fact,
                                 # drop_standing_clause, write_off_by_another,
                                 # and rules 15: unspend_without_authority, drop_may_owe_fact,
                                 # and rules 16: hide_lost_contact, drop_track_dropped,
                                 # forge_lost_acceptance, hide_reinstatement,
                                 # confirm_by_agent, move_drop, redrop,
                                 # and rules 19: drop_behaving_acceptance,
                                 # drop_behaving_lapse, behaving_acceptance_by_agent,
                                 # and rules 20: edit_munition,
                                 # and rules 24: edit_rounds (rules 26: a gun's bursts too),
                                 # and rules 25: drop_damage,
                                 # and rules 26: burst_after_stop,
                                 # and rules 27: move_mine, hide_lay_signature,
                                 # and rules 30: lost_by_agent (a confirmed loss credited to
                                 # an agent), hide_loss_revoked (every revocation of a loss
                                 # cut), redate_loss (a confirmation's report moved 20 s
                                 # later, everywhere the loss appears),
                                 # and rules 32: edit_reach (every sensor's reach now in the
                                 # decisions' inputs made a hundred times as long),
                                 # and rules 33: edit_own_confidence (every own look's
                                 # confidence in the pictures mirrored, c to 1 - c, their
                                 # hashes recomputed), drop_loosened (the policy's named
                                 # loosenings cut from run.started)
                                 # (one with nothing to edit in a run's log reports "not
                                 # applicable")
end_s = 120                      # seconds to run (default: the last step + 10)
after_effect = "out_of_play"     # or "persists"
cease_at_burst = 3               # rules 26, optional: a gun's target leaves play at this burst
                                 # (default 1, its release)
expect_bursts = 3                # rules 26, optional: the bursts the gun fired (the release is 1)
expect_fire_end = "ceased"       # rules 26, optional: how its fire ended: ceased, unknown,
                                 # rounds_out, window_closed, stopped
expect_rounds_left = 482         # rules 26, optional: its rounds left after its last burst
expect_mines_laid = 10           # rules 27, optional: the mines every lay order laid (mine.laid)
preload = "confirm"              # rules 6: how the platform answers its lost-link preload:
                                 # confirm (default), no_answer, mismatch (another action read
                                 # back), never (nothing reported), changed (confirmed, then
                                 # another action at the re-read near the release); a confirmed
                                 # one is read again every 5 s from 30 s before the window opens
expect_lost_link = "Loiter"      # optional: it followed its preloaded action on its own
expect_preload = ["set confirmed", "restore confirmed"]   # optional, in order (also
                                 # "verify confirmed", "verify changed")
expect_stop = "superseded"       # optional: how the last nomination was stopped (superseded,
                                 # system, picture, operator, revoked); superseded also means no
                                 # fallback was commanded
expect_ident = ["declared T-0001 hostile by rule:hostile_act"]   # rules 9, optional: substrings
                                 # of the identification reports, in order
expect_no_ident = ["declared T-0002"]   # rules 9, optional: substrings no report may contain
restore_after_s = 60             # rules 6: the platform answers the restore of its own
                                 # lost-link values only from this second on (default: at once)
expect_closed = ["expended by its own effect"]   # rules 12, optional: debts closed without a
                                 # restore that replay lists (`restores_closed`), in order
expect_nothing_owed = true       # rules 12, optional: nothing owed back when the log ends
expect_unspent = ["un-spent by op-2"]   # rules 15, optional: platforms un-spent that replay
                                 # lists (`unspent`), in order
expect_presumed = ["vehicle hunter-1: presumed lost (struck)"]   # rules 30, optional:
                                 # presumptions of loss and presumptions cleared that replay
                                 # lists (`nodes_presumed`), in order
expect_lost = ["vehicle hunter-1: confirmed lost by op-2"]   # rules 30, optional: confirmed
                                 # losses replay lists (`nodes_lost`: by whom, since when, on
                                 # what evidence, the note), in order
expect_revoked = ["vehicle hunter-1: loss revoked, heard again"]   # rules 30, optional:
                                 # revocations replay lists (`losses_revoked`), in order
expect_reacquired = ["T-0002 is T-0005 again"]   # rules 16, optional: lost contacts an
                                 # operator confirmed as a live track, in order
                                 # (expect_none_reacquired = true: none)
expect_claimed = ["T-0005 claims to be T-0002"]  # rules 16, optional: claims raised, in order
expect_reinstated = ["T-0002 counts again"]      # rules 16, optional: confirmations void
expect_lapsed = ["T-0002 lapsed (velocity)"]     # rules 19, optional: acceptances as behaving
                                 # now that lapsed on their own, in order (`t=<s> T-0002
                                 # lapsed (<kind>): <detail>`; kinds out_of_view, gone,
                                 # position, velocity)
expect_routed = ["routed around land at"]   # rules 22, optional: substrings of the domain and
                                 # geofence checks that routed a leg ("routed around ...") or
                                 # could not ("no route around ..."), in order
land = [[[-122.49, 37.81], [-122.48, 37.81], [-122.48, 37.82]]]   # rules 22, optional: the
                                 # land as polygons of [lon, lat], in place of the runner's two
                                 # boxes (San Francisco south of 37.800, Marin north of 37.832 west
                                 # of -122.46); the rest of its coverage is water 20 m deep
min_rules = 30                   # optional: the oldest decision rules it is designed for: it
                                 # runs under them when they are newer than the current ones
                                 # (rules being built, docs/NODE-LOSS.md section 8), else under
                                 # the current ones; murmur-scenarios --rules N overrides it
reaches = "physical"             # optional: the sensing model's reaches it is designed for
                                 # (`physical` or `legacy`), whatever --reaches says
# [since.34]                     # optional, while rules are built in stages: what changes in it
# expect = "failed"              # under those rules or newer (a table per version): any of its
# expect_identify = ["..."]      # keys, each replacing the file's whole (a list too,
                                 # `[[since.34.track]]` its tracks), read by a run that decides
                                 # under that version; a run of the current rules reads the file
                                 # as it is (its log as it was). The last stage writes the tables
                                 # into the file and notes what older rules did (rules 33's into
                                 # 75, 108, 111, 112, 117, 118 and 144)
# policy, catalog, areas = "path"  # optional, relative to the repo (defaults: sim/...)

[[carried]]                      # rules 15, optional: an earlier log whose carry failed at
log = "20261006T120000Z-4242.jsonl"   # the start (run.started's carried_from), with why
error = "Permission denied (os error 13)"   # and the vehicles (of this file) its fleet named:
may_owe = ["viper-1"]            # their missions wait and their nominations are refused
                                 # until it is written off

[[carried]]                      # rules 30, optional: an earlier log that was read (no
log = "20261112T120000Z-4242.jsonl"   # error) in which our vehicles were confirmed lost: the
lost = [                         # runner writes it with the engine (each vehicle's loss
    { uid = "hunter-1", by = "op-2" },   # confirmed by `by` on its silence since its last
    { uid = "hunter-2", by = "op-2", callsign = "OTHER 7" },   # report, `lost_for_s` before
]                                # the start, as the vehicle `platform` and `callsign` name,
                                 # default the file's own), carries it over as the bridge does
                                 # and hands it to replay

[[operator]]                     # an operator session; engage = engagement authority
id = "op-2"
engage = true

[[agent]]                        # optional, for agent nominations
id = "agent-planner"

[[vehicle]]                      # one of ours
uid = "blade-1"
platform = "switchblade"         # catalog id
at = [-122.4700, 37.8160]        # [lon, lat]
velocity = [0.0, 0.0]            # east, north m/s (default 0)
battery = 95                     # percent (default 95)
report_interval_ms = 1000        # its link's report interval (default 1000)
alt_m = 7000                     # rules 6: altitude MSL it reports (absent: none, 2D)
lost_link_s = 30                 # its own lost-link timer (default 5): what a preload shortens
lost_for_s = 60                  # lost before the run: silent, last reported this long before
home = [-122.4660, 37.8200]      # where it goes home to (default: where it starts)
callsign = "HUNTER 1"            # rules 23: what the bridge would call it (default: its uid)
lost_link = { action = "rtl", timer_s = 5, source = "read" }   # rules 30, optional: its own
                                 # lost-link failsafe as the bridge reads it (the vehicle fact
                                 # lost_link; a fixed wing's circle_m too); absent: not known
reach_now = { alt_m = 150, visibility_m = 50, sun_elevation_deg = 30, sensors = [
    { type = "eo_ir", detect_m = { surface = 77 }, identify_m = { surface = 166,
      size_class = { small_air = 160 } }, thermal = true }] }
                                 # rules 32, optional: its sensors' reach under the conditions
                                 # where it is, as the bridge computes it (the vehicle fact
                                 # reach_now, stamped with each report; a vehicle step's
                                 # reach_now replaces it); absent: not known

[[track]]                        # a perception track (not truth)
id = "T-0001"
at = [-122.4950, 37.8120]
velocity = [0.0, 0.0]
sigma_m = 15                     # 1-sigma now (default 15)
affiliation = "suspect"          # unknown, suspect, hostile, neutral, assumed_friend
declared_by = "op-1"             # optional: an operator's declaration (rules 9: made through
                                 # the engine's identification ledger before the timeline)
truth = "hostile"                # rules 9, optional: what its contact really is (labels only;
                                 # nothing the engine sees)
affiliation_confidence = 0.8     # how confidently the tracker holds its affiliation (default 0.8)
identity = "small_fast_boat"
identity_confidence = 0.9        # default 0.9
confidence = 0.9                 # existence, default 0.9
domain = "surface"               # default surface
age_s = 0                        # seconds since the last detection at the start
silent = false                   # true: no detections at all
sources = ["3:radar"]
alt_m = 0                        # rules 6: its estimated altitude (absent: unknown, 2D)
no_velocity_estimate = false     # rules 6: the tracker has no velocity for it yet
alt_age_s = 0                    # rules 6: its altitude was last ranged this long before the
                                 # start and never again (the radar lost it; the others keep
                                 # its position fresh); sources radar and ais are the ranging ones
ais = { mmsi = "366999501", name = "MV BAY TOUR", ship_type = "passenger" }   # rules 16,
                                 # optional: the AIS identity its contact broadcasts
tracked_s = 600                  # rules 12, optional: the tracker started it this long before
                                 # the start (`first_seen_t`); it has an average velocity once
                                 # tracked 45 s with a velocity, and until then it is young
                                 # (absent: 600, an established track; a `track` step that
                                 # makes one: 0, it appears then)
velocity_sigma_mps = 3.0         # rules 14 and 18, optional: its velocity's 1-sigma error as the
                                 # tracker reports it (`velocity_sigma_mps`; absent: none given,
                                 # the policy's `track_velocity_sigma_mps` stands in)

[[step]]
at_s = 0
action = "declare"               # track, affiliation, by
track = "T-0001"
affiliation = "hostile"
by = "op-1"
```

Actions (each `[[step]]` has `at_s` and `action`):

| action | fields | what it does |
|---|---|---|
| `declare` | `track`, `affiliation`, `by` | an operator's declaration (logged `operator.action track.declare`; from rules 9 `ident.declared` through the engine's ledger) |
| `act` | `by_track` or `at` [lon, lat] (an author we hold no track of), `against_vehicle` or `against_track` (neither: unknown), `sensors` (each further observer's uid prefix gives a fix of its own), `sigma_m` (default 20), `offset` [east, north] m | rules 9: a hostile act our sensors observed; the engine attributes it and declares its author by rule or flags it |
| `confirm_declaration` | `by`, `track` | rules 9: an operator confirms the rule declaration in force |
| `revoke_declaration` | `by`, `track`, `note` | rules 9: an operator revokes the declaration in force; the engagements on the track stop first |
| `resolve_flag` | `by`, `track`, `flag` (optional: its number; default the newest on the track), `resolution` (`confirmed`, `dismissed`) | rules 9: an operator resolves a review flag |
| `agent_declare` | `agent`, `track` | rules 9: an agent "tries" to declare: its port has no such method; recorded, nothing may change |
| `nominate` | `by` or `agent` (+ `rationale`), `platform`, `track`, `radius_m`, `fallback` (`abort`, `return`, `loiter`), `window_s`, `accept` (rules 6: vehicles the operator accepts as lost, each since its last report), `accept_lost` (rules 16: lost contacts the operator accepts, each since its last fix), `accept_behaving` (rules 19: live tracks the operator accepts as they are behaving now, each with its facts then) | a nomination; becomes "the last nomination" the other steps act on |
| `lay` | `by` or `agent` (+ `rationale`), `platform`, `points` (its line, `[lon, lat]` each), `count`, `live_h` (mandatory), `weapon` (the mine by name; absent: the platform's first) | rules 27: a lay order; becomes "the last nomination": `approve` signs it as an engagement's signers do, with no picture to look at; dispatched, the runner drives the layer along its transit's waypoints and the line once, steps the lays every second, and finishes the order at the line's end |
| `accept` | `by`, `uid` or (rules 16) `track` or (rules 19) `behaving`, `since_s` (optional: another report or fix than the last, which the engine refuses) | rules 6: an operator with engagement authority accepts a lost vehicle (rules 16: a lost contact; rules 19: a live track as it is behaving now) for the last nomination while it waits with no signature (without it: refused) |
| `revoke` | `by`, `uid` or (rules 16) `track` or (rules 19) `behaving` | rules 6: any operator withdraws an acceptance from the last nomination (pending or under way) |
| `drop_track` | `track` | rules 16: the tracker drops it (its facts and AIS now go to the engine, `track.dropped`); from then on it is a lost contact |
| `confirm_claim` | `by`, `track` (the lost contact), `by_track` (the live track claiming to be it) | rules 16: an operator confirms a claim on a lost contact (refused while anything contradicts it) |
| `agent_confirm` | `agent`, `track` | rules 16: an agent "tries" to confirm a claim: its port has no such method; recorded, nothing may move |
| `agent_accept` | `agent`, `uid` or (rules 19) `behaving` | an agent "tries" to accept a lost vehicle (rules 19: a live track as behaving now): its port has no such method; recorded, nothing may move |
| `look` | `by` | an operator with engagement authority asks for the picture (a picture token: the plan and the track now); their next `approve` quotes it |
| `approve` | `by` | a signature: through `approve_engagement` if the session has engagement authority (quoting the token from their last `look`, or one asked for now), else `approve` (which the engine refuses for an engagement) |
| `approve_task` | `by`, `acknowledge` | an operator approves the last `task`; `acknowledge` accepts that its dispatch stops an engagement in play |
| `task` | `by` or `agent` (+ `rationale`), `task` (`hold`, `rtl`, `orbit` with `center` [lon, lat] and `radius_m`, `patrol` with `route` [[lon, lat], ...], rules 21 `identify` with `track` and an optional `standoff_m`, rules 34 `transit` with `route`), `uids` | another order on our vehicles; once approved, dispatched if the link reaches them, else queued (rules 5), superseding any engagement on them (no fallback commanded; when queued, from rules 7); an identify order is never queued |
| `stall` | `seconds` | the loop that holds the picture stops for that long (no engagement step) |
| `reject` | `by` | an operator rejects it |
| `abort` | `by` | any operator stops it; the fallback is commanded |
| `agent_approve` | `agent` | an agent "tries" to approve: there is no such method on its port, so it is recorded and the intent must not move |
| `track` | `id` and any of `at`, `velocity`, `affiliation`, `declared_by`, `undeclare`, `identity`, `identity_confidence`, `sigma_m`, `domain`, `silent`, `velocity_error`, `alt_m`, `tracked_s`, `ais` (rules 16), `velocity_sigma_mps` (rules 18), `level` and `level_by` (rules 21), `level_conflict` (rules 22) | a track changes (or appears); `velocity` is how the contact really moves, `velocity_error` how far the tracker's estimate is off it; `tracked_s` (rules 12) when one this step makes was started (0: a new track); `velocity_sigma_mps` the velocity's 1-sigma error the tracker now reports; `level` (`detect`, `recognise`, `identify`) how specific its identity now is and `level_by` the sensors whose looks identified it (an identify order ends on it; from rules 22 only on one of our own sensors, not `ais`, after the order went out; from rules 23 only on the vehicle sent's own, its uid before the colon), `level_conflict` the tracker's identity conflict on it then |
| `write_off` | `by`, `uid` or `log`, `note` | rules 12: an operator writes off the lost-link values owed back to one of our vehicles, saying why (`restore.closed`); rules 15: or an earlier log whose carry failed (`log`) |
| `unspend` | `by`, `uid`, `note` | rules 15: an operator with engagement authority un-spends a platform spent by its own effect, saying why (`platform.unspent`), once the engagement that spent it ended and it was heard after that and is not silent; without engagement authority it is refused (a scenario has no credentials: no step-up) |
| `struck` | `uid`, `text` (optional) | rules 30: one of ours reports itself struck (`vehicle.event` struck, as the bridge logs a hit): evidence that presumes it lost once it is silent, within 30 s of its last report |
| `confirm_lost` | `by`, `uid`, `since_s` (optional), `note` (optional) | rules 30: an operator with engagement authority confirms one of ours lost on its silence since its last report (or since `since_s`, scenario seconds: the engine refuses another), with a note when nothing presumes it lost (`node.lost`); its waiting orders are voided, its dispatched ones end, the values owed it close; without engagement authority it is refused (a scenario has no credentials: no step-up) |
| `revoke_lost` | `by`, `uid`, `note` | rules 30: any operator revokes the loss of one of ours, saying why (`node.loss_revoked`, cause operator) |
| `finish` | `uid`, `failed` (optional: why) | rules 34: the newest order under way on that vehicle ends, done or failed, as the bridge reports a tasking's end (its group, if it has one, ends once every member order it sent has) |
| `vehicle` | `uid` and any of `at`, `velocity`, `silent`, `report_interval_ms`, `nav` (`gnss`, `dead_reckoning`), `gps_fix` (3: a 3D fix), `gps_glitch`, `circling`, `reported_circle`, `alt_m`, `damage` (rules 25: `{ mobility = true }`, `{ firepower = true }`) | one of ours changes; `silent` cuts its link (both ways: it hears no GCS either); `damage` is a damage report of its own (never healed) |
| `expect` | `state`, `rule`, `concurrences`, `task_state`, `task_reason`, `weapon`, `identify`, rules 34 `sent`, `waiting`, `excluded`, `member_states`, `warning`, `detail` with `detail_rule`, `at` | check the last nomination now (`at`: each named vehicle within 5 m of its [lon, lat]; `task_state`: the last `task` step's intent; `task_reason`: a substring of why it is in that state; `weapon`: of the last nomination's weapon check; `identify`: of the last `task` step's identify check, passed or failed; rules 34, the last `task` step's group order: the uids its last check sent, kept waiting and left out, each list whole; `member_states`, each member's member order's state by uid, `none` for no order; `warning`, a substring of a warning on its checks; `detail`, of a check of `detail_rule`'s) |

Approved engagements are dispatched at once (as the bridge does). Every second the runner
calls `Engine::step_engagements` (hold the picture, release, observe, assess) and, from rules
21, `Engine::step_identify` (an identify order's orbit moved with its track, flown at once; its
look ended, and the order sent afterwards taken at once, so the order ends then). A file with
no nomination ends on its last `task` (`expect`), and `expect_identify` lists what its identify
orders did, in order (substrings of `t=<s> <id> recentred to [lon, lat]`, `<id> held: ...`,
rules 23 `<id> meanwhile: ...`, rules 33 `<id> named: ...`, `<id> ended done: ...`). From rules 33
a track the file names without a `level` is at its label's (a size class's name `detect`, a
family's `recognise`, a kind's `identify`, as the bridge's tracker gives every named track one;
none for a label of none of them), which the engagement's identity check reads; and each look of
`level_by` at recognition or identification is that vehicle's own look, its sensor the label's
after the colon, its confidence the track's identity confidence then. From rules 30 the runner also calls
`Engine::step_nodes` every second, before the engagements' step, as the bridge's tick does: a
loss whose vehicle is heard again is revoked, a presumption cleared, a silent vehicle with
logged evidence presumed lost.

## The set

| file | invariant | attack | designed end |
|---|---|---|---|
| 01-nominal | audit | the approval's snapshot edited afterwards | done (ceased); tamper caught |
| 02-proposer-approves | two_person | proposer signs; one approver signs twice; log forged to name the proposer | refusals; done; tamper caught |
| 03-agent-authorize | agent_cannot_authorize | an agent approves; an operator without authority approves; log forged | refusals; done; tamper caught |
| 04-neutral-enters-keepout | keep_out_tracks | a neutral boat closes in after approval; the void suppressed in the log | aborted; tamper caught |
| 05-target-reclassified | target_identity | identity changes after approval | aborted |
| 06-target-undeclared | target_declared | declaration withdrawn after approval | aborted |
| 07-target-moves | track_deviation | the target gets under way after approval | aborted |
| 08-link-cut | platform_link | the platform (a USV, rules 6) goes silent mid-engagement | aborted, fallback loiter; it holds where it is on its own (its preloaded FS_ACTION 6, its 30 s timer shortened to 5 s); its own values back when the link returns |
| 09-friendly-sub-near-target | keep_out_friendly | our AUV 150 m from the target | rejected |
| 10-friendly-sub-closes | keep_out_friendly | our AUV heads in after approval | aborted |
| 11-stale-track | target_recency | nominate on a 30 s old track | rejected |
| 12-outside-zone | engagement_zone | target outside every engagement zone | rejected |
| 13-no-strike | no_strike | effect circle reaches the bridge | rejected |
| 14-suspect-not-declared | target_declared | an inferred suspect | rejected |
| 15-operator-abort | abort | abort en route | aborted, fallback return |
| 16-target-persists | assessment | detected after the effect; effect point edited in the log | done (persists); tamper caught |
| 17-unarmed-platform | platform_armed | an unarmed USV | rejected |
| 18-drop-signature | two_person | a signature deleted from the log | done; tamper caught |
| 19-window-too-short | reach | a window the platform can't make | rejected |
| 20-policy-without-engagement-tier | two_person | a policy file without the engagement tier (one approver); the log forged to say one was enough | 1 of 2 after one signature; done; tamper caught |
| 21-picture-moves-between-signatures | signed_picture | a signer quotes an expired picture; the target moves 880 m between signatures; a signer quotes a picture of the dropped plan; the reset cut from the log | refusals; first plan dropped; done; tamper caught |
| 22-agent-order-on-engaging-platform | engagement_in_play | an agent's hold, then rtl, on a platform with an engagement pending, then dispatched; an operator's hold; the log forged to make the agent's hold auto | the agent's orders wait for a human; the operator's supersedes (aborted, no fallback); tamper caught |
| 23-picture-gap | picture_gap | the loop that holds the picture stops for 10 s | aborted (picture_gap), fallback return |
| 24-strip-approvers | two_person | the logged policy and decision stripped of `approvers`, a signature dropped | done; tamper caught |
| 25-plan-slides | signed_picture | the second signature 237 s after the first on a track running at 10 m/s | the plan stops holding (reach); a new plan signed by both; done |
| 26-target-reverses | signed_picture | the target turns around just before the second signature | the plan dropped at once; a new plan; done |
| 27-transient-reclassification | signed_picture | the identity flips to ferry and back while a signature waits; the reset cut from the log | plan dropped; signed again; done; tamper caught |
| 28-look-before-reclassification | signed_picture | a signature quotes a picture taken before the target was reclassified; the logged policy loosened | refused; pending; tamper caught |
| 29-approve-order-on-engaging-platform | engagement_in_play | an agent's hold on the engaging platform approved without acknowledging it stops the engagement; the acknowledgement cut from the log | refused, then acknowledged: voided (superseded); tamper caught |
| 30-gnss-lost-en-route | platform_nav | the platform falls back to dead reckoning after dispatch | aborted (platform_nav), fallback return |
| 31-velocity-estimate-wobbles | signed_picture | the tracker's velocity estimate is 0.8 m/s off between a look and a signature, and while a signature waits | the plan holds; done |
| 32-second-engagement-on-platform | platform_free | a second nomination on a platform whose engagement is under way | rejected (platform_free) |
| 33-gps-glitch-en-route | platform_nav | the filter flags the GPS as glitching after dispatch, keeping its absolute-position flag | aborted (platform_nav) |
| 34-first-token-goes-stale | signed_picture | the target turns between the first signer's look and signature | refused ("look again"), not voided; done |
| 36-friendly-just-outside-keep-out | keep_out_friendly | our AUV closes on the effect point, predicted a few metres outside the keep-out at the effect time | aborted (keep_out_friendly: counted with its prediction uncertainty) |
| 37-friendly-circling-near-target | keep_out_friendly | one of our fixed wings circles 800 m from the effect point, its heading sweeping across it every lap | done, ceased (rules 3: predicted on its circle, not along its heading) |
| 38-friendly-off-its-reported-circle | keep_out_friendly | a fresh circle fact 1500 m north for a friendly 50 m from the effect point | rejected (keep_out_friendly: the circle doesn't hold its position, so its position counts) |
| 39-friendly-on-its-circles-slack-band | keep_out_friendly | a friendly reported inside the keep-out on the slack band of its circle, its position 6 s old at the release | aborted (keep_out_friendly: the disc includes the slack, never farther than where it was reported) |
| 35-real-course-change-while-waiting | signed_picture | a holding target gets under way at 6 m/s while a signature waits | the plan dropped at once; a new plan; done |
| 40-plane-cannot-preload-loiter | fallback_preload | a fixed wing with fallback loiter, which ArduPlane can't take on a lost link | rejected (fallback_preload) |
| 41-preload-no-answer | fallback_preload | the platform never answers its preload; the log forged to show it confirmed | aborted at once, fallback return; tamper caught |
| 42-preload-mismatch | fallback_preload | the platform reads back another lost-link action (glide for RTL) | aborted at once; its own values put back |
| 43-preload-never-reported | fallback_preload | no preload is ever reported | held but never ready (fallback_preload); aborted late (engage_window), no effect |
| 44-preload-dropped-from-log | fallback_preload | the preload report cut from a released engagement's log | done (ceased); tamper caught |
| 45-fast-ferry-far | keep_out_tracks | a 20 m/s ferry crosses 1.4 km from the effect point near the effect time | done (rules 4 blocked it: 2-sigma 1.28 km) |
| 46-fast-ferry-near | keep_out_tracks | the same ferry crosses 500 m away | rejected (keep_out_tracks) |
| 47-track-without-velocity | keep_out_tracks | a protected track with no velocity estimate 2.5 km away | rejected (anywhere it reaches at 25 m/s) |
| 48-high-friend-above | keep_out_friendly | our MALE aircraft circles over the target at 7,000 m | done (clear by altitude) |
| 49-low-friend-above | keep_out_friendly | our quadcopter hovers 120 m above the water next to the target | rejected (within the vertical keep-out) |
| 50-friend-altitude-unknown | keep_out_friendly | the MALE aircraft without its altitude reported | rejected (2D) |
| 51-target-altitude-unknown | keep_out_friendly | the MALE aircraft at 7,000 m over a target whose altitude is unknown | rejected (2D) |
| 52-high-aircraft-track-above | keep_out_tracks | a neutral airliner passes over the effect point at 3,000 m | done (clear by altitude) |
| 53-lost-friendly-blocks | keep_out_friendly | nominate a minute after one of ours went silent 2 km away, drifting with no order of ours on it (rules 30: nothing places it, so its top speed; still, it would count holding there) | rejected; the detail says since when |
| 54-lost-friendly-accepted | keep_out_friendly | the operator accepts the lost quadcopter; the acceptance cut from the log | done (ceased); tamper caught |
| 55-agent-cannot-accept | keep_out_friendly | an agent nominates and tries to accept the lost quadcopter | refusals; rejected |
| 56-lost-friendly-reappears | keep_out_friendly | the accepted quadcopter reports again 150 m from the point; the acceptance credited to an agent in the log | aborted (keep_out_friendly); tamper caught |
| 57-acceptance-needs-engagement-authority | keep_out_friendly | the review's probe A1: an operator without engagement authority accepts a lost quadcopter after a signer looked | the acceptance refused; voided (keep_out_friendly) |
| 58-token-before-acceptance-is-refused | signed_picture | a signer signs on a token from before another operator's acceptance; the log credits the acceptance to a session without engagement authority | refused (look again), then signed on a fresh token; done (ceased); tamper caught |
| 59-reset-clears-acceptances | keep_out_friendly | the frozen plan an acceptance was given for is dropped; a signer signs the new one | reset; voided (keep_out_friendly: the acceptance went with the plan) |
| 60-revoked-acceptance-stops-the-engagement | keep_out_friendly | an operator without engagement authority revokes the acceptance while the engagement is under way | aborted (keep_out_friendly), fallback |
| 61-passive-aircraft-altitude-stays-2d | keep_out_tracks | the airliner of 52 over the effect point, its altitude from EO/IR only | rejected (2D) |
| 62-preload-changed-before-release | fallback_preload | the lost-link action changed on the platform after the set; the re-read cut from the log | aborted at the re-read, fallback, own values back; tamper caught |
| 63-ground-target-stays-2d | keep_out_friendly | the review's probe 8: our quadcopter 400 m MSL over a ground target logged at 0 m | rejected (2D) |
| 64-stale-radar-altitude-stays-2d | keep_out_tracks | the second review's probe: an aircraft 100 m from the point whose radar altitude is five minutes old while EO/IR keeps its position fresh | rejected (2D) |
| 65-acceptor-signs-recorded | signed_picture | the operator who accepted a lost vehicle signs; the log hides that they did both | done (ceased), `self_accepted` on the signature; tamper caught |
| 66-airship-cannot-preload | fallback_preload | 17 with the small airship, whose firmware has no lost-link action (Blimp 4.8 can only disarm) and which carries no effect | rejected; its committed log pins the rules 6 wording of the refusal |
| 67-queued-order-supersedes-engagement | engagement_in_play | rules 7: an operator's hold queued for the USV whose link is cut mid-engagement | aborted, superseded by the hold, no fallback commanded; the USV holds on its preloaded lost-link action; when the link returns its own values go back and the hold goes out |
| 68-mission-waits-for-restore | restore_owed | rules 7: an orbit ordered after an abort while the USV's own lost-link values are not back (its restore answered 25 s late); the fact stripped from the log | the orbit waits in the queue and goes the second the restore confirms; tamper caught |
| 69-restore-never-confirmed | restore_owed | rules 7: the USV never confirms its own values back | the orbit waits, then goes back to a human after 120 s saying why; a hold goes out at once |
| 70-mission-refused-without-queue | restore_owed | rules 7: 68 under a policy that never queues an orbit (`[queue.kind.orbit] expire_s = 0`); the fact stripped from the log | the orbit is voided, saying why, not sent over the engagement's lost-link action; given again after the restore it goes at once; tamper caught |
| 72-hostile-act-declared-by-rule | rule_declaration | rules 9: a clear act on our USV by the only track there; the act cut from the log | declared by rule, confirmed by op-2, done (ceased); tamper caught |
| 73-ambiguous-attribution-flagged | rule_declaration | rules 9: an act between two tracks 15 m apart on a 60 m fix; an agent tries to declare; the flag forged into a rule declaration | flagged (attribution), nothing declared, the nomination rejected (target_declared); tamper caught |
| 74-neutral-alongside-hostile-flagged | rule_declaration | rules 9: a ferry 150 m alongside a declared hostile, an act landing on the ferry; an agent nominates the ferry; the operator's declaration credited to another operator in the log | the ferry flagged (proximity, raised to suspect; the act: conflict), never declared; rejected; tamper caught |
| 75-rule-declaration-revoked-mid-engagement | rule_declaration | rules 9: an untracked author beside a fishing boat (a false rule declaration), nominated before a confirmation, revoked en route; the stop cut from the log | aborted (revoked), fallback return; tamper caught (rules 33: the boat identified as one, a kind; up to rules 32 the radar's `small_craft`, which rules 33 refuse at the nomination) |
| 76-rule-declared-bystander-protected | keep_out_tracks | rules 9: a rule-declared, unconfirmed hostile 250 m from another engagement's target; the confirmation planted in an earlier picture in the log | rejected (keep_out_tracks) until confirmed, then done; tamper caught |
| 77-revoked-bystander-stops-engagement | keep_out_tracks | rules 9 (review): a confirmed rule declaration exempts a bystander from an engagement's keep-out; op-3 revokes it mid-engagement; the stop cut from the log | aborted in the revocation's step, cause revoked (keep_out_tracks), fallback; tamper caught |
| 78-revoked-bystander-before-first-picture | keep_out_tracks | rules 9 (second review): as 77, but op-3 revokes the bystander's declaration in the second the engagement is approved, before its first picture; the stop cut from the log | voided in the revocation's step, cause revoked (keep_out_tracks), not a later picture failure; tamper caught |
| 71-refused-mission-supersedes-engagement | engagement_in_play | rules 7: 70's policy, the orbit ordered while the engagement is still under way, link up; the fact stripped from the log | the orbit is voided and in the same step the engagement is aborted, superseded by it, no fallback, its reason saying so; given again before the restore it is refused saying the restore is pending; after it, it goes at once; tamper caught |
| 79-young-track-blocks-until-tracked | keep_out_tracks | rules 12: a ferry first detected a second before the nomination, 1.2 km north of the effect point and heading away (no average velocity yet); its first detection dropped from the log | rejected (keep_out_tracks: anywhere it reaches at 25 m/s), then, tracked 59 s, the same nomination waits for its signers; tamper caught |
| 80-expended-munition-owes-nothing | restore_owed | rules 12: a switchblade's engagement released (the munition spent by its effect); then an orbit and a second nomination for it; the catalog fact dropped from every picture in the log | done (ceased); its values closed right after the release (`restore.closed`, expended), nothing owed at the end; the orbit and the nomination rejected (readiness: spent by its own effect); tamper caught |
| 81-refusal-names-the-order-still-queued | restore_owed | rules 12: 67 under 70's policy: the hold queued for the cut-off USV, then an orbit refused for its own values; the clause cut from the refusal in the log | the orbit voided, its reason naming the hold still queued (`i2 (hold, vehicle viper-1)`); the hold goes when the link returns; tamper caught |
| 82-restore-written-off | restore_owed | rules 12: 69, the USV never confirming its values back; op-1 writes them off; the write-off credited to another operator in the log | the waiting orbit goes at once after the write-off, nothing owed at the end; tamper caught |
| 83-aircraft-may-turn-back | keep_out_tracks | rules 14 (murmur-oho): a light aircraft 2 km north of the effect point, flying north at 50 m/s on a steady course (an average velocity, no manoeuvre seen), its height not ranged | rejected (keep_out_tracks: an aircraft may turn or speed up at any moment), then, 12 km out, the same nomination waits for its signers; rules 13 let the first through (`--rules 13`) |
| 84-spent-munition-unspent | restore_owed | rules 15: 80, then op-1 (no engagement authority) asks to un-spend the switchblade, op-2 un-spends it saying why, its engagement ended (assessed) and the munition still reporting; the un-spend credited to op-1's session in the log | done (ceased); the first orbit rejected (spent), op-1's un-spend refused, op-2's taken: its own values owed again, the next orbit queued for the restore and sent when it confirms, nothing owed at the end; tamper caught |
| 85-nomination-refused-while-a-carry-may-owe | fallback_preload | rules 15: a failed carry at the start whose fleet named the USV; a nomination on it; the fact dropped from the refused decision in the log | rejected (fallback_preload: the log may owe it its values) until op-1 writes the log off, then the same nomination signed, dispatched and aborted, its values restored; tamper caught |
| 86-lost-contact-blocks | keep_out_tracks | rules 16: a fishing boat 400 m from the target goes out of view and is dropped by the tracker before the nomination; the lost contact cut from every decision in the log | rejected (keep_out_tracks: the lost contact, since when, may be accepted; `--rules 15`, the version before, waits for its signers); tamper caught |
| 87-lost-contact-accepted | keep_out_tracks | rules 16: the nominating operator accepts 86's lost contact for the engagement; the tracker's drop cut from the log | done (ceased); tamper caught |
| 88-accepted-lost-contact-reappears | keep_out_tracks | rules 16: an accepted lost tour boat (AIS) is seen again 150 m from the effect point under a new track id; its claim confirmed by op-2; the acceptance credited to an agent in the log | claimed (`T-0005 claims to be T-0002`), confirmed, aborted (keep_out_tracks: the live track, confirmed or not), fallback; tamper caught |
| 89-lost-contact-acceptance-needs-authority | keep_out_tracks | rules 16: op-4 (no engagement authority) accepts a lost contact for a waiting nomination, and nominates accepting it; the drop's last fix moved 6 km in the log | both refused; voided at the next signature (keep_out_tracks: the lost contact's grown reach); tamper caught (the drop's fix differs from the one the nomination had seen) |
| 90-forged-lost-contact-acceptance | keep_out_tracks | rules 16: an acceptance given while the nomination waits, then re-dated in the log to name a fix a minute later than the contact's last, everywhere it appears | done (ceased); tamper caught (not lost since that fix; the checks no longer follow) |
| 91-lost-contact-turns | keep_out_tracks | rules 16: a ferry 600 m from the target, last seen steaming away at 5 m/s, goes out of view and is dropped; its course alone clears the keep-out by the effect; the drop logged twice | rejected (keep_out_tracks: anywhere at 25 m/s, its domain's top speed: it may have turned back or sped up; `--rules 15` waits for its signers); tamper caught (an id dropped twice) |
| 92-out-of-view-before-the-drop | keep_out_tracks | rules 16, the review's case: 91's ferry, nominated 5 s and 170 s after it went out of view, before the tracker drops it | the first waits for its signers (in view: its course), the second rejected (keep_out_tracks: out of view, no detection for 169 s, anywhere at 25 m/s); `--rules 15` waits for its signers |
| 93-claimed-by-a-spoof | keep_out_tracks | rules 16, the review's spoof probe: a track carrying a lost tour boat's MMSI appears 1.9 km from the target 10 min on; an agent tries to confirm it; op-2 confirms it; it turns suspect after dispatch; the reinstatement cut from the log | the first nomination rejected (the claim unconfirmed), the agent's attempt recorded, confirmed, signed and dispatched, then reinstated (`T-0002 counts again: T-0009 is suspect`) and aborted (keep_out_tracks: the lost contact), fallback; tamper caught |
| 94-claim-confirmed-by-an-operator | keep_out_tracks | rules 16: a claim on a lost contact confirmed by op-3 after an agent's attempt; the confirmation credited to an agent in the log | done (ceased); tamper caught (not the operator's session) |
| 95-transit-crosses-land | domain | rules 17: an orbit west of the bridge and a nomination for the armed USV in Richardson Bay, waypoints and effect point on the water, the straight way across the Marin shore | the orbit and the nomination rejected (domain: `transit leg ... crosses land at`, where and how far along; from rules 22 the transit is routed round the shore and the orbit is refused for its straight return home instead, the nomination's way is never routed); `--rules 16` dispatches the orbit and leaves the nomination waiting for its signers |
| 96-route-round-the-point | domain | rules 17: the USV in the strait; an orbit in Richardson Bay across the shore's corner, then a patrol round the point, then a nomination on a clear way | the orbit rejected (domain: its transit; from rules 22 its return home, the transit being routed), the patrol dispatched, the nomination waiting for its signers; `--rules 16` dispatches the orbit |
| 97-velocity-known-to-3-mps | keep_out_tracks | rules 18: a fishing boat whose course passes 700 m from the effect point at the effect time, fixed by a radar only, its velocity known to 3 m/s as the tracker reports it | rejected (keep_out_tracks: 2-sigma 644 m over 100 s, `velocity 300 m at 3.0 m/s`), then, the boat 1.4 km off by the effect, the same nomination waits for its signers; `--rules 17` (every velocity good to the policy's 1 m/s, 2-sigma about 310 m) lets the first through |
| 98-velocity-error-grows-under-way | keep_out_tracks | rules 18: a ferry whose course passes 850 m from the effect point, its velocity known to 0.5 m/s (AIS), signed and dispatched; 20 s on its AIS goes quiet and the tracker reports its velocity known to 4 m/s | aborted (keep_out_tracks: 2-sigma 705 m over 85 s), fallback return; `--rules 17` goes ahead (done, ceased) |
| 99-aircraft-accepted-as-behaving | keep_out_tracks | rules 19: 83's aircraft, 2 km north and flying away, accepted by the nominating operator as it is behaving now; the acceptance cut from the log | pending, signed by two others, done (ceased): the aircraft counts along its accepted path and its course now, not anywhere it could turn or speed up to; tamper caught (`--rules 18`: the nomination refused) |
| 100-behaving-acceptance-lapses-on-a-jump | keep_out_tracks | rules 19: the accepted aircraft's estimate 600 m off its accepted path while no signature has frozen a plan; a signer signs on a token from before the lapse; the lapse cut from the log | lapsed (position) at the next step; refused (look again); accepted again by a person; done (ceased); tamper caught |
| 101-behaving-acceptance-lapses-out-of-view | keep_out_tracks | rules 19: the accepted aircraft goes unseen while a signature waits | lapsed (out_of_view) 10 s on; the plan dropped (reset); voided at the next signature (keep_out_tracks) |
| 102-behaving-acceptance-revoked | keep_out_tracks | rules 19: an operator without engagement authority revokes the acceptance under way; the log credits the acceptance to an agent | aborted (keep_out_tracks), fallback return; tamper caught |
| 103-reset-clears-behaving-acceptance | keep_out_tracks | rules 19: the frozen plan an acceptance as behaving was given for is dropped; a signer signs the new one | reset; voided (keep_out_tracks: the acceptance went with the plan) |
| 104-agent-cannot-accept-behaving | keep_out_tracks | rules 19: an operator without engagement authority, then an agent, accept the aircraft as behaving now | refusals; rejected (keep_out_tracks) |
| 105-behaving-acceptance-lapses-under-way | keep_out_tracks | rules 19: the accepted aircraft turns back toward the effect point under way; the lapse cut from the log | lapsed (velocity) at the next step, aborted (keep_out_tracks) at that step, fallback return, nothing released; tamper caught |
| 106-standoff-missile-released-early | audit | rules 20: an armed MQ-9B at 7,600 m engages a boat running west at 5 m/s with its Hellfire-class missile (425 m/s, 8,000 m slant range); the missile's impact moved 10 s later in the log | released within the missile's slant range some 33 s before the plan's time, struck 18 s later inside its tolerance, the boat out of play at the impact, observed from then on, done (ceased); tamper caught (the flight recomputed from the releasing picture); `--rules 19` releases over the point, no munition |
| 107-standoff-missile-out-of-range | engage_window | rules 20: the MQ-9B at 9,000 m, beyond its missile's slant range even overhead | never released (arrival: the slant range); aborted late (engage_window: the missile could not strike within 60 s of the plan's time), fallback return; `--rules 19` releases over the point |
| 108-identify-a-holding-boat | identify | rules 21: an operator sends PUMA 1 to identify a small boat the radar only recognises; first a radar-only MQ-9B is sent | the MQ-9B refused (no EO/IR); the Puma's order flown as an orbit at its camera's identification standoff, done once a look identifies the boat, the Puma holding (no order to go back to); from rules 33 the end says the look's confidence and the criteria (`--rules 32`: not) |
| 109-identify-follows-a-moving-boat | identify | rules 21: the boat comes in from the west at 10 m/s | the orbit moved with it three times (each logged before it went out), done once identified |
| 110-identify-track-lost | identify | rules 21: the boat goes unseen and the tracker drops it 180 s later | failed at the drop (a lost contact), the Puma holding |
| 111-identify-times-out | identify | rules 21: no look identifies the boat | failed at the deadline (the time to get there plus five minutes on station), the Puma holding; from rules 33 the timeout says its best own look (none) and the criteria (`--rules 32`: not) |
| 112-identify-returns-to-its-patrol | identify | rules 21: the Puma is sent from a patrol | done once identified, and sent back to the patrol it replaced (re-checked first); from rules 33 the end says the look's confidence and the criteria (`--rules 32`: not) |
| 113-patrol-routed-through-the-main-span | domain | rules 22, the owner's order of 10-27: HUNTER 1 east of the bridge sent on a patrol west of it, the straight transit into the south tower's pier (the Gate's land as tidewater draws it) | dispatched, the transit routed through the main span (1 waypoint, +40 m) and flown that way (`intent.dispatched` `via`); `--rules 21` refuses it as the live run did |
| 114-ugv-routed-round-the-water | domain | rules 22: a recon UGV sent from one pier to orbit on another, water between them, a causeway round the north | dispatched, the transit routed along the causeway (4 waypoints, +1,336 m) |
| 115-aircraft-routed-round-a-no-go-box | geofence | rules 22: PUMA 1 east of the bridge sent to orbit west of it, its straight transit through the south tower's no-go box | dispatched, the transit routed round the box 100 m clear of its edge (1 waypoint, +16 m) |
| 116-no-route-into-an-enclosed-pool | domain | rules 22: HUNTER 1 sent to orbit in a pool land encloses | rejected (domain: `no route around land at ...`, where the straight way first meets land, and why) |
| 117-second-look-contradicts-ais | identify | rules 22, the owner's widening: PUMA 1 sent to identify a boat its AIS has identified already; our camera calls it a patrol boat, the tracker's identity conflict raised | dispatched (`--rules 21`: refused, identified already); from rules 33 the look in conflict is a naming that falls short and the order ends failed at its deadline, saying the look and the conflict (`--rules 32`: done on that look, saying it contradicts the identity before) |
| 118-own-look-not-ended-by-another-camera | identify | rules 23, the owner's run of 10-27 17:11Z (intent 4): PUMA 1 sent to a cargo ship HUNTER 1's camera has identified already; HUNTER 1's camera identifies it again 2 s later | the order goes on (an `identify.step` meanwhile: "HUNTER 1 identified it as cargo_ship meanwhile; PUMA 1 continues for its own look"), done once PUMA 1's camera identifies it, the outcome saying both (from rules 33 each with its confidence and the criteria); `--rules 22` ends it on HUNTER 1's look, as the run did |
| 119-rounds-spent-then-another-weapon | audit | rules 24: two GBU-12 named and released, a third named; tamper `edit_rounds` | the third refused (no round left); none named: the Hellfire picked |
| 120-inside-the-weapons-minimum-range | weapon | rules 24: the MQ-9B 300 m over the point, inside the Hellfire's 500 m minimum | aborted late (never released) |
| 121-weapons-out-of-action | weapon | rules 25: the MQ-9B reports its weapons out of action before the first signature; tamper `drop_damage` | rejected (weapon) |
| 122-loitering-munition-propulsion-lost | weapon | rules 25: a Switchblade that lost its propulsion is nominated | rejected (weapon) |
| 123-gun-bursts-until-the-target-ceases | audit | rules 26: the armed UGV's M2 at a boat 1,450 m off; the boat leaves play at the third burst; tamper `edit_rounds` | done (ceased): 3 bursts of 6, a burst every 5 s, fire held on a target not seen since the last burst, 482 of 500 left |
| 124-gun-rounds-run-out | weapon | rules 26: the UGV 2,000 m off goes 275 m to its firing position; the boat persists | the 84th burst fires the last 2 rounds, the fire ends (rounds_out), assessed persists; a new nomination naming the M2 rejected (no round left) |
| 125-gun-stopped-mid-fire | abort | rules 26: op-1 stops the gun after its second burst; tamper `burst_after_stop` | done (persists): the fire stopped with the fallback (hold), no burst after it |
| 126-gun-superseded-mid-fire | engagement_in_play | rules 26: op-1 orders the firing UGV to hold | done (persists): superseded, no fallback, no burst after it |
| 127-gun-line-of-fire-and-keep-out | keep_out_friendly | rules 26: a protected sailboat crosses the line of fire, then one of ours enters the keep-out | done (persists): fire held while the sailboat is on the line, then stopped for the picture (keep_out_friendly), 3 bursts |
| 128-minefield-laid-on-two-signatures | audit | rules 27: the mine-laying UGV ordered to lay 10 TM-62M mines along 88 m, live 0.05 h; an agent and the orderer (no engagement authority) can't sign; tamper `move_mine` | done: op-2's signature leaves it waiting, op-3's approves it; 10 mines laid in order as the UGV passes, each arming 60 s and going inert 180 s after its laying, 10 of 20 left; the field logged inert once its period is over; a mine moved 40 m off the line in the log caught |
| 129-minefield-refused-for-traffic-on-its-line | minefield | rules 27: the same row, a neutral car driving north 150 m short of it at 3 m/s | rejected (minefield): the car on its course now passes the row within the field's danger area, how soon and how far short; `--rules 26` refuses the lay order outright |
| 130-our-field-is-a-keep-out-for-us | geofence | rules 27: the row laid, live an hour; a logistics UGV across it sent home, a recon UGV across it sent to orbit beyond it | done: the rtl goes with a geofence warning (path_crosses_minefield), the orbit's transit routed round no-go minefield MF-1 |
| 131-missile-from-the-shore | domain | rules 28: the armed UGV on the shore 1,450 m from a boat, its Javelin named | done (ceased): launched where it stands (inside the band, 140 to 4,675 m), 0 of 1 left; `--rules 27` rejected (domain: driven onto the water) |
| 132-missile-opens-to-its-minimum-range | weapon | rules 28: a hostile truck 55 m from the UGV, effect radius 50 m | done (ceased): the UGV opens 60 m to the band's near edge (115 m) and launches; `--rules 27` aborted late (engage_window: driven onto the truck, inside the minimum range) |
| 133-missile-closes-to-its-range | domain | rules 28: a boat 5,230 m off, beyond the Javelin's 4,750 m | done (unknown): the UGV closes 555 m to the band's far edge (4,675 m) and launches, a 31 s flight; `--rules 27` rejected (domain) |
| 134-handed-over-orbit-goes-when-heard | handover | rules 29: the fleet's starting task for SKY 1, an orbit over the strait, handed over at the start; SKY 1 silent until 3 s | queued (nothing logged) while unheard, dispatched once heard and its checks pass, the policy asked nothing; replay checks it against the fleet |
| 135-handed-over-orbit-on-land-voided | handover | rules 29: a USV's starting task an orbit on land | voided at its re-check (domain), never sent, saying which check failed |
| 136-handed-over-unverified-to-a-person | handover | rules 29: a Puma on dead reckoning, its starting task a patrol | to a person (readiness unverified; the policy asked only who), dispatched once op-1 approves acknowledging it |
| 137-silent-patroller-bounded-by-its-route | keep_out_friendly | rules 30: HUNTER 1 falls silent on its 500 m patrol leg 1.3 km east of the target, its own failsafe an rtl home to the leg's south end | done (ceased): counted on its leg and its way home (widened for its turns and a cross current), 825 m clear; `--rules 29` rejected (keep_out_friendly: its cruise reach, 1.24 km by the effect) |
| 138-silent-patrollers-way-home-crosses-the-keep-out | keep_out_friendly | rules 30: the same leg, its home 0.9 km west of the target | rejected (keep_out_friendly): if it lost us too, its rtl failsafe's way home passes over the effect point; the explanation says waiting clears nothing while it is silent |
| 139-struck-and-silent-presumed-lost | keep_out_friendly | rules 30: HUNTER 1, 1.2 km from the drone's station, reports itself struck and goes silent | presumed lost on its struck report (cited by seq), a flag that decides nothing: a nomination 15 s into its silence passes (rejected by an operator), one 415 s into it is rejected (keep_out_friendly); heard again, the presumption is cleared |
| 140-confirmed-lost-ends-its-orders | keep_out_friendly | rules 30: VIPER 1, its preload owed after an aborted engagement and an orbit queued for it, cut off and struck; op-1 (no engagement authority) tries to confirm the loss; tamper `lost_by_agent` | refused for op-1; op-2's confirmation cites the struck report: the queued orbit is voided, the values owed close (`restore.closed` lost), the drone's nomination then passes with VIPER 1 left out (`--rules 29`: rejected, keep_out_friendly) and is done; nothing owed at the end; tamper caught |
| 141-lost-vehicle-reports-again | keep_out_friendly | rules 30: HUNTER 1, silent a minute 900 m off, confirmed lost on a note (nothing presumes it); under way it reports again 130 m from the effect point; tamper `redate_loss` | a confirmation without a note refused; with it the nomination passes and is approved; the report revokes the loss (cause heard) before the picture reads it, and the engagement aborts (keep_out_friendly); tamper caught |
| 142-loss-carried-to-the-same-vehicle-only | keep_out_friendly | rules 30: an earlier log confirmed HUNTER 1 and the vehicle then under hunter-2 (OTHER 7) lost; this run starts with HUNTER 1 and another vehicle, HUNTER 2, under hunter-2; tamper `hide_loss_revoked` | both carried; HUNTER 1's orbit refused (confirmed lost), HUNTER 2's hold queued as for any silent vehicle; the nomination leaves HUNTER 1 out and counts HUNTER 2 until op-2 accepts its unknown position; HUNTER 1's first report revokes the carried loss; done; replay checks the carry against the earlier log; tamper caught |
| 143-silent-with-nothing-known-at-top-speed | keep_out_friendly | rules 30: our Magura silent under way at 12 m/s, 1.8 km east of the target, no order of ours on it | rejected (keep_out_friendly): nothing places it, so anywhere its top speed (25 m/s, and the water's 2.5) reaches; `--rules 29` lets it through (its cruise reach 330 m clear) |
| 144-identify-a-small-drone-close-in | identify | rules 31: PUMA 1, 150 m up, sent to identify a track the picture shows only as `small_air` (a radar's size class, no altitude); on the physical reaches, its reach now a clear day's (rules 32) | dispatched to an orbit of 788 m, its camera's reach of a small drone (9,160 m; an eighth, 1,145 m; its reach now, 1,167 m, takes nothing away), done once its camera identifies it (from rules 33 the end saying its confidence and the criteria); `--rules 30` an orbit of 1,411 m, sized by the domain's 3 m reference aircraft |
| 145-identify-a-drone-refused-from-too-high | identify | rules 31: the MQ-9B at its catalog height, 7,600 m, sent to the same kind of track, then SKY 1 (an X10, 100 m up); on the physical reaches, their reach now a clear day's (rules 32) | the MQ-9B rejected (identify: from 7,600 m up its camera identifies no small drone now), SKY 1 dispatched to an orbit of 420 m; `--rules 31` refuses the MQ-9B for its height (beyond an eighth of its catalog reach, about 4,068 m); `--rules 30` sends both (the MQ-9B 5.6 km out, SKY 1 at 1,380 m) |
| 146-identify-in-fog-refused-until-it-lifts | identify | rules 32: PUMA 1, 150 m up, sent to identify a `small_air` track in fog of 50 m visibility (its reach now, the vehicle fact `reach_now`), then again once the fog thins to 1.2 km; on the physical reaches; tamper `edit_reach` | rejected (identify: in fog of 50 m visibility PUMA 1's camera identifies a small_air contact only within about 160 m, its tightest orbit keeping it beyond that), then dispatched to an orbit of 599 m (its reach in 1.2 km visibility, 882 m); `--rules 31` sends it at once to the clear day's 788 m; tamper caught |
| 147-identify-at-night-needs-a-thermal-channel | identify | rules 32: at night, MAGURA 1 (a visible channel only) then PUMA 1 (day and LWIR) sent to identify a `small_surface` boat; on the physical reaches | MAGURA 1 rejected (identify: at night its camera identifies nothing, having no thermal channel), PUMA 1 dispatched to the clear day's orbit, 1,246 m (its LWIR's 2,018 m at night takes nothing from the clear day's 1,793 m); `--rules 31` sends both |
| 148-assessment-in-fog-sees-nothing | assessment | rules 32: scenario 131's Javelin launch in fog of 50 m visibility, the UGV's sight reaching about 156 m (its `reach_now`); on the physical reaches; tamper `edit_reach` | done (unknown): no sensor of ours covers the effect point 1,450 m off; `--rules 31` assesses it ceased on the clear day's reach; only the observation steps log the reach now; tamper caught |
| 149-identify-goes-on-past-a-first-naming | identify | rules 33: PUMA 1's camera first names a radar-recognised boat a small fast boat at 0.11 (the F8 boat), then at 0.65; tamper `edit_own_confidence` | done: the naming logged (`named`, "not yet identified (the bar is 0.6); it goes on"), the order ends at 0.65 saying the confidence and the criteria; `--rules 32` ends it at 0.11; tamper caught |
| 150-identify-a-submerged-contact-by-sonar | identify | rules 33: HUNTER 1 sent to identify a contact its sonar holds under water (`subsurface`, a detection); its sonar recognises it as submerged at 0.70 | done: identified by the subsurface domain's criterion, recognition by sonar (planned on the runner's legacy reaches: the physical model gives no camera a reach under water) |
| 151-ais-never-ends-an-identify | identify | rules 33: a ship identified by its AIS alone at 0.95; PUMA 1 sent; AIS heard again, then the camera recognises a large vessel at 0.80, then identifies a cargo ship at 0.70 | done: AIS ends nothing, the recognition is a naming that falls short (identification by EO/IR is the criterion), the identification ends it |
| 152-a-conflict-holds-an-identify | identify | rules 33: the camera identifies a small fast boat at 0.80 against the boat's AIS (a conflict), then the AIS goes quiet and the conflict drops | done: held while in conflict (a naming that falls short), ended once it clears; `--rules 32` ends it at the contradicting look |
| 153-identify-timeout-names-the-best-look | identify | rules 33: the camera names the boat at 0.11, then 0.45, never the bar | failed at the deadline (its transit and 300 s on station), naming PUMA 1's best look (0.45 by its eo_ir) and the criteria; `--rules 32` ends it at 0.11 |
| 154-size-class-is-not-an-identification | target_identity | rules 33: a drone the radar holds as `small_air` at 0.93, declared hostile and nominated for FURY 1's AMRAAM | rejected (target_identity: a detection, its size class, not an identification; the bar said); `--rules 32` passes it to the signers |
| 155-identified-below-the-bar | target_identity | rules 33: a small fast boat identified at 0.55 nominated | rejected (target_identity: below the bar, 0.6, an engagement needs identification at 0.6 or more) |
| 156-a-stricter-bar-holds-both | target_identity | rules 33: a policy whose bar is 0.75 (policies/bar-075.toml); PUMA 1's look at 0.70, then 0.80; the boat nominated at 0.70 | rejected (target_identity: below the bar, 0.75); the identify order's naming at 0.70 says the bar of 0.75 and its end at 0.80 too |
| 157-a-named-loosening-and-its-name-dropped | audit | rules 33: a drill's policy lowers the engagement bar to 0.5, named in `loosened` (policies/drill-bar-050.toml); a boat identified at 0.55 engaged; tamper `drop_loosened` | done (ceased): the check passes at the drill's bar, saying it; the loosening's name cut from the logged policy is caught |
| 158-group-ring-one-member-waits | group_order | rules 34: ten X10s ordered to orbit together, SKY 10 silent a minute (its facts a minute old) | dispatched: laid out ten slots 94 m apart, SKY 10 waiting, ten of ten counted (the minimum eight); nine member orders dispatched, SKY 10's queued until it is heard, then re-checked and dispatched, never in the inbox |
| 159-group-done-at-strength-despite-a-failsafe | group_order | rules 34: five X10s' group orbit; four reach their shares, one's failsafe takes it home on the way; tamper `move_member_slot` | done: four of five at the minimum of four, the reason counting each end; a member's altitude moved 30 m after the layout caught |
| 160-group-below-its-minimum-refused | strength | rules 34: five X10s, two too low on battery to orbit and keep their reserve | rejected (strength): 3 of 5 can take it, 2 excluded (reserves), the minimum 4 |
| 161-group-auv-ring-in-depth-layers | depth | rules 34: six REMUS on a 40 m ring in water 52 m deep, from 30 m, then from 40 m | the first dispatched in two layers 5 m apart (30 and 35 m); the second rejected (strength): its 45 m layer leaves 7 m above the seabed (depth), three of six under the minimum of five |
| 162-group-ring-too-tight-for-its-band | separation | rules 34: twelve X10s on an 8 m ring at 100 m (section 8 sets it with fixed-wing stations, the transit kind a later stage's) | rejected (separation): the ring holds 6 in 3 layers (100, 110, 120 m) at least 15 m apart; 12 do not fit |
| 163-group-leaves-out-an-engaged-member | group_order | rules 34: BLADE 1 nominated and waiting; op-1 orders it and four X10s to orbit together; tamper `admit_raised_member` | the group dispatched to the four, BLADE 1 left out (platform_free: held by its engagement), the nomination untouched (pending_approval); a member order added for BLADE 1 caught |
| 164-group-waiting-member-that-would-raise-the-tier | group_order | rules 34: four Pumas; PUMA 4 silent, so waiting; heard again on dead reckoning; tamper `member_without_group` | dispatched: PUMA 4's member order voided at its re-check (would raise the tier: a check nobody could verify), never sent to the inbox; member orders created while their group waits for approval caught |
| 165-group-hold-overlapping-fixed-wing-circles | group_order | rules 34: three Pumas about 100 m apart told to hold | dispatched, every member sent; the separation check warns that their loiter circles overlap at one height |
| 166-group-reforms-after-three-drop | group_order | rules 34, stage 4: twenty X10s' group orbit; SKY 4, SKY 11 and SKY 17 fall silent ten seconds apart; SKY 5, by SKY 4's gap, reports only every ten seconds by then; tamper `forge_reform` | one re-form once the drops settle (t=75): only a neighbour of each gap moves (SKY 2, SKY 8, SKY 18), each on a member order of its own; SKY 5 keeps its slot and SKY 4's gap is closed from SKY 2's side; the drops' facts made fresh in the re-form's inputs caught (its drops are not what its facts show) |
| 167-group-reform-that-would-raise-the-tier | group_order | rules 34, stage 4: eight Pumas' group orbit (minimum five); PUMA 3 and PUMA 7 fall silent; PUMA 5, between the gaps, on dead reckoning | the re-form refused (`group.reform_refused`: PUMA 5 would raise the tier, a check nobody could verify) and nothing moves; the same drops are not tried again |
| 168-group-below-its-minimum-suspended | group_order | rules 34, stage 4: twenty X10s' group orbit (minimum sixteen); twelve fall silent, six and six, then are heard again | suspended once the drops settle (eight of a minimum of sixteen), never re-formed, and it does not resume when they are heard |
| 169-group-member-back-rejoins-at-the-next-reform | group_order | rules 34, stage 4: ten X10s' group orbit; SKY 3 and SKY 8 fall silent, then are heard again; tamper `reform_outside_envelope` | re-formed round the eight (SKY 6 and SKY 10 moved); the two back rejoin only at the next re-form, a minute later at the soonest, each in a gap on a member order of its own; a re-formed share raised out of its band caught (the re-form's layout differs from the one its facts give) |
| 170-transit-one-copter-holds-at-its-end | transit | rules 34, stage 7: op-1 sends SKY 1 along three points over the water and to hold at the last | dispatched, flown along the route and done holding at its last point; `--rules 33` refuses it (no transit in those rules) |
| 171-transit-routed-round-the-pier | domain | rules 34 with rules 22: HUNTER 1 east of the bridge sent along the channel to hold west of it, the route's one leg straight into the south tower's pier | dispatched, the leg routed round the pier (`domain`: the task path leg 1-2 routed around land, 1 waypoint, +39 m; `intent.dispatched` `path`), flown that way and done holding at the route's end |
| 172-group-transit-to-stations | group_order | rules 34, stage 7: five X10s' group transit along a 880 m line; tamper `move_member_station` | dispatched: five stations 220 m apart, one layer, each member straight to its own (the pairing keeps their ways from crossing) and holding there; done once all five hold; a member order moved 50 m off its station caught |
| 173-group-transit-to-a-block | group_order | rules 34, stage 7: six Corsair USVs sent to one point off Crissy Field as a `block` | dispatched: three columns and two rows 30 m apart round the point, each boat straight to its own cell, judged on the water along its own legs; done once all six hold station |
