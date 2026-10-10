# The headless simulator: gates and the family split, frozen before any gate run

Written 2026-10-09 for murmur-8ak, before any headless episode, gate run or result existed, and
frozen: a later change is a new dated section at the end, never an edit of this one. The design
is [`docs/HEADLESS.md`](../../docs/HEADLESS.md); where the two differ, this file's definitions
hold. The gates are the research report's step 1 (G1 fidelity, G1b speed), made exact here.

## G1: fidelity on Harbor watch

### The runs

- **Full stack**: `make situation NAME=<copy> AI=playbook OPERATOR=autoplay`, one run at a time
  on the assigned port block (`MURMUR_BASE_PORT=29000`, `MURMUR_BRIDGE_BASE_PORT=30000`,
  `MURMUR_HTTP_PORT=9565`, `MURMUR_LINKEMU_PORT=9566`, `MURMUR_WEB_PORT=5973`,
  `MURMUR_TIDEWATER_PORT=9570`, `MURMUR_SITL_INSTANCE_BASE=220`), for k = 1 to 10, where the copy
  is `sim/situations/harbor-watch.toml` with only `seed = k` changed. **How the runs are varied:**
  Harbor watch's one seed knob is `seed`, the autoplay operator's draws; the rest of the spread is
  the stack's own (process timing, packet order through the emulator, the stand-ins' wall-clock
  seeds). Each run's audit log is replayed with its anchors after `make down`; the contacts' truth
  log and the processes' start times are kept with the run.
- **Headless**: the same copy, episode seed k = 1 to 10 (the operator's seed is then k as on the
  full stack), the default operator (`proxy`, which is op-auto), the playbook's config from the
  file's `[playbook]` table, at the same commit as the full-stack runs (recorded with the
  binaries' hashes).
- **A failed run**: one that fails for the infrastructure (the stack does not come up, the fleet
  is not ready within `ready_timeout_s`, the playbook does not start) is re-run once with the same
  seed, and listed. If fewer than 8 seeds have a good run on both sides, G1 fails.

### Calibration, the only use of full-stack results to set the simulator

Three process start offsets, and nothing else, are set from the full stack: `c0` (the contacts
process's start), `s0` (the stand-ins' start) and `a0` (the playbook's first tick after
`ai_start_s`), each on the situation's clock, each the median over three full-stack calibration
runs with `seed` = 101, 102 and 103 (never 1 to 10). They go into a dated section below before
the first gate run. Any other change to the simulator after a gate run is a dated section with
its reason, and a gate re-run (on fresh seeds 11 to 20) is reported beside the first, never
instead of it.

### Proposals and their targets

A **proposal** is a `proposal` line of the playbook's log (`agent.jsonl` on the full stack; the
same lines in the headless record), refused at submission or not, with:

- **time**: its evidence record's `tick_t` on the situation's clock (full stack: the bridge's
  clock mapped to UTC by the run's anchors, minus the situation's zero in `situation.jsonl`;
  headless: simulated);
- **play**;
- **target**: a track target by its truth identity (below); any other target (`battery:<uid>`,
  `alert:<...>`, `gap:<...>`) as it is;
- **vehicle**: its uid.

**Truth identity of a track target**: the truth contact nearest the track's `predicted` position
in the evidence record (`facts`, `track:<id>.kinematics`) at `tick_t`, if it is within 500 m and
no other contact is within 1.5 times its distance; else `ambiguous`, which matches nothing. Truth
on the full stack: the contacts process's truth log at the scenario second nearest `tick_t`, its
seconds placed on the situation's clock by that process's start time; headless: the simulator's
truth at `tick_t`. The rule is the same on both sides; the headless records also the tracker's
own association and reports how often the two agree.

### Agreement

Per seed, in time order, each headless proposal is paired with the earliest unpaired full-stack
proposal with the same play, the same target and the same vehicle whose time differs by at most
10 s; each proposal is paired at most once. Over the ten seeds, with M pairs and H and F
proposals on the two sides:

    agreement = M / (H + F - M)

**G1 needs agreement of at least 0.80**, and the metrics below.

### Truth-labelled metrics

Per episode, for Harbor watch (the fast boat is `fastboat-west`; it needs a look by the time it
first comes within 3 km of the bridge, by truth, plus 60 s; no other contact needs one):

| metric | kind | definition |
|---|---|---|
| `look_covered` | proportion (per episode) | a look (`isr.investigate`) was proposed on the fast boat by its deadline |
| `time_to_look` | time (episodes with a look on it) | from the fast boat first within 3 km of the bridge to the first look proposed on it, s |
| `false_looks` | count (per episode) | proposals of any `isr.*` play on a contact that needs no look |
| `precision` | proportion (per proposal, pooled) | the legitimate share of the playbook's human-tier proposals: a look or recentre on a contact that needs a look is legitimate, one on a contact that needs none is not, a P0 command is legitimate when the fault it answers is real by truth |

**G1 needs each metric's mean on one side inside the other side's 95 % interval, both ways**:

- a proportion: the Wilson score interval of the side's pooled count;
- a time or a count: the Student t interval of the side's per-episode values (`eval/stats.py`
  `t_interval`), its half-width at least 2 s for a time (one tick) and 0.5 for a count, so that
  ten equal values do not make an interval of no width;
- a metric with no value on one side (no episode had a look) passes only if the other side has
  none either.

Reported beside the gate, not gated: the inbox load metrics (human-tier cards, own pending at
once, expired unread, override rate, goodput), the operator's latency and answers per card,
withdrawals, every seed's proposals side by side with their times, and the start offsets each
full-stack run actually had.

## G1b: speed

Harbor watch on the simulator with the default operator, `python -m eval.headless.batch` with
seeds 1 to 200 on W workers on this machine (W stated, at most the cores free when it runs).

    episodes per core-hour = episodes / (hours from the first episode's start to the last's end x W)

Everything is inside that time (each episode's process start, the simulator, the driver, the
playbook, writing the record) except building the binary. **G1b needs at least 1,000.** Reported
with it: the single-worker rate and the 95th percentile of an episode's wall time.

## The family split

By family, as committed in [`split.toml`](split.toml), before any result:

| split | families |
|---|---|
| tuning | `fast-unknown`, `decoy`, `link-cut-mid-look`, `low-battery` |
| held-out | `two-at-once`, `stuck-vehicle`, `approval-flood` |

Rules:

- No parameter of the simulator or of the playbook is set, tuned or chosen on a result from a
  held-out family.
- A held-out family's generator and labels are frozen when they are first committed, before its
  first result; a later change makes a new version of the family (`two-at-once/2`), reported
  apart.
- No report pools tuning and held-out families.
- Seeds: family reports use 1 to 30 per family; tuning work (the research's step 3) uses 1000 and
  up on tuning families only; G1's calibration uses 101 to 103 and its gate 1 to 10 (a re-run 11
  to 20).

## What a failed gate means

- **G1 fails**: the report says where the two sides diverge (per seed, the proposals side by
  side, each metric on both sides, the start offsets measured) and why. Nothing is learned or
  tuned on the simulator until a G1 passes (the research's rule). A fixed simulator is gated
  again on seeds 11 to 20, reported with the first run.
- **G1b fails**: the report gives the profile, and the speed-ups of HEADLESS.md section 13 are
  tried in that order.

## Calibration, 2026-10-09 (before the first gate run)

The three offsets, measured on the full-stack calibration runs with `seed` = 101, 102 and 103
(`python -m eval.headless.fullstack run --seeds 101 102 103`, then `python -m
eval.headless.calibrate`), each the median of the three, in `eval/headless/calibration.toml`:

| offset | 101 | 102 | 103 | median |
|---|---|---|---|---|
| `contacts_start_s` | 0.838 | 0.823 | 0.821 | **0.823** |
| `standins_start_s` | 20.588 | 0.973 | 1.001 | **1.001** |
| `ai_first_tick_s` | 1.590 | 0.923 | 1.031 | **1.031** |

Run 101's stand-ins started 20.6 s late because a `cargo build` of the simulator held cargo's lock
while `sim/swarm.py` asked cargo for the stand-in engine; the median leaves it out. From here on no
build runs beside a full-stack run. With these values the simulator agreed with the three
calibration runs on 9 of 9 proposals (an agreement of 1.0 by this file's definition); that is
in-sample, the gate is not.

## Fix round 1, 2026-10-10 (written before the second gate run)

**Why.** An independent review of the first gate run (seeds 1 to 10, agreement 0.800, every
metric inside: on record, reported beside the re-run, never replaced) found that G1 compared
unlike link randomness. The full stack's link emulator ran `sim/links.toml`'s seed 1 in every run
(scripts/up.sh passed no `--seed`), while the simulator's ran the episode seed; the emulator gives
each vehicle and direction its own stream of draws, so every full-stack Harbor watch run met the
same draws (PUMA 2 failed safe at 317 s in all ten) and every headless episode other ones. The
reviewer's diagnostic, a simulator change not run under this file's rules and so not a gate
result: with the headless emulator on seed 1 as well, seeds 1 to 10 agree 25/26 = 0.962.

**What changed in the simulator and the runner** (all committed before the re-run):

1. **The emulator's seed** is the situation's own `seed` on both sides:
   `eval/headless/fullstack.py` passes it to the full stack as `MURMUR_LINKEMU_SEED` (up.sh hands
   it to linkemu as `--seed`; unset, the links file's seed as before), and the simulator uses the
   same (`situations.emulator_seed`). Harbor watch's own file has `seed = 1`, so a plain `make
   situation NAME=harbor-watch` and a headless batch of it keep seed 1; G1's copies (`seed = k`)
   run seed k on both sides.
2. **Timing.** The contacts' truth comes from their own per-step log on both sides (the simulator
   sampled it every 2 s, about 0.15 s late). The bridge is up at `bridge_start_s` (the full
   stack's first anchor: its decision layer and loops start about 1.02 s after the emulator; the
   simulator had them at 0). The playbook's first tick after `ai_start_s` is drawn for each
   episode seed, uniformly over the range measured on the full stack (it had one fixed value).
3. **link-cut-mid-look** is version 2: the cut follows the first look's dispatch, and an episode
   with no look dispatched by 600 s records "no look to cut" instead of cutting PUMA 1 anyway.
   Version 1's results are reported apart.
4. No computer-time line in the record; the agent reconnects after 2 s doubling to 30 s and opens
   a new session at once after three missed ticks, as the live agent; a tool result that is not
   JSON reads as `{"text": ...}`, as the live session reads it.
5. Provenance: every record holds the links file's path and hash and the space cache's content
   hash; the bridge reads the links file the emulator reads; each full-stack run records its
   binaries' hashes, its emulator seed and the host's load average at its start.

**The code now matches this file's definitions, and three places this file left open are fixed
here:**

- `look_covered` and `time_to_look` count `isr.investigate` only, as written above (the code had
  also counted `isr.armed_overwatch`).
- `precision`: over the playbook's human-tier proposals, a look (`isr.investigate`) or recentre
  (`isr.recentre`) is legitimate when its truth target needs a look and not otherwise (ambiguous
  included); a P0 command (`safety.low_reserve`, `safety.stuck`) is legitimate when its fault was
  real by truth: the vehicle's true charge under the low-reserve line (25 %) at the nearest sample
  of our vehicles, or a scripted stop of that vehicle before the proposal. **Left open above, fixed
  here:** a human-tier proposal of any other play (`isr.release`, `isr.armed_overwatch`, `link.*`),
  and a P0 command in a record without the truth of our vehicles (every full-stack record), has no
  rule in this file; it is listed as unjudged and left out of the proportion (the code had judged
  every `isr.*` play by its target and no P0 command).
- **Truth, changed here:** this file said "headless: the simulator's truth at `tick_t`". Both
  sides now take the truth log's sample at the scenario second nearest `tick_t` (the simulator's
  contacts write the same per-step log as the full stack's), one rule on both sides; the code had
  interpolated between samples on both. **Left open above, fixed here:** a contact's crossing
  within 3 km of the bridge is the first truth sample within it (the code had interpolated).

**Calibration**, fitted on the calibration seeds only (101 to 103 as before, and 104 to 106 run
2026-10-09 for this round with the new runner, `MURMUR_LINKEMU_SEED` = the seed), in
`eval/headless/calibration.toml` (`python -m eval.headless.calibrate`):

| offset | 101 | 102 | 103 | 104 | 105 | 106 | used |
|---|---|---|---|---|---|---|---|
| `contacts_start_s` | 0.838 | 0.823 | 0.821 | 0.820 | 0.812 | 0.821 | median **0.821** |
| `standins_start_s` | 20.588 | 0.973 | 1.001 | 0.960 | 0.922 | 0.921 | median **0.966** |
| `bridge_start_s` (first anchor) | 1.048 | 1.026 | 1.025 | 1.023 | 1.016 | 1.022 | median **1.024** |
| `ai_first_tick_s` | 1.590 | 0.923 | 1.031 | 1.572 | 0.711 | 1.332 | range **0.711 to 1.590**, drawn per seed |

Run 101's stand-ins were late for a cargo build (calibration section above); the median leaves
it out. In-sample, with these values, the simulator agrees with runs 104 to 106 (run on the
matching emulator seeds) on 9 of 9 proposals: that says the fit took, nothing more.

**The re-run**, under this file's rules as amended here:

- **G1 on seeds 11 to 20**, fresh (never used for anything): ten full-stack runs of Harbor watch
  copies with `seed` = k and `MURMUR_LINKEMU_SEED` = k, one at a time on the assigned port block,
  each replayed with its anchors; then headless episodes of the same copies; agreement at least
  0.80 and every metric inside the other side's interval both ways, as above. A failed run is
  re-run once; fewer than 8 good seeds fail the gate. The seeds 1 to 10 run is reported beside it.
- **G1b at W = 8 workers**, stated now, before measuring: Harbor watch, seeds 1 to 200, the
  default operator, measured when the host is quiet (its load stated with the result); at least
  1,000 episodes per core-hour.

## Fix round 2, 2026-10-10 (no gate re-run asked)

The emulator's seed is the **episode** seed, not the situation file's (`situations.emulator_seed`):
fix round 1's rule put every episode of a Harbor watch batch on seed 1 (its file's), the same
draws every time (G1b's 200 episodes: PUMA 2 failed safe at 318 s in all). Anything compared with
the full stack passes the same seed to it as `MURMUR_LINKEMU_SEED`. G1's copies set `seed` = k and
run as episode k on both sides, so the value G1 used is unchanged and the seeds 11 to 20 records
stand. `link-cut-mid-look`'s aggregate counts the cuts that fell on a vehicle already going home
on its own failsafe (no look in progress cut) beside the episodes with no look to cut.
