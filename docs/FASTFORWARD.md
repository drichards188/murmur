# Fast forward (design)

Status: **design for review, 2026-10-12; nothing built.** Spikes answered the feasibility
questions (SITL taking a live speed change, how far and how soon, section 8.1) and measured the
host's scheduling lateness (section 8.2); their scripts are not in the repository. The user's
request, verbatim: "can we make a time fast forward series of buttons and a button to fast forward
until the ai makes a pending request". They had just watched Harbor watch at 1x: 12 minutes, most
of it waiting for the fast boat.

## 1. The decision in brief

- **One world clock for the whole stack, with a rate that changes during a run.** The bridge is its
  only authority. It writes the clock as a piecewise-linear schedule (segments: from host
  monotonic instant `m`, world elapsed `e`, rate `r`) to one small JSON file per stack, atomically,
  each change effective 250 ms ahead so every process switches at the same instant. Every
  process derives world time from that file and the host's `CLOCK_MONOTONIC` (shared by every
  process on one Linux host), in Rust through one reader in `murmur_space` and in Python through a
  twin; none integrates its own rate, so no error accumulates between processes (section 3).
- **The decision engine runs on world time.** The bridge's `clock::wall()` (the engine's clock,
  every fact's stamp, every time in the audit log) and `clock::mono()` become world time. Keep-outs,
  fact ages, link staleness, task timeouts and every engine window then mean what they mean at 1x,
  in world seconds, and replay recomputes every decision unchanged from the logged times (section
  4). Wall time stays only where a person's or a process's own time is meant (console refresh, MCP
  and login sessions, HTTP timeouts).
- **Human time is protected by the rate, not by a second clock.** Any rate above 1x drops to 1x,
  saying why, the moment something new needs the operator (a proposal or order waiting in the
  Inbox, an identification review item, an alert that asks for an act). "Until the AI asks" runs at
  the top rate on the same terms. The drop on a new pending approval is the engine's own act, in
  the same step as the transition, so replay can check it. Fast forward is refused in a
  credentials run (so in every study), while any engagement is in play, with `MURMUR_RATE` other
  than 1, and on a stack started without the clock file (the evaluation, the headless simulator)
  (section 6).
- **Rate changes are audit events** (`clock.rate`), gated as rules 17 (`FAST_FORWARD_SINCE`):
  decisions do not change, but the engine starts refusing and dropping, and replay starts checking
  that, and the project gates such behaviour on a named constant (section 7). Open question 1 asks
  whether to skip the bump.
- **Top rate: 10x by default, then whatever the stack is measured to hold.** For stand-in fleets
  (Harbor watch) the CPU is not the limit: the load test's 300-node fleet at 1x carries the message
  load of the 22-node fleet at 14x. The limits are the margin between a scheduling stall (times the
  rate) and the shortest world timer (a Puma's 5 s lost-link failsafe), the AI's wall-time latency,
  and, for SITL fleets, CPU: one SITL vehicle held 9.98x at 10x for 0.27 to 0.33 of a core and 17.5x
  to 19.6x at 20x on this host at load average 50 to 70 (section 8). The bridge measures every
  follower's achieved rate from data it already receives and drops to 1x when one falls behind;
  the console reads the top rate and why from `GET /api/clock`.
- **Two phases.** Phase 1, stand-in fleets (Harbor watch, the user's case): bridge, engine and
  replay, stand-ins, contacts, link emulator, `up.sh`, the situation runner, the console; about 8
  to 10 working days plus review rounds. Phase 2, SITL fleets: `sim/swarm.py` sets `SIM_SPEEDUP`
  live on a simulation port and steers each SITL to the schedule, `tidewater drive` follows the
  schedule (a change in `~/tidewater`, on a branch); about 3 days (section 11).

## 2. Three times

| time | what it is | who reads it |
| --- | --- | --- |
| **world time** | how far the simulated world has run: vehicles' physics, contacts' motion, link latency and outages, every failsafe and timeout on a vehicle, the decision engine's clock. Runs at the current rate. | every process, from the schedule |
| **wall time** | a person's and a process's own time: how long the operator has to think, how long a model takes to answer, a session's idle limit, an HTTP request's timeout. Always 1x. | the console's refresh, MCP and login sessions, the AI's model calls |
| **calendar time** | the scenario's date and time of day (`MURMUR_AT`): tides, satellite passes, sun. `MURMUR_AT` plus world elapsed times `MURMUR_RATE`. | tidewater conditions, the space segment, the link emulator's SATCOM, the stand-ins' GNSS clock |

Today world time is wall time in every process but one: `MURMUR_RATE` moves the contacts at the
scenario rate (`crates/murmur-contacts/src/main.rs`: `dt = wall x rate`), while the vehicles fly at
wall speed and the bridge's clock runs at 1x (`eval/spec.py` says so: "rate speeds up the contacts
and the scenario clock, not the stand-in vehicles"). Section 10 says what that does to keep-outs.
Fast forward makes world time a clock of its own; calendar time keeps its meaning on top of it.

## 3. One world clock

### 3.1 The schedule

```json
{"v": 1, "stack": 9465, "at": "2026-06-14T10:00:00Z", "calendar_rate": 1.0, "max_rate": 10.0,
 "segments": [
   {"m": 313547.2134, "e": 0.0,    "r": 1.0},
   {"m": 313619.9001, "e": 72.69,  "r": 10.0, "seq": 1},
   {"m": 313669.4512, "e": 568.20, "r": 1.0,  "seq": 2}
 ]}
```

- `m` is the host's `CLOCK_MONOTONIC` in seconds, `e` world seconds since the stack's origin, `r`
  world seconds per wall second (> 0). World elapsed at monotonic `t` is
  `e_k + (t - m_k) x r_k` for the last segment with `m_k <= t`; its inverse (when world elapsed
  `x` comes) is `m_k + (x - e_k) / r_k` in the segment holding `x`. Both are continuous and
  increasing.
- **Keyed on `CLOCK_MONOTONIC`, not UTC.** On this host the wall clock stepped back 0.5 to 0.9 s
  every 32 s for the first hours after boot (DECISIONS, "The bridge keeps one clock that never
  steps"); at 10x that is a world clock going back 9 s. `CLOCK_MONOTONIC` never steps and is one
  clock for every process on one Linux kernel (WSL2 is one VM). Python's `time.monotonic()` is
  `clock_gettime(CLOCK_MONOTONIC)` (checked on this host); Rust needs the raw value, which `std`
  does not expose and the workspace's `unsafe_code = "forbid"` keeps from `libc`: `rustix`
  (feature `time`, a safe `clock_gettime`) is the one new dependency, in `murmur-space`. A
  follower maps it to tokio's clock by one paired reading at start.
- **Origin.** `scripts/up.sh` writes the file (segment 0, rate 1) before it starts anything, with a
  one-line command of the Python twin, and passes its path to every process as
  `MURMUR_CLOCK_FILE` (`sim/run/clock-<http port>.json`, per stack like the bridge's pointers).
  Calendar time is then `MURMUR_AT + e x MURMUR_RATE` in every process from one origin, where
  today each process starts its own scenario clock at its own start (seconds apart). Counters that
  mean "since my start" keep that meaning, in world seconds: an events file's `at_s` (from the
  emulator's start), the contacts' `start_s`, a stand-in's time since boot.
- **Only the bridge writes it after the origin.** A change appends a segment whose `m` is 250 ms
  ahead (`LEAD`), its `e` computed from the segment before (continuity). A change that comes
  before the last one took effect replaces it. Written to a temporary file and renamed. The bridge
  keeps its own copy, re-reads the file every second, and if anything else changed it, rewrites
  its own and says so in its log.
- **Followers poll** the file's modification time every 50 ms (a `stat`, cheap in both languages)
  and re-read it on change. A follower that reads a change late still computes world time
  correctly (it is a function of the file and the monotonic clock); only what it already did at
  the old rate in the late part is off, and section 5 says how each catches up.

### 3.2 Why every process derives world time, and none integrates its own rate

If each process switched its own rate when told and counted from there, a process switching 50 ms
late at 10x would run 0.45 world seconds behind the others for good, and switch after switch the
processes would drift apart at random. Deriving world time from one schedule makes every process
agree to the precision of its clock read. The one follower that cannot do this is ArduPilot SITL,
which runs its own clock at `SIM_SPEEDUP`; phase 2 steers it (section 5.6), and the bridge
measures what every vehicle achieves (section 8.4).

### 3.3 Alternatives considered

- **Push from the bridge** (HTTP long-poll, WebSocket, or UDP datagrams to followers). Lower
  latency than polling, but each follower needs a client, and the link emulator and the stand-ins
  start before the bridge does. The file is there from the first process, survives a follower's
  restart, and is readable in a shell. The console, on another machine's clock, is the one
  follower served by the bridge (section 5.10).
- **A clock server answering pings** (NTP-like). Needs no shared monotonic clock, so it would work
  across machines; Murmur's stacks run on one host, and it has the push alternative's start-order
  problem.
- **Keyed on UTC.** Simplest to read everywhere, but steps (above). Each process could steer its
  own reading as the bridge does, but steered clocks agree within about 2 s, which is 20 world
  seconds at 10x.
- **A clock process of its own.** The bridge already holds what decides the rate (the inbox, the
  engagements, the operator's request) and the audit log that records it.
- **tokio's paused clock, as the headless simulator.** It moves only when told to, or jumps to the
  next timer whenever the runtime is idle, so it cannot run a live stack whose work arrives on
  sockets from other processes; the simulator runs everything in one process.
- **Skipping ahead instead of running faster.** A stand-in could be moved in one jump; SITL cannot,
  and nothing that happens in the skipped time (a link cut, a track forming) would happen.

## 4. The engine runs on world time

### 4.1 Why

A keep-out counts a track anywhere it could reach since its last fix: speed times (decision time
minus fix time). The bridge stamps a fact when it arrives (`clock::wall()`), the tracker keeps
`last_seen_t` on that clock, and the engine decides on it (`clock::BridgeClock`). If the world ran
at 10x while those stayed on wall time, every interval the engine measured would be a tenth of the
world's: a track 10 s unseen in the world would count as 1 s unseen, a reach a tenth of the truth.
That is unsafe, so the engine's clock must be world time. Then nothing in the engine changes: its
decisions are the same functions of logged times, the times are world times, and replay recomputes
them as it does now.

### 4.2 The bridge's clocks

- `clock::wall()` = the steered base it is today (`Steer`, within about 2 s of UTC at the monotonic
  rate, slewing at most 5 %) **plus the lead**, the world seconds fast forward has added:
  `lead = e(t) - (t - m_0)`. At 1x the lead is constant and the clock is today's; after a fast
  forward the bridge's clock is ahead of UTC by the lead. The anchors already record UTC beside
  every anchor's time (`Anchor::utc`), so the lead shows in them. During a fast forward it runs at
  the schedule's rate give or take the steer's 0.05 s a second, and it stays within the steer's
  bound (about 2 s) of the world elapsed the other processes read: the tolerance the bridge's clock
  already has against their monotonic clocks today.
- `clock::mono()` = world elapsed (plus its fixed one-hour margin): link statistics, TIMESYNC
  round trips, task timeouts, queue patience, frame ages.
- `clock::utc()` unchanged: CoT dates, the resume pointer, the run id, the anchors' UTC.
- **The rule for every timer:** anything about vehicles, links, tracks or decisions reads
  `clock::mono()` or `clock::wall()` and sleeps with new world primitives
  (`clock::sleep_until_world`, `clock::world_interval`, `clock::world_timeout`, which sleep the
  wall time the schedule gives and re-arm when it changes); anything about a person's or an
  agent's process uses tokio's clock directly. Without a clock file the primitives are tokio's own
  (rate 1, same tick phases), which keeps every existing test and the headless simulator's golden
  episode byte for byte (`sim_clock` stays as it is).

The bridge's timers, classified (from `crates/murmur-bridge/src`):

| world (converted) | wall (unchanged) |
| --- | --- |
| the GCS heartbeat thread (`heartbeat.rs`: round `k` due at world second `k`; at 10x, 10 rounds a wall second, or a 5 s lost-link failsafe fires on every vehicle) | the console push (`push.rs`, 250 ms) and its WebSocket |
| the decision tick and the terrain prefetch (`decision/mod.rs`, 1 s each) | MCP sessions and the change feed's age (`mcp/`) |
| world housekeeping (`world.rs`, 1 s), the track sweep (`tracks/mod.rs`, 1 s), the water step (`estimate.rs`) | logins, step-ups, sweeps (`auth.rs`; credentials runs refuse fast forward anyway) |
| task-layer deadlines (`tasks/exec.rs`: readiness, ACKs, mission items, arming, mode, confirm, patience), the preload wait (`decision/preload.rs`), queue patience (`decision/queue.rs`) | HTTP timeouts to tidewater and the emulator (`decision/terrain.rs`, `emu.rs`), the space cache poll (`space.rs`), CoT (`cot.rs`), the heartbeat statistics line |
| the emulator poll (`emu.rs`; its numbers are on the emulator's world clock) | |

About 35 sites; each conversion is mechanical, and the classification is what review should check.

### 4.3 Human time

The engine's windows that are about a person (an approval waiting 120 s, a human's approval held
120 s in the queue, a picture token's 15 s, signatures within 120 s) are also about the world: an
approval expires because the picture it was given on goes stale, and the world is what makes it
stale. They stay on the engine's (world) clock. What protects the person is that a new item puts
the world back to 1x, so while it waits its window runs in wall seconds as today. If the operator
then fast forwards with items still waiting, those items age at the world's rate (120 s is 12 wall
seconds at 10x); the console says so on the button (section 6.4). Engagements, whose windows are
the tightest and the most consequential, refuse fast forward altogether.

### 4.4 Alternatives considered

- **The engine on wall time, every projection converting.** Every speed-times-interval in the
  engine (keep-outs, reach, recency, track deviation, plan tolerance) would map its interval
  through the schedule, and replay would need the schedule to recompute anything. One missed site
  undercounts a reach by the rate. Rejected.
- **Two clocks in the engine** (world for the world, wall for people). Every window in the engine
  would have to be classified and both clocks logged; replay would recompute with both; and an
  approval window running in wall seconds while the picture behind it goes stale ten times faster
  is the wrong window. The drop to 1x makes it unnecessary. Rejected.

## 5. What each process changes

| process | change | phase |
| --- | --- | --- |
| `murmur_space::run_clock` (new) | the schedule: read, follow (poll, re-read), world elapsed and its inverse, calendar time; `rustix` for `CLOCK_MONOTONIC`; tests against shared vectors | 1 |
| Python twin (`sim/run_clock.py`, new) | the same, for `up.sh` (writes the origin), the situation runner, the storyline, the autoplay proxy, `swarm.py`; the same vectors | 1 |
| `murmur-bridge` | world `wall()` and `mono()`, the world timers, the heartbeat thread, `GET /api/clock`, `POST /api/clock`, `POST /api/sim/clock`, the clock in every frame, the drop triggers outside the engine (alerts), the governor (section 8.4), no carry of open intents from a log that ran ahead (section 7.4) | 1 |
| `murmur-decide` | `clock.rate`, `Engine::set_rate` and its refusals, the drop on a pending approval and on a review item, replay checks, rules 17 | 1 |
| `murmur-standins` | ticks paced by the schedule | 1 |
| `murmur-contacts` | ticks paced by the schedule; motion on world time | 1 |
| `murmur-linkemu` | its clock is world elapsed; the delay queue re-armed on a change | 1 |
| `scripts/up.sh` | writes the origin; `MURMUR_CLOCK_FILE` to every process | 1 |
| situation runner, storyline, autoplay proxy | situation time is world time | 1 |
| console | the rate control, rate-aware clocks | 1 |
| watch officer | one comparison fixed (section 9) | 1 |
| `sim/swarm.py` | `SIM_SPEEDUP` on each SITL's SERIAL2, steered to the schedule | 2 |
| `tidewater drive` (`~/tidewater`) | calendar time from the schedule | 2 |
| `tidewater serve` | none (it answers for the time it is asked) | |
| evaluation, headless simulator | none: no clock file, so 1x and today's code paths | |

### 5.1 Bridge

Section 4.2 for the clocks and timers. The API:

- `GET /api/clock`: world time, calendar time, the rate and since when, the mode (`fixed` or
  `until_ai`), the top rate and its basis (`configured`, or `measured` with the follower that set
  it), whether fast forward is available and if not why, each follower's achieved rate, the last
  drop and why, the schedule's segments. Also pushed on the WebSocket (`type: "clock"`) on every
  change, and every frame carries `{world_t, rate}` so the console extrapolates between frames.
- `POST /api/clock {"rate": 5}` or `{"until": "agent_proposal"}` or `{"rate": 1}`: an operator's
  act (the dev header, under the operator's session), handed to the engine (section 7), then
  written to the schedule. 403 in a credentials run; 409 with an engagement in play, with
  `MURMUR_RATE` other than 1, or without a clock file; 422 above the top rate. Agents have no way
  to it (no MCP tool, and the route refuses an agent).
- `POST /api/sim/clock {"rate": 1, "why": "situation_end"}`: the situation runner's drop at the
  end, under the stack's simulation token (checked before the body is read, as
  `/api/sim/contacts`); it can only lower the rate.

Not in the MCP tools: the AI is not told the world is fast. Every fact it reads already carries its
age, in world seconds, which is the truth it acts on.

### 5.2 Decision engine and replay

Section 7.

### 5.3 Stand-ins

A stand-in is a discrete machine on its tick count (`Vehicle::now` is ticks over `PHYSICS_HZ`, 10):
failsafes, missions, telemetry intervals and the battery all run on it. Fast forward ticks faster:
tick `n` of a vehicle is due at world elapsed `e_0 + n / 10`, its wall instant from the schedule,
each tick still 0.1 world seconds of physics. A late tick is caught up tick by tick up to
`MAX_CATCH_UP`, which becomes 10 ticks: one world second, because what it guards against (a GCS
failsafe firing on heartbeats still queued in the socket) is on world timers. A longer stall is
skipped by moving `e_0` on, as the wall version moves its interval, and the vehicle's clock stays
that much behind the world (the bridge sees it, section 8.4). A slower rate read late leaves a
vehicle ahead: it waits for the world. Calendar time for tidewater conditions (`conditions.rs`,
polled every world second) and the GNSS clock (`gnss.rs`) come from the schedule. CPU: the load test's engine ran 1,000 vehicles at 10 Hz
for 7 to 9 % of a core; Harbor watch at 10x is 400 ticks a second.

### 5.4 Contacts

Ticks at `MURMUR_CONTACTS_HZ` per world second, `s` and `dt` from world elapsed since the
process's start (motion at world speed, not at `MURMUR_RATE`). The truth message's `t` stays
calendar time at the contacts' start plus `s`; the tracker reads it only for intervals between
fixes. At 10x the bridge gets 10 truth messages a wall second, one a world second as now.

### 5.5 Link emulator

The emulator already runs every link on one number, nanoseconds since its start (`now_ns` in
`proxy.rs`): `Link::decide` gives each packet its delivery time on that clock (latency, jitter,
bandwidth queues, loss bursts, terrain and SATCOM outages, scripted events). Fast forward makes
that number world elapsed, and the delay queue's timer sleeps until the earliest delivery's wall
instant from the schedule, re-armed when the schedule changes (packets in flight keep their world
delivery times). Nothing in the link model changes, which is the point: a link's latency stays the
same in world terms, so its wall latency shrinks and its wall bandwidth grows with the rate. The
Iridium-class link and the 300 bps acoustic modem then behave at 10x exactly as at 1x in world
seconds: ArduPilot's 8 s mission-upload timeout (sim time) still meets the same world round trips.
Left on wall time, the acoustic link would carry a tenth of its world bytes, every upload would
time out, and a SATCOM handover outage would cost ten times the packets. SATCOM visibility reads
calendar time from the schedule; `/stats` reports `uptime_s` in world seconds (the situation
runner's zero reads it). The headless simulator already drives this code on its own clock.

### 5.6 SITL (phase 2)

`SIM_SPEEDUP` is a parameter SITL re-applies when it changes (`libraries/SITL/SIM_Aircraft.cpp`,
`set_speedup`), and its physics step is fixed in sim time (the speed-up only changes how long it
sleeps between steps), so the EKF sees nothing different. The spike confirmed it live (section 8.1).
Who sets it: not the bridge over the vehicle's own link (the emulator delays and drops that link,
and scripted cuts would leave a vehicle at the old rate; simulation control does not belong on the
C2 link) and not `tidewater drive` (conditions only, another repo). `sim/swarm.py`, which launched
the SITL vehicles and stays running, opens each one's SERIAL2 (TCP 5763 + 10 x instance, MAVLink2
by default, unused today), sends `PARAM_SET SIM_SPEEDUP` at each segment's monotonic instant, and
every 2 s compares the vehicle's `SYSTEM_TIME.time_boot_ms` with world elapsed and steers
`SIM_SPEEDUP` within 10 % of the rate so the vehicle's clock tracks the world (a SITL switches 5
to 18 ms after the request, section 8.1; steering removes that and its slow drift at 1x). A vehicle
that cannot keep up even steered is what the governor acts on. SERIAL2's stream rates go to 0 in
`sim/params/common.parm` (as SERIAL0's `MAV1_*` do) so it carries nothing it does not ask for.

### 5.7 tidewater (phase 2)

`tidewater drive` keeps its own scenario clock (`drivers/ardupilot.py`, `Clock(at, rate)` on
`time.monotonic()`), so after a fast forward its tides and winds would be minutes behind
everyone's. Change: a `--clock-file` option whose `Clock.now()` reads calendar time from the
schedule (the format above, documented in tidewater as a small contract, no Murmur import). Its push
period stays wall time: conditions then update every period times the rate in world seconds,
fine for tides and wind. Done on a branch of `~/tidewater` in a worktree, never in the working
tree the live stacks run from. Harbor watch has no SITL vehicle, so `up.sh` does not start
`tidewater drive` for it, and phase 1 does not need this.

### 5.8 `scripts/up.sh`

Writes the origin before tidewater, the emulator and the vehicles; passes `MURMUR_CLOCK_FILE` and
`MURMUR_FF_MAX` (the configured top rate, default 10); with `MURMUR_RATE` other than 1 it still
writes the file (calendar rate) and the bridge refuses a rate change, saying why.

### 5.9 Situation runner, storyline, autoplay proxy

Situation time is world time: the runner's zero is world elapsed at the emulator's start (its
world `uptime_s`), and every `at_s`, `ai_start_s`, `duration_s` and the 2 s status loop's elapsed
read the twin; waits that are about the world (`storyline.py`'s steps, the autoplay proxy's
attention and reading times, the scripted signers) sleep in world time. The proxy is part of the
simulated world in autoplay: its 20 s look interval stays 20 world seconds. At `duration_s` the
runner drops the rate (`POST /api/sim/clock`) before it leaves the stack up. The status document
carries the rate, so the panel's clock runs at it; `situation.jsonl` records each change; the
summary says how much was fast forwarded and, for each decision the person made, how long it
waited in world and in wall seconds. The AI's `--until` becomes a bound the runner never relies
on (it already stops the AI at the end). Each change keeps `eval/` behaviour identical without a
clock file, since the evaluation shares `storyline.py` and `proxy.py`.

### 5.10 Console

- **The control**, in the top bar beside the clock: `1x 2x 5x 10x` (rates above the top disabled,
  with why on hover) and **Until the AI asks**. While above 1x a pill says the rate and, for the
  second button, "until the AI asks"; with items still waiting it adds "2 waiting age 10x faster".
  On a drop, a notice in the top bar and the Situation panel: "Back to 1x at 07:58: the AI's orbit
  for PUMA 1 waits in your Inbox" (or the alert, the review item, the governor's reason, the end of
  the situation). Hidden, with the reason on hover, when `GET /api/clock` says fast forward is not
  available.
- **Rate-aware clocks.** `useBridgeClock` (`decisions.ts`) extrapolates at the rate (it assumes
  1x: at 10x it falls 9 world seconds behind for every wall second between updates); the scenario
  clock (`useScenarioNow`) already takes a rate and gets the effective one; the Situation panel's
  clock takes the status's rate; display smoothing (`glide.ts`) scales its extrapolation by the
  rate, or is off above 1x.
  Each of the console's 42 `Date.now()` and `performance.now()` reads is checked: compared with a
  bridge time it must go through the bridge clock (after a fast forward the bridge's times lead
  UTC); a person's own time (alert snoozes, how long a card was open) stays wall time.

## 6. Behaviour

### 6.1 The rates

1x, 2x, 5x, 10x by default, and the top rate when lower (section 8). No pause (SITL cannot freeze,
and a stack that stops sending heartbeats trips every failsafe) and nothing below 1x (not asked
for).

### 6.2 What drops to 1x

The moment, within the 250 ms lead (2.5 world seconds at 10x), that something new needs the
operator:

- **a new pending approval**, from any source: an AI's proposal, a scripted agent's, a human-tier
  order of another station. The engine drops the rate in the same step as the transition;
- **a new identification review item**: a rule declaration to confirm, a flag, a claim on a lost
  contact (also the engine's);
- **an alert that asks for an act**: may be running, values owed back, a failed carry, a spent
  platform still heard, a space task missed (the bridge's, which asks the engine to log it);
- **the governor**: a follower fell behind (section 8.4);
- **the end of a situation** (the runner).

Not: a lost link, a vehicle going home on its own failsafe, a new track, an order going out by
policy, a hold or rtl an AI proposed (these go out at once and wait for no one). A list in code,
not policy: no policy schema change.

### 6.3 "Until the AI asks"

The top rate, on the same terms: in practice it ends at the AI's first proposal that waits for the
operator. The button says what it waits for, and with `AI=none` it says it will run to the end. It
is the same mechanism as the rate buttons because a run that kept going at 10x while something else
waited for the operator would take their time away from it.

### 6.4 Fast forward while something waits

Allowed. The operator chose it; the items age at the world's rate (an approval's 120 s is 12 wall
seconds at 10x) and the button and pill say so before and while it runs. Refusing it instead would
make one stale card stop the whole run (open question 4).

### 6.5 Refused

- **A credentials run, and so every study.** Human time under pressure is what those measure.
  Refused by the engine (section 7.2), so replay can check it.
- **Any engagement in play** (nominated, signing, approved, under way). Its windows are the
  tightest and the decision the most consequential. A nomination drops the rate (it is a pending
  approval) and no rate above 1x is taken until the engagement ends.
- **`MURMUR_RATE` other than 1.** That mode moves the contacts and the calendar at the rate while
  the vehicles fly at 1x (section 10); composing a fast forward on it would mean nothing clear.
- **No clock file**: the evaluation (`eval/stack.py`), the headless simulator, a bridge started by
  hand.

## 7. Audit log, rules and replay

### 7.1 The event

```json
{"type": "clock.rate", "rate": 10.0, "from_rate": 1.0, "effective_t": 1791886931250,
 "until": "agent_proposal", "top": 10.0,
 "by": {"operator": "op-1", "session": "s-3"}}
```

`by` is an operator under their session, or `{"system": "pending_approval", "intent": 12}`,
`{"system": "review", ...}`, `{"system": "alert", "alert": "may_be_running:3"}`,
`{"system": "governor", "detail": "PUMA 1 held 7.3x of 10x"}`,
`{"system": "situation_end"}`. `effective_t` is when it takes effect on the engine's (world)
clock: the event's `t` plus `LEAD` times the rate it leaves.

### 7.2 What the engine does

- `Engine::set_rate(by, rate, until)` logs it, or refuses (nothing logged): in a credentials run,
  with an engagement in play, above the top rate. The engine knows the run's authentication and
  every engagement; the bridge's own refusals (`MURMUR_RATE`, no clock file) come before it.
- When an intent enters `pending_approval`, or a review item is raised, while the logged rate is
  above 1, the engine logs `clock.rate` back to 1 right after it, in the same step, citing it. The
  bridge writes the schedule from the engine's events, never the other way round: the engine is the
  authority on the rate, the bridge on the clock.

### 7.3 Replay

- **Decisions:** unchanged. Every time in the log is a world time, and every decision is
  recomputed from logged times as now.
- **New checks:** a `clock.rate` only in a dev run (`run.started`'s roster and authentication),
  never above 1 while an engagement is in play, never above its `top`; every pending transition or
  review item logged while the rate was above 1 followed at once by its drop; an operator's change
  under that operator's session on the roster; `effective_t` within `LEAD` times the rate it leaves.
- **With `--anchors`: the clock ran as the log says.** Every anchor records UTC beside the log's
  time. The lead the log's `clock.rate` events imply at an anchor's time must equal the anchor's
  `t - utc` within the steer's bound (2 s plus 5 %), so a log whose clock ran faster than its
  events say (or a 1x claim on a fast run) fails. It needs no new data.
- **Reported:** the rate history, and for each human decision how long it waited in world and in
  wall seconds (from the segments).
- **What replay cannot show:** that the followers really ran at the schedule (that is the
  governor's job, live, and its drops are logged), or how long a person looked at anything (dev
  runs are never evidence about people).

### 7.4 Rules version and carry-over

Rules 17, `FAST_FORWARD_SINCE`, read through the named constant: `clock.rate` joins the
`RULES_MARKERS` (a v16 log carrying one is flagged), the engine's refusals and drops are gated on
it, `RULES_VERSION` moves to it. Cost: logs of rules 16 replay with `--rules 16` as after every
bump, and the headless simulator's golden episode is re-digested (its `run.started` says the rules
version). Without a clock file nothing else in a log changes.

**Carry-over.** A restarted bridge starts its clock at UTC; a log that fast forwarded is ahead of
UTC by its lead, so the open intents it carried would bring deadlines that much late (the bug
DECISIONS' clock entry fixed for drift). So a log with a lead over 2 s carries no open intents (a
crash after a fast forward drops its open orders, saying why; such runs are dev runs and situations,
which start as fresh stations anyway). What a log owes is still carried (a platform's own lost-link
values, a spent mark): the one time compared across logs found in this review is a spent mark's
assessment window, which a lead only makes longer, the safe way (implementation checks for others).

## 8. Top rate: measured, what limits it, how the console learns it

### 8.1 Spike: SITL takes a live speed change

One SITL vehicle on its own (instance 200, UDP in this job's block), `SIM_SPEEDUP` set over
MAVLink every 11 s, the GCS heartbeat sent at the rate in wall Hz, `SYSTEM_TIME` and
`GLOBAL_POSITION_INT` asked for at 2 Hz (sim time); achieved rate from `time_boot_ms` against the
wall clock over 8 s; CPU from `/proc`. Host: 28 cores, load average 48 to 71 from other work.

| rate asked | ArduCopter (quad) achieved, CPU (cores), msg/s | ArduPlane achieved, CPU, msg/s |
| --- | --- | --- |
| 1 | 1.00x, 0.11, 6 | 1.00x, 0.10, 6 |
| 2 | 2.00x, 0.14, 11 | |
| 5 | 5.01x, 0.20, 26 | 5.01x, 0.18, 29 |
| 10 | 9.98x, 0.33, 51 | 9.98x, 0.27, 56 |
| 20 | 17.51x, 0.45, 90 | 19.62x, 0.38, 111 |
| 50 | 30.98x, 0.72, 159 | |
| 100 | 10.66x, 0.32, 54 | |

Down as well as up: ArduPlane asked 10, 1, 10, 2, 1 achieved 10.03x, 1.00x, 10.02x, 2.00x, 1.00x.
How soon: fitting `time_boot_ms` sampled at 20 Hz (sim time) before and after a request, the clock
changed rate 18 ms after it going from 1x to 10x and 5 ms after it going back (the request's UDP
hop included), so a SITL vehicle that is told at the segment's instant is 0.2 world seconds off at
most per switch at 10x, before any steering. SITL runs flat out when it cannot keep up (it does not
catch up later: its sleep debt is clamped), and past about 30x the copter's achieved rate fell. The
Golden Gate default fleet runs 15 SITL vehicles: about 4 to 5 cores at 10x, 6 to 7 at 20x with each
falling 2 to 13 % short on this host. Harbor watch has none.

### 8.2 The stall margin

A scheduling stall of `s` wall seconds is `rate x s` world seconds in which the bridge sends no
heartbeat and the emulator forwards nothing. The shortest world timer it can trip is a Puma's 5 s
lost-link failsafe (`sim/situations/README.md`): at 10x a 500 ms stall trips it, at 20x 250 ms. A
10 ms sleep on this host woke late by 0.10 ms (median), 5.3 ms (99th percentile), 12.1 ms
(99.9th) and 18.2 ms at most in one minute at load average 69 (`time.sleep`, 5,707 sleeps); rarer
long stalls happen (the stand-in engine's notes record a 7 s one). 10x leaves a margin of about 25
times the worst minute's stall.

### 8.3 Load

Stand-in fleets are not CPU-bound at these rates: the message load scales with vehicles times the
rate, and the load test (`docs/load-test.md`) ran 300 stand-ins at 1x (2,460 msg/s) for 3.2 to
3.7 % of a core in the bridge and 3.3 % in the stand-in engine. Harbor watch at 10x is about 320
msg/s, the 22-node fleet at 10x about 1,800. The decision tick runs 10 times a wall second at 10x,
the tracker takes 10 truth messages; neither is measurable at Harbor watch's size. Not measured
live, since nothing is built; phase 1 measures it (section 11).

### 8.4 The governor, and how the console learns the top

The configured top is `MURMUR_FF_MAX` (default 10; a situation file may lower it). The measured
top is what the followers hold, from data the bridge already gets: each vehicle's `time_boot_ms`
against the world time its messages arrive (the latest-arriving offset only grows if the vehicle's
clock falls behind), the contacts' `s` against arrival, the emulator's world `uptime_s`. If any
falls behind by more than 5 % of the world time over a 20 world-second window, the governor drops
to 1x, logs why (`by.system = "governor"`), and lowers the top for the rest of the run to the
highest offered rate below what was held. `GET /api/clock` gives the top, its basis, and the table
behind it; the console disables the buttons above it and says why on hover.

**Recommended default: 10x for both fleets.** Stand-in fleets: bound by the stall margin and by
what a person can follow (a 15 m/s boat crosses 150 m of map a wall second at 10x), and the AI's
latency (section 9), not by CPU. SITL fleets: 10x held at about 0.3 of a core per vehicle; 20x
held only roughly on a loaded host.

### 8.5 Other things wall time does at speed

Anything the stack computes in wall time takes the rate times longer in world time. Two matter:

- A collect's space answer, prepared outside the engine's lock, must be within 5 s of its check
  (`PREPARED_SPACE_MAX_AGE_MS`, rules 13): 0.5 wall seconds at 10x, so a slow query fails closed
  ("too old"). Space tasks are best submitted at 1x; stated in the console's space form.
- The AI (section 9).

## 9. What fast forward does to the AI

The AI runs on wall time: a model wake takes 10 to 30 wall seconds, so at 10x it reads the picture
100 to 300 world seconds before its proposal lands. The engine checks the proposal on the world as
it is then, so a stale proposal fails its checks or waits; nothing unsafe follows. For "until the
AI asks" that is acceptable: in Harbor watch the boat holds near the bridge from 6:10, so an orbit
proposed late still fits, and at 10x the person waits about a minute of wall time for it instead
of five or more.

| agent | timer | about | at 10x |
| --- | --- | --- | --- |
| watch officer | model call, turn timeout (300 s), wakes per hour (60), escalation cooldown (1800 s), minimum gap between wakes (10 s) | the model and the user's subscription | wall; unchanged (a budget must not burn faster) |
| watch officer | coalesce (3 s), heartbeat (60 s), flap window (300 s), one subject per 300 s | the world, but on `time.time()` | wall in phase 1: it wakes on a world ten times more eventful; open question 6 |
| watch officer | `render_mine` compares the bridge's history times with `time.time()` (`agents/watch/picture.py`) | a bug once the bridge leads UTC: ended proposals stay "recent" | fixed in phase 1 to use the bridge's time from the read |
| playbook | tick (2 s, `time.time()`) | pacing | wall: a tick every 20 world seconds; it proposes up to 20 world seconds later |
| playbook | cooldowns, inbox budget, thresholds (`rejected_s`, `human_every_s`, `stuck_s`, `unseen_s`, ...) | the world | already on the bridge's `t` from its reads, so world time with no change |
| autoplay proxy | attention (20 s), reading (6 s plus 1.5 s a check) | a simulated operator | world (section 5.9) |

## 10. Findings outside fast forward

- **`MURMUR_RATE` other than 1 already undercounts keep-outs.** The contacts move at the scenario
  rate while the bridge's clock runs at 1x, so a track out of view for 10 scenario seconds counts
  in a keep-out as 1 s at 10x, and the filter's fix intervals (scenario clock) disagree with its
  ages (bridge clock). No shipped spec or situation sets a rate other than 1, and the evaluation
  documents the mode, so nothing has produced an unsafe result; it should be refused for any run
  with engagements, or redefined so `MURMUR_RATE` moves only the calendar (tides, satellites) and
  physics follows world time (worth a bd issue either way).
- **Each process's scenario clock starts at its own start**, seconds apart (the scenario clock's
  own doc says `MURMUR_AT` is "what the scenario clock reads when the process starts"). The
  schedule's origin removes that for stacks `up.sh` starts.

## 11. Phases and effort

Working days of one implementer, before review rounds (this repository's features have taken one
to three, a day or two each).

**Phase 1: stand-in fleets (Harbor watch).**

| piece | days |
| --- | --- |
| `murmur_space::run_clock` and the Python twin, shared test vectors | 1 |
| bridge clocks, world timers (about 35 sites), heartbeat thread; headless golden unchanged | 2 |
| bridge API, WebSocket, drop triggers for alerts, governor, carry rule | 1.5 |
| engine `clock.rate`, `set_rate`, drops, replay checks and the anchors check, rules 17, README | 2 |
| stand-ins, contacts, link emulator (each with a test that a rate change leaves world behaviour as at 1x) | 1.5 |
| `up.sh`, situation runner, storyline, proxy (no change without a clock file) | 1 |
| console control and rate-aware clocks | 1.5 |
| live: Harbor watch at 10x on autoplay with the playbook and live with Claude; replay with anchors; load and the governor measured; docs (DECISIONS, CLAUDE.md, the situations README, the emulator's and contacts' READMEs) | 1 |

About 11 days as listed; 8 to 10 with the overlap between pieces.

**Phase 2: SITL fleets.** `swarm.py`'s SERIAL2 follower with steering (1), `tidewater drive`'s
`--clock-file` on a tidewater branch (0.5), Golden Gate at 5x, 10x and 20x measured, the governor
tuned on SITL (1), docs (0.5): about 3 days.

A cheaper phase 1 exists: rate changes logged by the bridge outside the audit chain (in its log
and `situation.jsonl`), drops decided by the bridge alone, no rules bump. It saves about 2 days and
loses replay's check that the person got their time back.

## 12. Open questions

1. **Rules 17, or no bump?** Recommended: rules 17, since the engine starts refusing and dropping
   and replay checks it. Without the bump, `clock.rate` is accepted in any log and its checks apply
   where it appears; no golden re-digest, no `--rules 16`.
2. **The lead: 250 ms** (2.5 world seconds at 10x before a drop takes effect), with followers
   polling every 50 ms. Shorter needs a push to the followers.
3. **The default top: 10x**, and the offered steps 1, 2, 5, 10.
4. **Fast forward with items already waiting:** allowed with the warning (recommended), or refused
   until they are decided?
5. **Autoplay:** drop to 1x when an item waits for `op-auto` too (recommended: the same rule, so the
   viewer sees each decision), or keep going?
6. **The watch officer's world-facing timers** (coalesce, heartbeat, flap, per-subject): leave on
   wall time in phase 1 (recommended), or read world time? It has no clock file (it runs with no
   `MURMUR_*` variable, in an empty directory), so it would infer the rate from the bridge's times.
7. **Watch items:** should a rate above 1x stop at each of a situation's watch items? Recommended no
   (eight in Harbor watch); the panel lists them as they come due.
8. **A run that fast forwarded carries no open intents after a crash** (recommended), or the next
   bridge starts its clock at the carried lead?
9. **`MURMUR_RATE`'s mode** (section 10): refuse it with engagements, or redefine it to move only
   the calendar?
