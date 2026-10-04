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
- Rules 5: a dispatched platform answers its lost-link preload a second after the dispatch
  (`preload`, below), as the bridge reports it; with it confirmed and its link cut for longer
  than the preloaded timer it follows the preloaded action on its own
  (`lost_link_action`); once the engagement ended and it can hear us, its own values go back
  (a restore). Result fields: `lost_link_action`, `preload_reports`.

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
                                 # and rules 5: drop_preload, forge_preload,
                                 # drop_verify, drop_acceptance, acceptance_by_agent,
                                 # acceptance_without_authority, hide_self_accepted
                                 # (one with nothing to edit in a run's log reports "not
                                 # applicable")
end_s = 120                      # seconds to run (default: the last step + 10)
after_effect = "out_of_play"     # or "persists"
preload = "confirm"              # rules 5: how the platform answers its lost-link preload:
                                 # confirm (default), no_answer, mismatch (another action read
                                 # back), never (nothing reported), changed (confirmed, then
                                 # another action at the re-read near the release); a confirmed
                                 # one is read again every 5 s from 30 s before the window opens
expect_lost_link = "Loiter"      # optional: it followed its preloaded action on its own
expect_preload = ["set confirmed", "restore confirmed"]   # optional, in order (also
                                 # "verify confirmed", "verify changed")
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
alt_m = 7000                     # rules 5: altitude MSL it reports (absent: none, 2D)
lost_link_s = 30                 # its own lost-link timer (default 5): what a preload shortens
lost_for_s = 60                  # lost before the run: silent, last reported this long before

[[track]]                        # a perception track (not truth)
id = "T-0001"
at = [-122.4950, 37.8120]
velocity = [0.0, 0.0]
sigma_m = 15                     # 1-sigma now (default 15)
affiliation = "suspect"          # unknown, suspect, hostile, neutral, assumed_friend
declared_by = "op-1"             # optional: an operator's declaration
identity = "small_fast_boat"
identity_confidence = 0.9        # default 0.9
confidence = 0.9                 # existence, default 0.9
domain = "surface"               # default surface
age_s = 0                        # seconds since the last detection at the start
silent = false                   # true: no detections at all
sources = ["3:radar"]
alt_m = 0                        # rules 5: its estimated altitude (absent: unknown, 2D)
no_velocity_estimate = false     # rules 5: the tracker has no velocity for it yet
alt_age_s = 0                    # rules 5: its altitude was last ranged this long before the
                                 # start and never again (the radar lost it; the others keep
                                 # its position fresh); sources radar and ais are the ranging ones

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
| `declare` | `track`, `affiliation`, `by` | an operator's declaration (logged `operator.action track.declare`) |
| `nominate` | `by` or `agent` (+ `rationale`), `platform`, `track`, `radius_m`, `fallback` (`abort`, `return`, `loiter`), `window_s`, `accept` (rules 5: vehicles the operator accepts as lost, each since its last report) | a nomination; becomes "the last nomination" the other steps act on |
| `accept` | `by`, `uid`, `since_s` (optional: another report than the last, which the engine refuses) | rules 5: an operator with engagement authority accepts a lost vehicle for the last nomination while it waits with no signature (without it: refused) |
| `revoke` | `by`, `uid` | rules 5: any operator withdraws an acceptance from the last nomination (pending or under way) |
| `agent_accept` | `agent`, `uid` | an agent "tries" to accept a lost vehicle: its port has no such method; recorded, nothing may move |
| `look` | `by` | an operator with engagement authority asks for the picture (a picture token: the plan and the track now); their next `approve` quotes it |
| `approve` | `by` | a signature: through `approve_engagement` if the session has engagement authority (quoting the token from their last `look`, or one asked for now), else `approve` (which the engine refuses for an engagement) |
| `approve_task` | `by`, `acknowledge` | an operator approves the last `task`; `acknowledge` accepts that its dispatch stops an engagement in play |
| `task` | `by` or `agent` (+ `rationale`), `task` (`hold`, `rtl`), `uids` | another order on our vehicles; dispatched at once if its tier lets it run, superseding any engagement on them (no fallback commanded) |
| `stall` | `seconds` | the loop that holds the picture stops for that long (no engagement step) |
| `reject` | `by` | an operator rejects it |
| `abort` | `by` | any operator stops it; the fallback is commanded |
| `agent_approve` | `agent` | an agent "tries" to approve: there is no such method on its port, so it is recorded and the intent must not move |
| `track` | `id` and any of `at`, `velocity`, `affiliation`, `declared_by`, `undeclare`, `identity`, `identity_confidence`, `sigma_m`, `domain`, `silent`, `velocity_error`, `alt_m` | a track changes (or appears); `velocity` is how the contact really moves, `velocity_error` how far the tracker's estimate is off it |
| `vehicle` | `uid` and any of `at`, `velocity`, `silent`, `report_interval_ms`, `nav` (`gnss`, `dead_reckoning`), `gps_fix` (3: a 3D fix), `gps_glitch`, `circling`, `reported_circle`, `alt_m` | one of ours changes; `silent` cuts its link (both ways: it hears no GCS either) |
| `expect` | `state`, `rule`, `concurrences`, `task_state` | check the last nomination now (`task_state`: the last `task` step's intent) |

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
| 08-link-cut | platform_link | the platform (a USV, rules 5) goes silent mid-engagement | aborted, fallback loiter; it holds where it is on its own (its preloaded FS_ACTION 6, its 30 s timer shortened to 5 s); its own values back when the link returns |
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
