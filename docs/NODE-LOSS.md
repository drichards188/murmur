# Losing a node: silent, presumed lost, lost

A spec, approved by the owner on 2026-11-13 with section 9's defaults (bd murmur-zb1d, the owner's
question and agreed direction of 2026-11-06). It adds decision rules 30 (it was written as 29, which
the watch handover took first). Built the same day in section 8's seven stages and the default
decision rules from stage 7; the design below is kept as approved, and the "As built" notes say
where the build differs or adds to it: section 3 (stage 2, the region and the figures its headless
check set), section 4 (the console's display of a vehicle confirmed lost), section 5 (stages 5 and 6:
the bridge's routes and items, autoplay, custody, the watch officer, and the live runs) and section 8
(stage 7: the flip, the fixtures, the digests). Two names changed in the build: the carried key is
`carried_from.nodes_lost` (not `lost`, for the reason section 7 gives), and the tests are
`crates/murmur-decide/tests/rules30_prediction.rs` and `rules30_states.rs`, with the bridge's
`decision/tests/node_loss.rs` and `lost_link.rs`.

## 1. The problem

From the C2's seat a destroyed vehicle and a cut link look the same at first: silence. The
difference is evidence, and declaring one of ours lost removes a friendly-fire protection, so it is
a person's decision on evidence, as a hostile declaration is. Today the engine has no such state.

What happens today when a hostile sinks HUNTER 1 (hostile-act-air-to-air, the contacts' seed 127,
docs/AUTONOMOUS-NODES.md section 8; hostile-act-engagement's default run only takes its propulsion):

- At the impact the bridge logs HUNTER 1's `vehicle.event` `struck` and a one-minute `struck` alert
  (`crates/murmur-bridge/src/tracks/mod.rs:1074-1098`, `:1103-1158`), straight from the truth side,
  skipping its link (bd murmur-3nkc); a miss no longer (bd murmur-aset, fixed in 84fdaef). The stand-in goes silent
  (`crates/murmur-standins/src/damage.rs:10`).
- Every engagement check then finds its position stale (`crates/murmur-decide/src/engage.rs:3083`):
  no velocity (`:3112`), no circle (a circle holds only a fresh position, `:3050-3056`), so it counts
  anywhere its catalog cruise speed reaches since its last report (`:3190-3226`; 6 m/s for
  `seahunter`, `sim/platforms.toml:794`): 360 m a minute of span, from 0.9 km out, against a keep-out
  of the effect radius plus 200 m. Only an operator with engagement authority accepting its unknown
  position for that one engagement clears it (rules 6, `engine.rs:1776`); no script does, and the
  explanation says "waiting does not clear it" (`keepout.rs:1474`).
- HUNTER 1's camera held the target: a silent vehicle senses nothing (`tracks/mod.rs:465-479`), so
  the drone's identity stays at 0.59 of 0.60 and its fix loosens to 149 m of 37.5 m. FURY 1 never
  passes and the timeline gives up.
- Its `lost_link` alert stays for good (`decision/alerts.rs:109-124`), queued orders for it wait up
  to 900 s, and lost-link values an engagement preloaded on it are owed and retried for ever.

## 2. The states

| state | entered when | the engine logs | who decides | evidence shown | leaves when |
|---|---|---|---|---|---|
| **reporting** | its position is fresh (`rules.rs:965` `fresh`, the link-aware age limit) | nothing new | | | its position goes stale |
| **silent** | its position is stale | nothing new: derived from the facts every decision logs | the engine | last report, its predicted region (section 3) | reported again (reporting), or evidence (presumed lost), or a person confirms (lost) |
| **presumed lost** | silent, and strike evidence within `PRESUME_WINDOW_S` (30 s, GUESS) of its last report, or damage it reported before going silent, or spent by its own effect (section 6) | `node.presumed` {uid, since, evidence (each cited by seq), world_t, its facts}; `node.presumption_cleared` when heard again | the engine, from logged inputs only; a flag, it decides nothing | each item with its time and source, the silence, the region | heard again, or a person confirms |
| **lost** | an operator confirms (section 5); still silent since exactly the report the review showed (`engage::lost_since_report`, `engage.rs:1141`, reused) | `node.lost` {uid, since, by, session, note, evidence, world_t, vehicle, identity, step_up}; then its consequences (section 4) | a person with engagement authority (decision D1); never an agent; in autoplay a scripted signer | as presumed lost, plus what confirming does | revoked: automatically when it is heard again (`node.loss_revoked` {cause: heard}, logged before any decision reads the report), or by any operator with a note (it only restricts) |

A person may also confirm a silent vehicle with no presumption, saying why (decision D2). Revocation
checks nothing of the vehicle's identity: any report under the uid restores its protection (fail
closed); the alert says if the identity differs. Murmur's nodes are uncrewed: a crewed platform would
need a catalog flag that keeps it in the keep-outs (survivors). None exists.

## 3. Where a silent node can be

The engine replaces the cruise-reach rule for a silent vehicle (`engage.rs:3190-3226`) with a union
of hypotheses, each a pure function of logged facts and the engine's own record, so replay recomputes
it. Shapes are discs and corridors (a polyline with a half-width); the keep-out reads the nearest edge
of any shape, as it reads the disc today.

**Inputs.** Its last position `p0` at `t0`; its velocity and circle at that report; its home; the
orders that may be running (the order in force whose op was active at `t0`, and any whose start went
out unconfirmed, rules 5); its lost-link plan (the confirmed preload of an engagement holding it,
`LostLinkPreload`, `engage.rs:1481`; else its own GCS failsafe action and timer, a new vehicle fact,
section 7); its `damage` fact (`world.rs:98`); presumption evidence.

| hypothesis | applies | region |
|---|---|---|
| **C**, it runs its order (the link may be cut one way) | an order may be running, or it was circling or still | patrol: the route's corridor (it loops, so no along-track bound); transit or task path: the rest of the route from `p0`, then the end point's hold; orbit or loiter (its circle at `t0`): the disc, for the whole span (today `CIRCLE_CARRY_S` applies to fresh vehicles only); hold: the hold point's disc; rtl: the corridor `p0` to home, then home's hold; no order: its circle, or a hold at `p0` if it was under `holding_speed_mps` (1.5) |
| **L**, its lost-link failsafe fired (it lost us too) | the plan is known and acts | rtl: the convex hull of C and home (a sound bound for "anywhere on C, then straight home", ignoring the timer); hold, loiter, brake, land, depth hold: C's shapes; surface or disarm on the water: C then drift |
| **D**, it may be adrift or falling | presumed lost, or a `mobility` damage report (then D alone: it cannot run its order) | surface: a disc from `p0` growing at 2.5 m/s (the Gate's spring current, GUESS); subsurface 1.0 m/s; ground: 10 m; fixed wing: its glide footprint, altitude x 10 (GUESS); rotary: altitude x 1 (GUESS) |
| **F**, nothing about its plan is known | no order, circle or stillness to go on, or no position history | anywhere within its top speed since `t0`: catalog `max_mps`, else the domain's (`engage.rs:3298`: air 100, ground 30, surface 25, subsurface 15 m/s) (decision D3) |

**Figures.** Corridor half-width `PATH_TOLERANCE_M` 50 m (the `CIRCLE_SLACK_M` figure, `engage.rs:2890`:
waypoint radius, GNSS 2 sigma, cross-track error; GUESS) plus where in its report interval it was
(last speed x interval / 2); hold discs 10 m plus drift on the water. Corridors and holds do not grow
with time: no one can retask a silent vehicle. Every shape is capped by the reach disc `p0` + top
speed x span (the larger of the two distances counts, which never overstates the clearance), and past
the endurance its last battery report leaves, D is added. Constants in code, as `CIRCLE_SLACK_M` and
`LINE_OF_FIRE_M` are: no policy schema change.

**Keep-outs and the explanation.** `friendly_standing` returns the nearest shape and names its
hypothesis in `how`; `FriendlyKind` gains `route`, `failsafe_home`, `drift` and `top_speed` beside
today's kinds. The explanation (murmur-12e, `keepout.rs:1384-1470`) says which hypothesis blocks ("if
it lost us too, its way home passes 120 m from the effect point"), says that waiting clears nothing
for C and L and only grows D and F, and adds "or an operator confirming it lost (Review)" to `other`.
The rules 6 acceptance for one engagement stays as it is.

**As built (stage 2, bd murmur-zb1d.2; `crates/murmur-decide/src/silent.rs`).** What may be running on
each silent vehicle comes from the engine's record of its intents and their op states
(`silent::in_force`: newest first, the newest it started runs and nothing older does, a newer one whose
start is not confirmed may run too, one that failed after it started leaves what it does unknown, F),
filled into every engagement decision's inputs as `WorldSnapshot.silent_orders` for the silent vehicles
only, with whether a presumption of its loss is in force (the node ledger: D counts too), logged with the
decision, and recomputed by replay from its own record. A vehicle confirmed lost is left out of the
keep-out's explanation as the check leaves it out. The headless check below
changed some figures: a corridor is also widened by the vehicle's turning circle's diameter (a turn back
at a waypoint; radius from the catalog's turn rate, else 10 degrees a second, a fixed wing's never under
the policy's 60 m), a leg on the water by half its length times the current over the vessel's cruise (a
vessel steering at its next point in a cross current is set off up to 1/e of that), and a vessel no
faster than the current (or an airship than the wind, 10 m/s) by the current times the span; the
subsurface drift is the surface's 2.5 m/s (the simulator's water runs the surface current at every
depth); the reach adds the current or the wind. A plan not known (no `lost_link` yet) counts every action
its autopilot has. A fixed wing holds and arrives home on its own loiter radius, which the lost-link plan
now carries (`LostLinkPlan.circle_m`, RTL_RADIUS or else WP_LOITER_RAD, which the bridge's read fills
once it has them); not known, its reach bounds that circle. Its battery
failsafe may send it home once its last battery report could fall to 25 % by the effect, and D is added
past its endurance (or with none known). A drifter is D alone. The explanation's region carries a
non-disc shape's outline.

**The check (eval/headless/node_loss.py, 2026-11-13).** The 22-type fleet as stand-ins
(`eval/headless/node_loss.fleet.toml`), on the Golden Gate world's grid and currents, 18 orders running
(patrols, orbits, holds, an rtl; the rest with none), every link cut at 300 s for 600 s, each silent
vehicle's region against its stand-in's truth every second, with each vehicle's failsafe as the bridge
read it (stage 4), as the check maps the stand-in's parameters itself, and not known: cut both ways (the
failsafes act) and only the vehicles' sending (they run their orders on), 12,936 samples each, none
outside its region; the worst margin 7.4 m (the drifting buoy 10 s in). The bridge's read and the
check's own mapping differ for five vehicles, each disarmed or a rover in RTL when last heard, where the
bridge's mode-aware read says no failsafe acts; the regions hold on either. The first runs found three outside: a SeaHunter circling its home on its 4 degree a
second turn and set off by the current on its way (105 m), a BlueROV adrift at the surface current's
speed under water (110 m), the Saildrone unable to make way against the current (371 m); one-way, a
Switchblade's turn back at a patrol end (180 m).

**HUNTER 1 again.** Its patrol leg runs north from its home at [-122.4850, 37.8130], 0.89 km from the
drone's station: C and L are the leg itself, about 450 m clear of the keep-out for as long as it is
silent. A comms loss would never block. Sunk (presumed lost: an act against it observed), D at
2.5 m/s reaches the keep-out about three minutes after its last report, so the timeline's nomination
at 3:20 would pass or fail on that margin; a confirmed loss clears it either way. Custody is the
second half: another sensor must take the drone (section 5).

## 4. What "lost" changes, reader by reader

| reader | today | lost | reported again |
|---|---|---|---|
| friendly keep-out, every engagement check (`engage.rs:2901`; nomination `:2532`, plans `:4527`, picture `:4946`, release `:5412`) | counted, cruise reach | left out; the pass detail lists it ("confirmed lost by op-2 at ...") | counted from its fresh report at once |
| line of fire, rules 26 (`engage.rs:5461`) | via the keep-out | left out | counted |
| keep-out explanation (`keepout.rs:653-669`, `:873`) | an item | no item | an item |
| picture tokens (`engage.rs:804`, `:4784`) | carry rules 6 acceptances | also carry the lost set; a signature on a token whose set differs is refused | the same refusal |
| readiness (`rules.rs:1044`) | spent refuses every order (`:1050-1067`) | refuses every order, hold and rtl included: "confirmed lost" | takes orders |
| queue, rules 5 (`queue.rs:251`) | waits up to `expire_s` | every pending or queued intent naming it is voided (`Engine::void`, `engine.rs:5222`, reason "vehicle confirmed lost"); a dispatched order's op on it ends `lost`, the intent ends when no vehicle is left on it | nothing comes back: new orders |
| engagements | link gap stops one within `link_gap_s` (`engage.rs:5083`), fallback from its preload | refused while one holds the platform (as a write-off, `engine.rs:2634`); a released munition flies on (rules 20) | unchanged |
| owed lost-link values, rules 6, 7, 12 (`engine.rs:2568`, `:2615`) | retried for ever, `restore_pending` alert | closed: `restore.closed`, a new closure `lost` (`audit.rs:1255`) | owed back again, a mission waits for the restore (as an un-spend, `engine.rs:2751`) |
| spent marks, rules 12 and 15 (`replay.rs:1292`) | separate | unchanged and separate (D6) | unchanged |
| rounds, rules 24 | ledger | unchanged | unchanged |
| minefield check's "ours", rules 27 (`mines.rs:692-731`) | its position, home and orders | left out, and out of `orders_in_play` | counted |
| identify, rules 21 to 23 | runs on | its identify ends with its op (`lost`); the track needs another sensor | |
| assessment coverage (`engage.rs:5714`) | fresh positions only | unchanged | unchanged |
| domain rule, geofence, planner, space tasks | read an intent's vehicles; collect and cover name none (`sim/policy.toml:43`) | nothing to read | |
| carry over a restart, rules 12 and 15 (`replay.rs:1218`) | spent, owed, rounds, fields | `carried_from.lost`, the same vehicle only (platform, callsign, sysid: `VehicleIdentity::same_vehicle`, `engage.rs:1442`) | revoked at its first report in the new run |
| bridge alerts (`decision/alerts.rs`) | `lost_link` for good | `lost_link`, `restore_pending` and `may_be_running` close; a `node_lost` info alert | `node_reported_again`, high |
| bridge task layer, restore jobs (`decision/engage.rs:1290-1320`) | retry | stop | resume |
| display estimate (`estimate.rs`) | display only | unchanged; the vehicle shows LOST | |
| truth side | | never reads it | |

**As built (the console, stages 5 and 7).** The display estimate is unchanged at the bridge, but the
console no longer draws it once a vehicle is confirmed lost: no ghost, ellipse or label on the map
and no Estimate section in its details, whose Now section reads as its last report (dimmed, with its
age), with neither Select for tasking nor Engage; the fleet list tags it LOST where a silent vehicle
is tagged EST (stage 7, the owner's direction after stage 6's live run: what it last reported is not
where it went). The Review card names our sensors in the evidence by callsign and lists the region
one line for each distinct part, with the number of shapes it covers. A run before rules 30 shows
what it always did.

## 5. Interfaces

| what | shape |
|---|---|
| review items | `GET /api/review` (`decision/ident.rs:247`) gains `node_losses` (presumed lost, and silent vehicles anyone flagged by hand) and `nodes_lost` (confirmed), each with uid, callsign, `since`, silent seconds, evidence, region, `actions` |
| confirm | `POST /api/review/nodes/{uid}/lost` {since, note, password or passkey}: 403 without engagement authority and 409 when the rules refuse (not silent since `since`, an engagement holds it), both before any credential is asked for, then 401 without a valid step-up in a credentials run, as `/api/restores/unspend` (`api.rs:606-636`). Hard-coded like the un-spend's step-up, not an `[approval] step_up_acts` word: no policy schema change |
| revoke | `POST /api/review/nodes/{uid}/revoke` {note}: any operator, no step-up |
| console | ReviewSection: a Node losses group, one card each: the evidence with times, the region on the map, what confirming does (leaves keep-outs, ends its orders, closes owed values, undone if it reports), a note, Confirm lost (step-up). Map and VehicleDetails: SILENT (the region drawn as the engine counts it), PRESUMED LOST, LOST |
| agents (MCP) | read only: `get_fleet` and `get_vehicle` carry `standing` (omitted while reporting, so recorded reads do not change), `get_inbox` carries the review items. No tool confirms or revokes (`mcp/tests.rs:345` extended); the instructions list it with the other human acts (`mcp/mod.rs:97`) |
| autoplay | `[autoplay] confirm_node_losses = true` beside `confirm_rule_declarations` (`eval/proxy.py:44-48`, `:339-370`): reads each presumed loss (`read_s`), confirms it, the note saying scripted. With D1 the confirmer is op-2, a scripted signer with engagement authority; op-auto has none (`eval/situation.py:66-67`) |
| custody | no `track` order on a contact exists (`Task::Track` points an antenna tracker at one of ours, `intent.rs:116`): custody is an `identify` order (rules 21 to 23, any live track from rules 22). The playbook gains a play: when the vehicle that held a declared or suspect track goes silent, propose an identify by the nearest capable sensor; the watch officer wakes on `node.presumed` |

**As built (stage 5, bd murmur-zb1d.5; `crates/murmur-bridge/src/decision/nodes.rs`).** All of it only
under rules 30: a rules 29 bridge answers the node routes 404 (nothing opened or logged) and its review,
inbox, alerts and agents' reads are as before. `Engine::step_nodes` runs on the tick after the vehicle
events are logged (a struck report is evidence that tick) and before its decisions; the same tick takes
each silent vehicle's region (stage 2's `Engine::silent_regions`, a vehicle confirmed lost left out) and
keeps it until the next, for display only. A review item's `since` is the engine's milliseconds of the
vehicle's last position report, what the confirmation sends back; it also carries `since_t`, `silent_s`,
`last_position`, `needs_note` (nothing presumes it), `would_void`, `would_end` and `would_close` (what
confirming would end now), `refused` (why the engine would refuse it now) and `flagged`. Two routes the
table above implies: `POST /api/review/nodes/{uid}/flag` {note} puts a silent vehicle on the list by hand
and `.../dismiss` takes it off (any operator, logged as `operator.action`, deciding nothing), and
`GET /api/nodes` lists each of ours not reporting with its standing and region (the map's SILENT regions,
polled while any is). A request that comes as an agent (its `X-Murmur-Agent` header or a key) is refused
403 on every node route, whatever operator it names. The passkey act is `{"act": "node_lost", "uid",
"since"}`. A confirmation stops the vehicle's op in the task layer (shown superseded, never reported to
the engine after it ended it `lost`), stops the restore of its closed values, and closes its `lost_link`,
`restore_pending` and `may_be_running` alerts behind a `node_lost` notice; a loss revoked by a report
raises `node_reported_again` for five minutes and owes its closed values back (restored when it
answers), an operator's revocation the same without the alert. The bridge's snapshot gives a manifest
vehicle not yet heard its platform, callsign and system id, so a carried loss holds from the start.
`standing` on `get_fleet` (compact: its state) and `get_vehicle` carries the region too; the
instructions add a paragraph under rules 30 only, so the tool lists and the rules 29 instructions do not
move.

**As built (stage 6, bd murmur-zb1d.6).** Autoplay: `[autoplay] confirm_node_losses = true`
(`eval/proxy.py`; every situation that sets `confirm_rule_declarations` sets it) reads the review
once a look, after the declarations, and confirms each `node_losses` item whose `state` is
`presumed_lost`, once a silence (its uid and `since`), after a `read_s` draw, as op-2
(`eval/situation.py` `LOSS_CONFIRMER`), with the item's `since` and a note saying it is scripted; a
silent vehicle flagged by hand asks a judgment and is left alone. The headless operator
(`eval/headless/operator.py`) mirrors it with the same draws in the same order
(`tests/test_operator.py`). Without the setting the operator's recorded params are as they were.
The panel, `situation.jsonl` and the summary tell `node.presumed`, `node.lost`,
`node.presumption_cleared` and `node.loss_revoked` from the audit log (`EngagementChain`), and the
start record says the rules asked for (`rules_asked`): `MURMUR_RULES` reaches the bridge from `make
situation` as it is. Custody is the playbook's `isr.custody`, off by default (the shipped config and
its hash unchanged) and on in hostile-act-air-to-air: an identify on a track declared or shown
hostile or suspect whose holding camera or sonar went silent, while the track is in view and no
sensor of ours holds it (a reporting camera with the identity settled at 0.6; a far camera that
cannot identify it does not), by the unarmed EO/IR soonest there, which may be on someone else's
order (the identify gives it back); its premise goes with the track out of view, the holder heard
again, or custody held again. The watch officer reads `get_inbox`'s review every 5 s
(`Poller.node_losses`; the change feed has no node items): `node.presumed` and `node.loss_revoked`
wake it, a presumption cleared and a loss confirmed are notes; under rules 29 it reads once and
stops. The headless officer (`eval/headless/officer.py`) does not read it yet.

**The check (hostile-act-air-to-air, the contacts' seed 127, `OPERATOR=autoplay AI=playbook`, live,
2026-11-13; rules 30 by `MURMUR_RULES=30`).** The drone fires at HUNTER 2, not HUNTER 1 (murmur-qese), so HUNTER 1 is never struck;
HUNTER 2 is sunk by the first missile in both runs. Times on the situation clock (m:ss, from the
link emulator's start):

| | rules 30 | rules 29 | rules 30, the guard |
|---|---|---|---|
| act observed, the drone declared hostile by rule; op-auto confirms | 1:57; 2:02 | 1:57; 2:00 | 1:57; 2:01 |
| HUNTER 2's struck report | 2:04 | 2:04 | 2:04 |
| presumed lost (an act against it, its struck report) | 2:14 | (no such state) | 2:14 |
| confirmed lost by op-2 | 2:17 | (no such state) | 2:17 |
| nomination (the timeline, FURY 1, AIM-120C) | 3:20 | 3:20 | 3:20 |
| signed by op-2, op-3; dispatched | 3:24, 3:27 | 3:25, 3:28 | 3:24, 3:27 |
| released; assessed | 3:36 (12 s of flight); 4:03 ceased | 3:49 (13 s); 4:18 ceased | 3:47 (14 s); 4:16 ceased |

The chain completes under both rules: HUNTER 2 sunk 3.5 km off reaches no keep-out before the strike
(cruise reach under rules 29, a confirmed loss out of it under rules 30), and HUNTER 1's camera keeps
the drone, so `isr.custody` stays quiet ("held by HUNTER 1 eo_ir"). The release times differ with
where FURY 1 is on its loiter when dispatched, not with the rules. All three audit logs replay with
their anchors, every decision reproduced. The first two runs had the custody play without its guard
for a track out of view: after the strike destroyed the drone it previewed an identify on the dead
track five times, which the engine refused each time, proposing nothing; the third, with the guard,
made no preview ("out of view" until the track dropped). The presumption stood about 3 s before
op-2's confirmation (its look, then `read_s`), so a person watching the console sees PRESUMED LOST
only briefly on autoplay.

## 6. Evidence

| signal | what the C2 gets today | honest? | used |
|---|---|---|---|
| silence | its reports stop | yes, and ambiguous | the silent state only, never a presumption |
| `ident.act` against it (`Against::Vehicle`, `ident.rs:205`) | an observed act naming it as the victim | the act is observed; its victim is the truth's at the launch (by-construction shortcut, AUTONOMOUS-NODES 2.7 item 2) | evidence, the shortcut stated |
| its `struck` report | at the truth tick of the impact, off its link (3nkc); a hit only since 84fdaef (aset) | no, until 3nkc is fixed | evidence now that aset is fixed (decision D5) |
| its damage report (`DAMAGE mobility|firepower` STATUSTEXT, rules 25) | over its own link, repeated every 30 s | yes | evidence, and D alone when `mobility` |
| spent by its own effect (`restore.closed` expended) | the engine's record | yes | evidence |
| a weapon track aimed at it (`tracks/weapons.rs`) | display only, never a decision input | yes | not used: the act covers the observed launches |
| another sensor seeing the wreck or the blast | nothing (bd murmur-s7hs) | | a follow-up |
| SITL vehicles | ignore damage: struck, they keep reporting (AUTONOMOUS-NODES 2.6) | | never presumed lost until murmur-y85j stage 8 |

murmur-aset is fixed (84fdaef, 2026-11-07): the confirmation rests on what the person is shown, and
a miss shown as "struck" was false evidence. Leave 3nkc: once fixed, a destroyed
vehicle sends no struck report at all, and the observed act remains the evidence, which this spec
already relies on.

## 7. Rules 30

- **Gate**: `pub const NODE_LOSS_SINCE: u32 = 30;` in `rules.rs` (`:307`, beside `HANDOVER_SINCE`),
  `RULES_VERSION = NODE_LOSS_SINCE` from stage 7 (until then `RULES_NEWEST`, section 8), read through
  `RulesConfig::node_loss()`, never a literal.
- **Decisions that change**: a silent vehicle's region (section 3); a lost vehicle leaves the friendly
  keep-out, the line of fire and the minefield check; readiness refuses a lost vehicle; pending,
  queued and dispatched orders on it end at the confirmation; its owed values close; picture tokens
  carry the lost set.
- **New inputs**: the vehicle fact `lost_link` {action, timer_s, source: read or configured} (the
  bridge reads the firmware's failsafe parameters at first contact and after a reboot, with the job
  `decision/preload.rs` already runs; configured from `sim/links.toml` where the link cannot carry a
  read, as `estimate.rs` does); `WorldSnapshot.nodes_lost`, engine-filled (not `lost`: rules 16's
  `engage.accepted` already logs a `lost` key, and replay tells a log's rules by its keys). Both
  dropped before logging under older rules, as the damage fact is.
- **New events**: `node.presumed`, `node.presumption_cleared`, `node.lost`, `node.loss_revoked`;
  `restore.closed` closure `lost`; `carried_from.lost`. `Engine::step_nodes` runs on the bridge's
  tick beside `step_lays` (`engine.rs:6281`).
- **Replay** recomputes each region, each presumption (evidence in the log within the window, the
  vehicle silent on its logged facts) and each confirmation (authority, not an agent, the step-up in a
  credentials run, silent since exactly `since`, no engagement holding it); flags a decision that
  counts a lost vehicle, one that leaves out a vehicle heard after its loss without a revocation
  logged first, an order dispatched to a lost vehicle, and a carried mark that its earlier log does not
  owe.
- **Scenarios** (`sim/scenarios/engage/`, `tests/rules30.rs`): 137 a silent patroller bounded by its
  route (refused under rules 29, passes); 138 its way home crosses the keep-out (refused, the
  explanation says why); 139 struck and silent: presumed lost, D grows until it blocks; 140 confirmed
  lost: the nomination passes, its queued order is voided, its owed values close; 141 it reports again:
  revoked, it blocks, the picture stops an approved engagement; 142 carried over a restart, and not to
  another vehicle under the uid; 143 nothing known: top speed. Runner keys `confirm_lost`,
  `expect_presumed`, `expect_lost`, `expect_revoked`.
- **Tampers**: `lost_by_agent` (a confirmation credited to an agent's session), `hide_loss_revoked`
  (the revocation cut), `redate_loss` (the confirmation's `since` moved to a later report).
- **Digests**: the golden and paused digests (`eval/headless/tests/test_episode.py`,
  `test_charged_clock.py`) are re-taken for `rules_version` 30 for 29, checking that with that number
  put back each gives the digest before it. No new MCP tool, and `standing` is omitted while reporting,
  so tool lists and recorded reads do not move.

## 8. Stages

| stage | size | files | the claim, and what checks it | after | parallel |
|---|---|---|---|---|---|
| 0. Struck only on a hit (murmur-aset): done, 84fdaef | S | `murmur-bridge/src/tracks/mod.rs`, its truth types | "a miss is never reported as struck": one unit test | | yes |
| 1. Seed | S | `rules.rs` (the gate), `world.rs` (`lost_link`, `WorldSnapshot.nodes_lost`) | none: types only | | first |
| 2. Prediction | L | `engage.rs` `friendly_standing` only, a new `silent.rs`, `keepout.rs` | "a silent vehicle's true position stays inside its region": headless, the 22-type fleet's links cut for 10 minutes with world currents on, every second checked against the stand-ins' truth (their failsafes as the firmware runs them, `standins/src/failsafe.rs`); scenarios 137, 138, 143 | 1 | with 3, 4 |
| 3. States and consequences | L | `engine.rs`, `replay.rs`, `audit.rs`, `rules.rs` readiness, `mines.rs`, `queue.rs`; in `engage.rs` only the keep-out loop and the token check | "only a person confirms, on the silence they saw; a report undoes it before any decision; it carries to the same vehicle only": scenarios 139 to 142, the three tampers; rules 29 logs replay as before (the existing `tests/versions.rs`) | 1 | with 2, 4 |
| 4. The lost-link fact | M | `decision/preload.rs`, `decision/snapshot.rs`, `vehicle.rs` | "the plan read is the vehicle's": one stand-in test per firmware | 1 | with 2, 3 |
| 5. Bridge, console, MCP | M | `api.rs`, `decision/{ident,engage,alerts}.rs`, `tracks/mod.rs` routes, `mcp/`, `web/src/{ReviewSection,VehicleDetails,MapView}.tsx` | "agents cannot confirm": the MCP no-such-tool test, REST 403 for an agent | 3 | |
| 6. Situations and custody | S | `eval/proxy.py`, `eval/situation.py`, `agents/playbook/plays/isr.py`, `agents/watch/wake.py` | none new: re-measure hostile-act-air-to-air's default run (seed 127) on autoplay and report whether the chain completes | 5, 0 | |
| 7. Notebook and digests | S | CLAUDE.md (rules 30), `crates/murmur-decide/README.md`, DECISIONS.md, the digests | | all | |

Stages 0, 2, 3 and 4 run as parallel agents in their own worktrees once stage 1 lands: their files are
disjoint but for `engage.rs`, where 2 owns `friendly_standing` and 3 touches only the keep-out loop and
the token check.

**Building rules 30 before it is the default.** Until stage 7, `RULES_VERSION` stays 29 and
`RULES_NEWEST` (`rules.rs`, beside it) is 30, so every run, fixture and digest stays on rules 29 and
`make check` passes on the branch at every stage; rules 30 is decided only when asked for. A scenario
designed for it says `min_rules = 30` (the runner decides under the newer of it and the current rules
unless `--rules N` says otherwise, so the suite's sweeps run 137 to 143 under 30 and the rest under
29); a test sets `policy.rules.rules_version = NODE_LOSS_SINCE` (a bridge rig: `rig_with`) or
`RunOptions::rules_version`; a live or headless stack takes `MURMUR_RULES=30` (the bridge refuses any
number but 29 and 30, and `eval/headless/sim.py` passes it on from the caller's environment). Replay
accepts a log of any version up to `RULES_NEWEST`, by default too, and flags a newer one; its markers
catch a log claiming 29 that carries `lost_link` or `nodes_lost`. Gate every change on
`RulesConfig::node_loss()`, never on `RULES_VERSION`, and gate on it too what the bridge itself adds
(the lost-link read, alerts, review items), so that a rules 29 run, the golden and paused episodes
among them, stays byte for byte: no stage before 7 adds a v30 fixture or re-takes a digest, and a
digest that moves on the branch is a bug. What rules 29 did with a timeline is `--rules 29`
(`RunOptions::rules_version: Some(HANDOVER_SINCE)`). Stage 7 sets `RULES_VERSION = NODE_LOSS_SINCE`
and `RULES_NEWEST = RULES_VERSION`, adds the v30 fixtures and re-takes the digests; `min_rules` and
`MURMUR_RULES` then stay, inert until the next version is built the same way.

**As built (stage 7, bd murmur-zb1d.7).** The flip moved what rules 30 decides differently: scenarios
53, 55, 57, 59 and 60 (rules 6's lost quadcopter, hovering still when its link went) no longer
blocked, since rules 30 counts a silent vehicle that was still holding where it was; each now drifts
at 2 m/s when last heard, so nothing places it and it blocks at its top speed as it blocked at its
cruise reach, ending as designed under rules 30 and `--rules 29`. Tests written against rules 29's
cruise reach name `HANDOVER_SINCE`. The scenario runner's start reports (stage 2) followed rules 7: a
lay order's start is not logged ahead (scenario 128 found it). The v30 fixtures are scenario 01 (as
rules 29 decided it) and 140 (a confirmed loss, caught relabelled rules 29). The golden and paused
digests moved by more than the version number, all of it from stage 4's read: with the read left out
rules 30 gives every record of rules 29's but `run.started`'s `rules_version`, and the read's
parameter packets move the emulator's draws on each link, so the Pumas' failsafes behind terrain
fire at other times (docs/HEADLESS.md 17.2).

## 9. Decisions for the owner

Approved 2026-11-13: the default of each of the six below.

1. **Who confirms a loss.** Default: an operator with engagement authority, re-entering the credential
   in a credentials run (as an un-spend), because it removes a friendly-fire protection from every
   engagement. Then autoplay's confirmer is op-2, not op-auto. The alternative is any operator, as a
   write-off.
2. **A loss with no presumption.** Default: allowed, with a note saying why (a crew's report, a
   picture from outside); the evidence reads "the operator's own". The alternative allows a
   confirmation only on the engine's presumption.
3. **The speed when nothing is known.** Default: top speed (catalog `max_mps`, else the domain's), as
   the direction says. It is stricter than today's cruise speed, and rarely reached, since an order, a
   circle or stillness gives C. The alternative keeps cruise.
4. **Where the lost-link plan comes from.** Default: read from the vehicle (a GCS reads its vehicles'
   parameters), configured only where the link cannot carry a read, the source logged. The alternative
   is configured everywhere, as the display estimate does: cheaper, but it trusts that the vehicle is
   set up as configured.
5. **Drift, and the struck report.** Default: a fixed bound per domain in code now, and tidewater's
   current sampled at the last position and logged per decision as a follow-up; fix murmur-aset before
   stage 3 and count `struck` as evidence only after it. The alternatives: sampled currents from the
   start (honest, larger), and counting `struck` as it is.
6. **Spent and lost.** Default: kept separate. Spent governs tasking after a platform's own effect;
   a spent platform that is silent is presumed lost with "spent by its own effect" as evidence. The
   alternative makes spent imply lost, which would drop an un-spendable platform from the keep-outs
   without a person.
