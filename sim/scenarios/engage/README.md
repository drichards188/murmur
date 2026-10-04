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
                                 # drop_reset, agent_order_auto, drop_ack, loosen_policy
                                 # (one with nothing to edit in a run's log reports "not
                                 # applicable")
end_s = 120                      # seconds to run (default: the last step + 10)
after_effect = "out_of_play"     # or "persists"
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
| `nominate` | `by` or `agent` (+ `rationale`), `platform`, `track`, `radius_m`, `fallback` (`abort`, `return`, `loiter`), `window_s` | a nomination; becomes "the last nomination" the other steps act on |
| `look` | `by` | an operator with engagement authority asks for the picture (a picture token: the plan and the track now); their next `approve` quotes it |
| `approve` | `by` | a signature: through `approve_engagement` if the session has engagement authority (quoting the token from their last `look`, or one asked for now), else `approve` (which the engine refuses for an engagement) |
| `approve_task` | `by`, `acknowledge` | an operator approves the last `task`; `acknowledge` accepts that its dispatch stops an engagement in play |
| `task` | `by` or `agent` (+ `rationale`), `task` (`hold`, `rtl`), `uids` | another order on our vehicles; dispatched at once if its tier lets it run, superseding any engagement on them (no fallback commanded) |
| `stall` | `seconds` | the loop that holds the picture stops for that long (no engagement step) |
| `reject` | `by` | an operator rejects it |
| `abort` | `by` | any operator stops it; the fallback is commanded |
| `agent_approve` | `agent` | an agent "tries" to approve: there is no such method on its port, so it is recorded and the intent must not move |
| `track` | `id` and any of `at`, `velocity`, `affiliation`, `declared_by`, `undeclare`, `identity`, `identity_confidence`, `sigma_m`, `domain`, `silent`, `velocity_error` | a track changes (or appears); `velocity` is how the contact really moves, `velocity_error` how far the tracker's estimate is off it |
| `vehicle` | `uid` and any of `at`, `velocity`, `silent`, `report_interval_ms`, `nav` (`gnss`, `dead_reckoning`), `gps_fix` (3: a 3D fix), `gps_glitch` | one of ours changes; `silent` cuts its link |
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
| 08-link-cut | platform_link | the platform goes silent mid-engagement | aborted, fallback loiter |
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
| 35-real-course-change-while-waiting | signed_picture | a holding target gets under way at 6 m/s while a signature waits | the plan dropped at once; a new plan; done |
