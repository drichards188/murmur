# Showing link coverage ahead of time

Status: **approved 2026-10-26** (bd murmur-hei; the fix list's item 2). Section 3's predictor,
bridge, agents, playbook and console are built (section 7).

## 1. Why

Murmur shows each vehicle's link as it is now (`/api/links`, the console's link badges), never
where a link will hold before a vehicle goes there. The measurements say that is the biggest gap
between Murmur and a well-informed operator:

- Scoring v2's ladder: knowing link coverage was worth 18 points of need served pooled, up to 79
  in the stuck-vehicle family (eval/results/headless/v2-ladder-1100/REPORT.md).
- The south shore's radio shadow decided v2's stuck-vehicle results: a Puma sent to the right
  place lost its link and its own failsafe took it home (eval/SCORING-V2.md 0.6).
- In world v3, the playbook and agents lose looks to shadows they could not see coming.

An operator or an agent that can see "a Puma orbiting there will lose its link within a minute"
can send another vehicle, pick another orbit, or accept the loss knowingly.

## 2. What exists to build on

The link emulator (`crates/murmur-linkemu`) already models every link class Murmur uses, from
`sim/links.toml`:

- **Line of sight and mesh:** loss rising with distance from the ground station
  (`range_ok_km`, `range_max_km`, `range_exp`), and terrain obstruction with earth bulge and
  Fresnel clearance (`terrain.rs`, `path_obstruction_m`: block or ramp).
- **SATCOM:** the best satellite of a constellation class (murmur-space).
- **Acoustic:** an AUV's modem while submerged.

Its random draws (per-packet loss) sit on top of that deterministic geometry.

## 3. Proposal: one predictor, shown everywhere (no decision-rules change)

1. **A deterministic coverage predictor in the link emulator's own code**, so prediction and
   simulation can never disagree on geometry. For a link class, a point and an altitude (and a
   time, for SATCOM), it returns the expected link quality: up, degraded or down, with the
   probability of loss the class's range curve gives, and why (range, terrain, no satellite).
2. **The bridge serves it**, computed outside the decision lock:
   - a coverage grid per link class and altitude for the map (cached; terrain doesn't move);
   - a route answer: for a proposed order, each leg's predicted link, the first point where it
     goes down, and how long it stays down against the class's lost-link failsafe time (so
     "the vehicle will trip its failsafe here" can be said, not inferred).
3. **Everyone sees the same answer:**
   - **Console:** a coverage layer per link class at a chosen altitude, and the route answer on
     the order form beside the dry run.
   - **Agents:** a new read-only MCP tool (`get_link_coverage`) and the route answer inside
     `preview_intent`'s result, documented in agents/README.md.
   - **The playbook:** reads it when it chooses a vehicle and an orbit.
4. **Honest about uncertainty:** the predictor shows the model's expectation. Real links vary
   more than any prediction; the display says "predicted", and the emulator's random draws
   still decide what happens.

Nothing in the decision engine changes: it is information for whoever decides, not a new
check, so no decision-rules version.

## 4. Later, if wanted (a rules version, your call then)

The engine could warn about, or hold for a person, an order whose route leaves predicted
coverage for longer than the class's failsafe time, the way rules 17 warns on an rtl across
land. Not proposed now: show it first, measure whether people and agents use it well, then
decide whether the machine should insist.

## 5. How it is checked (small runs, as the owner asks)

- Unit tests: the predictor agrees with the emulator's deterministic geometry at sampled points
  (range and terrain), and the route answer finds the first shadow on a known leg.
- A small paired comparison: the honest oracle with and without reading the predictor, on the
  families where links decide (F2, F7, F11, and the stuck-vehicle-like breakdowns in F13), tens
  of episodes. The expectation is that it closes part of the link-blind rung's gap.
- A look at the console layer in the browser.

## 6. Decision asked

Approve phase 3 (predictor, bridge, console, agents, playbook; no rules change)? Section 4
stays a later decision.

## 7. As built (2026-10-26)

- **Predictor**: `crates/murmur-linkemu/src/coverage.rs`. The link's own physics is shared, not
  copied: `model::range_loss` and `model::terrain_effect` are what `Link::cond` now calls, and the
  predictor asks a `Terrain` as `Link::set_position` does (with `TidewaterTerrain`, the emulator's
  250 m cells, 10 m altitude bands and profile sampling). States: up, degraded from 20 % loss,
  down from 95 % or wherever the emulator blocks (the live link badges' thresholds, now one
  constant). Reasons: `range`, `terrain`, `no_sat`, `submerged`, `surfaced`, the link's own
  `loss`. A route's failsafe is walked heartbeat by heartbeat (the bridge's period on that link)
  with the predicted loss where the vehicle then is: the chance it trips on the way and in five
  minutes on station; `holds` when that is at most 10 %. Checked against the emulator's `Link` at
  sampled points (a ridge, and a 7 x 7 x 3 lattice on the Golden Gate's grid for both
  terrain-limited classes: equal to the last bit) and on known shadows (the Marin Headlands and the
  city's hills at a Puma's 85 m).
- **Bridge**: `crates/murmur-bridge/src/coverage.rs`, outside the decision lock, never logged.
  `GET /api/links/coverage?class=los&alt=85&bbox=w,s,e,n&res=250` (or `uid=`, `point=`,
  `above_surface=1`, `at=`) and `POST` with the same as JSON, a `route`, an `order` or `queries`.
  The dry run and `preview_intent` carry the order's answer as `link_coverage`, per vehicle: its
  legs (transit, task path, return, as `murmur_decide::route::route_for` lays them out) with their
  stretches up, degraded and down, `first_down` and `trip_down` (where, why, how long at its
  cruise speed), `failsafe_s`, `p_failsafe`, `holds` and a sentence. Terrain from tidewater or a
  baked surface grid (`MURMUR_SURFACE_GRID`), cached for good per cell; a grid without satellites
  cached whole.
- **Agents**: `get_link_coverage` (agents/README.md).
- **Console** (`web/src/coverage.ts`, `CoverageControl.tsx`, `DryRun.tsx`, `linkLayers.ts`): the
  topbar's Links button (off by default) shows a card, labelled predicted, to pick a link class or
  the one selected vehicle's own link, and an altitude (presets, a value, above sea level or above
  the surface; the vehicle's mission altitude for its own link). The map gets the grid for the view
  (`GET /api/links/coverage`, the bbox clamped to 30 km around the centre and snapped to blocks of
  cells so a small pan asks for the same grid, at the 250 m terrain cell or coarser to stay within
  12 000 cells, 600 where satellites decide it; asked 450 ms after the view settles) as one image
  draped on the terrain under the labels, and the cell under the pointer says its state, reason and
  loss. The order form runs the dry run (`POST /api/intents/preview`) once the order is complete and
  again 5 s after each answer: the checks' verdict, then per vehicle the route answer's sentence, the failsafe's
  chance, `holds`, and a bar per leg; the map colours the order's legs by their stretches (each at
  its share of the leg drawn from the order's own geometry) and marks `first_down` and `trip_down`.
  Information only: the Send button never waits on it.
- **Playbook**: `isr.investigate` version 2 ranks the free sensor aircraft whose predicted link
  holds for the orbit first (one `get_link_coverage` call a tick) and says so in its rationale.
- Not predicted, as section 3 says: scripted events, handover outages, a landed aircraft's at-rest
  check.

## 8. The paired comparison (2026-10-26; MODEL RESULTS ONLY)

**What the world covers.** World v3's Golden Gate map only (`golden-gate-v3`): its whole fleet on its
opening orders (the Pumas, FVR-90s and airship on LOS radios, the X10 and two UGV types on the mesh,
the MQ-9B and RQ-4 on GEO SATCOM, the Corsair, Magura, Sea Hunter and Saildrone on LEO SATCOM, the
buoy on Iridium, REMUS acoustic, the ROV and the antenna tracker tethered), the physical
sensing model, the baked terrain and land/water grids, the scripted approve-all operator, one
replicate per situation. Four families where links decide: F2 (a drone), F7 (three contacts at
once), F11 (link trouble: an RF jammer, or the AUV made to surface; neither is predicted) and F13
(vehicle faults), five situations each on dev seeds 5480-5484 (scoring v3's dev range, unused
before), 60 episodes: `none`, `oracle-v3-honest`, and `oracle-v3-honest-linked`, the same honest
ceiling where its link map would be reading `get_link_coverage` for the orbit it would order (from
the vehicle's position; it plans only looks whose link holds). Not the San Diego map, not the other
families, no model agent.

| family | `none` | honest | honest + predictor | orders ended in a failsafe after a lost link (honest / + predictor) |
|---|---|---|---|---|
| F2 drone | 0 / 5 | 1 / 5 | 0 / 5 | 6 / 0 |
| F7 three at once | 9 / 15 | 11 / 15 | 12 / 15 | 10 / 0 |
| F11 link trouble | 1 / 5 | 2 / 5 | 2 / 5 | 11 / 6 |
| F13 vehicle faults | 2 / 5 | 3 / 5 | 4 / 5 | 7 / 1 |
| pooled | 12 / 30 | 17 / 30 | 18 / 30 | 34 / 7 |

Need served is eligible needs served by their deadline (SCORING-V3.md). An order "ended in a failsafe
after a lost link" is one the oracle logged as ended with its vehicle in a return or landing mode
within 120 s of a `lost_link` alert on that vehicle: 13 Pumas, 8 recon UGVs, 7 FVR-90s, 4 X10s and 2
logistics UGVs for the honest ceiling; 3, 1, 1 and 2 (no logistics UGV) with the predictor. The six
left in F11 are its jammer's, which no prediction shows.

Paired by situation, need served moved in 5 of 20 (3 up, 2 down; net +1): within what 20 situations
can tell, so no claim on need served follows. What the predictor does is clear: the orders lost to a
link the honest ceiling could not see fell from 34 to 7, and with them the failsafe returns (60 orders
ended in a return or landing mode, 20 with it). Needs were not lost to the links here (every lost
need's cause is `looked` or `no_look`: the contact was not identified in time, causes.py), so a vehicle
kept on station did not turn into a need served. The predictor's calls: 10,488 over 30 episodes, no
error, and no longer episodes (median 45 s of computer time against 47 s; three workers).

    .venv/bin/python -m eval.headless.v3.run batch --families v3-f2-drone-gg v3-f7-three-at-once-gg \
        v3-f11-link-trouble-gg v3-f13-vehicle-faults-gg --seeds 5480-5484 \
        --agent oracle-v3-honest oracle-v3-honest-linked --workers 3 --out <dir>
