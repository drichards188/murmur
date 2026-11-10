# Commanding a swarm: one order, many members

> **Shelved draft, not approved (the owner, 2026-11-10).** Saturation was meant as a performance
> test, not a design of group command. What Murmur calls a swarm is a group of independent
> vehicles managed together; a coupled swarm (members coordinating on board over vehicle-to-vehicle
> links) is not modelled. This draft keeps the analysis and the owner's answers so far for when the
> work is picked up. Its rules numbers are provisional (NODE-LOSS is rules 29 again). The faults it
> found are recorded in bd murmur-3kqi; the shelving in bd murmur-5wxc.

A spec for approval before anything is built (bd murmur-5wxc; the owner, 2026-11-10: "I think we
need to really think through how to implement swarms"). It designs the follow-up DECISIONS.md
"Swarm units" (2026-11-01) left open ("a distributed task, each member its own lane or slot, is a
follow-up"), answers what bd murmur-3kqi found running Saturation, and designs a group declaration
of a hostile swarm. It adds decision rules 29 (`GROUP_ORDER_SINCE`; the owner, 2026-11-10: built
before docs/NODE-LOSS.md, which becomes rules 30), the group declaration under a gate of its own
(`HOSTILE_GROUP_SINCE`, section 7.9) and policy schema 8. Nothing here is built yet. Every
invented number is marked GUESS.

## Decisions for the owner

0. **Who is in a swarm. Decided (the owner, 2026-11-10): fixed by the fleet file for a run.** A
   swarm is its `[[swarm]]` table (`sim/swarm.py`); nothing creates, edits, splits or merges a
   swarm while the stack runs, and `/api/swarms` stays read-only. Orders go to any selection of
   vehicles (a whole swarm, part of one, members of several); a swarm's name is a way to pick its
   members, never a decision input. Managing membership at run time is left for later.
1. **What a swarm order is.** Recommended: **a group order.** The person, or an agent, proposes
   one intent to many vehicles; the engine lays out one share per member (its slot, layer and
   way there), checks each share, and the person decides once: the group's checks, policy
   decision and approval cover its **member orders**, each a one-vehicle intent dispatched under
   that approval. Why: every per-vehicle mechanism Murmur has (the queue and its re-checks,
   requeue on a lost link, the start logged ahead, carry-over after a crash, supersession, an
   engagement's protection, NODE-LOSS's voids) is built and replayed for one-vehicle intents, so
   under a group order each works per member unchanged and one member's stale fact or failsafe
   stops being its swarm's. Alternatives: (a) today's single intent, all or nothing (section 1);
   (b) one intent holding a state per member, which needs a per-member state machine inside the
   queue, the requeue, the resume and replay, all written for whole intents.
2. **Who lays out the members.** Recommended: **the decision engine**, as a pure function of the
   members' logged facts and their types' catalog figures, at the first check and at every
   re-form (decision 4), so each member's slot, altitude and route is checked, logged and
   replayed. Today the task layer invents them after the checks, and no check sees a quadcopter
   sent to 591 m or an AUV to 238 m (section 1). Alternatives: the task layer (today); the
   vehicles allocating among themselves, as fielded swarms are reported to (section 12): Murmur's
   link emulator carries no vehicle-to-vehicle traffic, so it would be a fiction, and the checks
   would judge an envelope, never a member.
3. **Members that can't take the order.** Recommended: **excluded or waiting, each with its
   reason, against a minimum strength the order states.** A hard reason (cannot do the task,
   spent, lost, propulsion damaged, short of reserve, held by an engagement) excludes the
   member; a soft one (stale facts, out of link) makes it wait in the queue and join if it passes
   its checks within the order's wait. The order is refused only when fewer than its minimum can
   take it. The proposer sets the minimum; default 80 % of the selection (GUESS), one member for
   hold and rtl. Alternative: today's refusal of the whole order.
4. **A member that drops mid-task. Decided (the owner, 2026-11-10): the swarm re-forms itself.**
   The rest spread back over the same task, under the original approval, logged; it stops and
   asks the person only below its minimum. What the person approves is the **envelope** (task,
   geometry, spread, band, minimum, the selection), not the exact slots. Designed in section 4.3.
   Still open: the re-form terms (every figure GUESS), and whether a suspended swarm resumes on
   its own once enough members are back. Recommended: **it does not**; below its minimum the
   person re-forms it (one click), since a swarm that fell below its own minimum has met the
   condition the person set for being asked. Alternative: resume after 60 s back above it
   (GUESS).
5. **Alerts.** Recommended: **fold a swarm's member alerts into its swarm card from the first
   member** for kinds that ask nothing about that one vehicle (lost link, left task, not moving,
   task failed, low battery), plus two group alerts, `under_strength` (below its minimum, the
   re-forms stopped) and `reform_refused` (a re-form would have left the envelope or raised the
   tier); kinds that ask for an act on one vehicle (struck, damaged, may be running, restore
   pending, a way home across land or a minefield, identify outcomes, a presumed loss) keep their
   own card. Display only, never logged. Alternative: today's fold at 3 members or 5 %
   (`swarms.rs:27-29`), which leaves up to four loose cards per hundred-member swarm.
6. **Engagements.** Recommended: **the chain does not change.** A member is nominated as any
   platform is (one platform, one track, two signatures), and a group order or a re-form never
   stops an engagement: a member held by one is excluded and said so, where today an operator's
   own order to it supersedes the engagement (DECISIONS.md:712-717). A salvo or a swarm strike,
   one plan for many platforms, is left for its own spec.
7. **New kinds.** Recommended: **`transit`** (go along a route and hold at its end; for a group,
   stations along it or a block round its end) with the group orders, since a relay chain and
   moving a swarm somewhere both need it; **`search`** (an area swept in lanes sized by the
   members' sensors) and a **group identify** (tracks paired with members) later.
8. **A stopgap for Saturation now. Decided (the owner, 2026-11-10): the hold fix only**, as stage
   0 (arm an airborne fixed wing before its LOITER, section 1 fault 3).
9. **Reactions to threats. Decided (the owner, 2026-11-10): orders only, for now.** A member does
   its order and its type's failsafes, nothing else: no evasion, no self-defence, no fire of its
   own. Self-defence reactions are a later spec.
10. **Declaring a hostile swarm** (section 7; the owner chose to design it now). Recommended:
    - **the engine proposes groups from the picture alone** (tracks of one domain moving together
      over the tracker's last minute), never from the truth's swarm membership, and never
      declares one;
    - **a person with engagement authority declares**, one act on the proposal as shown, a
      step-up in a credentials run, as the Standing Rules of Engagement let an appropriate
      authority declare a force hostile (section 12);
    - **joining needs a person** (batched), **leaving is automatic** and fails closed: a member
      that leaves the group's motion or region, shows a transponder or a neutral identity, or is
      seen too poorly to hold, is protected again at once;
    - **bounded in time**: it expires after 10 minutes (GUESS) unless re-confirmed;
    - **a member is a declared hostile and nothing more**: out of other engagements' keep-outs and
      nominable; each strike stays one platform, one track, two signatures;
    - **its own gate and stages**, at 29 if it lands with the group orders.
    Alternatives: the engine declares a group by rule when members commit acts (rejected: it would
    declare members no sensor saw do anything); any operator declares, as one track today; or no
    group declaration (each of 500 tracks its own declaration).

## 1. The problem

Today a swarm is a group in the fleet file (`[[swarm]]`, `sim/swarm.py:96-133`), named on each
member's manifest entry, summarised by the bridge (`crates/murmur-bridge/src/swarms.rs:193-270`,
`/api/swarms`, MCP `get_swarms`) and folded in the inbox (`swarms.rs:319-383`); neither view is
logged or read by a decision (DECISIONS.md:3374-3377). An order to a swarm is one intent whose
`uids` are all its members, checked, routed and logged as any intent (DECISIONS.md:3366-3372).
Saturation's 36 opening orders are 36 such intents of about 100 vehicles each
(`sim/situations/swarm-saturation.toml:157-432`). What one of them does:

| # | fault | where | at 100 members |
|---|---|---|---|
| 1 | altitude stack | the task layer numbers a tasking's vehicles of each kind (`tasks/mod.rs:290-307`) and flies slot i at base + step x i (`tasks/caps.rs:175-193`), the step at least 2 % of the cruise altitude | X10 (cruise 100 m, step 5 m): 96 + 5 x 99 = 591 m above home; Puma (800 m, step 16 m): 800 + 16 x 99 = 2,384 m over a home at sea level (bd murmur-3kqi saw about 2.3 km); REMUS (-40 m, step -2 m): 238 m deep, about twice the channel at the bridge (GUESS; tidewater's bathymetry has the figure). The altitude is in no order (`Task::Orbit` is a centre and a radius, `intent.rs:105-109`; `intent.dispatched` the task and the uids, `audit.rs:343-374`): no check reads it, the log does not hold it, and replay, whose claim is that what was done is what was approved (DECISIONS.md:2753), cannot bind it |
| 2 | one lane | a patrol gives every member the same waypoints (`tasks/exec.rs:1321-1330`); an orbit spreads members round a waypoint ring by slot (`exec.rs:1286-1290`) but gives fixed wings one LOITER_UNLIM on one centre (`exec.rs:1281-1284`) | 100 Pumas fly one 6.7 km lane in trail, out and back along the same line, 16 m apart in height; a fixed-wing ring is 100 circles round one point |
| 3 | hold | `hold()` sets the hold mode and nothing else (`exec.rs:1494-1515`); a stand-in plane airborne and disarmed glides (`crates/murmur-standins/src/behavior/plane.rs:589-591`), and Saturation's Pumas start aloft, disarmed until their first order (`sim/fleets/swarm-saturation.toml:319`) | a hold sends a search swarm gliding into the sea; a copter on the ground holds on the ground, which is what hold says |
| 4 | stale facts | readiness fails a member whose navigation status is older than the limit (`rules.rs:905-929`, `:1010-1014`), and an intent passes only if every result does (`rules.rs:864-870`) | one member's status 52 s old refuses 100 (docs/load-test.md:398-401); the situation runner retries stale-only refusals six times, 5 s apart, as a stopgap (`eval/situation_run.py:185-201`, `:899-907`) |
| 5 | failsafe | a member that leaves its mission's mode fails its op (`exec.rs:438-456`); the intent fails once its ops are all ended and any one is not done (`decision/mod.rs:2553-2583`) | one member's failsafe RTL on the way out fails the swarm's order although 99 reach the ring |
| 6 | queue (found here) | rules 5 holds an order until every one of its vehicles is reachable (`queue.rs:468-470`, `engine.rs:5260-5268`) | one member behind a hill keeps 99 waiting up to 900 s (`sim/policy.toml:103`), then the order expires |
| 7 | routing (found here) | the policy's facts are "some vehicle" (`sim/policy.toml:25-30`) | right for a group (any armed member makes it a person's), but a member left out must not count, since it is not sent |
| 8 | engagements (found here) | an operator's own order to an engaging platform runs at once and supersedes the engagement (DECISIONS.md:712-717) | a click on a swarm with one engaging member stops that engagement as a side effect |
| 9 | scale | | the decision tick reached 1.6 to 3.2 s while the 36 orders went out (docs/load-test.md:402-403); alerts grew to about 2,000 with the fleet idle (`:374`) and the inbox held 2,217 when the host swapped (`:409-415`); an approval card lists every member and every check result (`web/src/InboxPanel.tsx:309-330`, `:353-360`), some 700 lines for a hundred members |
| 10 | hostile swarms | a track is exempt from another engagement's keep-out only when declared hostile by an operator or by a confirmed rule (`engage.rs:3913-3920`, `ident.rs:537-541`), and the rule declares one track on one act attributed to it (`ident.rs:1-60`) | Saturation's 500 hostile drones are 500 protected unknown air tracks, each counted anywhere it could fly by the effect (rules 14), so the reserve's strike on the shooter stays blocked until about 11:30, when all five swarms are 7 km off (`sim/situations/swarm-saturation.toml:93-94`) |

The live run directory named for this task (logs/situations/situation-swarm-saturation-
20261101T145856Z) was stopped 65 s in, before the situation ran (`situation.jsonl`: SIGTERM, phase
`interrupted`); its audit log holds only `run.started`. The figures above are docs/load-test.md's
runs of the same situation.

## 2. The group order

### 2.1 Shape

Under rules 29 a proposal naming more than one vehicle is a group order; a one-vehicle intent is
exactly what it is today. The proposal gains an optional `group` (defaults in brackets):

```json
{ "kind": "orbit", "params": { "center": [-122.4755, 37.8380], "radius_m": 400 },
  "uids": ["1", "2", "...", "100"],
  "group": { "swarm": "PICKET-1", "spread": "ring", "min_members": 80, "alt_m": 100,
             "wait_s": 120, "reform": true } }
```

- `swarm`: a label for the console and agents [none]. Never a decision input: the bridge may
  expand `{ swarm = "PICKET-1" }` into uids from its manifest, as the situation runner does
  (`eval/situation.py:853-865`), and the engine logs the uids (decision 0).
- `spread`: how the members share the task (section 3) [per kind].
- `min_members`: the minimum strength [ceil(0.8 n), GUESS; 1 for hold and rtl].
- `alt_m`: the layer base, depth for subs [each type's cruise altitude, as `mission_alt` uses].
- `wait_s`: how long a waiting member may join [the queue's wait for the kind, at most 120 s
  under a person's approval, as `human_expire_s` caps it].
- `reform`: whether the group re-forms itself when members drop (section 4.3) [true; hold and rtl
  never re-form].

The **envelope** is what the person approves: the task and its geometry, the spread, the band
(from `alt_m` and the members' types), the minimum, the selection (`uids`: a re-form never adds a
vehicle outside it) and `reform`. A member order is an intent with one uid, its `parent` (the
group's id) and its share as the task's own parameters: `alt_m`, and where the spread needs them
`phase_deg` (where it enters a ring) or `entry_m` (how far along a route it enters). The task
layer flies exactly that.

### 2.2 Its life

| step | the engine | logged |
|---|---|---|
| proposed | validates the spec (as today) | `intent.created` with `group` |
| laid out | the share of each member (section 3), from the members' facts and catalog figures | the layout in `check.result` |
| checked | every vehicle rule on each member's own share (capability, readiness, limits, domain, geofence, reserves, and the new depth and separation rules); each member classed sent, waiting or excluded (section 4); the new `strength` rule: sent plus waiting at least `min_members` | one `check.result` for the group |
| routed | the policy once, on the members sent or waiting (an excluded member never counts) | `policy.decision` |
| approved | automatic, the submitter's own (DECISIONS.md:250-257), or one inbox card; the approval covers the envelope, and the card shows the first layout inside it | as today |
| dispatched | one member order per member sent or waiting: `intent.created` {parent}, `intent.state` `under` (proposed to approved, citing the group's approval), then its own dispatch or queue (rules 5) | the group goes dispatched |
| running | each member order lives as a one-vehicle intent does today. A waiting member whose re-check fails, or whose own facts would raise the tier (an armed member under an unarmed group's approval, a route leaving the approved area), is voided and reported on the group, never sent to the inbox on its own | `intent.state` |
| done or failed | when every first member order has ended: done if at least `min_members` reached their shares, else failed, the detail counting each end ("94 on station, 3 never sent (waited 120 s), 2 left the task (failsafe RTL), 1 superseded"). A member taken by a newer order counts neither way (the minimum drops by one for each); a group all of whose members were taken is voided as superseded, naming the newer order (DECISIONS.md:2546) | `intent.state` |
| in force | a done group stays in force for re-forms (4.3) until every member has a newer order that is not its, or it is suspended below its minimum | `group.reformed`, `group.suspended` |

**The approval covers the envelope, not the slots.** Inside the envelope the engine lays the
members out at the first check and lays them out again at a re-check (an approval's, the
queue's) or a re-form, each layout checked and logged as the first is; anything outside it (a
layer outside the band, a share outside the approved area, another tier) goes back to a person.
This differs from a signature, which freezes one exact plan (DECISIONS.md:297-310): a signer
releases a weapon, while a group order moves our vehicles inside limits the person set, and the
checks judge every share before it flies. Below the minimum at any check the group is refused,
saying how many still pass.

### 2.3 Who decides what

| level | who | what |
|---|---|---|
| the swarm | the person; an agent proposing | the task and its geometry (ring, route, area, end point), which vehicles, the spread, the minimum, the altitude when not the type's own, whether it re-forms; approving an agent's group order; re-forming a suspended swarm; recalling (a hold or rtl to the swarm, one order) |
| each member's share | the engine, deterministically | which member takes which slot, lane or station, at the first layout and at every re-form; its layer within its type's envelope; its way there (rules 22's planner); whether it is sent, waits or is left out, and why |
| how a member flies its share | the vehicle, by its type | hover or circle, its turn radius, its hold mode, its lost-link and battery failsafes; nothing else (decision 9) |
| per member, a person, unchanged | whoever the act's authority requires | a nomination and its signatures; accepting a lost friendly or a blocker; confirming a loss (NODE-LOSS; batched, 4.4); un-spending; writing off; a lay |

This is the self-contained-nodes principle (docs/AUTONOMOUS-NODES.md:7-12, approved 11-06) on our
side: the order says what, the type says how. What it does not do is let a swarm choose its own
slots, even when it re-forms: the engine does that, centrally. Three reasons: the link emulator
links each vehicle to the ground station and carries nothing between vehicles
(`sim/fleets/swarm-saturation.toml:32-35`), so on-board consensus would have no channel to run
on; the domain rule, the geofence and the reserves judge a member's own altitude, route and
endurance, which an envelope does not fix; and replay could not bind what each member did to
what was approved. Allocation on board needs vehicle-to-vehicle links modelled first.

## 3. Laying out the members

### 3.1 Assignment

The engine computes the slots first (3.2), then pairs members with slots by minimum total
distance (the Hungarian method: about a million steps for 100 members), ties broken by uid, so the
same facts give the same layout. In a minimum-total-length pairing of points in a plane no two
straight paths cross (exchanging the ends of two crossing paths would shorten their sum), so
members leaving a launch block do not cut across each other; paths can still converge near their
slots, which the separation rule judges (3.3). Members are laid out per domain: air, surface and
subsurface never share a layer. Within a domain each kind keeps its own band; where two kinds'
bands overlap, the larger minimum separation applies to both. A re-form lays out by the same
function with one more input, which members may move (4.3).

### 3.2 By kind

| kind | spread | the slots | layers | notes |
|---|---|---|---|---|
| orbit | `ring` | n points evenly round the circle; each member enters at its own and flies the ring (fixed wings too: a waypoint ring no tighter than its turn, instead of one shared LOITER_UNLIM) | one layer when the spacing 2πR/n is at least the type's lateral minimum, else k = ceil(minimum / spacing) layers, neighbours on the ring in different layers; on the surface, concentric rings the lateral minimum apart within 25 % of the radius (GUESS) instead | members of different speeds drift along the ring (a fixed wing in wind): shown, and evened out when a re-form runs for a drop |
| patrol | `along` | members evenly along the closed route by distance, each entering at its point; a two-point route becomes a racetrack whose legs are two turn radii (or the lateral minimum) apart, so members going opposite ways never meet head on | as orbit | Saturation's lanes and today's relay chain |
| patrol | `lanes` | m copies of the route offset sideways by the order's lane spacing, members split evenly over them, then along each | as orbit | a search band, short of a search kind |
| hold | `here` | none: each member holds where it is, by its type (`tasks/caps.rs:62-75`) | where it is | a safety command, never refused for its layout and never re-formed; overlapping fixed-wing circles at one height are a warning (3.3), as an rtl across land is (rules 17) |
| rtl | `home` | none: each to its own home | its RTL altitude | warnings only; never re-formed. A launch block's members share an RTL altitude and converge: staggering it is the fleet's set-up of each vehicle's own parameter (`sim/swarm.py`, GUESS 5 m apart for copters), not an order |
| transit (new) | `stations` | n points evenly along the route; each flies there and holds | one layer when the spacing fits the type's hold (a copter: its lateral minimum; a fixed wing: its loiter circle, two turn radii), else layers | a picket line or a relay chain |
| transit | `block` | a grid round the route's end, ceil(√n) columns, the lateral minimum apart | one layer | moving a swarm somewhere |
| search (later) | `lanes` | back-and-forth lanes over an area, spaced by the order or by the members' sensor reach x 0.8 overlap (GUESS); each member a contiguous block of lanes, flown as its own patrol | as orbit | |
| identify (later) | `pairs` | k tracks paired with k members by the assignment on time to standoff; each pair a member identify (rules 21 to 23 unchanged); the rest keep their orders | | k looks on one approval |
| track | | an antenna tracker pointed at one of ours (`intent.rs:116-119`): no group meaning | | refused for a group |
| engage, lay | | one platform each (`intent.rs:513-523`; rules 27) | | refused for a group (section 6) |

### 3.3 Envelopes and separation

New catalog figures, logged platform facts as the loadout is: `alt_band_m` (air, above the home's
ground), `depth_band_m` (subs), `sep_m = { lateral, vertical }`. Where an entry says nothing, a
default by autopilot kind:

| Saturation's types | lateral | vertical | band | basis |
|---|---|---|---|---|
| copter (X10) | 15 m | 10 m | 30 to 120 m above ground | GUESS; 120 m is the US small-UAS ceiling (400 ft, 14 CFR 107.51) for scale, not a rule Murmur follows |
| small fixed wing (Puma) | 100 m | 30 m | the order's altitude ± 150 m | GUESS; a Puma stand-in loiters at 150 m (`standins/src/params.rs:157`) |
| USV (Corsair, Saildrone) | 30 m, 50 m | none | the surface | GUESS |
| AUV (REMUS) | 50 m | 5 m | 10 m below the surface to 10 m above the seabed at the slot | GUESS; the seabed from the terrain oracle's water depth (`murmur-decide/src/terrain.rs:18`) |

Two new rules. **`depth`**: a sub's layer clears the seabed margin at its slot and along its share,
on the oracle's samples (logged as rules 17's are). **`separation`**: the layout itself puts no two
members of one domain within both minima on their task paths; if it cannot (more members than its
layers or rings hold), the group is refused with the most that fit ("a 150 m ring holds 93 USVs on
three rings, at 120, 150 and 180 m; 100 do not fit"). Converging transit legs within both minima at
overlapping times (estimated at cruise speed, ± 10 s, GUESS) are a warning, never a refusal; hold
and rtl get warnings only. Murmur has no collision model (stand-ins and SITL vehicles fly through
each other), so separation is a claim the engine checks about its orders, not one a simulated
collision tests.

Saturation laid out under these figures:

| order | n | spacing | layout |
|---|---|---|---|
| PICKET-1, ring 400 m, X10 | 100 | 25 m | one layer at 100 m |
| PICKET-6, ring 300 m, X10 | 100 | 19 m | one layer |
| SURFACE-1, ring 700 m, Saildrone | 100 | 44 m | below 50 m: two rings, 650 and 750 m, 50 members on each, 82 and 94 m apart |
| UNDERSEA-1, ring 300 m, REMUS | 100 | 19 m | three layers, 40, 45 and 50 m deep, 57 m apart in each, if the channel is 60 m deep or more all round the ring (the depth rule's to say) |
| SEARCH-1, lane 6.7 km, Puma | 100 | 143 m along a 14.3 km racetrack, legs 300 m apart | one layer at 800 m |
| RELAY-1, 4.8 km of the shore, Puma | 100 | 48 m | as stations (transit): 300 m loiter circles need 7 layers 30 m apart, 180 m of the 300 m band: allowed, and the card says so |

## 4. Members that can't take it, and members that drop

### 4.1 At the check

| the member | class | joins later |
|---|---|---|
| passes every check on its share | sent | |
| navigation status or position too old (`rules.rs:905`) | waiting | if it passes its checks within `wait_s` |
| not reachable for this order (rules 5, `queue.rs:251-260`) | waiting | the same |
| silent (NODE-LOSS) | waiting | the same |
| cannot do the task (`rules.rs:937`) | excluded | no |
| spent (rules 12, 15) or confirmed lost (NODE-LOSS) | excluded | no |
| propulsion damaged (the `damage` fact, rules 25): new for groups; one vehicle's own order is as today | excluded | no |
| short of its reserve on its share (`reserves`) | excluded | no |
| its share crosses the wrong surface, a no-go area or a minefield with no route (rules 17, 22, 27) | excluded | no |
| its layer under the seabed margin (new `depth`) | excluded | no |
| held by an engagement pending, approved or under way | excluded, "held by engagement #E" | no |
| a fixed wing on dead reckoning (`Unverified`, `rules.rs:1038-1043`) | sent; the group goes to a person (as today) | |

A member in its own lost-link failsafe is none of these: an order after a jam is how the person
brings it back (Saturation's director note). The class is typed in the check's result (a stale
fact is its own outcome detail), never read from the text as the runner does today.

### 4.2 Mid-task

A member **drops** when its member order ends not done (failed, lost, voided) or, its share
reached, its facts put it off its share (4.3). A swarm in force answers drops by re-forming (4.3);
below its minimum it stops and asks. A drop of fewer members than a re-form needs leaves a gap,
shown on the swarm card. Once a group is no longer in force, a member leaving is the swarm view's
business: "PICKET-1: 23 of 100 left the task (failsafe RTL; jam 23)".

### 4.3 Re-forming

The engine steps every group in force on the bridge's tick (`Engine::step_groups`, every 5 s,
GUESS, beside `step_lays`, `engine.rs:6149`), on the vehicle facts it then logs if anything
changes. Every figure below is a GUESS for the owner to tune.

| term | the rule |
|---|---|
| off its share | its last report older than its link's stale limit (the queue's reach rule, `queue.rs:251`), or its fresh position farther from its share's path than three lateral minima (a fixed wing: its loiter circle and one lateral minimum), for 20 s running; 20 s is longer than Saturation's 10 s lost-link timer, so a member its failsafe sends home has gone before it counts, and a brief gap in its link does not count |
| back | a member that dropped and is heard, with fresh facts, for 30 s running, and passes its checks for a share; it rejoins at the next re-form, never by itself, and is shown "back, waiting to rejoin" until then |
| material | a re-form runs only when at least max(2, 5 % of the selection) members dropped or came back since the last layout (5 of 100); fewer leave gaps |
| settled | and no member dropped or came back in the last 15 s, so a jam that takes a swarm's links over half a minute makes one re-form, not ten |
| rate | at most one re-form per group a minute |
| who moves | the new layout is over the members on their shares and those back. A member whose link is not healthy now (not reachable for a mission upload, rules 5) keeps its slot: no upload goes into a link that just failed. Among the rest, the layout moves as few members as it can while the largest gap stays under 1.5 times the even spacing of the members laid out, and a member is re-tasked only when its slot moves by more than half that spacing. A ring re-formed after 10 of 100 drop moves the neighbours of each gap, not the whole ring |
| re-checks | each moved or returning member's new share passes every vehicle rule (capability, readiness, limits, domain, geofence, reserves on the endurance it has left, depth); the new layout passes `separation`; the group passes `strength`. A member whose new share fails keeps its old one, or stays out if it was back |
| the envelope | the same task, geometry, spread, band and selection; a share needing a layer outside the band or a way out of the approved area is never laid out |
| the tier | the policy decision over the re-formed set is computed as the first one was; a re-form whose tier or unverified checks differ from the approval's is refused |
| refused | nothing moves; `group.reform_refused` {why} and the `reform_refused` alert; the members on their shares fly on; the person decides (a new order, or Re-form) |
| below the minimum | members on their shares and back fewer than `min_members`: no re-form; `group.suspended` and the `under_strength` alert; the group stops re-forming until the person acts: **Re-form** on the swarm card (a new group order, the same task to the members heard, approved at submission) or a new order |

**Logged.** `group.reformed` {group, seq, cause: each member that dropped (why, since) and each
back, the vehicle facts of every member considered (as a check's inputs), the new layout (each
share), the members moved (each with its new member order's id) and kept, the largest gap against
the even spacing}, then for each member moved or back a member order as at dispatch (`intent.
created` {parent, reform seq}, `intent.state` `under`, its dispatch or queue); the member order it
replaces ends superseded by it, as any newer order does. `group.reform_refused` and
`group.suspended` carry the same inputs. Replay recomputes each from its logged inputs: the drops
and returns were what the facts show, the settle and rate limits held on the logged times, the
layout and the members moved are what the layout function gives, each new member order is its
share, and the tier is the approval's. It cannot show that a re-form was due when none was logged:
a missed re-form is a gap left open, never an act nobody approved.

**Saturation.** The jammer takes PICKET-1, -7 and -8 whole (300 X10s, 4:00 to 7:00): each falls
below its minimum, is suspended, and raises one `under_strength` card; after the jam the person
re-forms each with one click, which is what the director's note asks of a good watch. The USV the
shooter strikes is one of 100: a gap, no re-form. Smaller, scattered drops (a member's battery,
a terrain shadow) are what the re-form is for; the headless run adds them (section 11).

### 4.4 Losses (docs/NODE-LOSS.md, rules 30)

A silent member waits for new orders and drops from its share's count after 20 s (4.3); NODE-LOSS's
predicted region bounds it in the keep-outs. A confirmed loss ends its member order (`lost`), which
with member orders is just the intent ending (NODE-LOSS section 4: "the intent ends when no vehicle
is left on it"). What a swarm adds is volume: Saturation's two attack swarms (90 one-way drones
each over the Saildrones and SURFACE-5) could leave dozens presumed lost at once if ordered in.
Review groups presumed losses by swarm, and one confirm may list several: one step-up, one
`node.lost` per vehicle, each checked as NODE-LOSS says (silent since exactly the report shown, no
engagement holding it), the refused ones listed. Each vehicle's own evidence is still shown; only
the act is shared. That is a small addition to NODE-LOSS's `POST /api/review/nodes/{uid}/lost`.

## 5. Policy, approvals and alerts

- **One approval.** A group order gets one policy decision and, in a person's tier, one inbox
  card. Its member orders never reach the inbox on their own (2.2), and neither does a re-form:
  one inside the envelope and the tier goes ahead, one outside them is refused and raises one
  alert. An operator's own group order is approved at submission like any of theirs.
- **Routing facts** are computed over the members sent or waiting: `armed` if any of them is,
  `leaves_approved_area` if any share does, `unverified` if any check is. A waiting or returning
  member that would raise the tier is voided or left out instead, so a person's approval never
  covers more than its envelope.
- **Policy schema 8**: `transit` (and later `search`) in `when.kind` lists; the shipped
  `sim/policy.toml` and the watch policy's agent tier (`sim/situations/policies/watch.toml:265-268`)
  add `transit` beside orbit and patrol; the group declaration's keys under `[identification]`
  (7.9). No size condition is proposed: an agent's moving orders already wait for the person in a
  situation, whatever their size.
- **The card** (an agent's group order): a headline ("Orbit at Lime Point, 400 m: 97 of PICKET-1
  now; 3 join if heard within 120 s; re-forms itself above 80"), the envelope and the first layout
  on the map, why it needs a person (once), the checks folded by rule with counts ("readiness: 97
  pass, 3 wait: navigation status stale"), the excluded members by reason, and the members one
  click away (docs/PRODUCT.md, principle 1). It says what approving does: "the engine re-forms the
  swarm inside these limits; below 80 it stops and asks you".
- **Alerts** (decision 5): the fold in `swarms.rs:319-383` gains a threshold of one for the
  routine kinds and keeps the rest unfolded; `under_strength` and `reform_refused`, one per group,
  high, naming the causes. All display only.

## 6. Engagements

**What must not change**: an effect is released only on one frozen plan that two distinct
operators with engagement authority signed, never its proposer, never an agent (DECISIONS.md:258-
268, 297-310); one platform and one track per nomination (`intent.rs:513-518`); one engagement per
platform (DECISIONS.md:682-685); every release judged by its own keep-outs at its own time; an
armed platform's orders always wait for a person (the `armed-platform` tier); agents have no
method to approve, sign, accept or confirm.

**What changes**:

1. Neither a group order nor a re-form supersedes an engagement: a member it holds is excluded
   (4.1) and a re-form keeps it out. To stop the engagement the person orders that platform
   alone, which says it stops it (DECISIONS.md:719-725), or aborts it.
2. A nominated member is held by its nomination; once dispatched, the engagement supersedes the
   member's own member order, as it does any order on that vehicle, the swarm card shows it
   detached ("SKY 812: engagement #E"), and it drops from its group's count.
3. A swarm near a target is one line in the keep-out explanation ("PICKET-9: 14 members inside
   the keep-out at the effect time; nearest SKY 812, 140 m"), grouped by the bridge from the
   manifest, display only; the engine's items stay per vehicle.
4. An armed swarm's ordinary orders go to a person once per group, not once per member.
5. Our members do not react to threats (decision 9): a member under fire does its order and its
   type's failsafes, and a re-form never moves members toward or away from a threat.

**Not in this spec**: a salvo (several platforms on one track) or a swarm strike (several on
several). They turn one human decision into many effects, the question no doctrine found settles
(section 12), and need their own spec. A hostile swarm's declaration is section 7.

## 7. Declaring a hostile swarm

### 7.1 What there is today

- **Declarations** (rules 9, `crates/murmur-decide/src/ident.rs:1-60`): any operator may declare
  any track; the engine declares one track hostile by rule when an observed act against ours is
  attributed to it at the policy's threshold (0.9) with nothing conflicting, and flags what it is
  unsure of; a rule declaration is a review item any operator confirms or revokes, and a revocation
  stops the engagements in play on the track first (`GET /api/review`,
  `crates/murmur-bridge/src/decision/ident.rs:209-228`; `[identification]`,
  `sim/policy.toml:292-295`).
- **Keep-outs**: a track is exempt from an engagement's keep-out only when it is the target or a
  declared hostile, an operator's declaration or a confirmed rule's (`track_exempt`,
  `engage.rs:3913-3920`; `exempt_in_keep_out`, `ident.rs:537-541`). Every other track counts by
  `live_standing` (`engage.rs:3641`): along its velocity with its uncertainty, at its domain's top
  speed when it has no velocity or is out of view (rules 14, 16), along an accepted path when a
  person accepted it as behaving (rules 19). The ledger fills each track's `declared_by` and
  `declaration` before any decision reads the picture (`Ledger::fill`, `ident.rs:1165-1179`), and
  replay checks every picture against it (`check_facts`, `:1203`).
- **Keeping company** is already weak evidence: a track within 300 m of a declared hostile and
  moving with it is flagged and shown suspect, never declared (`escort`, `ident.rs:858-910`).
- **What the picture may read**: our sensors' tracks only. The weapon picture's rule, that nothing
  in it may read the truth's weapon kind, name, speed or victim (docs/OPPONENT-WEAPONS.md:493),
  applies here to the truth's swarm membership. Association is by the simulation's contact id, a
  stated simplification (`crates/murmur-bridge/src/tracks/mod.rs:15-18`): no two drones swap
  tracks. At Saturation's density an active air track was a median 211 to 316 m from the nearest
  drone and its own 1-sigma a median 2.65 km (docs/load-test.md:393-396).

### 7.2 What makes a group

The engine proposes; it never declares. Every step (with the group orders' step, 5 s, GUESS) it
links pairs of tracks and proposes each connected set as a **candidate group**:

| link test (both tracks) | figure (GUESS) | why |
|---|---|---|
| the same domain | | an aircraft over a boat is not company (as `escort`) |
| in view, and their own position 1-sigma under the ceiling | seen within 10 s (`OUT_OF_VIEW_S`); 500 m | a link on a loose fix is a guess |
| no transponder claim (`WorldSnapshot::track_ais`), not declared, not shown neutral or assumed friend at the conflict confidence (0.5, `ident.rs:96-101`) | | a ferry, a yacht, an airliner on its transponder is never proposed |
| identities compatible: either unclassified, or both the same class at the identity threshold | 0.6 (`min_identity_confidence`) | a helicopter is not one of a quadcopter swarm |
| moving: both mean velocities over the tracker's last minute (`mean_velocity_mps`) above a floor | 3 m/s | two things hovering near each other are not moving together |
| together: positions within the link distance now, mean velocities within the link speed | 500 m; 4 m/s | Saturation's swarms fly 35 to 70 m apart (scripts/threat_swarms.py:19-20, :70-127) with track errors of a few hundred metres |
| big enough: a connected set of at least | 5 tracks | fewer are an escort's business |

The one-minute mean is how the test sees co-motion without a track history the engine does not
keep: the tracker already averages it. Proximity alone never links, so a swarm first seen holding
is not proposed until it moves (section 14). The engine has no record of where a track came from,
so a common origin is evidence only while it is happening, and a launch in the weapon picture links
a weapon to its shooter, not tracks to a group: neither is used. A candidate is logged when it
first forms and when its members change (`group.proposed`, the linked tracks' facts as inputs, as
`escort_inputs` logs two), at most once a minute per candidate (GUESS).

### 7.3 Who declares

**A person with engagement authority**, on a candidate as shown: the console's Review list shows
it (its tracks on the map, the links, its motion, how long it has held together, what none of it
is: no transponders, no neutral identities), and Declare quotes the candidate's token (as a
signature quotes a picture token, DECISIONS.md:701-711: refused when the members changed since,
"look again"), with a note and, in a credentials run, a step-up. Why engagement authority: one act
removes the protection of a hundred tracks at once, more than any single declaration; NODE-LOSS
asks the same of a loss for the same reason. The person may leave members out before declaring
(the declared set is a subset of the candidate's, never a superset). An agent sees candidates and
can propose nothing about them (`AgentPort` gains no method). In autoplay op-2, a scripted signer
with engagement authority, declares when `[autoplay] declare_groups` says so (`"after_act"`: the
candidates within reach of a nomination's keep-out, once a member's act is confirmed), its note
saying scripted.

Not chosen: **the engine by rule** (k members' acts attributed at the threshold declare their
group, unconfirmed): acts are rare (Saturation has one, by a lone shooter), and it would declare
members no sensor saw do anything, which is a judgment, not an observation. What the engine does
instead: a candidate with a member declared hostile (by anyone) is shown first, and a nomination
form whose keep-out is blocked by a candidate's tracks says so, as a fact beside the others ("82 of
the blockers are candidate group C-3"), never as advice (docs/PRODUCT.md, principle 2).

### 7.4 Membership over time

| event | effect | logged |
|---|---|---|
| a candidate track joins the group's motion | proposed as a joiner (`group.join_proposed`), never a member until a person with engagement authority confirms; several confirm in one act | `group.joined` {track, its link evidence, by} |
| a track crosses the group's region without moving with it (a ferry under a drone swarm, an airliner passing over) | nothing: region alone never joins | |
| a member leaves the group's motion (moving group: its velocity outside the link speed of the group's mean, allowing its 2-sigma) or its region (farther than the link distance from every other member, allowing its 2-sigma) | out at once, protected again | `group.left` {track, cause, the figures}, logged before any decision reads the picture, as rules 19's lapse is |
| a member shows a transponder claim, or a neutral or friend identity at the conflict confidence, or is declared otherwise by anyone | out at once | `group.left` |
| a member's own 1-sigma passes the ceiling (500 m) | suspended: protected while it holds, back when it is under the ceiling and still passes the tests | `group.left` {suspended}, `group.rejoined` |
| a member out of view | stays a member (nothing shows it left); tested again when seen; dropped by the tracker (180 s), it is gone, and a new track of the same contact is a stranger | `group.left` {dropped} |
| the whole group holds still | the motion test pauses (its mean speed under the 3 m/s floor); the region test goes on | |
| the declaration's time runs out | every member protected again unless a person re-confirmed it, shown the members now, who joined and who left | `group.expired`, `group.reconfirmed` |

The test runs at every step and inside every decision that reads the picture, on that picture,
so a member that left is never exempt in a decision. Leaving is automatic and joining is not: an
error out of the group restores a protection (fail closed), an error into it removes one.

### 7.5 What a declaration does, and does not

**Does**: each member, while a member, is a declared hostile in every decision: out of other
engagements' keep-outs (`track_exempt`) and passing `target_declared` for a nomination. The ledger
fills the member's existing `declared_by` ("group:G-3 (op-2)") and `declaration` (a new `Declarer`
variant, `Group`, carrying the group, the declaration and the member's own inclusion evidence), so
the picture token, the frozen plan and every check carry how this track came to be declared. No
`TrackFacts` field is added, so `tracks/tests/before_oho` stands (CLAUDE.md, rules 16).

**Does not**: release anything, or make a group a target. Each strike stays one platform, one
track (a member), two signatures on one frozen plan, with its own keep-outs; a member the strike
would hit that is not the target is a declared hostile like any other, exempt as another declared
hostile is today (`engage.rs:4030-4032` calls that an open design question; a group makes it a
hundred times larger, section 14). It exempts no track that is not a member, joins no track
without a person, and changes no keep-out of ours. The escort rule names a group once per
non-member it flags, not once per member it keeps company with.

An engagement on a member holds its picture every second as today: if its target leaves the group
(and has no declaration of its own), `target_declared` fails and the engagement voids or aborts
with its fallback; if a bystander leaves and now blocks, the held keep-out fails the same way.
Nothing new is needed for either.

### 7.6 Revocation

Any operator may revoke a group declaration or take one member out, with a note: it only
restores protection, as a revocation of one declaration does. It stops the engagements in play on
the affected tracks first, in the same step (`ident.revoked`'s order, `revocation_bystanders`,
`ident.rs:1244`), then logs `group.revoked` or `group.left` {by}. An individual declaration of a
member (as neutral, say) takes it out of the group.

### 7.7 Audit, replay, tampers, scenarios

- **Events**: `group.proposed`, `group.join_proposed`, `group.declared` {group, by, session,
  step-up, note, the candidate's token, members each with its link evidence, the group's motion
  and region, `expires_t`}, `group.joined`, `group.left` {cause}, `group.rejoined`,
  `group.revoked`, `group.reconfirmed`, `group.expired`; each with the facts of the tracks it
  names as inputs.
- **Replay** recomputes each candidate, each link and each membership test from the logged facts,
  checks that a declaration came from an operator with engagement authority (never an agent, a
  step-up in a credentials run) on a token whose members it does not exceed, and that no decision
  treated a track as a member before its `group.declared` or `group.joined` or after its
  `group.left`, `group.revoked` or `group.expired` (`Ledger::check_facts`, extended).
- **Tampers**: `add_group_member` (a track in a declaration its candidate never held),
  `hide_group_left` (a `group.left` cut, so a later decision exempts a track that had left),
  `group_by_agent` (a declaration credited to an agent's session), `stretch_group_expiry`.
- **Scenarios** (after the group orders', section 8): 153 co-moving tracks proposed, a neighbour
  with a transponder never; 154 declared by an operator with engagement authority, members exempt,
  a strike passes its keep-out; refused for an agent and for an operator without the authority;
  155 a member leaves the motion: protected again, an approved engagement whose keep-out relied on
  it aborts with its fallback; 156 a joiner proposed and confirmed; a neutral crossing the region
  never proposed; 157 a revocation stops an engagement on a member first; 158 expiry and
  re-confirmation; 159 a member declared neutral by an operator leaves the group. Runner keys
  `expect_group_candidate`, `declare_group`, `expect_group_members`, `expect_group_left`.

### 7.8 Saturation with group declarations

The five hostile swarms come in moving (recon from 0:30, wave 0:32, decoy and attack-west 0:50,
attack-east 2:00; `sim/scenarios/swarm-saturation.toml:11-23`), so each should be a candidate
within a minute or two of being tracked, if its tracks are seen well enough (section 14). The
shooter flies with the recon swarm until about 3:30, so it is first a member of recon's candidate
and leaves it when it turns to hover west of the bridge; at 10:00 its act declares its own track by
rule, and the person confirms. The person may declare candidates whenever their judgment says
(the brief warns of hostile swarms; under the Standing Rules a force may be declared hostile with
no act of its own, section 12) or only once an act is seen. With the swarms within reach of the
strike declared, nothing left blocks the reserve but our own vehicles and the harbour traffic,
none near the shooter: the strike could pass within about a minute of the confirmation, near
10:30 instead of 11:30 (GUESS, for the headless run to measure). The brief and the watch policy
(`group_declaration = true`) would say so; the director's note would name what a good watch weighs:
declaring 400 tracks hostile on their behaviour, or waiting.

The headless Saturation measures (section 11): the time from the first act to a nomination on the
shooter passing its keep-out, with and without group declarations; how many members leave or are
suspended (and block again) by the time of the strike; and members wrongly included, which
Saturation cannot show, since it has no air neutrals: a variant adds some (GUESS: a medevac
helicopter crossing the strait, a news helicopter loitering by the decoy, a light aircraft along
the coast, an airliner on the SFO approach), and counts candidates and confirmed members whose
truth is not hostile (evaluation labels only, never an input).

### 7.9 Gate and stages

Its own constant, `HOSTILE_GROUP_SINCE`, read through `RulesConfig::hostile_groups()`: 29 if it
lands with the group orders (as rules 22 held two constants), otherwise the next free version
(31 after NODE-LOSS's 30): the named constant makes that one line. Off unless the policy turns it
on (`[identification] group_declaration = true`; keys `group_link_m`, `group_link_mps`,
`group_moving_mps`, `group_min_tracks`, `group_sigma_max_m`, `group_expire_s`; policy schema 8, the
same bump as `transit`). With it off, no declaration differs from today's. Its code is disjoint from the
group orders' but for `audit.rs` and `replay.rs`, so its stages (G1 to G4, section 13) run beside
them.

## 8. Audit, replay and rules 29

- **Gate**: `pub const GROUP_ORDER_SINCE: u32 = 29;` in `rules.rs`, `RULES_VERSION =
  GROUP_ORDER_SINCE`, read through `RulesConfig::group_orders()`, never a literal.
- **Decisions that change**: a proposal naming more than one vehicle is laid out and checked per
  member; members are excluded or wait instead of refusing the whole; the `strength`, `depth` and
  `separation` rules; routing over sent and waiting members; member orders dispatched under the
  group's approval; a waiting member that would raise the tier is voided, not sent to the inbox; a
  group's outcome by its minimum; re-forms inside the envelope, refused outside it or below the
  minimum; a group order or re-form excludes members held by an engagement.
- **New inputs**: the catalog's `alt_band_m`, `depth_band_m` and `sep_m` (dropped from logs of
  older rules, as the damage fact is); water depth at a sub's slots, from the oracle's samples.
- **New fields and events**: `IntentSpec.group` (with `reform`), `IntentSpec.parent`, a member
  order's `reform` seq; the member task fields `alt_m`, `phase_deg`, `entry_m`; the transition
  `under`; the layout in `check.result`; `group.reformed`, `group.reform_refused`,
  `group.suspended`; rule ids `strength`, `depth`, `separation`; the kind `transit`. All skipped
  when absent, so older logs re-serialize byte for byte (crates/murmur-decide/README.md, "Decision
  rules versions").
- **Replay** recomputes each layout from the logged facts and binds every member order to it (task,
  share, uid); checks that each `under` cites an approved group whose sent or waiting set, or a
  logged re-form, holds the member, and whose tier its own facts do not raise; recomputes each
  re-form (4.3); recounts each group's outcome against its minimum; and flags a member order
  dispatched to a uid the layout excluded or outside the selection.
- **Tampers**: `move_member_slot` (a member's altitude or phase changed after the layout),
  `member_without_group` (a member dispatched while its group waits for approval),
  `admit_raised_member` (an armed member joining an unarmed group's approval), `forge_reform` (a
  re-form whose logged facts show no drop, or within the rate limit), `reform_outside_envelope` (a
  re-formed share outside the band or the approved area).
- **Scenarios** (`sim/scenarios/engage/`, `tests/rules29.rs`; 134 to 140 stay NODE-LOSS's): 141 a
  ring of 10 with one stale member (it waits, then joins); 142 one member's failsafe on the way
  (the group done at strength); 143 below the minimum (refused, counting why); 144 an AUV ring laid
  in depth layers, and one too deep for the seabed (refused); 145 fixed-wing stations too dense for
  the band (refused with the most that fit); 146 a member held by an engagement (excluded, and kept
  out of a re-form); 147 an armed member waiting under an unarmed group (voided); 148 a group hold
  with overlapping fixed-wing circles (sent, warned); 149 3 of 20 drop over 10 s (one re-form after
  the settle, only the gaps' neighbours moved, a member with a failing link kept in place); 150 a
  re-form whose moved share would leave the approved area (refused, the person asked); 151 12 of 20
  drop (below the minimum: suspended, `under_strength`, no re-form); 152 a member back after a drop
  (it rejoins at the next re-form, not before). Runner keys `expect_sent`, `expect_waiting`,
  `expect_excluded`, `expect_group_outcome`, `expect_reformed`, `expect_reform_refused`,
  `expect_suspended`.
- **Log size**, estimated from Murmur's own logs (a vehicle's facts 1.0 KB median, n = 234; a check
  result 0.11 KB median, 0.23 at p90): per member per group order about 2 KB in the group's check,
  1 KB in its policy decision and 3 KB in its member order (created, under, dispatched with its
  inputs, about five op states): some 6 KB, 0.6 MB for a hundred, 22 MB for Saturation's 36 opening
  orders. A re-form logs every member's facts once (about 100 KB for a hundred) and 3 KB per member
  moved. Two savings, both rules 29: the policy decision and a member's dispatch straight from the
  approval cite the group's check inputs by seq instead of repeating them (about half). For scale,
  one 100-minute load test logged 22.6 MB of failsafe `vehicle.event` lines alone
  (logs/audit/20261101T022743Z-186123.jsonl).
- **Digests**: the headless golden and paused digests (`eval/headless/tests/test_episode.py`,
  `test_charged_clock.py`) are re-taken for the new rules and the MCP tool changes, checking that
  with the old number and tool list put back each gives its digest before.

## 9. Scale at 3,600

| what | now | target (GUESS) | how |
|---|---|---|---|
| decision tick while 36 orders go out | 1.6 to 3.2 s for a minute or two (docs/load-test.md:402-403), not profiled | p99 under 250 ms | lay out and plan outside the lock on a snapshot, as space answers are prepared (`PREPARED_SPACE_MAX_AGE_MS`, 5 s), the engine re-verifying under the lock; profile first (section 14) |
| one 100-member group order, the lock held | unknown | under 100 ms | the same |
| the group step (re-forms, candidates) | none | under 20 ms a step for 37 groups and 500 tracks | a spatial grid for the track links (500 tracks, about 125,000 pairs without one); a layout only when a group's drops are material |
| route plans (rules 22) | one per vehicle on a 40 m grid (`plan.rs:42`), computed in the prefetch and again under the lock (`decision/mod.rs:2199-2213`, `:1469`) | once per distinct (start cell, end cell) | the planner is a function of its cells, so sharing plans is exact, not an approximation |
| telemetry | 44,000 packets a second handled (docs/load-test.md:363, :405) | unchanged | |
| mission uploads | 100 per order, each at the link's measured pace (`link.rs` `LinkTiming`) | a re-form uploads only to the members it moves, on healthy links | 4.3 |
| the console's decisions feed | every intent with its checks | group orders with counts; member orders on request (`/api/intents?parent=41`) | 3,600 member orders at about 2 KB each would be 7 MB a fetch |
| the inbox | about 2,000 alerts idle, 2,217 when the host swapped | a few cards per swarm event | decision 5 |
| the log | section 8 | about 6 KB per member per order, half with the savings | |

## 10. Agents and the console

**Agents (MCP).** `preview_intent` and `propose_intent` take `group` and `swarm` (expanded by the
bridge, the uids logged); the preview answers the layout's bottom line (sent, waiting and excluded
by reason, layers, spacing, the minimum, whether it re-forms, warnings), each member only with
`members: true`. `get_intents` lists group orders with their counts and folds member orders unless
asked; `get_swarms` adds each swarm's group order in force, its strength and its last re-form;
`get_inbox` and the review read carry candidate groups and declared groups (members, causes of
leaving), read-only. Nothing new for agents to approve, confirm or declare. The playbook gains one
play: propose re-sending a suspended swarm to its task once enough of its members are back and
heard, woken by `under_strength`. A tool change re-takes the digests (section 8).

**Console.** With a swarm selected, the order form offers the spread, the minimum, the altitude and
re-forming, and its dry run draws each member's slot and layer (deck.gl at the surface for boats,
CLAUDE.md's weather gotcha) with the counts and reasons, bottom line first. The inbox card is
section 5's. The Swarms panel shows each swarm's group order, strength and last re-form ("re-formed
2 min ago: 6 dropped, 9 moved"), and Re-form on a suspended one. Review lists candidate groups
(outlined on the map, the evidence in plain words, Declare with what it does: "these 82 tracks
stop protecting themselves in every engagement's keep-out until they leave the group or 10 minutes
pass"), declared groups with their changes (joined, left and why, time left) and presumed losses
by swarm (4.4).

## 11. Evaluation

The headless Saturation (bd murmur-lleq, being built) runs the bridge's own code on a simulated
clock (docs/HEADLESS.md section 2), so its group and member orders, re-forms and declarations are
the live ones. Each report opens with what the world covers: Saturation's 3,602 stand-ins (no
SITL), six platform types, air, surface and subsurface (no ground), the Golden Gate map, the
physical sensing model, association by contact id, no collision model and no vehicle-to-vehicle
links. The operator is the proxy, a documented model and never human data (DECISIONS.md:1601-1606);
runs wait for the owner's go-ahead on scoring, as the evaluation's others do.

| claim | measure | holds when |
|---|---|---|
| what a member flies is inside what was approved | replay recomputes every layout and re-form and binds each member order; the five tampers fail | every log, no mismatch |
| one member does not hold up its swarm | of the 36 opening orders: refusals, retries, the share of heard members sent within 60 s | no refusal on stale facts; 95 % of heard members sent within 60 s (GUESS) |
| members stay in their envelopes and apart | each second, every member's altitude or depth against its band; the closest pair per domain against the minima | inside, but for warned holds and rtls |
| a swarm keeps its task when members drop | a variant with scattered drops (GUESS: 10 % of each swarm's links cut at random times, half for good); the largest gap against the even spacing, each second; uploads per re-form | the largest gap under 1.5 times the spacing within 2 minutes of each material drop (GUESS) |
| re-forms do not storm | re-forms per group per minute; uploads into links that failed in the last 30 s | at most one; none |
| a swarm below its minimum asks once | `under_strength` cards per jammed swarm; re-forms during the jam | one each; none |
| the person's work grows with swarms, not members | cards and alerts per operator-minute, Saturation against Harbor watch | three cards or fewer per jammed swarm (GUESS) |
| a hostile swarm is one decision, not hundreds | declarations made to clear the reserve's keep-out, with and without group declarations | five or fewer with |
| the decision that matters is not crowded out, and comes sooner | the time from the act to the rule declaration, to its confirmation, to a nomination on the shooter passing its keep-out, on the same seed: without the 36 swarms' traffic, with it, and with group declarations | reported; today about 90 s after 10:00 at best (11:30), GUESS about 30 s with declarations |
| declared groups hold what they should | hostile members out of their groups (left or suspended) at the strike; with air neutrals added, candidates and confirmed members whose truth is not hostile | reported; any wrongly confirmed member is a finding about the test's figures |
| a swarm can be stopped | from a hold to the swarm being sent to every member holding | one order, under 30 s for members on streaming links (GUESS) |
| it runs in real time | the decision tick's p50 and p99, the lock per group order, the group step, log bytes per member | section 9's targets |

What these do not show: whether a real person understands a hundred-member envelope from one card,
or weighs a group declaration of 400 tracks as the decision it is. That needs people, and the
study mode (credentials, passkeys) is where it would be measured.

## 12. Prior art

Searched on 2026-11-10. Labels: **high**, the primary source read; **medium**, a reputable
secondary source; **low**, a search result's summary only, or memory not checked this time. Some
primaries would not open (DoDD 3000.09's official PDF, SIPRI's 2019 report, the Kolling survey,
the CCW's December 2025 text, the Operational Law Handbook's PDF).

**Closed programmes and products.** The common pattern: the person sets intent (an area, an
objective, a play) and authorizes force; allocation, routing, formation and deconfliction are the
system's, and the fielded ones fill gaps on their own. None says how many human authorizations a
swarm strike needs.

| work | what it shows | label |
|---|---|---|
| DARPA OFFSET, 2017-21 ([program](https://www.darpa.mil/research/programs/offensive-swarm-enabled-tactics), [FX-6](https://www.darpa.mil/news/2021/offset-swarms-take-flight)) | one operator directing swarms of 250 and more; tactics composed from swarm behaviours, shared in a tactics exchange; at FX-6, 300+ platforms, "swarm commanders tasked the swarm agents to execute swarm tactics" | high |
| Raytheon BBN's CCAST, an OFFSET team ([DroneDJ](https://dronedj.com/2022/01/13/new-raytheon-tech-lets-a-single-operator-control-130-drones/)) | one operator, 130 physical and 30 simulated UAVs; software matches drones to tasks and fills gaps in flight | medium |
| Adams, Hamell and Walker 2023, OFFSET ([arXiv 2308.00102](https://arxiv.org/abs/2308.00102)) | one swarm commander, 100 heterogeneous robots: workload "frequently exceeded the overload threshold", missions still completed | high |
| DARPA CODE, 2015-19 ([program](https://www.darpa.mil/research/programs/collaborative-operations-in-denied-environment)) | the vehicles recommend coordinated actions, which a supervisor approves or disapproves | high |
| ONR LOCUST ([ONR](https://www.onr.navy.mil/media-center/news-releases/locust-autonomous-swarming-uavs-fly-future)); ONR's 13-boat USV swarm, 2014 ([Breaking Defense](http://breakingdefense.com/2014/10/who-pulls-trigger-for-new-navy-drone-swarm-boats/)) | the swarm flies itself, a person monitors and can step in; the boats plan their own intercepts, designating and destroying a target stay human | high; medium |
| Perdix, 2016 | 103 drones, "a collective organism, sharing one distributed brain", self-healing | low |
| AFRL IMPACT, 2017 ([proceedings](https://corescholar.libraries.wright.edu/isap_2017/85)) | the operator calls plays; autonomy allocates assets, plans routes, recommends courses of action; about a dozen simulated vehicles | high |
| DIU Replicator, 2024 ([DIU](https://www.diu.mil/latest/defense-innovation-unit-announces-software-vendors-to-support-replicator)) | "automated coordination of swarms of hundreds or thousands of uncrewed assets across multiple domains"; nothing on human judgment | high |
| Auterion, Jan 2026 ([vendor](https://auterion.com/auterion-global-first-drone-swarm-live-fire/)) | one operator "assign[s] objectives and authorize[s] lethal effects"; software navigates, keeps formation, deconflicts and guides; 3 drones, 3 targets | high, a vendor's claim |
| Swarmer, Ukraine ([WSJ via To Vima](https://www.tovima.com/wsj/ai-powered-drone-swarms-have-now-entered-the-battlefield/)) | the operator sets a zone and orders the engagement; the drones decide which one releases when; usually 3, tested to 25 | medium |
| Shield AI Hivemind ([vendor](https://shield.ai/hivemind-for-operational-read-and-react-swarming/)); Anduril Lattice ([Breaking Defense](https://breakingdefense.com/2023/05/andurils-new-tech-could-allow-single-operator-to-control-hundreds-of-autonomous-systems/)) | "a minimal cadre"; an operator "could control hundreds"; both silent on approving force | high (vendor); medium |
| Dstl, "Many Drones Make Light Work", 2021 ([Airforce Technology](https://www.airforce-technology.com/news/uk-flies-20-drone-swarm-in-major-test/)) | 20 drones, 3 operators, an automatic airspace deconfliction layer | medium |
| MIT RESCHU SP ([MIT TLO](https://tlo.mit.edu/industry-entrepreneurs/available-technologies/research-environment-supervisory-control-unmanned)) | one operator supervising air, surface and undersea vehicles defending an asset; patented, licensed | medium; its vehicle mix low |

**Doctrine and law.** DoDD 3000.09 (2023) asks for "appropriate levels of human judgment over the
use of force", engagements completed "within a timeframe and geographic area" or ended or referred
to an operator, and lets a semi-autonomous system engage "individual targets or specific target
groups" an operator selected; neither "swarm" nor "multiple" appears, so one selection may cover a
group of targets and the directive does not settle whether a swarm engagement is one decision or
many (high, read in the [full text](https://agora.eto.tech/instrument/50)). The ICRC (2021) asks
for limits on duration, geographic scope and scale "in relation to a specific attack", and says
nothing of swarms ([ICRC](https://www.icrc.org/en/document/icrc-position-autonomous-weapon-systems),
high). The CCW's rolling text of November 2024 has limits on target types, duration, scope and
scale ([Article 36](https://article36.org/updates/key-comments-on-the-current-ccw-gge-rolling-text-on-autonomous-weapons/),
medium); no swarm provision was found, unverified for the 2025 text. A NATO STO group drafting
human-swarm guidance calls for "intent-based" control (low).

**Declaring a force hostile.** Under the US Standing Rules of Engagement (CJCSI 3121.01B, 2005,
revised since), a force declared hostile by an appropriate authority may be engaged without its
members having committed a hostile act or shown hostile intent, and the authority to declare is
limited and set out in an appendix (low: search summaries of the Operational Law Handbook's
reprint, whose PDF would not open; the current revision unread). "There is no requirement for
members of the forces declared hostile to have demonstrated any hostile intent": their status as
members is the basis, unlike a civilian targeted for what they do
([Cherry and Rizzotti, Lieber Institute, 2021](https://lieber.westpoint.edu/understanding-self-defense-law-armed-conflict/),
medium). Air defence controls fire over a zone by a weapons control status (free, tight, hold)
rather than per target (low: memory, not checked). Section 7 follows the first: a person with
authority declares a set, the engine never infers one, and the set is bounded in time; what it
adds is how membership is judged from a sensor picture and lapses on its own, which none of these
sources describes.

**Research.** One command for any number of robots (drawing a region) costs the operator the same
as one for a single robot, while independent robots cost one each (Lewis and colleagues, low); a
person supervises some 8 to 12 robots one by one (Wang et al. 2009 as cited, low). Counting wait
times cut predicted operator capacity by up to 67 % ([Cummings and Mitchell
2008](https://dspace.mit.edu/handle/1721.1/90280), high). Management by consent beat management by
exception with up to four UAVs (Ruff, Narayanan and Draper 2002, low). Delegation by plays (Miller
and Parasuraman 2007, low), the human-swarm interaction survey (Kolling et al. 2016, IEEE THMS
46(1), low) and neglect benevolence (Walker et al. 2012, low) round it out. Game-theoretic
re-allocation after losses ([arXiv 1905.12191](https://arxiv.org/abs/1905.12191)) and graceful
degradation by relaxing constraints ([arXiv 2009.04593](https://arxiv.org/abs/2009.04593)) exist
(low); no published minimum strength for a group order was found.

**Open source.** QGroundControl shows every vehicle but plans one at a time
([docs](https://docs.qgroundcontrol.com/master/en/qgc-user-guide/fly_view/fly_view.html), high).
MAVProxy's swarm module handles 2 to 10 vehicles, modes per vehicle or for all
([docs](https://ardupilot.org/mavproxy/docs/modules/swarm.html), high). ArduPilot's Follow mode
holds an offset from a leader ([docs](https://ardupilot.org/copter/docs/follow-mode.html), high);
Mission Planner's swarming is beta and "does no path prediction"
([docs](https://ardupilot.org/planner/docs/swarming.html), high). PX4 runs many vehicles in SITL
with no swarm logic ([docs](https://docs.px4.io/main/en/sim_gazebo_gz/multi_vehicle_simulation.html),
high). Skybrush, the drone-show suite, flies pre-planned trajectories with transitions planned on
its server ("proprietary maths"; separation during a transition at least D/√2 for static
formations D apart) and lets the operator take over any drone
([docs](https://docs.skybrush.io/public/skybrush-studio-for-blender/latest/concepts.html), high).
Crazyswarm2 (high), Aerostack2, Buzz and ROS2swarm (low) are research frameworks for small swarms.
For allocation: CBBA, with MIT ACL's code ([ACL](https://acl.mit.edu/projects/consensus-based-bundle-algorithm),
high); SciPy's `linear_sum_assignment`, a modified Jonker-Volgenant
([docs](https://docs.scipy.org/doc/scipy/reference/generated/scipy.optimize.linear_sum_assignment.html),
high); AFRL's OpenUxAS, task allocation for teams of uncrewed vehicles, inactive since about 2018
(low); DARP's equal-area partition for coverage (low). ATAK-CIV's repository was archived in May
2025 (low).

**What this spec takes, and what it does not claim.** Group orders laid out by the system and
swarms that fill their own gaps are the fielded norm (IMPACT, CCAST, Swarmer), pairing members
with slots by minimum total distance is textbook (Hungarian, Jonker-Volgenant), and declaring a
force hostile is doctrine. None of these is new here: one operator over a hundred vehicles
(OFFSET, Adams 2023), command by plays or intent (Playbook, IMPACT), the person authorizing force
while the system executes (CODE, the ONR boats, Auterion), a multi-domain supervisory simulator
(RESCHU SP, IMPACT), altitude layers or collision-free transitions (Skybrush), engagements bounded
in time and space (3000.09, ICRC). What looks uncommon: an open, instrumented testbed whose measure
is the quality of human judgment over consequential decisions rather than workload (medium
confidence), at some 3,600 vehicles in 37 swarms across domains (medium-low: Replicator aims at
thousands but is closed; ARGoS scales without a C2 layer); and making "how many human decisions one
swarm engagement needs" an explicit variable, which no source found does (medium). Murmur departs
from the fielded pattern on purpose: allocation and re-forming are central and logged, not on
board (2.3), so replay can bind them; and a strike stays one platform per decision until the owner
decides otherwise (section 6).

## 13. Stages

| stage | size | files | the claim, and what checks it | after | parallel |
|---|---|---|---|---|---|
| 0. Hold arms an airborne fixed wing (decided) | S | `tasks/exec.rs` `hold` | "a stand-in Puma started aloft holds at its height": one stand-in test | | yes |
| 1. Seed | S | `rules.rs` (gate), `intent.rs` (`group`, `parent`, member task fields, `transit`), `catalog.rs` (bands, separation), `audit.rs` (event shapes) | none: types only | | first |
| 2. Layout | L | a new `murmur-decide/src/group.rs`: assignment, ring, along and racetrack, lanes, stations, block, layers, envelope, separation, and the re-form's layout (who may move, fewest moves, largest gap) | "the same facts give the same layout; no member outside its band; no crossing paths; a re-form moves only gap neighbours on healthy links; Saturation's 36 orders laid out as section 3.3 says": unit and property tests on Saturation's own orders | 1 | with 3, G2 |
| 3. Group orders in the engine | L | `engine.rs` (group decide, member orders, `under`, outcome), `rules.rs` (strength, depth, separation; typed soft classes), `queue.rs` (a member's wait), `policy.rs` (facts over sent and waiting; schema 8), `replay.rs` | "one approval covers its members and nothing more; a member never reaches the inbox alone; replay binds each member": scenarios 141 to 148, the first three tampers; rules 28 logs replay as before (`tests/versions.rs`) | 1 | with 2, G2 |
| 4. Re-form | M | `engine.rs` (`step_groups`, `group.reformed`, refused, suspended), `replay.rs` | "a re-form runs only on material, settled drops, at most once a minute, inside the envelope and the tier, and replay recomputes it": scenarios 149 to 152, `forge_reform`, `reform_outside_envelope` | 2, 3 | with G3 |
| 5. Bridge | M | `tasks/{mod,exec,caps}.rs` (fly the share's altitude, phase and entry; no slot arithmetic under rules 29), `decision/mod.rs` (follow member orders, the group step on the tick, layout outside the lock, shared plans), `swarms.rs` (strength, the fold, the group alerts) | "the task layer flies what was logged": a stand-in test per kind; a 100-member ring on stand-ins, 10 links cut, one re-form | 4 | |
| 6. Console and MCP | M | `web/src/{TaskPanel,DryRun,InboxPanel,SwarmsPanel,MapView,ReviewSection}.tsx`, `mcp/tools.rs`, `mcp/world.rs` | "agents propose groups and approve, re-form or declare nothing": the MCP tests; digests re-taken | 5 | |
| 7. Transit | M | `intent.rs`, `group.rs`, `route.rs`, `tasks/exec.rs` | "a member holds where its route ends, by its type": a stand-in test per type | 3 | with 6 |
| 8. Saturation measured | M | the situation's orders (spreads, minimums; the relay as stations), its brief and policy (group declarations), `eval/headless` (the drop and air-neutral variants) | section 11, on the headless Saturation | 6, 7, G4, lleq | |
| 9. Later: search, group identify | L | `group.rs`, `identify.rs` | its own claims | 8 | |
| 10. Notebook | S | CLAUDE.md (rules 29), `crates/murmur-decide/README.md`, DECISIONS.md | | all | |
| G1. Seed | S | `rules.rs` (`HOSTILE_GROUP_SINCE`), `ident.rs` (`Declarer::Group`, the `[identification]` keys) | none: types only | | with 1 |
| G2. Grouping | M | `ident.rs` or a new `hostile_group.rs`: links, candidates, the membership test | "candidates come from the picture alone (a test feeds a truth with other swarm ids and gets the same candidates); a transponder or a neutral is never linked; leaving is detected on the 2-sigma": unit tests on generated swarms crossing neutrals | G1 | with 2, 3 |
| G3. Declarations in the engine | M | `engine.rs` (declare, join, leave, revoke, expire; the group step), `ident.rs` (`Ledger::fill`, `check_facts`), `engage.rs` (`track_exempt` reads it), `replay.rs`, `audit.rs` | "only a person with engagement authority declares, on what was shown; a member is never exempt outside its membership": scenarios 153 to 159, the four tampers | G2 | with 4 |
| G4. Bridge, Review and autoplay | M | `decision/ident.rs` (review items, routes), `web/src/ReviewSection.tsx`, `mcp/`, `eval/proxy.py` (`declare_groups`) | "agents cannot declare": the MCP no-such-tool test, REST 403 for an agent | G3, 5 | with 6 |

Stages 2 and 3 can run as parallel agents in their own worktrees once 1 lands: 2 owns `group.rs`,
3 calls it through one function (`group::layout`) whose signature stage 1 fixes. G1 to G3 touch
the identification code and share only `audit.rs` and `replay.rs` with the group orders. Stage 0 is
independent of everything.

## 14. Open questions

1. **What takes the decision tick to 1.6 to 3.2 s.** Not profiled. Suspects: the per-vehicle route
   plans, done twice; the domain rule's points every 20 m along each surface leg; the dry run's
   link coverage per member (`decision/mod.rs:1465-1484`). The headless Saturation should measure
   it before stage 5 picks a remedy.
2. **The separation figures, bands and re-form terms** are guesses; the catalog cites none. A source
   per type, or the owner's call that guesses are good enough for a testbed, is needed before stage
   2's tests pin them.
3. **Whether the baked land/water grid** (headless, without tidewater) **carries water depth.**
   The oracle's type does (`terrain.rs:18`); if the grid does not, the depth rule cannot evaluate
   headless, and a sub's layer would be `not evaluated` there.
4. **How fast a fixed-wing ring bunches.** ArduPlane keeps no along-track spacing, so phases drift
   with wind; a re-form evens them out only when it runs for a drop. If drift alone opens gaps,
   whether drift should trigger a re-form is the owner's call.
5. **Search lane spacing.** `physical_reach_m` is against a reference contact; a lane sized by it
   may be far wider than one that finds a small boat. A search kind needs a target class.
6. **Whether Saturation's picture can hold a group.** With track 1-sigmas a median 2.65 km
   (docs/load-test.md:393-396), the 500 m ceiling would suspend most members most of the time, and a
   suspended member blocks again. The headless run will say how many; if most, it is the tracker,
   not the declaration, that holds the strike, and the owner chooses between a looser ceiling (a
   member's identity is safe while association is by contact id, which a real tracker would not
   give) and fixing the tracker first.
7. **A swarm first seen holding** is never proposed, since proximity alone never links. A stricter
   proximity-only candidate (closer, longer) is possible; it would also propose a neutral hovering
   among drones.
8. **Exempting other declared hostiles** near a target is already an open design question in the
   keep-out (`engage.rs:4030-4032`); a group declaration exempts hundreds at once.
9. **The relay chain relays nothing**: stations along the shore are geometry until the emulator
   carries one vehicle's traffic through another.
10. **Carry-over after a crash**: member orders carry as one-vehicle intents (rules 12); whether a
    group in force carries its re-form state or is rebuilt from its member orders is stage 3's to
    settle. A group declaration should not carry: a new run's tracks are new ids.
