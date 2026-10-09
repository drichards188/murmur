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
