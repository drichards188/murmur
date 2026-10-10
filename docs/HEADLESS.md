# The headless simulator (murmur-8ak)

Status: **built** (phase 2, 2026-10-09); gates run, results in section 11. The design was
reviewed before any code (commit 89c8bed); this file now describes what was built, and says
where that differs from the design. The gates and the split of the situation families were
frozen separately, before any result, in [`eval/headless/CRITERIA.md`](../eval/headless/CRITERIA.md)
and [`eval/headless/split.toml`](../eval/headless/split.toml).

**What its numbers are.** Every result from it is a model result: a scripted operator on scripted
or generated situations. The operator (section 7) is a load and regression model, not a model of
human judgment, and anything optimized through it learns to flood the inbox.

## 1. What it is for

The playbook engine (`agents/playbook/`) had been checked on a handful of live Harbor watch runs,
about 47 episodes an hour on the whole machine at best. The research report on trained AI
(section 6, step 1) asks for a simulator that runs it over thousands of generated situations,
deterministic given (situation, seed), with the same tools and the same JSON the bridge's MCP
server answers, checked against the full stack (gate G1) and fast enough to matter (gate G1b:
at least 1,000 Harbor-class episodes per core-hour). Step 2 (a truth objective, an oracle, a
random agent, the headroom above the playbook) is not in this job; it plugs into the same
episode record (section 14).

## 2. The bridge's own code, in process, on a simulated clock

**One process per episode runs the bridge's own code, the stand-ins' own code, the link
emulator's own model and the contacts' own motion, on tokio's paused clock, with every socket
replaced by an in-memory port.** The crate `crates/murmur-headless` compiles the bridge's 21
source modules (all of `crates/murmur-bridge/src/` except `main.rs`) by path and links the
libraries `murmur-standins`, `murmur-linkemu`, `murmur-contacts`. What answers the playbook is not
a model of the bridge but the bridge:

| what the playbook sees | where it comes from in the simulator |
|---|---|
| every MCP tool answer | the bridge's `mcp::tools::call` on the bridge's own `Mcp` (`mcp::Mcp::new`), after logging `session.connected` as the MCP handler does on an agent's first call |
| the decision path: checks, tiers, the inbox, approvals, expiry, the queue, alerts, the audit log and its anchors | the bridge's `Decision`, built with the production constructor `Decision::load` (audit log and anchors in the episode's directory), its loop running |
| vehicle facts, link health, telemetry plan, TIMESYNC, task-layer timeouts, mission uploads | the bridge's `World`, `Link`, `LinkStats`, `TaskManager` and `housekeeping`, fed MAVLink frames by the real stand-ins |
| tracks | the bridge's sensing model and IMM tracker (`tracks::init`), fed truth on its own simulation route (`POST /api/sim/contacts` through the bridge's router, with the stack's simulation token) |
| the operator's REST answers | the bridge's own axum router (`api::router`, with `emu` and `situation` merged as `main.rs` merges them), called in process (`tower::ServiceExt::oneshot`) |
| our vehicles | `murmur_standins::vehicle::Vehicle::seeded` stepped at its 10 Hz, frames handed to `on_datagram` and taken back from its link |
| loss, latency, jitter, bandwidth and queues, range, terrain line of sight, SATCOM visibility, scripted link cuts | murmur-linkemu's own model per vehicle: `Link::decide`, the proxy's `admit`, `Delay` and `gauge`, its `TidewaterTerrain` cache, `sat::refresh`, its event schedule, `LinkStats` and `summary_json` |
| contacts (truth) | `murmur_contacts::motion::Mover::step` and `truth::TruthMessage::build`, at the contacts process's 1 Hz |

The playbook runs **unchanged** in Python: `agents/playbook/engine.py`'s `Engine` is handed a
session adapter with `AgentSession`'s surface. Nothing in `agents/playbook/` changed.

### 2.1 What changed in existing code (the seams)

Every seam only the simulator needs is behind `#[cfg(murmur_headless)]`, a cfg that only
`crates/murmur-headless/build.rs` sets, so the production bridge binary cannot contain them. Two
checks hold that: a test in the bridge's own suite (`clock.rs`,
`the_bridge_build_has_no_headless_seams`, `const { assert!(!cfg!(murmur_headless)) }`), and
`eval/headless/tests/test_labels_families.py`, which fails if the bridge gains a build script or
a `rustc-cfg`. The rest are a refactor and visibility changes, covered by the bridge's existing
tests, which pass unchanged.

| file | change | kind |
|---|---|---|
| `crates/murmur-bridge/src/clock.rs` | `sim_clock`: once the simulation sets its epoch, `wall()` and `utc()` read the epoch plus tokio's (paused) time since, and `mono()` 3600 s plus that; a read outside the simulation's runtime panics (it would leak real time) | `cfg(murmur_headless)` |
| `crates/murmur-bridge/src/link.rs` | the receive loop's body moved into `Link::receive(world, datagram, from)`, which `run` calls; `Link::in_memory` and `mem_out` (sends go to a function, the emulator, instead of the socket) | refactor + `cfg(murmur_headless)` |
| `crates/murmur-bridge/src/world.rs` | `attach_link`: an in-memory link where `ensure_ports` would put a bound one, so `housekeeping` runs unchanged over it | `cfg(murmur_headless)` |
| `crates/murmur-bridge/src/heartbeat.rs` | one GCS heartbeat round moved into `send_round(world, k, msg)`, which the heartbeat thread calls and the simulator calls once a simulated second | refactor |
| `crates/murmur-bridge/src/emu.rs` | `Emu::ingest` made `pub(crate)`: the simulator hands it the in-memory emulator's `/stats` JSON every 2 s, as the poll hands it the HTTP answer | visibility |
| `crates/murmur-linkemu/src/proxy.rs` | `Pending`, `Delay`, `admit` made `pub`; the 250 ms link gauge update moved into `pub fn gauge`, which the proxy calls | visibility + refactor |
| `Cargo.toml` (workspace) | `unexpected_cfgs` check-cfg for `murmur_headless` | lint config |

Not changed: `agents/playbook/`, the tracker and sensing model, murmur-decide, the stand-ins, the
contacts, the engine's behaviour. The stuck-vehicle fault needed no hook in `murmur-standins`
(the design's fallback): the stand-in's own configuration stops it (section 8). The live bridge
built from the refactored files ran the three calibration runs and the ten gate runs below, each
replayed with its anchors.

## 3. Modules

### 3.1 Rust: `crates/murmur-headless`

| path | what |
|---|---|
| `Cargo.toml`, `build.rs` | the bridge's dependencies (the build script fails, naming the difference, if they drift from `crates/murmur-bridge/Cargo.toml`) plus `murmur-standins`, `murmur-linkemu`, `murmur-contacts`; sets `cfg(murmur_headless)`; the library is `test = false` |
| `src/lib.rs` | the 21 bridge modules by path, `#[cfg(not(test))]`, dead code allowed; `pub mod sim` |
| `src/main.rs` | `murmur-headless --reply-fd N`: the episode spec on stdin's first line, then one JSON request per line on stdin, one answer per line on descriptor N |
| `src/sim/mod.rs` | `Episode`: the runtime (current thread, paused), the bridge assembled in `main.rs`'s order minus sockets, threads and HTTP clients, the timed tasks; `step_to`, `mcp_call`, `api_call`, `truth` (contacts, our vehicles, the tracker's association), `fault`, `mcp_over_http` (the bridge's own `/mcp` service on the same `Mcp`, for the view test), `finish` |
| `src/sim/server.rs` | the request loop: `step`, `mcp`, `api`, `truth`, `fault`, `tools`, `now`, `perf`, `mcp_http`, `end`; a request may carry `at` (step first); every other request is checked not to move the clock |
| `src/sim/spec.rs` | the episode spec (seed, epoch, fleet, stand-in configs, links, events, terrain grid, SATCOM, contacts, start offsets, battery) |
| `src/sim/emulator.rs` | murmur-linkemu per vehicle: both lanes queued by its `admit`, delivered on the simulated clock; terrain and SATCOM rounds; `/stats` for the bridge's `emu`; link events (cuts) |
| `src/sim/fleet.rs` | the stand-ins: `Vehicle::seeded` from the episode seed, a 10 Hz task each with the stand-in engine's phase offsets, a stop fault (cruise and maximum speed to zero) |
| `src/sim/contacts.rs` | the contacts loop of `murmur-contacts` on the simulated clock |
| `src/sim/terrain.rs` | the baked surface-height grid as the emulator's terrain sampler (section 12) |
| `src/sim/space_cache.rs` | the SATCOM look cache (section 12) |
| `src/sim/perf.rs` | computer time per task and per request kind (diagnostics only) |
| `tests/pieces.rs` | the grid answers tidewater's heights; a cached SATCOM look equals the look it would have computed |

### 3.2 Python: `eval/headless/`

| path | what |
|---|---|
| `sim.py` | the `murmur-headless` process client; `HeadlessSession`, the session adapter (`AgentSession`'s surface and error semantics: a tool error raises `RuntimeError("<tool>: <error>")`, a refused proposal comes back with `refused`) |
| `episode.py` | one episode: a discrete-event loop (the situation runner's steps, the operator, the playbook's tick loop, a truth sampler) on simulated time, and the record |
| `script.py` | the situation runner's steps (wait for the fleet, the opening orders as `op-setup` after its dry run or by `agent-setup`, faults, scripted proposers) |
| `operator.py` | the operator on simulated time (section 7) |
| `situations.py` | a situation file as an episode spec, the `murmur-headless` spec and environment, the calibration |
| `families.py` | the family generator (section 8) |
| `labels.py` | truth labels per contact; the target association; what the picture showed at each proposal |
| `metrics.py` | CRITERIA.md's metrics and the reported ones, from an episode record |
| `fullstack.py` | full-stack Harbor watch runs on the assigned port block, and the same record built from their files |
| `calibrate.py` | the three start offsets from the calibration runs |
| `batch.py` | `python -m eval.headless.batch`: episodes on a process pool, a record each, the aggregate, the speed and where the time went |
| `gate.py` | G1 as CRITERIA.md defines it |
| `bake_terrain.py` | the terrain grid, sampled once from `tidewater serve` |
| `calibration.toml`, `split.toml`, `CRITERIA.md` | the offsets; the frozen split; the frozen gates |
| `tests/` | section 6 and 5's tests, the operator against `eval/proxy.py`, the session, labels and families |

## 4. One episode, step by step

Times are the situation's clock (seconds from the stack's start).

1. **Spec** (Python): the situation, the seed, the operator model; the fleet manifest and stand-in
   configs from `sim/swarm.py`'s own functions. `murmur-headless` starts with the stack's
   `MURMUR_*` variables as `scripts/up.sh` and `eval/situation_run.py` set them, so the bridge
   reads its configuration with its own `from_env` functions.
2. **t = 0**: the emulator, and the bridge's parts (World, Decision, Tracks, the router, Mcp).
   Contacts start at `c0`, the stand-ins at `s0` and the bridge is up at `b0` (its loops start
   then; a frame or a contacts message that reaches it earlier is lost, as on the full stack where
   the bridge's decision layer starts about 1 s after the emulator); all three calibrated
   (CRITERIA.md, 2026-10-10). Vehicles connect on their first heartbeat the bridge hears and its
   housekeeping picks their telemetry plan, as live.
3. **Readiness**: every second the runner reads `/api/state`; once every vehicle is ready, the
   opening orders go out as the runner issues them, and the autoplay operator starts.
4. **The AI** comes on at `ai_start_s` plus `a0`, drawn for each episode seed uniformly over the
   calibrated range (the full stack's agent starts and connects in a time that varies run to
   run, and that sets the phase of every later tick): `Engine.start`, then `playbook_agent.py`'s
   tick loop on simulated time (a tick, then `get_changes` every 0.5 s until 2 s after the tick
   began, ticking at once on a waking change; a failed connect again after 2 s doubling to 30 s,
   three missed ticks a new session at once, as the live agent).
5. **Between events** the driver asks for the next event's time (a request's `at`); the runtime
   runs every bridge loop, vehicle, link and contact up to it.
6. **End** at `duration_s`: the tracker's association, the record, the audit log; the process
   saves its new SATCOM looks and exits through the decision layer's own shutdown.

## 5. Clock and determinism

- **One clock.** One current-thread tokio runtime with its clock paused. The bridge's clocks
  (`clock::mono`, `clock::wall`, the engine's `BridgeClock`, every time in the audit log and every
  `t` an agent reads) are tokio's time from fixed origins (section 2.1), the epoch being the
  situation's `at` (Harbor watch 2026-06-14T10:00:00Z). The emulator's clock is the same tokio time;
  the stand-ins count their physics ticks; the contacts their scenario seconds.
- **Stepping.** `step_to(t)` sleeps the runtime to `t` (every timer up to `t` fires in order), then
  yields 64 times so every task woken at `t` runs before the next request. A request between steps
  runs at the current instant, and the server fails the episode if the clock moved during it.
- **Draws.** The stand-ins `Vehicle::seeded(.., k)`; the emulator's seed the situation's own
  `seed` (`situations.emulator_seed`), which the full-stack runner gives the full stack's emulator
  for the same situation (`MURMUR_LINKEMU_SEED`), so both sides meet the same link draws (until
  fix round 1 the full stack always ran the links file's seed 1 and the simulator the episode
  seed: section 11.2); the operator `random.Random(seed * 7919 + 17)` (eval/proxy.py's, the
  situation's `seed`); the playbook's first tick `random.Random("ai-first-tick:<k>")`; the
  contacts' motion and the sensing model the contacts scenario's own `seed`. **For G1, Harbor watch keeps its contacts scenario's seed 11 in
  every episode, as every full-stack Harbor watch run does; the families draw a contacts seed per
  situation, so the traffic's motion varies with the episode seed** (tested:
  `test_a_family_seed_is_one_situation_and_seeds_vary_it`).

### 5.1 The determinism audit

Every place where the by-path bridge code, or a library the simulator links, could escape the
paused clock or the seed, and how each is handled. Found by reading the sources
(`thread::spawn`, `spawn_blocking`, `std::time::Instant` and `SystemTime` outside `clock.rs`,
blocking sleeps, rayon, unseeded random draws, `HashMap`/`HashSet` iteration), then checked
empirically by the tests below. No bridge-side nondeterminism was found that reaches an output,
so nothing went to bd and nothing is sorted in the simulator.

| site | what it is | handled |
|---|---|---|
| `space.rs` `router`: `std::thread::spawn` loader, then a 30 s `thread::sleep` poll | loads the space segment (element sets) on a real thread and hands it to the decision layer | the episode's `MURMUR_ROOT` is an empty directory: the load fails at once ("no element sets", one stderr line), the segment never reaches the engine (no `space.loaded`, no collect or cover; no family uses them) and the poll finds nothing, ever. The live Harbor watch run has the segment; the playbook never uses it |
| `heartbeat.rs`: the GCS heartbeat thread, its `Instant` and `thread::sleep` | 1 Hz heartbeats on a real thread | not started; `send_round` on a simulated 1 s interval |
| `decision/terrain.rs`, `estimate.rs`, `heartbeat.rs` thread spawns and sleeps further down | test code | inside `#[cfg(test)]` modules, not compiled |
| `spawn_blocking`: `platforms::catalog` (`get_platforms`, `/api/platforms`), `Decision::prepare_space` (space tasks), `keepout::explain` (engagement options), auth (password checks) | work on tokio's blocking pool, a real thread | tokio's paused clock does not auto-advance while a blocking task is in flight, so its result lands at the simulated instant it was asked; `at_once` checks a request did not move the clock. Only `get_platforms` runs in the families; the others never (no space, no engagements, dev auth) |
| `tasks/exec.rs`, `decision/preload.rs`, `mcp/mod.rs`, `mcp/changes.rs` (`taken`) `Instant` | timeouts, patience, session use, digest age | these are `tokio::time::Instant`: the paused clock |
| `mcp/changes.rs` `Feed::default`: `SystemTime::now`, pid, counter | the change feed's nonce, so a cursor from an earlier bridge is told to reset | real by design and only ever compared for equality. Two runs of one seed had different nonces and came out identical in everything else, which shows nothing decides on its value; the golden test enforces it: each run has exactly one feed id, the two differ, and with one put in place of the other everywhere the whole record and audit log are byte-identical (`only_the_feed_id_differs`) |
| `space.rs` `Instant` (load timing, answer cache age) | space answers | the space segment is off (above) |
| `auth.rs` `Instant`, `SystemTime` | login rate limits, credential file times | a dev run: no logins |
| `world.rs` `SystemTime` | the fleet manifest's modification time, printed with its age | stderr only |
| `emu.rs` summary from the emulator's `/stats` | the emulator's terrain request and SATCOM round milliseconds (computer time) | display only (`/api/links` `summary`, as live); no tool reads it; the two-process test leaves it out |
| murmur-linkemu `terrain.rs`, `sat.rs` `Instant` | request and round timing stats | the same stats, display only. linkemu's own SATCOM loop (`sat::start`, `spawn_blocking` on the wall clock) is not used: the emulator calls `sat::refresh` itself on simulated time; its `Timeline` is built from the epoch, not `SystemTime` |
| murmur-linkemu `terrain.rs` `HashMap`/`HashSet` | the line-of-sight cache and the queued set | lookups and membership only; the work queue is a `VecDeque` in insertion order |
| murmur-space rayon (`par_iter` in `link_at`, windows, passes) | the SATCOM look's propagation | every parallel step is a map, filter or flat map collected into a `Vec`, which rayon keeps in input order; no parallel float reduction. The look cache answers the same with or without a cached look (`tests/pieces.rs`) |
| murmur-standins `messages::unix_usec`, `vehicle::wall_nanos` (`SystemTime`) | SYSTEM_TIME and GPS time fields; `Vehicle::new`'s seed | the bridge reads `time_boot_ms` (the stand-in's tick count), never the Unix fields; the simulator calls `Vehicle::seeded` with the episode seed |
| murmur-standins `Vehicle::started` (`Instant`) | TIMESYNC's `tc1` in the stand-in's reply | the bridge checks only `tc1 != 0` and times the round trip on its own (simulated) clock |
| murmur-standins `gnss.rs`, `conditions.rs` (threads, sleeps, `Instant`) | GNSS jamming and tidewater conditions pollers | not started (no tidewater in the simulator, section 12) |
| murmur-decide `anchor.rs` hook thread, `time.rs` `SystemClock` | an anchor hook command; the engine's default clock | no hook is configured; the bridge gives the engine `BridgeClock` |
| `HashMap`/`HashSet` in the bridge (`estimate.rs`, `decision/{mod,queue,engage,terrain}.rs`, `tasks/mod.rs`, `emu.rs`, `world.rs`, `link.rs`, `tracks/{mod,sensing}.rs`, `mcp/world.rs`) | per-uid and per-op state | every use is a lookup, an insert, a `retain`, an order-free `any`/`find` on a unique key, or a collection sorted before use (`TaskManager::current_ops` sorts by uid; the engagement lanes' platforms are sorted). `push.rs` and `cot.rs` (also maps) are not started |
| unseeded random draws | none found in the by-path modules or the linked libraries' used paths | |

**What proves it, empirically** (all in `make check`):

- the golden episode (Harbor watch, seed 1, `eval/headless/tests/golden-harbor-watch-s1.json`) is
  run twice: the whole record and audit log the same but for the feed id (above), and its digest
  equal to the committed one; seed 2 differs. The digest leaves out `perf` (computer time), the
  feed id, and the build commit in the audit log's `run.started` with what carries it on (the
  hash chain's `prev` in every later line, the record's own hash of the log): the golden holds
  the episode, not the build, and the chain is checked by the replay in the same test;
- the golden's audit log replays with its anchors: `murmur-replay --anchors`, "every decision
  reproduced exactly";
- the same feed-id check on two family episodes run twice each (`link-cut-mid-look`, a cut timed
  from a dispatch; `approval-flood`, a scripted proposer), the paths Harbor watch does not take;
- `test_two_processes_serve_the_same_views`: two processes on one seed answer every read tool and
  `/api/links` byte for byte at three instants (std's map hasher is seeded per process, so a map
  iterated into a view would differ here).

## 6. The JSON views are the bridge's: how the tests prove it

The simulator has no views of its own: every answer is the bridge's `mcp::tools::call` on the
bridge's own state. What could differ is the path, so `eval/headless/tests/test_episode.py`
compares paths on one state (the design put this in a Rust test; it is in Python, driving the
binary, because the session adapter is part of the path):

1. `test_the_views_are_the_bridge_mcp_endpoints`: an episode run to 200 s, then every tool the
   playbook calls with the arguments it uses (`get_fleet` compact, with `uids` and plain,
   `get_tracks`, `get_inbox`, `get_intents` mine with `limit` and plain, `get_policy`,
   `get_platforms`, `get_areas`, `get_nominations`, `explain_intent` of a real and a missing id,
   `get_vehicle`, `get_recent_events`, `preview_intent` twice, an unknown tool) and `get_changes`
   with a cursor, answered from the same `Mcp` directly and through the bridge's own `/mcp`
   service (rmcp's streamable HTTP server, in process, JSON-RPC `initialize` then `tools/call`
   with the agent's header): byte-identical.
2. `test_the_writers_answer_the_same_on_both_paths`: `propose_intent`, `withdraw_intent` with
   `if_state` matching and not, a proposal for a vehicle that does not exist and one missing its
   arguments, on two identical episodes, one per path: identical answers.
3. `test_the_reads_have_the_live_reads_schema`: every key path and JSON type of the recorded live
   reads (`agents/tests/fixtures/playbook/harbor-watch-reads.json`) is in what the playbook reads
   over a whole headless Harbor watch episode. Keys the headless reads have and the fixture lacks
   are printed, not failed: the fixture predates fields the bridge has since gained.

Neither says the simulated state evolves like the full stack's. That is G1.

## 7. The operator

**A model for load and regression testing, not a model of human judgment.** Its default is what
the full stack's autoplay operator is: `op-auto` in `make situation ... OPERATOR=autoplay`, which
is `eval/proxy.py`'s `ProxyOperator` with the situation's `[autoplay]` preset (Harbor watch:
`default`) and the situation's `seed`, re-expressed on simulated time with the same parameters,
rule book, card view, notes and draws in the same order from the same generator.
`tests/test_operator.py` runs eval/proxy.py's own class against a fake bridge with its sleeps
recorded and checks both make the same decisions after the same delays, across presets and
seeds.

Parameters, as recorded in each episode record (the `default` preset):

| parameter | default | meaning |
|---|---|---|
| `mode` | `proxy` | `proxy`; `approve-all`, `reject-all` (a look every 1 s); `never` (every human-tier card expires); `rates` |
| `attend_s`, `attend_sigma` | 20 s, 0.4 | the look interval (lognormal) |
| `read_s`, `read_per_check_s`, `read_sigma`, `skim_s`, `hurry` | 6 s, 1.5 s, 0.3, 2 s, 0.5 | reading and skimming times, shortened under load |
| `diligence`, `fatigue_queue`, `trust_agent` | 0.8, 6, 1.0 | whether it reads a card: `diligence x 2^(-(queue - 1) / fatigue_queue) x trust` |
| `c_min` | 0.5 | the rule book's affiliation threshold |
| `rates` mode: `approve`, `reject`, `latency_median_s`, `latency_sigma`, `reads` | none | a fixed operator |

**What it rewards, and why that matters.** It approves every card it does not read, and it reads
less as the queue grows. Anything optimized through it, directly or through what an approved card
brings, learns that a longer queue gets more approved unread: **a policy of flooding the inbox**,
the behaviour Murmur exists to detect. Step 2 and any tuning must take their reward from truth
only, charge each human-tier card a fixed cost, train with `approve-all`, and never tune the
operator-facing parameters (budget, cooldowns, materiality) against it.

## 8. Situations: Harbor watch, the families, the split

**Harbor watch** runs from its own file, unchanged, with the playbook config from its
`[playbook]` table: the G1 reference. Its operator and its link emulator draw from the file's
`seed` (1), as `make situation NAME=harbor-watch` does every time; a batch's episode seed varies
the stand-ins (and the playbook's first tick). G1's copies set the file's `seed` to the episode
seed, so there all of them vary, on both sides alike.

**The families** (`families.py`; `generate(family, seed, workdir)` draws from
`random.Random(f"{family}:{seed}")`) keep Harbor watch's base: its four vehicles and three opening
orders, its policy and `[playbook]` table, 720 s, the AI at 45 s, its ordinary traffic under a
contacts seed drawn per situation. The family adds its element, drawn as the design's table
said (unchanged since): `fast-unknown`, `decoy`, `link-cut-mid-look`, `low-battery` (tuning);
`two-at-once`, `stuck-vehicle`, `approval-flood` (held out). As built:

- the stuck fault sets the stand-in's cruise and maximum speed to zero at the drawn time (it stops
  making way; nothing goes through the bridge); no hook in `murmur-standins` was needed;
- the link cut is the emulator's own `cut` event, scheduled from the dispatch of the first look of
  ours (the record's `script` keeps the trigger). Version 2 (2026-10-10): no look dispatched by
  600 s, no cut, and the script records "no look to cut". Version 1 cut PUMA 1 at a drawn 200 to
  400 s whenever no look had gone out by then, which 19 of its 30 episodes did, so most of them
  cut no look; its results are reported apart from version 2's;
- the low battery is the stand-in's starting charge.

The split is by family and committed before any result (`split.toml`). Family reports use seeds
1 to 30 per family and never pool tuning and held-out families.

## 9. Truth labels and metrics

**Truth** on both sides is the contacts process's own log, one sample per step (its scenario
second placed on the situation's clock by the process's start): the full stack's
`contacts-truth.jsonl`, and the simulator's contacts writing the same lines (`truth` with
`log_from`), so neither side samples it from outside.

**Labels per contact** (`labels.py`): what it is (affiliation, kind, AIS, from the scenario) and
whether it needs a look and by when: unknown, suspect or hostile within 3 km of the bridge
(-122.4783, 37.8199) by truth needs a look by its first crossing (the first truth sample within
3 km) plus 60 s; no other contact needs one. Fault windows are in the record's `script`.

**The target of a proposal** is matched by truth identity, the same rule on both sides of G1: the
truth contact nearest the track's `predicted` position in the proposal's evidence record at its
`tick_t` (the truth sample at the scenario second nearest it, not interpolated), within 500 m
and with no other contact within 1.5 times that distance; else `ambiguous`. The record also holds the tracker's own association and their agreement, and **what
the picture showed at each proposal** (the track's affiliation and identity with their
confidences, `picture`), so a false look can be split by what the C2 showed. Recording it changed
nothing in G1's inputs.

**Metrics** per episode (`metrics.py`): CRITERIA.md's four as it writes them (`look_covered`
and `time_to_look` count `isr.investigate` only; `false_looks` any `isr.*` play on a contact that
needs none; `precision` over the human-tier proposals it can judge: a look or recentre by its
target's label, a P0 command by whether its fault was real by truth, where the record has the
truth of our vehicles, which a full-stack record does not; other plays are listed as unjudged),
and reported beside them: proposals and their outcomes, human-tier
cards (total and per operator-minute), approval latency, timeliness, objections, unfaithful
rationales, re-proposals after a rejection, the association agreement. The fault-handling
metrics of the design (a hold by the stuck alert plus 30 s, an rtl by the battery crossing, voided
cards inside a cut) are not computed yet: the record has what they need (the fault in `script`,
the proposals, the audit log), and step 2 should add them as functions of the record.

## 10. The episode record and the batch

One JSON record per episode: the situation (name, family, split, seed), the provenance (commit,
dirty flag, the binary's hash, the links file's path and hash, the space cache's content hash),
the playbook config and its label, the operator's parameters, the clock (epoch, duration, the
calibration, the drawn first tick, the emulator's seed, when the fleet was ready), the playbook's own log lines on the
simulated clock, the plans, every proposal (time, play, version, target track, truth target,
tracker target, the picture, vehicle, task, tier, state at submission, refusal), every operator
decision, our intents, the truth (the contacts' per-step log, our vehicles every 2 s, the
tracker's association, what a family states), the metrics, the audit log's event count, head and
hash (the log itself on request), and `perf` (computer time, never compared: the slow ticks the
live agent logs as `tick.slow` are counted there, not written into the record). A full-stack run's
record carries its `provenance.json`: the commit, the binaries' hashes, the emulator's seed, the
same untracked inputs and the host's load average at its start.

`python -m eval.headless.batch` (or `make headless`) runs episodes on a process pool, one
`murmur-headless` process each, writes the records and an aggregate per situation or family:
each metric's mean with a Student t 95 % interval and, for proportions, the pooled Wilson
interval (eval/stats.py); the speed (episodes per core-hour = episodes / (wall hours x workers),
the single-worker rate, the 95th percentile of an episode's wall time); and where an episode's
time went (`perf`: process start, the simulator's own time per task and per request kind, the
pipe, the driver and the playbook).

## 11. Results

### 11.1 Calibration (in-sample, not a gate)

The three start offsets were set from three full-stack runs with `seed` = 101, 102, 103 (CRITERIA.md
"Calibration"): `contacts_start_s` 0.823, `standins_start_s` 1.001, `ai_first_tick_s` 1.031. With
them the simulator agreed with those three runs on 9 of 9 proposals. **That is in-sample**: the
offsets were fitted on those runs, so they say only that the fit took; they are not part of G1,
whose ten seeds were never used for anything before the gate.

### 11.2 G1: fidelity on Harbor watch

Run 2026-10-09 as CRITERIA.md froze it: ten full-stack runs (`make situation` with Harbor watch
copies `seed` = 1 to 10, one at a time on the assigned port block, all ten good at the first
attempt, every audit log replayed with its anchors: "every decision reproduced exactly"), then
`python -m eval.headless.gate g1` (headless episodes of the same copies, seeds 1 to 10). The
binaries: the full stack's seed 1 built at 28e2fa7, seeds 2 to 10 rebuilt by `up.sh` at 23a950a
(a commit of the test's normalisation; no Rust changed between them), the simulator built at
28e2fa7. Records: `logs/headless-g1/gate/` (full stack), `logs/headless-g1/gate-headless/`
(headless, with audit logs), `logs/headless-g1/g1.json` (not committed).

**G1 passes, on the line**: agreement **0.800** (24 pairs; 29 headless and 25 full-stack
proposals; 24 / (29 + 25 - 24)), against the 0.80 required, and every metric inside the other
side's interval both ways:

| metric | headless | full stack | pass |
|---|---|---|---|
| `look_covered` | 10/10 [0.72, 1.00] | 10/10 [0.72, 1.00] | yes |
| `time_to_look` | -114.65 s (floor interval +-2 s) | -114.50 s [-115.00, -114.00] | yes |
| `false_looks` | 0.0 | 0.0 | yes |
| `precision` | 29/29 [0.88, 1.00] | 25/25 [0.87, 1.00] | yes |

Proposals side by side (time on the situation's clock; every target is `fastboat-west` by truth,
every vehicle PUMA 1):

| seed | headless | full stack | pairs |
|---|---|---|---|
| 1 | investigate 178.0, recentre 211.0, recentre 272.0 | investigate 178.7, recentre 210.3, recentre 271.4 | 3 |
| 2 | investigate 178.0, recentre 210.0 | investigate 178.8, recentre 217.4, recentre 279.0 | 2 |
| 3 | investigate 178.0, recentre 210.0, recentre 270.0 | investigate 177.1, recentre 210.2 | 2 |
| 4 | investigate 178.0, recentre 210.0, recentre 271.0 | investigate 178.1, recentre 210.2, recentre 271.8 | 3 |
| 5 | investigate 178.0, recentre 211.0, recentre 271.0 | investigate 177.1, recentre 209.1, recentre 270.3 | 3 |
| 6 | investigate 178.0, recentre 210.0, recentre 271.0 | investigate 178.1, recentre 210.2 | 2 |
| 7 | investigate 178.0, recentre 211.0, recentre 271.0 | investigate 177.1, recentre 209.2 | 2 |
| 8 | investigate 178.0, recentre 211.0, recentre 272.0 | investigate 178.6, recentre 210.7 | 2 |
| 9 | investigate 178.0, recentre 210.0, recentre 270.0 | investigate 178.1, recentre 210.2, recentre 271.4 | 3 |
| 10 | investigate 178.0, recentre 211.0, recentre 271.0 | investigate 178.6, recentre 210.2 | 2 |

**Where the two sides diverge, and why.** The look and the first recentre agree in every seed,
within 1.3 s but for seed 2's recentre (7.4 s). **Every divergence is one proposal: the second
recentre at about 271 s**, made in 9 of 10 headless episodes and 5 of 10 full-stack runs. In every
seed, on both sides, that proposal is made exactly when PUMA 1 has not yet gone home on its
lost-link failsafe by then: it flies the boat's orbit through a terrain shadow west of the bridge
(the emulator's line of sight from the ground station clears by 11 m until about 225 s, then is
obstructed by 3, 7 and 11 m, losing 17 to 56 % of packets on the `ramp` model), and the Puma's
GCS failsafe sends it home after 5 s without a GCS heartbeat. That early return came in 5 of 10
full-stack runs (at 243.5, 256.4, 256.4, 259.4 and 271.4 s; none in the three calibration runs)
and in 1 of 10 headless episodes (257 s, seed 2; none in the calibration seeds). Checked to be the
same on both sides: the obstruction along the Puma's path (the baked grid against tidewater's own
samples on the same profile: within 0.15 m; the live emulator's view polled during the seed 9 run
against the headless one: the same cells and depths, the path within tens of metres), the
downlink's packet counts (seed 10: 464 and 461 packets by 200 s, the same 2 a second after), and
the loss model (the same code; terrain loss is an independent draw per packet). Not the same,
by design: the random draws (the full stack's emulator uses `sim/links.toml`'s seed 1 in every
run, the simulator the episode seed). At these counts the difference (5 of 13 against 1 of 13
with the calibration runs) is not significant (Fisher's exact test, two-sided p about 0.16), but
it leans one way, and it is what decides the gate: if the full stack's early returns are not
chance, the simulator under-produces lost-link failsafes on a degraded link, and the second
recentre is over-produced. Measuring it needs many runs of one degraded-link case on both sides
(cheap headless; about 12 minutes a full-stack run); not done here, since nothing may be tuned on
the gate's seeds.

Reported beside the gate (not gated):

| | headless | full stack |
|---|---|---|
| human-tier cards per episode | 2.9 | 2.5 |
| approvals; voided at approval | 22; 7 | 22; 3 |
| approval latency, median (mean) | 20.5 s (21.4) | 24.8 s (24.3) |

The extra voided cards are the second recentres, refused at approval because PUMA 1's navigation
status was too old by then (its link in the shadow). Each full-stack run's own start offsets
(`python -m eval.headless.calibrate`'s measure; the simulator uses 0.823, 1.001 and 1.031): the
contacts 0.809 to 0.818 s, the stand-ins 0.915 to 0.967 s, the playbook's first tick 1.28 to
1.40 s (seed 2: 0.52 s). Fleet readiness: the full stack 2.8 to 4.0 s; the simulator 3 s in eight
seeds, 4 s and 8 s in seeds 9 and 10, and 31 s in seed 2 (the emulator's draws lost 4 of the
first 16 bridge-to-vehicle packets to PUMA 1, and its GPS fix first showed at 31 s, which looks
like a lost request repeated later; not traced further). Over 200 headless Harbor watch episodes
the fleet was ready at 3 s in 90 %, at up to 31 s in the rest; the full stack's runs draw their
own such delays (the calibration's seed 101 was 22 s late for another reason: CRITERIA.md's
calibration section).

### 11.3 G1b: speed

Harbor watch, the default operator, seeds 1 to 200, measured 2026-10-09 on this machine (Intel
i7-14700K: 8 performance cores with two threads each and 12 efficiency cores, 28 threads; the
user's 22-vehicle stack running beside it, about 2 threads busy):

| workers | episodes | wall | episodes per core-hour | episode wall mean | 95th percentile |
|---|---|---|---|---|---|
| 1 | 40 | 91 s | 1,585 | 2.27 s | 2.40 s |
| 8 | 200 | 73 s | **1,235** | 2.89 s | 3.31 s |
| 16 | 200 | 45 s | 998 | 3.48 s | 3.74 s |

**G1b passes at 8 workers (1,235) and misses by a hair at 16 (998).** An episode costs 2.27 s
alone and slows as workers are added (beyond eight they share performance cores' threads or land
on efficiency cores), so the rate per worker falls. CRITERIA.md lets W be stated; both are
reported so the pass is not read as more than it is. The design's estimate (1 to 1.5 s an episode,
2,400 to 3,600 per core-hour) was optimistic by about half.

Where an episode's time goes (Harbor watch, one worker, mean seconds per episode of 720 s
simulated; the simulator's tasks run inside its `call.*` requests, so they are parts of
`server`, not added to it):

| part | s | per unit |
|---|---|---|
| process start (spawn, assembly) | 0.10 | |
| simulator serving requests (`server`) | 1.71 | |
|   bridge decision loop | 0.35 | 0.48 ms a 1 s tick (721 ticks) |
|   contacts (motion, truth message, the bridge's sensing on its route) | 0.21 | 0.29 ms a second |
|   bridge tracker loop | 0.13 | 0.18 ms a tick |
|   stand-ins (4 vehicles, 10 Hz) | 0.07 | 2 us a step |
|   emulator, the bridge's link and emu, housekeeping | 0.12 | |
|   read tools (`get_tracks` 0.19, `get_changes` 0.18, `get_intents` 0.08, `get_fleet` 0.08, `get_inbox` 0.02) | 0.55 | `get_tracks` 0.55 ms a call |
|   REST (`/api/state`, the operator), truth reads | 0.08 | |
| pipe and JSON, both ends (`ipc`) | 0.24 | 0.07 ms a round trip, 3,500 a run |
| driver (Python) | 0.18 | |
|   the playbook's own decisions | 0.07 | 0.20 ms a tick |

At 8 workers the same parts take about 25 % longer each (contention, not a different profile).

On the families (8 workers, 30 seeds each; not G1b, which is Harbor watch, but the cost the
research's step 3 will pay): the tuning families 1,290 per core-hour (2.7 to 2.8 s an episode);
the held-out families 1,008 (`two-at-once` 2.95 s, `stuck-vehicle` 3.37 s, `approval-flood`
3.97 s: its flood of orders moves HUNTER 1 and REAPER 1 to new places, whose SATCOM looks are not
yet in the look cache (0.40 s computing them) and the decision loop about 1.6 times the work
(0.72 s)).

If more speed is wanted, in the design's order: fewer JSON round trips (a tick's reads in one
request, which would save part of the pipe's 0.24 s, not the read tools' own 0.55 s); the session
in process (PyO3). The bridge's decision tick is the largest single part; it is the bridge's own
code and stays as it is.

### 11.4 The families (model results, seeds 1 to 30 each)

The playbook as committed, the default operator, 2026-10-09
(`logs/headless-families/{tuning,held-out}/`, not committed). Coverage is per episode (every
must-look contact covered), with its Wilson interval; time to look is from the contact's first
crossing within 3 km of the bridge to the first look proposed on it, so a negative time is a look
before the crossing; false looks are `isr.*` proposals on contacts that need none, per episode;
precision is pooled over the playbook's human-tier proposals; cards are the playbook's
human-tier cards per episode. **Tuning and held-out are never pooled.**

| family | split | look covered | time to look, mean s | false looks | precision | cards |
|---|---|---|---|---|---|---|
| `fast-unknown` | tuning | 30/30 [0.89, 1.00] | -57.6 (30) | 0.0 | 94/94 | 3.13 |
| `decoy` | tuning | 15/15 [0.80, 1.00] | -93.9 (15) | 0.7 | 52/67 | 2.23 |
| `link-cut-mid-look` | tuning | 20/30 [0.49, 0.81] | -100.2 (20) | 0.0 | 41/41 | 1.37 |
| `low-battery` | tuning | 20/30 [0.49, 0.81] | -65.4 (20) | 0.0 | 60/60 | 2.00 |
| `two-at-once` | held-out | 4/30 [0.05, 0.30] | -99.5 (34 of 60 boats) | 0.0 | 95/95 | 3.17 |
| `stuck-vehicle` | held-out | 30/30 [0.89, 1.00] | -90.7 (30) | 0.0 | 98/98 | 3.27 |
| `approval-flood` | held-out | 30/30 [0.89, 1.00] | -89.0 (30) | 0.0 | 101/101 | 3.40 |

What the misses are (from the records and the playbook's own status lines):

- `low-battery`: all 10 misses are the episodes whose low vehicle is PUMA 1, the one free sensor.
  The playbook sends it home (`safety.low_reserve`, in every episode) and, as the family's label
  asks, does not choose it for a look; it then proposes no look with any other vehicle.
- `link-cut-mid-look`: all 10 misses are fallback cuts (no look had gone out by the cut, the boat
  arriving late): PUMA 1 went home on its own lost-link failsafe, and when the boat came the
  playbook's status was "no free sensor aircraft (... PUMA 1: recovering on its own (RTL) ...)".
  Every episode whose cut fell on a look under way (11) was covered, and 9 of the 19 fallback cuts.
- `two-at-once`: one free sensor for two boats: 26 episodes covered one boat, 4 both. Every look
  was flown by PUMA 1; the second boat waited on "recentred within the last 60 s" or "no free
  sensor".
- `decoy`: 13 looks on the decoy, each while the picture showed it `unknown`; 6 releases of it once
  shown `neutral`; 2 proposals on an inbound cargo ship shown `suspect`.

These are findings about the playbook on a model, for the research's step 2 and 3, not changes
made here; nothing was tuned on them.

## 12. What the simulator leaves out, as found

| difference | where it shows | effect measured or expected |
|---|---|---|
| process start offsets are medians from the calibration runs, not each run's own | when the first look comes | the gate runs' own offsets were within 0.1 s of the calibrated ones for contacts and stand-ins, and the playbook's first tick about 0.27 s later than calibrated (section 11.2); the look agreed within 1.3 s in every seed |
| the emulator's random draws: the episode seed in the simulator, `sim/links.toml`'s seed 1 in every full-stack run | packet loss on a degraded link, and so whether a lost-link failsafe fires | the one divergence G1 found (section 11.2): lost-link returns through PUMA 1's terrain shadow in 5 of 10 full-stack runs and 1 of 10 headless episodes; not significant at these counts, not yet explained |
| no tidewater currents or winds (the stand-ins run without conditions; drifting contacts do not drift) | HUNTER 1's patrol track; the Mile Rock boat | none seen on the playbook's proposals |
| the emulator's terrain line of sight from a baked surface-height grid (`sim/headless/terrain/golden-gate.grid.gz`, 20 m spacing, tidewater's `max(elevation, water level)` at the scenario's start; provenance beside it, its hash tested) instead of tidewater's samples; a cell's first answer at once instead of up to a second later | the Pumas' terrain shadows behind the Presidio | the grid is finer than the emulator's 250 m cells |
| SATCOM looks at the terminal's position rounded to 0.001 deg (about 100 m) and 100 m of altitude, cached between episodes; the 30-minute outlook (display only) off | Reaper's SATCOM link | under a tenth of a degree of elevation to a satellite 550 km up; the outlook is not read by any tool |
| the bridge's space segment off (section 5.1) | collect and cover tasks; the audit log has no `space.loaded` | none on the playbook (no space plays) |
| the domain rule "not evaluated" for surface vehicles (no tidewater for the decision layer's terrain) | the checks shown on HUNTER 1's orders | none on the playbook's air tasks |
| no concurrency: a tick's reads at one instant, zero time per call | timing within a tick | under the 10 s matching window |
| no console push, CoT export, situation panel | display and export | none |

## 13. Tests and checks

`make check` runs, beside everything it ran before: `cargo clippy` and `cargo test` for the new
crate (`tests/pieces.rs`) and the bridge's unchanged suite with its no-seams test; py_compile and
`pytest eval/headless/tests` (the operator against `eval/proxy.py`, the session adapter, labels,
the association rule, the split, the families' determinism and variation, boats on water, the
production build's cfg, the grid's provenance, G1's pairing and intervals, the golden with its
feed-id check and replay, the two view-path tests, two processes, the live reads' schema).

## 14. Step 2: how it plugs in

- **The truth objective** is a function of the episode record; nothing in the simulator changes.
  Add the fault-handling metrics (section 9) the same way.
- **Agents** are interchangeable in the driver (`Options.agent`): `playbook` (default) and `none`
  today; an `oracle` (handed the truth labels explicitly, recorded as such) and `random-valid`
  (uniform over actions whose preview passes) each get only a session.
- **Operators**: train and tune with `approve-all`; evaluate with `proxy` and its presets.
- **Families and seeds**: held-out families report separately and are never tuned on; tuning
  work uses seeds 1000 and up on tuning families only (CRITERIA.md).
- **The fidelity gate repeats** for each new family once a full-stack situation of its kind exists.

## 15. How to run

    make build && cargo build --release -p murmur-headless
    .venv/bin/python -m eval.headless.batch --situation harbor-watch --seeds 1-200 --workers 8 --speed
    .venv/bin/python -m eval.headless.batch --families fast-unknown decoy link-cut-mid-look low-battery --seeds 1-30 --workers 8
    .venv/bin/python -m eval.headless.batch --families two-at-once stuck-vehicle approval-flood --seeds 1-30 --workers 8
    make headless FAMILIES="fast-unknown decoy" SEEDS=1-30 WORKERS=8

    # G1: the full-stack runs (about 13 minutes each, one at a time, on the assigned port block),
    # then the gate against fresh headless episodes of the same seeds
    .venv/bin/python -m eval.headless.fullstack run --seeds 1 2 3 4 5 6 7 8 9 10 --out logs/headless-g1/gate
    .venv/bin/python -m eval.headless.gate g1 --full logs/headless-g1/gate --seeds 1-10 --out logs/headless-g1/g1.json

Records go to `eval/results/headless/<batch>/` unless `--out` says otherwise; the SATCOM look
cache to `logs/headless/space-cache/` (one file per set of element sets and link profiles).
