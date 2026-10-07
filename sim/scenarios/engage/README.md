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
```

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
- Rules 16: a `drop_track` step is the tracker's drop (as the bridge's after 180 s without a
  detection): the engine is handed the track's facts then (`track.dropped`) and the runner
  stops carrying it; every second the runner also calls `Engine::step_lost` (a lost contact
  seen again under a new track id with the same AIS identity, unambiguously, is retired), as
  the bridge's loop does. Result field: `reacquired` (`t=<s> T-0002 is T-0005 again`).

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
                                 # and rules 16: hide_lost_contact, drop_track_dropped,
                                 # forge_lost_acceptance
                                 # (one with nothing to edit in a run's log reports "not
                                 # applicable")
end_s = 120                      # seconds to run (default: the last step + 10)
after_effect = "out_of_play"     # or "persists"
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
expect_reacquired = ["T-0002 is T-0005 again"]   # rules 16, optional: lost contacts retired
                                 # as seen again, in order (expect_none_reacquired = true: none)
# policy, catalog, areas = "path"  # optional, relative to the repo (defaults: sim/...)

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
| `nominate` | `by` or `agent` (+ `rationale`), `platform`, `track`, `radius_m`, `fallback` (`abort`, `return`, `loiter`), `window_s`, `accept` (rules 6: vehicles the operator accepts as lost, each since its last report), `accept_lost` (rules 16: lost contacts the operator accepts, each since its last fix) | a nomination; becomes "the last nomination" the other steps act on |
| `accept` | `by`, `uid` or (rules 16) `track`, `since_s` (optional: another report or fix than the last, which the engine refuses) | rules 6: an operator with engagement authority accepts a lost vehicle (rules 16: a lost contact) for the last nomination while it waits with no signature (without it: refused) |
| `revoke` | `by`, `uid` or (rules 16) `track` | rules 6: any operator withdraws an acceptance from the last nomination (pending or under way) |
| `drop_track` | `track` | rules 16: the tracker drops it (its facts now go to the engine, `track.dropped`); from then on it is a lost contact |
| `agent_accept` | `agent`, `uid` | an agent "tries" to accept a lost vehicle: its port has no such method; recorded, nothing may move |
| `look` | `by` | an operator with engagement authority asks for the picture (a picture token: the plan and the track now); their next `approve` quotes it |
| `approve` | `by` | a signature: through `approve_engagement` if the session has engagement authority (quoting the token from their last `look`, or one asked for now), else `approve` (which the engine refuses for an engagement) |
| `approve_task` | `by`, `acknowledge` | an operator approves the last `task`; `acknowledge` accepts that its dispatch stops an engagement in play |
| `task` | `by` or `agent` (+ `rationale`), `task` (`hold`, `rtl`, `orbit` with `center` [lon, lat] and `radius_m`), `uids` | another order on our vehicles; once approved, dispatched if the link reaches them, else queued (rules 5), superseding any engagement on them (no fallback commanded; when queued, from rules 7) |
| `stall` | `seconds` | the loop that holds the picture stops for that long (no engagement step) |
| `reject` | `by` | an operator rejects it |
| `abort` | `by` | any operator stops it; the fallback is commanded |
| `agent_approve` | `agent` | an agent "tries" to approve: there is no such method on its port, so it is recorded and the intent must not move |
| `track` | `id` and any of `at`, `velocity`, `affiliation`, `declared_by`, `undeclare`, `identity`, `identity_confidence`, `sigma_m`, `domain`, `silent`, `velocity_error`, `alt_m`, `tracked_s`, `ais` (rules 16) | a track changes (or appears); `velocity` is how the contact really moves, `velocity_error` how far the tracker's estimate is off it; `tracked_s` (rules 12) when one this step makes was started (0: a new track) |
| `write_off` | `by`, `uid`, `note` | rules 12: an operator writes off the lost-link values owed back to one of our vehicles, saying why (`restore.closed`) |
| `vehicle` | `uid` and any of `at`, `velocity`, `silent`, `report_interval_ms`, `nav` (`gnss`, `dead_reckoning`), `gps_fix` (3: a 3D fix), `gps_glitch`, `circling`, `reported_circle`, `alt_m` | one of ours changes; `silent` cuts its link (both ways: it hears no GCS either) |
| `expect` | `state`, `rule`, `concurrences`, `task_state`, `task_reason` | check the last nomination now (`task_state`: the last `task` step's intent; `task_reason`: a substring of why it is in that state) |

Approved engagements are dispatched at once (as the bridge does). Every second the runner
calls `Engine::step_engagements` (hold the picture, release, observe, assess).

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
| 53-lost-friendly-blocks | keep_out_friendly | nominate a minute after one of ours went silent 2 km away | rejected; the detail says since when |
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
| 75-rule-declaration-revoked-mid-engagement | rule_declaration | rules 9: an untracked author beside a fishing boat (a false rule declaration), nominated before a confirmation, revoked en route; the stop cut from the log | aborted (revoked), fallback return; tamper caught |
| 76-rule-declared-bystander-protected | keep_out_tracks | rules 9: a rule-declared, unconfirmed hostile 250 m from another engagement's target; the confirmation planted in an earlier picture in the log | rejected (keep_out_tracks) until confirmed, then done; tamper caught |
| 77-revoked-bystander-stops-engagement | keep_out_tracks | rules 9 (review): a confirmed rule declaration exempts a bystander from an engagement's keep-out; op-3 revokes it mid-engagement; the stop cut from the log | aborted in the revocation's step, cause revoked (keep_out_tracks), fallback; tamper caught |
| 78-revoked-bystander-before-first-picture | keep_out_tracks | rules 9 (second review): as 77, but op-3 revokes the bystander's declaration in the second the engagement is approved, before its first picture; the stop cut from the log | voided in the revocation's step, cause revoked (keep_out_tracks), not a later picture failure; tamper caught |
| 71-refused-mission-supersedes-engagement | engagement_in_play | rules 7: 70's policy, the orbit ordered while the engagement is still under way, link up; the fact stripped from the log | the orbit is voided and in the same step the engagement is aborted, superseded by it, no fallback, its reason saying so; given again before the restore it is refused saying the restore is pending; after it, it goes at once; tamper caught |
| 79-young-track-blocks-until-tracked | keep_out_tracks | rules 12: a ferry first detected a second before the nomination, 1.2 km north of the effect point and heading away (no average velocity yet); its first detection dropped from the log | rejected (keep_out_tracks: anywhere it reaches at 25 m/s), then, tracked 59 s, the same nomination waits for its signers; tamper caught |
| 80-expended-munition-owes-nothing | restore_owed | rules 12: a switchblade's engagement released (the munition spent by its effect); then an orbit and a second nomination for it; the catalog fact dropped from every picture in the log | done (ceased); its values closed right after the release (`restore.closed`, expended), nothing owed at the end; the orbit and the nomination rejected (readiness: spent by its own effect); tamper caught |
| 81-refusal-names-the-order-still-queued | restore_owed | rules 12: 67 under 70's policy: the hold queued for the cut-off USV, then an orbit refused for its own values; the clause cut from the refusal in the log | the orbit voided, its reason naming the hold still queued (`i2 (hold, vehicle viper-1)`); the hold goes when the link returns; tamper caught |
| 82-restore-written-off | restore_owed | rules 12: 69, the USV never confirming its values back; op-1 writes them off; the write-off credited to another operator in the log | the waiting orbit goes at once after the write-off, nothing owed at the end; tamper caught |
| 83-aircraft-may-turn-back | keep_out_tracks | rules 14 (murmur-oho): a light aircraft 2 km north of the effect point, flying north at 50 m/s on a steady course (an average velocity, no manoeuvre seen), its height not ranged | rejected (keep_out_tracks: an aircraft may turn or speed up at any moment), then, 12 km out, the same nomination waits for its signers; rules 13 let the first through (`--rules 13`) |
| 84-lost-contact-blocks | keep_out_tracks | rules 16: a fishing boat 400 m from the target goes out of view and is dropped by the tracker before the nomination; the lost contact cut from every decision in the log | rejected (keep_out_tracks: the lost contact, since when, may be accepted; `--rules 14` waits for its signers); tamper caught |
| 85-lost-contact-accepted | keep_out_tracks | rules 16: the nominating operator accepts 84's lost contact for the engagement; the tracker's drop cut from the log | done (ceased); tamper caught |
| 86-accepted-lost-contact-reappears | keep_out_tracks | rules 16: an accepted lost tour boat (AIS) is seen again 150 m from the effect point under a new track id; the acceptance credited to an agent in the log | retired (`T-0002 is T-0005 again`), aborted (keep_out_tracks: the live track), fallback; tamper caught |
| 87-lost-contact-acceptance-needs-authority | keep_out_tracks | rules 16: op-4 (no engagement authority) accepts a lost contact for a waiting nomination, and nominates accepting it | both refused; voided at the next signature (keep_out_tracks: the lost contact's grown reach) |
| 88-forged-lost-contact-acceptance | keep_out_tracks | rules 16: an acceptance given while the nomination waits, then re-dated in the log to name a fix a minute later than the contact's last, everywhere it appears | done (ceased); tamper caught (not lost since that fix; the checks no longer follow) |
| 89-lost-contact-turns | keep_out_tracks | rules 16: a ferry 600 m from the target, last seen steaming away at 5 m/s, goes out of view and is dropped; its course alone clears the keep-out by the effect | rejected (keep_out_tracks: anywhere at its own plausible speed, 7 m/s: it may have turned back; `--rules 14` waits for its signers) |
