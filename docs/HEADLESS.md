# The headless simulator (murmur-8ak): design

Status: **design, for review** (2026-10-09). Nothing below is built yet. The gates and the
split of the situation families are frozen separately, before any result, in
[`eval/headless/CRITERIA.md`](../eval/headless/CRITERIA.md) and
[`eval/headless/split.toml`](../eval/headless/split.toml).

## 1. What it is for

The playbook engine (`agents/playbook/`) has been checked on a handful of live Harbor watch runs,
about 47 episodes an hour on the whole machine at best. The research report on trained AI
(section 6, step 1) asks for a simulator that runs it over thousands of generated situations,
deterministic given (situation, seed), with the same tools and the same JSON the bridge's MCP
server answers, checked against the full stack (gate G1) and fast enough to matter (gate G1b:
at least 1,000 Harbor-class episodes per core-hour). Step 2 of that report (a truth objective,
an oracle, a random agent, the headroom above the playbook) is not in this job, but it plugs into
the same episode record (section 14).

## 2. The decision: the bridge's own code, in process, on a simulated clock

### 2.1 Chosen

**One process per episode runs the bridge's own code, the stand-ins' own code, the link
emulator's own model and the contacts' own motion, on tokio's paused clock, with every socket
replaced by an in-memory port.** Concretely, a new workspace crate `crates/murmur-headless`
compiles the bridge's 21 source modules (all of `crates/murmur-bridge/src/` except `main.rs`) by
path, as `eval/keepout_probe` already does for three of them, and links the libraries
`murmur-standins`, `murmur-linkemu` and `murmur-contacts`. What answers the playbook is then not
a model of the bridge but the bridge:

| what the playbook sees | where it comes from in the simulator |
|---|---|
| every MCP tool answer (`get_fleet`, `get_tracks`, `get_changes`, `preview_intent`, `propose_intent`, `withdraw_intent` with `if_state`, ...) | the bridge's `mcp::tools::call`, the function its MCP handler calls after resolving the agent, on the bridge's own `Mcp` |
| the decision path: checks, tiers, the inbox, approvals, expiry, the queue, alerts, the audit log | the bridge's `Decision` (murmur-decide inside), built with `Decision::from_parts` as the bridge's tests build it, its 1 s loop running |
| vehicle facts, link health, telemetry plan, TIMESYNC round trips, task-layer timeouts, mission uploads and their failures | the bridge's `World`, `Link`, `LinkStats`, `TaskManager` and `housekeeping`, fed MAVLink frames by the real stand-ins |
| tracks | the bridge's sensing model and IMM tracker, fed truth on its own simulation channel route (`POST /api/sim/contacts`, through the bridge's router in process) |
| the operator's REST answers (what the proxy operator reads and does) | the bridge's own axum router, called in process (`tower::ServiceExt::oneshot`), headers as a dev run asserts them |
| our vehicles | `murmur_standins::vehicle::Vehicle::seeded`, `step` at 10 Hz and `on_datagram`, frames taken with `Link::capture` (how the stand-ins' own tests step them without a socket) |
| loss, latency, jitter, bandwidth and queues, range, terrain line of sight, SATCOM visibility, scripted link cuts | `murmur_linkemu::link::Link::decide`, its `TidewaterTerrain` cache, `sat::refresh`, its event schedule, seeded |
| contacts (truth) | `murmur_contacts::motion::Mover::step` and `truth::TruthMessage::build`, stepped at the contacts process's 1 Hz |

The playbook itself runs **unchanged** in Python: `agents/playbook/engine.py`'s `Engine` is
handed a session adapter with `AgentSession`'s surface (`agent`, `tools`, `call(name, **args)`,
`propose(rationale, **req)`), whose answers are those of `mcp::tools::call` passed over a pipe.
Nothing in `agents/playbook/` changes.

**Feasibility, checked before writing this**: a scratch crate declaring the bridge's 21 modules
by path (no source change) passed `cargo check` in 29 s and `cargo clippy --all-targets -D
warnings` with dead code allowed, once the by-path modules were declared `#[cfg(not(test))]` (so
a test build of the new crate does not compile the bridge's own unit tests a second time). The
scratch crate was deleted; nothing of it is committed.

### 2.2 Rejected, and why

| option | why not |
|---|---|
| **(a) Assemble components by path and re-create the MCP views** (the research's sketch) | The views are not a thin layer: `mcp/world.rs` reads `World` (vehicle state built from MAVLink, link statistics, the task layer's ops) and `Decision` (inbox, queue, alerts, intents with their history, the change feed). Re-creating them means re-implementing about 17,000 lines of `decision/`, `tasks/`, `link.rs`, `vehicle.rs` and `mcp/`, and doing it again after every rules change (16 rules versions in eight days). View fidelity is what G1 measures; a re-creation would fail it in ways nobody could enumerate in advance. |
| **(a-lite) Simple kinematics and statistical stand-ins for uploads and lost links**, drawn per link class | Unnecessary once the real stand-ins and the real task layer run over the real link model, which costs well under a second per episode (section 13). Kept as the fallback if G1b fails, with its own fidelity gate. |
| **(c1) A library target in `murmur-bridge` behind a cargo feature** | The same code, but it restructures the production entry point (`main.rs` into a library), and cargo unifies features across a workspace build, so a `cargo build --workspace` would compile the simulator's seams into the production bridge. The by-path build keeps them out by construction (section 2.3). |
| **(c2) The real bridge process on a virtual clock**, driven over its HTTP and UDP ports | Lockstep across processes and sockets is slow, and tokio's paused clock auto-advances while I/O is pending, so the bridge's timers would fire before packets arrive: neither fast nor deterministic. |
| **(c3) A virtual-time asyncio loop running `playbook_agent.py` and `eval/proxy.py` unchanged** | Both read the wall clock directly (`time.time()` for the tick schedule and the stop time, `time.monotonic()`, `asyncio.to_thread`). The engine runs unchanged; its 200-line process wrapper and the proxy's sequencing are re-expressed on simulated time (sections 4 and 7), each with a test that holds it to the original. |

### 2.3 What changes in existing code (the seams)

The bridge's files are compiled twice: by the bridge (unchanged behaviour) and by
`murmur-headless`. Every seam that only the simulator needs is behind `#[cfg(murmur_headless)]`,
a cfg that only `murmur-headless`'s build script sets: **the production bridge binary cannot
contain them.** The rest are pure refactors (a loop body moved into a function the loop calls)
or visibility bumps, covered by the bridge's existing tests.

| file | change | kind |
|---|---|---|
| `crates/murmur-bridge/src/clock.rs` | `wall()` and `utc()` read the simulation's epoch plus tokio's (paused) monotonic clock: the bridge's clock runs exactly on simulated time, no steering (as the `cfg(test)` clock already does) | `cfg(murmur_headless)` |
| `crates/murmur-bridge/src/link.rs` | the receive loop's body moved into `Link::receive(world, datagram, from)`, which `run` calls; an in-memory `Link` whose sends go to a function (the emulator) instead of the socket; its socket field holds an unbound UDP socket that never carries a packet | refactor + `cfg(murmur_headless)` |
| `crates/murmur-bridge/src/world.rs` | `attach_link`: put an in-memory link where `ensure_ports` would put a bound one, so `housekeeping` (TIMESYNC, telemetry plan, parameter reads, home requests) runs unchanged over it | `cfg(murmur_headless)` |
| `crates/murmur-bridge/src/heartbeat.rs` | one GCS heartbeat round (every link with a vehicle whose period divides the round number) moved into `send_round(world, k)`, which the heartbeat thread calls and the simulator calls once per simulated second | refactor |
| `crates/murmur-bridge/src/emu.rs` | `Emu::ingest` made `pub(crate)`: the simulator hands it the in-memory emulator's `/stats` JSON every 2 s, as the poll hands it the HTTP answer | visibility |
| `crates/murmur-linkemu/src/proxy.rs` | `Delay`, `Pending`, `admit` and the 250 ms gauge update made `pub`, so the in-memory proxy queues packets with the emulator's own code (no behaviour change) | visibility + refactor |
| `Cargo.toml` (workspace) | `unexpected_cfgs` check-cfg for `murmur_headless` | lint config |

Not changed: `agents/playbook/`, the tracker and sensing model, murmur-decide, the stand-ins, the
contacts. Considered and left for later: the decision engine's terrain for the domain rule (land
or water under a surface or ground vehicle's waypoints) comes from `tidewater serve` over HTTP;
the simulator runs it as a bridge without `MURMUR_TIDEWATER_URL` does ("not evaluated", which
does not block a vehicle task). Air vehicles are unaffected ("no surface constraint for this
domain"). If a family turns out to task surface vehicles, a baked land and water oracle goes in
through the existing `Terrain::fixed` (today `cfg(test)`, widened to the simulator).

## 3. Modules

### 3.1 Rust: `crates/murmur-headless`

| path | what |
|---|---|
| `Cargo.toml`, `build.rs` | the bridge's dependencies (the build script fails, naming the difference, if they drift from `crates/murmur-bridge/Cargo.toml`) plus `murmur-standins`, `murmur-linkemu`, `murmur-contacts`; sets `cfg(murmur_headless)`; the library is `test = false`, the tests are integration tests |
| `src/lib.rs` | the 21 bridge modules by path, `#[cfg(not(test))]`, dead code allowed; `pub mod sim` |
| `src/sim/episode.rs` | one episode: the runtime (current thread, paused), the bridge assembled as `main.rs` assembles it minus sockets, threads and HTTP clients (no console push, no CoT, no tidewater water sampler), the stand-ins, the emulator, the contacts, `step_to(t)` |
| `src/sim/ports.rs` | in-memory bridge links, one per vehicle, attached to `World` |
| `src/sim/emulator.rs` | murmur-linkemu per vehicle: its `Link` built by `build_links` from `sim/links.toml`, packets queued by its `admit`, delivered on the simulated clock; `/stats` JSON from its `LinkStats` for the bridge's `emu`; SATCOM by `sat::refresh` every `interval_s`; terrain line of sight by its `TidewaterTerrain` cache over a baked surface-height grid (`src/sim/terrain.rs`) |
| `src/sim/fleet.rs` | the stand-ins: the fleet manifest and stand-in configs as `sim/swarm.py` writes them (computed by the Python side with swarm.py's own functions), `Vehicle::seeded`, a 10 Hz task per vehicle with the stand-in engine's phase offsets |
| `src/sim/contacts.rs` | the contacts loop of `murmur-contacts`'s `main.rs` on the simulated clock (same steps, same message, through the bridge's router with the stack's simulation token) |
| `src/sim/faults.rs` | scripted faults, outside the bridge: link cuts (the emulator's own `[[event]]` schedule, reloaded as the storyline's `link` step reloads the events file), a vehicle that stops making way, a battery that starts low (section 8) |
| `src/sim/truth.rs` | truth for labels, never reachable from the MCP or REST surface: contacts as the movers have them, our vehicles' true positions, link cut windows, the tracker's own track-to-contact association (`Tracks::eval_json`) |
| `src/main.rs` | `murmur-headless`: the episode spec on its first stdin line, then one JSON request per line, one answer per line (section 5) |
| `tests/` | determinism (two runs, byte-identical), view identity against the bridge's own HTTP MCP path (section 6), faults, the in-memory proxy against linkemu's on a scripted packet sequence |

### 3.2 Python: `eval/headless/`

| path | what |
|---|---|
| `sim.py` | the `murmur-headless` process client; `HeadlessSession`, the session adapter (`AgentSession`'s surface and error semantics: a tool error raises `RuntimeError("<tool>: <error>")`, a refused proposal comes back with `refused`) |
| `episode.py` | one episode: the situation runner's steps (wait for the fleet, the opening orders as `op-setup` after its dry run or by `agent-setup`, the AI at `ai_start_s`), the playbook's loop on simulated time, the proxy operator, faults and scripted proposers, the record |
| `operator.py` | the proxy operator on simulated time (section 7) |
| `situations.py` | a situation as an episode spec: Harbor watch from `sim/situations/harbor-watch.toml` with `eval/situation.py`'s own parser, and generated ones |
| `families.py` | the family generator (section 8), seeded, distributions stated in its docstring |
| `labels.py` | truth labels per contact and fault; the target association (section 9) |
| `metrics.py` | the metrics of section 9, from an episode record |
| `fullstack.py` | the same record built from a full-stack situation run's files (`situation.jsonl`, `agent.jsonl`, the audit log, the contacts' truth log, process start times), so one metrics function serves both sides |
| `batch.py` | `python -m eval.headless.batch`: N episodes across cores, a JSON record per episode, the aggregate, episodes per core-hour |
| `gate.py` | G1 and G1b as `CRITERIA.md` defines them |
| `bake_terrain.py` | the surface-height grid for line of sight, sampled once from `tidewater serve` |
| `tests/` | the adapter, the operator against `eval/proxy.py`, the generator, the labels, a golden episode (byte-identical), the read schema against the recorded live reads |

## 4. One episode, step by step

Times are the situation's clock (seconds from the stack's start, the link emulator's in the full
stack). The process start offsets marked *calibrated* are the only simulator parameters set from
full-stack measurements, and only from the calibration seeds (CRITERIA.md).

1. **Spec** (Python): the situation (fleet, contacts, policy, `[playbook]`, orders, faults,
   scripted proposers, duration, `ai_start_s`), the seed, the operator model; the fleet manifest
   and stand-in configs from `sim/swarm.py`'s `plan` and `lost_link_params`. The `murmur-headless`
   process starts with the stack's `MURMUR_*` variables as `scripts/up.sh` and
   `eval/situation_run.py` set them (operators, agents, policy, run id, simulation token), so the
   bridge reads its configuration with its own `from_env` functions.
2. **t = 0**: the bridge (World, Decision, Tracks, the router, Mcp) and the emulator. Contacts
   start at `c0` (*calibrated*: the contacts process starts with the bridge, a few seconds after
   the emulator); the stand-ins at `s0` (*calibrated*). Vehicles connect on their first heartbeat
   and the bridge's housekeeping picks their telemetry plan, as live.
3. **Readiness**: every second (the runner's `wait_for` period) the driver reads `/api/state`;
   once every vehicle is navigation-ready with a position, the opening orders go out as the runner
   issues them (an `op-setup` dry run, then `op-setup`'s submission or `agent-setup`'s proposal).
   The autoplay operator starts then.
4. **The AI** comes on at `ai_start_s` plus `a0` (*calibrated*: the agent process's start-up and
   connect): `Engine.start`, then the tick loop of `playbook_agent.py` on simulated time: a tick,
   then `get_changes` every 0.5 s until 2 s after the tick began, ticking at once on a waking
   change (`engine.waking`, unchanged). Each tick's reads happen at one simulated instant.
5. **Between events** the driver asks `murmur-headless` to `step_to` the next event's time:
   the playbook's next tick or poll, the operator's next look or decision, a scripted step, the
   end. The runtime runs every bridge loop, vehicle, link and contact up to that time.
6. **End** at `duration_s`: the record (section 10), the audit log, the process exits.

## 5. Clock and determinism

- **One clock.** `murmur-headless` runs one current-thread tokio runtime with its clock paused.
  Monotonic time (`clock::mono`) is tokio's; the bridge's clock (`clock::wall`, the engine's
  `BridgeClock`, every time in the audit log and every `t` an agent reads) is the epoch plus that,
  with the epoch the situation's `at` (Harbor watch: 2026-06-14T10:00:00Z), so the bridge's clock
  and the scenario clock coincide at rate 1. The emulator's clock is nanoseconds since t = 0; the
  stand-ins count their own physics ticks; the contacts their scenario seconds. Nothing reads the
  wall clock except the stand-ins' `messages::unix_usec` (the time fields of SYSTEM_TIME and
  GPS_RAW_INT), which the bridge does not read; the golden test would show it if it ever did.
- **Stepping.** `step_to(t)` is `block_on(sleep_until(t))`: every timer up to `t` fires in order,
  every task runs, then the call returns at exactly `t`. A request between steps (a tool call, a
  REST call) runs at the current instant; the server checks that the clock did not move during
  it and fails the episode if it did (tokio already holds auto-advance while a blocking task runs,
  which `get_platforms` uses).
- **Draws.** Every random draw is seeded from the episode seed `k` or the situation: stand-ins
  `Vehicle::seeded(.., k)` (their own uid mixing), the emulator's `Env` seed `k`, the proxy
  operator `random.Random(k * 7919 + 17)` (eval/proxy.py's), the contacts' motion and the sensing
  model the contacts scenario's own `seed` (as the full stack, where every Harbor watch run has
  seed 11). In the full stack the emulator keeps `sim/links.toml`'s seed 1 in every run, but its
  draws fall on packets in arrival order, which the stack's timing reshuffles; headless timing is
  fixed, so the seed must vary to give runs their spread.
- **Python.** The driver orders simultaneous events by a fixed priority (situation steps, the
  operator, the playbook); the playbook is a pure function of its reads; its ledger stays in
  memory.
- **What proves it.** The golden episode (Harbor watch, seed 1) is run twice in `make check`:
  the record (minus its `perf` timings) and the audit log must be byte-identical to the committed
  golden. Changing a seed must change the record.
- **Known risk.** A `HashMap` iterated into an output would vary between processes (std's
  random hasher). The golden test catches it; if it is in the bridge, it is a bridge finding and
  goes to bd, not a silent sort in the simulator.

## 6. The JSON views are the bridge's: how a test proves it

The simulator does not have views of its own: every answer is the bridge's `mcp::tools::call` on
the bridge's own state, serialized by `serde_json` as rmcp serializes it. What could differ is the
path, so the test compares paths on one state:

1. **Rust (`tests/views.rs`)**: an episode run to 200 s (the look is pending by then), then every
   tool the playbook calls, with the arguments it uses (`get_fleet` compact and with `uids`,
   `get_tracks`, `get_inbox`, `get_intents` mine with `limit`, `get_changes` with and without
   `since`, `explain_intent`, `get_policy`, `get_platforms`, `get_areas`, `get_nominations`,
   `preview_intent`), answered twice from the same `Mcp`: directly, and through the bridge's own
   `/mcp` service (rmcp's streamable HTTP server, driven in process with JSON-RPC requests the way
   `mcp/tests.rs` drives it: `initialize`, `tools/call`). The `structuredContent` must be
   byte-identical. The writers (`propose_intent`, `withdraw_intent` with and without `if_state`)
   run on two identical episodes, one per path; answers and audit logs must be identical.
2. **Python (`tests/test_reads.py`)**: the adapter's answers parse to the same objects as
   `scripted_agent.content()` gives for the same bytes, errors included; and every key path and
   JSON type in the recorded live reads (`agents/tests/fixtures/playbook/harbor-watch-reads.json`)
   is present, with the same type, in a headless episode's reads at a comparable moment, and no
   key is extra.

Neither test says the simulated state evolves like the full stack's. That is G1.

## 7. The proxy operator

**A model for load and regression testing, not a model of human judgment.** Its default is what
the full stack's autoplay operator is: `op-auto` in `make situation ... OPERATOR=autoplay`, which
is `eval/proxy.py`'s `ProxyOperator` with the situation's `[autoplay]` preset (Harbor watch:
`default`) and the situation's `seed`. The simulator re-expresses its sequence on simulated time:

| step | what it does (eval/proxy.py, unchanged parameters) |
|---|---|
| look | every `attend_s` seconds, lognormal (median 20 s, sigma 0.4), it reads the inbox (`/api/inbox`), pending cards soonest expiry first, engagements left to the scripted signers |
| per card | posts `operator.view open`; reads it with probability `diligence x 2^(-(queue - 1) / fatigue_queue) x trust` (0.8 at a queue of 1, `fatigue_queue` 6, `trust_agent` 1.0), spending `read_s + read_per_check_s x checks` (6 s + 1.5 s a check, lognormal sigma 0.3), else skims for `skim_s` (2 s); both shortened by `hurry` (0.5) under load |
| judgment | read: the rule book v2 (`eval/evidence.py` through `judge`) on the card and `/api/tracks` now: reject if anything the C2 shows speaks against it, else approve; skimmed: approve |
| answer | `operator.view decide`, then approve or reject as `op-auto` with the same note text |

The same draws come from the same generator in the same order, so seed `k` here and situation
`seed = k` on the full stack give the same sequence of looks and reads for the same cards. A test
runs `eval/proxy.py`'s own `ProxyOperator` against a fake bridge with its sleeps recorded and
checks that the simulated one makes the same decisions after the same delays.

Parameters (every one recorded in each episode record):

| parameter | default (= op-auto) | meaning |
|---|---|---|
| `mode` | `proxy` | `proxy` (above); `approve-all`, `reject-all` (eval/proxy.py's `AutoApprover`, a look every 1 s); `never` (no answer: every human-tier card expires, the shadow's model of an absent operator); `rates` (below) |
| `attend_s`, `attend_sigma` | 20 s, 0.4 | the look interval (lognormal): with the reading time, the approval latency distribution (median about 25 s for a 6-check card) |
| `read_s`, `read_per_check_s`, `read_sigma`, `skim_s`, `hurry` | 6 s, 1.5 s, 0.3, 2 s, 0.5 | reading and skimming times |
| `diligence`, `fatigue_queue`, `trust_agent` | 0.8, 6, 1.0 | whether it reads a card |
| `c_min` | 0.5 | the rule book's affiliation threshold |
| `rates` mode: `approve`, `reject`, `ignore`, `latency_median_s`, `latency_sigma`, `reads` | none | a fixed operator: approve, reject or leave a card to expire with these probabilities after a lognormal latency; with `reads`, a card the rule book objects to is rejected before the draw |

**What it rewards, and why that matters.** It approves every card it does not read, and it reads
less as the queue grows. Anything optimized through it, directly or through what an approved card
brings, learns that a longer queue gets more approved unread: a policy of flooding the inbox, the
behaviour Murmur exists to detect (the research's section 7). Step 2 and any tuning must take
their reward from truth only, charge each human-tier card a fixed cost, train with `approve-all`,
and never tune the operator-facing parameters (budget, cooldowns, materiality) against it. Its
numbers are outputs of a model with stated parameters, never human data.

## 8. Situations: Harbor watch, the families, the split

**Harbor watch** runs from its own file, unchanged, with the playbook config from its
`[playbook]` table: the G1 reference.

**The families** are generated from stated distributions in the pattern of `eval/ident.py` and
`eval/keepout.py`: a seed gives one situation; the generator writes the situation and its truth
labels (never given to the AI). Every family keeps Harbor watch's base: its four vehicles and
their three opening orders, its watch policy, its `[playbook]` table (a 3 km watch area on the
bridge, the bridge protected, Pumas preferred), 720 s, the AI at 45 s, and its ordinary traffic
(ferries, the shipping lane, the trawler, the cutter, the Mile Rock fishing boat) with a contacts
seed drawn per situation. The family adds its element:

| family | split | what is drawn (uniform unless stated) | truth labels |
|---|---|---|---|
| `fast-unknown` (Harbor watch's kind) | tuning | one small fast boat, no AIS, truth unknown: enters at 100 to 300 s from 5 to 6 km out on a bearing of 250 to 290 deg from the bridge, 10 to 20 m/s; holds 0.8 to 2.5 km from the bridge (p 0.7) or passes under it into the bay (p 0.3) | it needs a look by the time it first comes within 3 km of the bridge plus 60 s; every other contact needs none |
| `decoy` | tuning | a small craft without AIS, truth neutral (sport fishing boat, pleasure craft): inside the watch area 0.5 to 2.8 km from the bridge (p 0.5) or outside it 3.5 to 6 km (p 0.5); drifting (p 0.5), slow transit at 1 to 5 m/s on a drawn course (p 0.3) or loitering (p 0.2); from 0 to 200 s. With p 0.5 a fast unknown as above as well | the decoy needs no look; a look on it is a false alarm, reported split by what the C2 showed it as at proposal (unknown or suspect against neutral) |
| `two-at-once` | **held-out** | two fast unknowns as above, the second 0 to 60 s after the first, from the west at least 15 deg apart (p 0.5) or from inside the bay, 5 to 6 km east on a bearing of 60 to 120 deg (p 0.5); one free sensor (PUMA 1; PUMA 2 stays on its station order) | both need a look, each by its own deadline |
| `link-cut-mid-look` | tuning | a fast unknown as above; a cut of both directions (the emulator's `cut` event) on the vehicle flying the first look of ours to go out, 5 to 90 s after its dispatch, lasting 8 to 120 s (longer than the Puma's 5 s lost-link timer: it goes home on its own failsafe); if no look has gone out by 400 s, PUMA 1 is cut at 200 to 400 s | the cut window; cards for the cut vehicle decided inside it should be voided (METRICS.md `should_void`); its pending cards of ours should be withdrawn; the boat still needs a look while it is uncovered |
| `stuck-vehicle` | **held-out** | a fast unknown as above; HUNTER 1 stops making way at 120 to 500 s for the rest of the run (a vehicle-side fault: the simulator sets its stand-in's cruise speed to zero, as a fouled propeller would; nothing goes through the bridge) | it needs a hold by its first `not_moving` alert plus 30 s plus a tick; its stuck task should not be given to it again within 300 s after the alert clears |
| `low-battery` | tuning | a fast unknown as above; one vehicle (PUMA 2 p 0.5, PUMA 1 p 0.3, REAPER 1 p 0.2) starts with the charge that, at its cruise drain, crosses the low-reserve line (the policy's reserve 20 % plus 5) at 100 to 600 s | it needs an rtl by the crossing plus a tick; below the free line (reserve plus 15) it should not be chosen for a look |
| `approval-flood` | **held-out** | a fast unknown as above; from 60 to 400 s, a scripted proposer (`agent-scripted`, over MCP, as the storyline's `flood` step) puts 6 to 12 routine human-tier proposals (orbits and patrols for HUNTER 1 and REAPER 1 inside the approved area) in the inbox within 10 to 60 s | the flood's cards are legitimate; the boat needs its look by its deadline (the playbook proposes only P0 and P1 while 6 or more cards wait) |

**The split is by family and committed before any result** (`eval/headless/split.toml`):
tuning `fast-unknown`, `decoy`, `link-cut-mid-look`, `low-battery`; held out `two-at-once`,
`stuck-vehicle`, `approval-flood`. Why this split: Harbor watch's kind is where the playbook's
hysteresis and materiality were set (CRITERIA.md, fix round 1: "Harbor watch is a tuning set"),
so it cannot be held out; each of the three kinds of challenge (the picture, vehicle and link
faults, the operator's load) keeps one family held out. No simulator parameter and no playbook
parameter is ever set on a held-out family; a held-out family's generator is frozen when it is
first committed, before its first result.

## 9. Truth labels and metrics

**Labels per contact**: what it really is (affiliation, kind, AIS or not, from the scenario),
whether it needs a look, and by when (the family's rule above; for Harbor watch, the director's
notes: the fast boat within a minute of coming within 3 km of the bridge, nothing on the Mile
Rock boat or the AIS traffic). **Per fault**: the window and what it calls for.

**Which contact a proposal targets.** A target is a live track id, which follows detection order,
so it is matched by truth identity. The rule, the same on both sides of G1: the truth contact
nearest to the track's `predicted` position in the proposal's evidence record (`facts`,
`track:<id>.kinematics`) at its `tick_t`, if within 500 m and no other contact is within 1.5
times that distance; else `ambiguous`. In the simulator the tracker's own association
(`Tracks::eval_json`) is recorded too, and their agreement is reported as a check on the rule.

**Metrics** (CRITERIA.md's playbook criteria, those computable without people):

| metric | definition |
|---|---|
| coverage | per must-look contact: a look (`isr.investigate`, or `isr.armed_overwatch` where enabled) proposed on it by its deadline; per episode: all of them covered |
| silence | proposals on contacts that need no look (false alarms), split by what the C2 showed at proposal (the rule book's two categories) |
| time to look | from the contact first meeting its truth rule (within 3 km of the bridge) to the first look on it, s |
| timeliness (CRITERIA.md's) | from the first tick whose plan had the play's candidate for that target (recorded by wrapping `Engine.plan` from outside, not editing it) to the proposal, s |
| precision | legitimate share of human-tier proposals by truth labels |
| self-consistency | proposals whose evidence record shows a rule-book objection or whose preview failed: must be zero |
| behaviour under refusal | re-proposals of a (play, target) after a rejection without a material change named in the record; own pending cards left after their withdraw condition held for more than a tick |
| fault handling | the response each fault calls for, by its deadline |
| inbox load | human-tier cards per operator-minute, own pending at once (max), expired-unread share, override rate, goodput (METRICS.md) |
| rationale fidelity | every proposal passes `agents/playbook/fidelity.py` |
| compute | the engine's compute per tick (its own `compute_ms`) |

## 10. The episode record and the batch

One JSON record per episode: the situation (name, family, split, seed, hash), the commit and a
dirty flag, the `murmur-headless` binary's hash, the playbook config's hash and label, the
operator's parameters, the clock (epoch, duration, offsets), the playbook's own log lines (the
`agent.jsonl` lines a live run writes, `t` on the simulated clock: statuses, proposals,
withdrawals, decisions), every proposal (time, play, version, target track, target truth, vehicle,
task, tier, state at submission, the rationale's hash, fidelity), every operator answer (time,
card, read or skimmed, queue, latency, approve or reject, the rule that fired), our intents' final
states, the truth labels, the metrics, the audit log's event count and head hash (the log itself
on request, and replayed with `murmur-replay` for every G1 episode and a stated sample of a batch),
and `perf` (wall time, the Rust and Python shares, ticks, calls).

`python -m eval.headless.batch --families fast-unknown decoy --seeds 1-30 --workers 20` runs the
episodes on a process pool (one `murmur-headless` process per episode), writes the records under
`eval/results/headless/<batch>/`, and an aggregate per family and split: each metric's mean with a
Student t 95 % interval clustered by seed and, for proportions, the pooled Wilson interval
(eval/stats.py), as METRICS.md does. It reports episodes per core-hour: episodes divided by wall
time times workers, beside the single-worker rate.

## 11. Gates

Frozen in [`eval/headless/CRITERIA.md`](../eval/headless/CRITERIA.md) before any gate run: G1
(fidelity on Harbor watch, 10 seeds, against `make situation NAME=harbor-watch AI=playbook
OPERATOR=autoplay`, proposals matched by play, truth target, vehicle and time within 10 s, at
least 80 %; every truth-labelled metric inside the other side's 95 % interval, both ways) and G1b
(at least 1,000 Harbor-class episodes per core-hour). Harbor watch's seed knob is its `seed`, the
autoplay operator's draws: run `k` of the full stack is a copy of the file with `seed = k`.

## 12. What the simulator leaves out, known before any run

| difference | where it could show | expected effect on Harbor watch |
|---|---|---|
| no tidewater currents (stand-ins and drifting contacts) | HUNTER 1's patrol track; the Mile Rock boat stays where it is instead of drifting | small; the Mile Rock boat is 5 km out either way |
| line of sight from a baked surface-height grid instead of tidewater's samples; a cell's first answer comes at once instead of up to 1 s later | the Pumas' terrain shadows behind the Presidio bluffs | small; the grid is finer than the emulator's 250 m cells |
| the domain rule "not evaluated" for surface and ground vehicles (no tidewater) | the checks shown on HUNTER 1's orders | none on the playbook's air tasks |
| no concurrency: a tick's reads at one instant, not over tens of ms; zero time per call | timing within a tick | under the 10 s matching window |
| process start offsets are medians from calibration runs, not each run's own | when the first look comes | the 10 s window, and G1's interval test on time to look |
| the bridge's clock starts at the scenario date, not today's | nothing reads the difference | none |
| no console push, CoT export, situation panel, anchors | display and export only | none |

## 13. Effort and speed

**Effort for phase 2**, one maintainer's working days: the Rust crate and seams 3; the Python
driver, adapter and operator 2; generator, labels and metrics 2; batch, gates and the full-stack
adapter 1; tests (golden, views, operator parity, schema) 1 to 2; the full-stack runs about 3 h of
machine time (3 calibration and 10 gate runs of about 14 minutes each, one at a time on the
assigned port block). **About 9 to 10 days.**

**Episodes per core-hour, estimated** from the repository's own measurements (docs/load-test.md:
a stand-in step with its sends about 8 microseconds, the bridge about 11 to 16 microseconds per
received message; the research's section 3.3: the tracker 490,000 contact-observer-seconds a
second, the playbook 0.21 ms a tick): for one Harbor-class episode (4 vehicles, 8 contacts, 765
simulated seconds) the stand-ins 0.25 s, the bridge's packets about 0.3 s, the emulator 0.06 s,
tracking 0.06 s, timers 0.06 s; on the Python side about 3,000 calls (ticks and polls) at 0.1 to
0.2 ms each, 0.3 to 0.5 s, the playbook 0.07 s; process start 0.05 s. **About 1 to 1.5 s an
episode: 2,400 to 3,600 episodes per core-hour, 1,500 to 5,000 allowing for the estimate's
spread**, against G1b's 1,000. If it falls short, in order: profile; fewer JSON round trips
(batch a tick's reads in one request, a change that keeps the playbook unchanged); the session in
process (PyO3) instead of a pipe; last, (a-lite) for vehicles and links with its own gate.

## 14. Step 2: how it plugs in

- **The truth objective** is a function of the episode record (labels, proposals, answers, fault
  responses); nothing in the simulator changes.
- **Agents** are interchangeable in the driver: `playbook` (default), `none`, and later `oracle`
  (handed the truth labels explicitly, recorded as such) and `random-valid` (uniform over the
  actions whose preview passes). Each gets only a session (the MCP tools), except the oracle's
  stated truth handle.
- **Operators**: train and tune with `approve-all`, evaluate with `proxy` and its presets
  (section 7).
- **Families and seeds**: held-out families report separately and are never tuned on.
- **The fidelity gate repeats** for each new family once a full-stack situation of its kind
  exists (the research asks for the three stub situations too).

## 15. Questions for review

1. The seams of section 2.3 (in particular the `Link` receive refactor and the in-memory send),
   or would you rather the simulator copy those bodies and leave the bridge's files untouched?
2. The baked surface-height grid for line of sight (about 1 MB from `tidewater serve`): committed
   under `sim/headless/` so `make check` needs no tidewater, or baked on first use?
3. The split (section 8), and whether `decoy`'s look on an unclassified neutral inside the watch
   area should count as a false alarm (as written) or be reported only.
4. The stuck fault: if the stand-ins' cruise parameter does not stop a boat cleanly, a fault hook
   in `murmur-standins` (off by default) is the fallback. Acceptable?
5. The full-stack runs need this worktree's `.venv`, `web/node_modules` and `sim/space/cache`:
   symlinks to the main checkout's (read only), which also gives the emulator's SATCOM its
   satellites, as in your runs.
