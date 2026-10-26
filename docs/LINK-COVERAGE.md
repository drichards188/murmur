# Showing link coverage ahead of time

Status: **design note for the owner's approval, 2026-10-26** (bd murmur-hei; the fix list's
item 2). No code.

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
