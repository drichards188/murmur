# The headless simulator (murmur-8ak)

Status: **built** (phase 2, 2026-10-09; review fix round 1, 2026-10-10); gates run, results in
section 11 (G1 re-run on seeds 11 to 20 after the fix round, the first run beside it). The design was
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
| `crates/murmur-bridge/src/space.rs` | `sim_loader`: the space segment loads at once inside the simulation's runtime and the 30 s look at the element sets runs on the simulated clock (no loader thread); the answer cache ages on tokio's paused clock (section 16.2) | `cfg(murmur_headless)` |
| `Cargo.toml` (workspace) | `unexpected_cfgs` check-cfg for `murmur_headless` | lint config |
| `crates/murmur-bridge/src/decision/terrain.rs`, `decision/mod.rs` (world v3, 2026-10-23) | the baked land/water grid as the domain rule's source when no tidewater URL is set (`MURMUR_LAND_WATER_GRID`, `Settings::land_water`, `Terrain::with_grid`, `Grid`); with a URL nothing changes (section 17) | feature, live and headless |
| `crates/murmur-standins/src/conditions.rs` (world v3) | `Slot::position` and `Slot::put`: a source of conditions other than the HTTP poller | visibility |

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
| `src/sim/server.rs` | the request loop: `step`, `mcp`, `api`, `truth`, `fault`, `tools` (with `defs`, the tools' definitions as the bridge's `tools/list` gives them, for the model agent), `now`, `perf`, `mcp_http`, `end`; a request may carry `at` (step first); every other request is checked not to move the clock |
| `src/sim/spec.rs` | the episode spec (seed, epoch, fleet, stand-in configs, links, events, terrain grid, SATCOM, contacts, start offsets, battery; optional `space_root` and `effects`, section 16; world v3's water, section 17) |
| `src/sim/emulator.rs` | murmur-linkemu per vehicle: both lanes queued by its `admit`, delivered on the simulated clock; terrain and SATCOM rounds (the catalog and element sets under `space_root` when given); `/stats` for the bridge's `emu`; link events (cuts) |
| `src/sim/fleet.rs` | the stand-ins: `Vehicle::seeded` from the episode seed, a 10 Hz task each with the stand-in engine's phase offsets, a stop fault (cruise and maximum speed to zero), a vehicle leaving the world (`remove`: a munition spent by its own effect); world v3's battery draw, reboot and GNSS environment, and the conditions poller (section 17) |
| `src/sim/faults.rs` | world v3's faults (section 17): gnss jam and spoof, battery, reboot, current; the events files' GNSS events for the stand-ins |
| `src/sim/water.rs` | world v3's water (section 17): the world's current field, and the tidewater samples our vehicles that take conditions get |
| `src/sim/contacts.rs` | the contacts loop of `murmur-contacts` on the simulated clock, each contact with the water's current at it (none in still water, section 17); with `effects`, each engagement effect's draw and a spent platform's removal (section 16.1) |
| `src/sim/terrain.rs` | the world's baked surface-height grid (read with the bridge's own `decision::terrain::Grid`) as the emulator's terrain sampler (section 12), and the height of an airborne spawn's home (section 8) |
| `src/sim/space_cache.rs` | the SATCOM look cache (section 12) |
| `src/sim/perf.rs` | computer time per task and per request kind (diagnostics only) |
| `tests/pieces.rs` | the grid answers tidewater's heights; a cached SATCOM look equals the look it would have computed |

### 3.2 Python: `eval/headless/`

| path | what |
|---|---|
| `sim.py` | the `murmur-headless` process client; `HeadlessSession`, the session adapter (`AgentSession`'s surface and error semantics: a tool error raises `RuntimeError("<tool>: <error>")`, a refused proposal comes back with `refused`, a result that is not JSON reads as `{"text": ...}`) |
| `episode.py` | one episode: a discrete-event loop (the situation runner's steps, the operator, the AI, a truth sampler) on simulated time, and the record |
| `officer.py` | the model watch officer (`Options.agent = "model"`, section 14.1): agents/watch_officer.py's trigger table, picture and prompt on simulated time, a Messages API tool-use loop in place of the CLI, the cost guard, the play category of each proposal |
| `script.py` | the families' faults and scripted proposers, and a situation's timeline (its `[[step]]`s, section 17) |
| `operator.py` | the operator on simulated time (section 7) |
| `situations.py` | a situation file as an episode spec (its world, `[headless]` choices and timeline), the `murmur-headless` spec and environment, the calibration |
| `families.py` | the family generator (section 8) |
| `labels.py` | truth labels per contact; the target association; what the picture showed at each proposal |
| `metrics.py` | CRITERIA.md's metrics and the reported ones, from an episode record |
| `fullstack.py` | full-stack Harbor watch runs on the assigned port block, and the same record built from their files |
| `calibrate.py` | the three start offsets from the calibration runs |
| `batch.py` | `python -m eval.headless.batch`: episodes on a process pool, a record each, the aggregate, the speed and where the time went |
| `gate.py` | G1 as CRITERIA.md defines it |
| `provenance.py` | the commit and dirty flag, the binaries' hashes, the untracked inputs (links file, space cache) |
| `bake_world.py` | a world's grids, sampled once from `tidewater serve`: the surface heights, the land/water grid, the current field (section 17) |
| `../world.py` | the worlds (`sim/worlds/<name>.toml`, section 17) |
| `signers.py`, `review.py`, `scripted.py` | section 16: the engagement signers, the reviewer, the scripted agent (engage, collect, cover) |
| `space.py` | section 16.2: a world's pinned element sets (make, verify, unpack) |
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
   three missed ticks a new session at once, as the live agent). The model agent comes on at the
   same instant (section 14.1); `none` never does.
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
- **Draws.** The stand-ins `Vehicle::seeded(.., k)`; the emulator's seed `k`, the episode seed
  (`situations.emulator_seed`). Anything compared with the full stack passes that same seed to the
  full stack's emulator as `MURMUR_LINKEMU_SEED` (the full-stack runner does it for G1, whose
  copies set the situation's `seed` to k and run as episode k), so both sides meet the same link
  draws. Until fix round 1 the full stack always ran the links file's seed 1 and the simulator the
  episode seed (section 11.2); from fix round 1 to fix round 2 the simulator took the situation
  file's `seed`, which for G1's copies is k (so G1's records hold) but put every episode of a
  Harbor watch batch on seed 1 (section 11.3). The operator `random.Random(seed * 7919 + 17)` (eval/proxy.py's, the
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
| `space.rs` `router`: `std::thread::spawn` loader, then a 30 s `thread::sleep` poll | loads the space segment (element sets) on a real thread and hands it to the decision layer | from section 16 the headless build has no loader thread (`sim_loader`, `cfg(murmur_headless)`): the first load runs at once inside the runtime as the router is built (the episode's start), the poll on the simulated clock. Without `Options.space_snapshot` the episode's `MURMUR_ROOT` is an empty directory: the load fails at once ("no element sets", one stderr line), the segment never reaches the engine (no `space.loaded`, no collect or cover) and the poll finds nothing, ever, as before (the golden holds). With it, `MURMUR_ROOT` is the unpacked snapshot, which never changes, so it never reloads |
| `heartbeat.rs`: the GCS heartbeat thread, its `Instant` and `thread::sleep` | 1 Hz heartbeats on a real thread | not started; `send_round` on a simulated 1 s interval |
| `decision/terrain.rs`, `estimate.rs`, `heartbeat.rs` thread spawns and sleeps further down | test code | inside `#[cfg(test)]` modules, not compiled |
| `spawn_blocking`: `platforms::catalog` (`get_platforms`, `/api/platforms`), `Decision::prepare_space` (space tasks), `keepout::explain` (engagement options), auth (password checks) | work on tokio's blocking pool, a real thread | tokio's paused clock does not auto-advance while a blocking task is in flight, so its result lands at the simulated instant it was asked; `at_once` checks a request did not move the clock. Only `get_platforms` runs in the families; space queries and the engagement options run in section 16's episodes (a collect's answer is computed at the instant of its check, so rules 13's `computed_t` equals it); auth never (dev auth) |
| `tasks/exec.rs`, `decision/preload.rs`, `mcp/mod.rs`, `mcp/changes.rs` (`taken`) `Instant` | timeouts, patience, session use, digest age | these are `tokio::time::Instant`: the paused clock |
| `mcp/changes.rs` `Feed::default`: `SystemTime::now`, pid, counter | the change feed's nonce, so a cursor from an earlier bridge is told to reset | real by design and only ever compared for equality. Two runs of one seed had different nonces and came out identical in everything else, which shows nothing decides on its value; the golden test enforces it: each run has exactly one feed id, the two differ, and with one put in place of the other everywhere the whole record and audit log are byte-identical (`only_the_feed_id_differs`) |
| `space.rs` `Instant` (load timing, answer cache age) | space answers | the answer cache ages on tokio's paused clock in the headless build (`CacheInstant`), so whether an answer comes from the cache is a function of simulated time; the load timing is printed only |
| `auth.rs` `Instant`, `SystemTime` | login rate limits, credential file times | a dev run: no logins |
| `world.rs` `SystemTime` | the fleet manifest's modification time, printed with its age | stderr only |
| `emu.rs` summary from the emulator's `/stats` | the emulator's terrain request and SATCOM round milliseconds (computer time) | display only (`/api/links` `summary`, as live); no tool reads it; the two-process test leaves it out |
| murmur-linkemu `terrain.rs`, `sat.rs` `Instant` | request and round timing stats | the same stats, display only. linkemu's own SATCOM loop (`sat::start`, `spawn_blocking` on the wall clock) is not used: the emulator calls `sat::refresh` itself on simulated time; its `Timeline` is built from the epoch, not `SystemTime` |
| murmur-linkemu `terrain.rs` `HashMap`/`HashSet` | the line-of-sight cache and the queued set | lookups and membership only; the work queue is a `VecDeque` in insertion order |
| murmur-space rayon (`par_iter` in `link_at`, windows, passes) | the SATCOM look's propagation | every parallel step is a map, filter or flat map collected into a `Vec`, which rayon keeps in input order; no parallel float reduction. The look cache answers the same with or without a cached look (`tests/pieces.rs`) |
| murmur-standins `messages::unix_usec`, `vehicle::wall_nanos` (`SystemTime`) | SYSTEM_TIME and GPS time fields; `Vehicle::new`'s seed | the bridge reads `time_boot_ms` (the stand-in's tick count), never the Unix fields; the simulator calls `Vehicle::seeded` with the episode seed |
| murmur-standins `Vehicle::started` (`Instant`) | TIMESYNC's `tc1` in the stand-in's reply | the bridge checks only `tc1 != 0` and times the round trip on its own (simulated) clock |
| murmur-standins `gnss.rs`, `conditions.rs` (threads, sleeps, `Instant`) | GNSS jamming and tidewater conditions pollers | not started (no tidewater in the simulator, section 12). World v3 (section 17): the stand-ins' GNSS events and faults use `gnss::Env` with no sky table (no geometry thread) on the vehicles' own tick clock; their conditions come from the simulator's own task on the paused clock (`fleet::feed_water`); a reboot's fresh autopilot draws from the episode seed and its reboot count |
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
`[playbook]` table: the G1 reference. Its operator draws from the file's `seed` (1), as `make
situation NAME=harbor-watch` does every time; a batch's episode seed varies the stand-ins, the
link emulator and the playbook's first tick. G1's copies set the file's `seed` to the episode seed
k, and the full stack runs them with `MURMUR_LINKEMU_SEED` = k, so there everything varies, on
both sides alike.

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

**A Puma sent home lands** (murmur-a5v, 2026-10-22; a change to the world in every situation,
v1 and v2, and in the full stack's Harbor watch, whose Pumas are stand-ins too). The Puma's
stand-in carries ArduPlane's RTL_AUTOLAND 1 and a hand launch, TKOFF_THR_MINACC 15
(`standin_params` in `sim/platforms.toml`, given by `sim/swarm.py` `standin_params`, which
`situations.py` uses). An RTL, whatever started it (an rtl order, the lost-link or the battery
failsafe), flies home; on the RTL circle, at least 3 s into the RTL, it hands over to the
AUTOLAND mode, ArduPlane's landing at home (the stand-in's step: ArduPlane's RTL hands over only
to a DO_LAND_START sequence in the mission, which the bridge's missions never carry;
docs/AUTHENTICITY.md), loiters down to 55 m beside the final approach's start 400 m out, lands
at home and disarms. Down, it reports AUTOLAND, disarmed, draws the avionics load only (5 % of
cruise's), and refuses to arm ("Arm: landed, no crew for a hand launch"), so a later order for
it fails, the order's failure and the inbox's `task_failed` alert carrying those words (the
bridge's arm retry now ends with the autopilot's last word, as its mode change already did).
From an RTL at home to the disarm takes 75 to 105 s, about 0.9 % of a Puma's pack. Section 11's
results predate it. On dev seeds 1000-1029 (S1, S5, S6, S7; playbook, oracle, none;
approve-all) an episode's truth, picture or audit log differs from before exactly when a Puma
handed over to a landing (360 of 360 checked; of five records compared in full, the four with
a landing, the golden's among them, were byte-identical up to it, the fifth throughout): all of
S5, 59 of 90 in S7, 33 of 90 in S1,
12 of 90 in S6, outside S5 mostly after the Pumas' own lost-link failsafe; no served share and
no S5 fault-met share changed. In S5 the playbook's
rtl (at its 25 % line, 21.8 to 25.0 %) lands every Puma above 20 % (20.8 to 24.2 %); the
oracle's (60 s before its true battery reaches 20 %, at 20.65 %) lands none above it (19.6 to
20.0 %): the landing costs more than that minute.

**A plane that starts in the air has its home on the surface below it** (murmur-a5v, the same
day). Until then it was at 0 m, so a Puma down at Crissy Field (PUMA 1's v2 home: land, 3.0 m)
lay 3 m under it, and the link emulator, which keys its terrain answers on 10 m altitude bands
and puts a LOS vehicle's antenna at its altitude, judged it at 0 m behind the DEM's 8 to 12 m
structures between it and the ground station: 17 m of obstruction, 80 to 90 % of its packets
lost, lost-link alerts on it for the rest of the episode (354 in S5, 218 in S7 over dev seeds
1000-1029). Now the simulator puts an airborne spawn's home at the baked grid's height there (the
stand-in engine live, at tidewater's: `surface_m` in its config), and the emulator judges a
landed, disarmed aircraft at rest: at its own altitude plus a 0.5 m antenna (`AT_REST_ANTENNA_M`,
murmur-linkemu `link.rs`), not rounded into a band; nothing else changes for any other vehicle.
Down at 3.0 m, PUMA 1 still has the DEM's structures 13.7 m above its ray to the mast, which
stays: about 70 % of its packets lost, a whip on the ground behind them. PUMA 2 rests on the water at
0.4 m. On the same seeds: lost-link alerts on a landed Puma 354 to 6 in S5, 218 to 1 in S7, 56
to 0 in S1, 6 to 0 in S6; no served share and no S5 fault-met share moved. Every plane that
starts in the air reports its altitude above that home (REAPER 1's over the water, 0.4 m), so the
engine's logged check inputs carry it, and float rounding in its missions' relative altitudes
moves REAPER 1's battery by a hundredth of a percent late in an episode; no decision changed in
the episodes compared in full.

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
calibration, the drawn first tick, the emulator's seed, when the fleet was ready), the AI's own log lines on the
simulated clock (the model agent's: section 14.1), the plans, every proposal (time, play, version, target track, truth target,
tracker target, the picture, vehicle, task, tier, state at submission, refusal), every operator
decision, our intents, the truth (the contacts' per-step log, our vehicles every 2 s, the
tracker's association, what a family states), the metrics, the audit log's event count, head and
hash (the log itself on request), and `perf` (computer time, never compared: the slow ticks the
live agent logs as `tick.slow` are counted there, not written into the record). A full-stack run's
record carries its `provenance.json`: the commit, the binaries' hashes, the emulator's seed, the
same untracked inputs and the host's load average at its start. A model episode (section 14.1)
adds `model` (its configuration, budgets, the cost guard's summary, every wake, the totals),
`budget_stopped` and `model_failed`, and its provenance names the models, efforts and the prompt's and tools'
hashes; a playbook or `none` record has none of these keys.

`python -m eval.headless.batch` (or `make headless`) runs episodes on a process pool, one
`murmur-headless` process each, writes the records and an aggregate per situation or family
(and per agent: `--agent` runs each situation and seed once per agent, never pooled):
each metric's mean with a Student t 95 % interval and, for proportions, the pooled Wilson
interval (eval/stats.py); the speed (episodes per core-hour = episodes / (wall hours x workers),
the single-worker rate, the 95th percentile of an episode's wall time); and where an episode's
time went (`perf`: process start, the simulator's own time per task and per request kind, the
pipe, the driver and the playbook). With more than one agent, `paired` sets each family and
seed's metrics side by side, agent by agent; with the model agent, each group's `model_cost`
(US dollars per episode and in all, wakes, model and tool calls, tokens) and the batch's
`model` (the ledger, the total, the budget-stopped episodes).

## 11. Results

### 11.1 Calibration (in-sample, not a gate)

The first calibration (CRITERIA.md "Calibration") set three start offsets from full-stack runs
with `seed` = 101, 102, 103: contacts 0.823 s, stand-ins 1.001 s, the playbook's first tick 1.031
s; the simulator then agreed with those runs on 9 of 9 proposals. Fix round 1 (CRITERIA.md,
2026-10-10) refitted on 101 to 106 (104 to 106 run for it, with the emulator on each run's seed):
the contacts at 0.821 s, the stand-ins at 0.966 s, the bridge up at 1.024 s (medians), and the
playbook's first tick drawn per episode seed from 0.711 to 1.590 s (the six runs' range). With
these the simulator agrees with runs 104 to 106 on 9 of 9 proposals. **Both are in-sample**: the
values were fitted on those runs, so they say only that the fit took; neither is part of G1.

An observation for a later calibration, not used here (no calibration is fitted on gate seeds): in
19 of the 20 gate runs the playbook's first tick came 1.28 to 1.41 s after `ai_start_s` (0.46 and
0.52 s in the other two), a narrower spread than the six calibration runs showed.

### 11.2 G1: fidelity on Harbor watch

G1 has run twice. The first run (seeds 1 to 10) stays on record as run; the second (seeds 11 to
20) follows fix round 1, under CRITERIA.md as amended on 2026-10-10 before it.

#### The re-run, seeds 11 to 20 (2026-10-10)

Ten full-stack runs (`make situation` with Harbor watch copies `seed` = k and
`MURMUR_LINKEMU_SEED` = k, one at a time on the assigned port block, all ten good at the first
attempt, each replayed with its anchors: "every decision reproduced exactly"), then `python -m
eval.headless.gate g1 --seeds 11-20`. Everything at commit 0887dca, clean on both sides (the full
stack's `provenance.json`, the simulator's records). The host was shared: its load average at the
runs' starts was 1.5 to 17.4 (a browser and the user's 22-vehicle stack). Records:
`logs/headless-g1/gate2/` (full stack), `logs/headless-g1/gate2-headless/` (headless, with audit
logs), `logs/headless-g1/g1-rerun.json` (not committed).

**G1 passes**: agreement **0.857** (24 pairs; 26 headless and 26 full-stack proposals;
24 / (26 + 26 - 24)), and every metric inside the other side's interval both ways:

| metric | headless | full stack | pass |
|---|---|---|---|
| `look_covered` | 10/10 [0.72, 1.00] | 10/10 [0.72, 1.00] | yes |
| `time_to_look` | -114.45 s (floor interval +-2 s) | -115.12 s (floor interval +-2 s) | yes |
| `false_looks` | 0.0 | 0.0 | yes |
| `precision` | 25/25 [0.87, 1.00] | 26/26 [0.87, 1.00] | yes |

Proposals side by side (situation's clock; every target `fastboat-west` by truth, every vehicle
PUMA 1):

| seed | headless | full stack | pairs |
|---|---|---|---|
| 11 | investigate 178.5, recentre 211.5 | investigate 178.3, recentre 210.4, recentre 270.5 | 2 |
| 12 | investigate 177.9, recentre 211.9, recentre 271.9 | investigate 177.1, recentre 209.2, recentre 269.4 | 3 |
| 13 | investigate 178.3, recentre 210.3, recentre 270.3 | investigate 177.1, recentre 209.2, recentre 270.9 | 3 |
| 14 | investigate 178.4, recentre 213.4, recentre 273.4 | investigate 178.1, recentre 213.3, recentre 273.4 | 3 |
| 15 | investigate 178.5, recentre 210.5 | investigate 177.2, recentre 210.3 | 2 |
| 16 | investigate 178.8, recentre 211.8 | investigate 177.1, recentre 210.2 | 2 |
| 17 | investigate 178.3, recentre 210.3 | investigate 178.2, recentre 210.3 | 2 |
| 18 | investigate 178.7, recentre 210.7 | investigate 178.6, recentre 210.7, recentre 275.9 | 2 |
| 19 | investigate 178.3, recentre 211.3, stuck hold 305.3, investigate 311.3 | investigate 177.1, recentre 210.7 | 2 |
| 20 | investigate 178.0, recentre 223.0, recentre 284.0 | investigate 178.1, recentre 223.3, recentre 283.9 | 3 |

**Every unpaired proposal, and why:**

- **Seed 11, the full stack's recentre at 270.5 s.** The headless PUMA 1 went home on its
  lost-link failsafe at 258 s in the terrain shadow west of the bridge (below), the full stack's
  not until 301.5 s. The same seed gives both emulators the same streams of draws, but a draw falls
  on whichever packet comes next, so the two loss patterns line up only while the two packet
  sequences do; here they had not.
- **Seed 18, the full stack's recentre at 275.9 s.** Both sides withdrew the waiting recentre when
  PUMA 1's link dropped (274.7 and 274.9 s), and on the next tick both selected it again. The full
  stack proposed it (and withdrew it a second later, the link still lost); the headless's preview
  failed the reserves check: PUMA 1's last battery report was 10.6 s old against a 10 s limit.
  Where that report fell in the second decided it, on either side.
- **Seed 19, the headless's stuck hold at 305.3 s and its investigate at 311.3 s.** The full
  stack's PUMA 1 went home on its failsafe at 240.4 s. The headless's link held long enough for
  the recentre's upload to start and fail ("mission upload unconfirmed", 266 s) and was then lost;
  the bridge raised a stuck alert on the failed task, the playbook held PUMA 1 and, the hold done,
  looked again. The full stack's Puma was home by then. Same cause as seed 11, the other way.

**The early return** (PUMA 1's first RTL after 230 s coming before 272 s, which in 94 % of 200
headless episodes decides whether the second recentre is made): the full stack 4 of 10 gate runs
(seeds 15, 16, 17, 19) and 0 of 3 calibration runs (104 to 106), 4 of 13; the simulator 4 of 10 on
the same seeds (11, 15, 16, 17) and **44 of 200 (22 %)** over G1-style episodes of seeds 21 to 220
(copies with `seed` = k, the emulator on k); 4 of 13 against 44 of 200, Fisher two-sided p 0.50:
no difference shows. The two sides put a seed in the same class (early or
late) in 8 of 10 seeds, and the RTL within 1.5 s in 4. The reviewer's diagnostic for the first run
(the simulator with the emulator on seed 1, as the full stack had it): 24.5 % over 200 episodes.

Reported beside the gate (not gated): human-tier cards per episode 2.5 headless and 2.6 full
stack; approvals 20 and 21, voided at approval 5 and 5; approval latency median 28.3 s headless
and 19.9 s full stack (means 27.0 and 23.0). Each full-stack run's own offsets: contacts 0.809 to
0.837 s, stand-ins 0.918 to 0.977 s, the bridge up at 1.016 to 1.045 s, the playbook's first tick
1.285 to 1.408 s (seed 11: 0.459 s); the simulator drew 0.719 to 1.547 s for the same seeds.
Fleet readiness: the full stack 2.8 to 3.9 s, the simulator 3 s in all ten.

#### The first run, seeds 1 to 10 (2026-10-09), on record

As CRITERIA.md first froze it: agreement **0.800** (24 pairs; 29 headless and 25 full-stack
proposals), every metric inside both ways (`look_covered` 10/10 and 10/10, `time_to_look` -114.65
and -114.50 s, `false_looks` 0 and 0, `precision` 29/29 and 25/25). Binaries: the full stack's
seed 1 at 28e2fa7, seeds 2 to 10 rebuilt by `up.sh` at 23a950a (no Rust changed between them),
the simulator at 28e2fa7. Records: `logs/headless-g1/gate/`, `logs/headless-g1/gate-headless/`,
`logs/headless-g1/g1.json`.

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

Every divergence was the second recentre at about 271 s (9 of 10 headless, 5 of 10 full stack),
made exactly when PUMA 1 had not gone home on its lost-link failsafe by then. PUMA 1 flies the
boat's orbit through a terrain shadow west of the bridge. There the emulator's line of sight from
the ground station clears by 11 m until about 225 s, then is obstructed by 3, 7 and 11 m, losing 17
to 56 % of packets on the `ramp` model. The Puma's GCS failsafe sends it home after 5 s without a
heartbeat. Early returns came in 5 of 10 full-stack runs and 1 of 10 headless episodes. The
terrain (the baked grid within 0.15 m of tidewater's own samples on the same profiles), the
Puma's path, the downlink's packet counts and the loss code were checked to be the same on both
sides. The difference was the draws, which **fix round 1 found and removed**: every full-stack run
had the links file's seed 1, and so the same draws (PUMA 2 failed safe at 317 s in all ten), while
each headless episode had its own (CRITERIA.md, 2026-10-10). Its fleet readiness, offsets and
inbox numbers are in the git history of this file (commit bbf894c).

### 11.3 G1b: speed

**At the stated W = 8** (CRITERIA.md, 2026-10-10), Harbor watch, the default operator, seeds 1 to
200, measured 2026-10-10 after the gate runs, with this worktree's stacks down. The host was
shared, not idle: load average 14.7 just before, about 77 % of its 28 threads idle (a browser
using about 5 threads and the user's 22-vehicle stack about 2), 24.5 just after:

| workers | episodes | episodes per core-hour | episode wall mean | 95th percentile |
|---|---|---|---|---|
| **8** | 200 | **1,074** | 3.33 s | 3.54 s |
| 1 | 30 | 1,361 | 2.65 s | 2.72 s |

**G1b passes at W = 8: 1,074 episodes per core-hour.** On 2026-10-09, on a quieter host and before
fix round 1, the same measure gave 1,235 at 8 workers, 998 at 16 and 1,585 at 1. Every part of an
episode (the bridge's tasks, the playbook's unchanged Python, the pipe) took about 15 % longer on
2026-10-10 alike, which points at the host, not the fix round's code. CRITERIA.md lets W be stated;
the 16-worker miss stays reported so the pass is not read as more than it is. Fix round 2 found
that these 200 episodes all ran the link emulator on seed 1 (the situation file's; PUMA 2 failed
safe at 318 s and the fleet was ready at 3 s in every one), the correlation fix round 1 removed
from G1 now inside a batch; a Harbor watch batch now runs each episode on its own seed. G1b was not
re-run for it (the speed does not depend on which draws fall where). The design's
estimate (1 to 1.5 s an episode, 2,400 to 3,600 per core-hour) was optimistic by about half.

Where an episode's time goes (2026-10-09, Harbor watch, one worker, mean seconds per episode of
720 s simulated; the simulator's tasks run inside its `call.*` requests, so they are parts of
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

On the families (8 workers, 30 seeds each, 2026-10-09; not G1b, which is Harbor watch, but the
cost the research's step 3 will pay): the tuning families 1,290 per core-hour (2.7 to 2.8 s an
episode); the held-out families 1,008 (`two-at-once` 2.95 s, `stuck-vehicle` 3.37 s,
`approval-flood` 3.97 s: its flood of orders moves HUNTER 1 and REAPER 1 to new places, whose
SATCOM looks are not yet in the look cache (0.40 s computing them) and the decision loop about 1.6
times the work (0.72 s)).

If more speed is wanted, in the design's order: fewer JSON round trips (a tick's reads in one
request, which would save part of the pipe's 0.24 s, not the read tools' own 0.55 s); the session
in process (PyO3). The bridge's decision tick is the largest single part; it is the bridge's own
code and stays as it is.

### 11.4 The families (model results, seeds 1 to 30 each)

The playbook as committed, the default operator, after fix round 1 (2026-10-10,
`logs/headless-families/rerun-{tuning,held-out}/`, not committed; the metrics as CRITERIA.md
writes them, `link-cut-mid-look` version 2). Coverage is per episode (every must-look contact
covered), with its Wilson interval; time to look is from the contact's first crossing within 3 km
of the bridge to the first look proposed on it, so a negative time is a look before the
crossing; false looks are `isr.*` proposals on contacts that need none, per episode; precision is
pooled over the human-tier proposals it judges; cards are the playbook's human-tier cards per
episode. **Tuning and held-out are never pooled.**

| family | split | look covered | time to look, mean s | false looks | precision | cards |
|---|---|---|---|---|---|---|
| `fast-unknown` | tuning | 30/30 [0.89, 1.00] | -57.8 (30) | 0.0 | 91/91 | 3.03 |
| `decoy` | tuning | 15/15 [0.80, 1.00] | -95.3 (15) | 0.73 | 49/64 | 2.17 |
| `link-cut-mid-look` (version 2) | tuning | 30/30 [0.89, 1.00] | -70.5 (30) | 0.0 | 78/78 | 2.60 |
| `low-battery` | tuning | 20/30 [0.49, 0.81] | -65.7 (20) | 0.0 | 59/59 | 1.97 |
| `two-at-once` | held-out | 4/30 [0.05, 0.30] | -93.5 (35 of 60 boats) | 0.0 | 94/94 | 3.13 |
| `stuck-vehicle` | held-out | 30/30 [0.89, 1.00] | -90.9 (30) | 0.0 | 96/96 | 3.20 |
| `approval-flood` | held-out | 30/30 [0.89, 1.00] | -84.4 (30) | 0.0 | 103/103 | 3.43 |

One proposal in all 210 episodes was unjudged (a `link.cover_gap` in a `decoy` episode).

`link-cut-mid-look` version 2 cut the vehicle flying the first look in all 30 episodes and
recorded "no look to cut" in none, and all 30 were covered; but in 3 (seeds 14, 18, 28) PUMA 1 was
already going home on its own lost-link failsafe when the cut came (by 25, 4 and 11 s on the 2 s
samples of our vehicles), so **27 of 30 cut a look in progress**. The family aggregate now counts
both cases (`faults`: `look_in_progress_cut` 27, `recovering_before_cut` 3, `no_look_to_cut` 0;
`logs/headless-families/round2-link-cut/`). Version 1 (2026-10-09, reported apart) cut a look in only
11 of 30 and PUMA 1 regardless in the other 19, and covered 20 of 30: its 10 misses were all among
those 19.

What the misses are (from the records and the playbook's own status lines):

- `low-battery`: all 10 misses are the episodes whose low vehicle is PUMA 1, the one free sensor.
  The playbook sends it home (`safety.low_reserve`, in every episode) and, as the family's label
  asks, does not choose it for a look; it then proposes no look with any other vehicle.
- `two-at-once`: one free sensor for two boats: 26 episodes covered one boat, 4 both. Every look
  was flown by PUMA 1; the second boat waited on "recentred within the last 60 s" or "no free
  sensor".
- `decoy`: looks on the decoy while the picture showed it `unknown`, releases of it once shown
  `neutral` (an `isr.*` play, so a false look by CRITERIA.md), and a few on a cargo ship shown
  `suspect` (2026-10-09: 13, 6 and 2 of the 21).

These are findings about the playbook on a model, for the research's step 2 and 3, not changes
made here; nothing was tuned on them.

## 12. What the simulator leaves out, as found

| difference | where it shows | effect measured or expected |
|---|---|---|
| process start offsets are medians from the calibration runs, the playbook's first tick a uniform draw over their range, not each run's own | when the first look comes, and the phase of every tick | the re-run's own offsets were within 0.05 s of the calibrated medians; the look agreed within 1.7 s in every seed (section 11.2) |
| the emulator's draws fall on packets in arrival order, the same streams on both sides for a situation (fix round 1) but not always the same packets | packet loss on a degraded link, and so whether a lost-link failsafe fires | the re-run's three unpaired cases (section 11.2); the early-return rate 4 of 13 full stack, 22 % of 200 headless episodes |
| no tidewater currents or winds (the stand-ins run without conditions; drifting contacts do not drift) in every v1 and v2 situation (Harbor watch pins `currents = "none"`); a world v3 situation takes its world's baked field and depth, the tide held at its bake (section 17) | HUNTER 1's patrol track; the Mile Rock boat | none seen on the playbook's proposals |
| the emulator's terrain line of sight from a baked surface-height grid (`sim/headless/terrain/golden-gate.grid.gz`, 20 m spacing, tidewater's `max(elevation, water level)` at the scenario's start; provenance beside it, its hash tested) instead of tidewater's samples; a cell's first answer at once instead of up to a second later | the Pumas' terrain shadows behind the Presidio | the grid is finer than the emulator's 250 m cells |
| SATCOM looks at the terminal's position rounded to 0.001 deg (about 100 m) and 100 m of altitude, cached between episodes; the 30-minute outlook (display only) off | Reaper's SATCOM link | under a tenth of a degree of elevation to a satellite 550 km up; the outlook is not read by any tool |
| the bridge's space segment off (section 5.1) unless `Options.space_snapshot` pins a world's element sets (section 16.2) | collect and cover tasks; the audit log has no `space.loaded` | none on the playbook (no space plays) |
| the domain rule "not evaluated" for surface vehicles (no tidewater for the decision layer's terrain) in every v1 and v2 situation (Harbor watch pins `domain_rule = "not_evaluated"`), which also blocks a boat's or UGV's consequential engagement there; a world v3 situation's is answered by its world's baked land/water grid (section 17), so boat and UGV engagements can pass it (section 16.4) | the checks shown on HUNTER 1's orders; `magura`, `ugv_armed` | none on the playbook's air tasks |
| no concurrency: a tick's reads at one instant, zero time per call | timing within a tick | under the 10 s matching window |
| no console push, CoT export, situation panel | display and export | none |

## 13. Tests and checks

`make check` runs, beside everything it ran before: `cargo clippy` and `cargo test` for the new
crate (`tests/pieces.rs`) and the bridge's unchanged suite with its no-seams test; py_compile and
`pytest eval/headless/tests` (the operator against `eval/proxy.py`, the session adapter, labels,
the association rule, the split, the families' determinism and variation, boats on water, the
production build's cfg, the grid's provenance, G1's pairing and intervals with the 10 s window
pinned, the golden with its feed-id check and replay, the two view-path tests, two processes, the
live reads' schema, and `test_fixes.py`: one test per finding of the fix round, each shown to fail
with its fix undone: the same emulator seed on both sides, the link cut waiting for a look, no
computer time in the record, the provenance and the bridge's links file, CRITERIA.md's metrics,
the per-step truth, the drawn first tick, the bridge's start, the full stack's binary hashes, the
reconnect delays; and `test_officer.py`: the model agent of section 14.1 on the real binary with a
fake Anthropic client (`tests/fake_anthropic.py`, no network): a whole episode and its record,
the routine conversation carried and the consult fresh, the clock still across every wake, the
look mapped to `isr.investigate` and judged as a playbook look, a model that does nothing, a
wake the guard refuses (`cost_guard`, `budget_stopped`, nothing sent), a spend past the cap, the
shared ledger, the aggregate leaving budget-stopped episodes out, the mapping table, the
configuration and the permission it needs; `test_charged_clock.py`: section 14.1.1's charged
clock, each call's charge and answer time, the operator, script and samplers inside a turn in
order, an action at its charged time, repeats, an answer after the end, a failed call, the
playbook's tick charge, the loop's order, and the paused clock's records byte-identical to before
it; `test_v2_leakage.py`: the token list, same tasking (the mission as the only brief, one prompt
and one tool list for every situation), a reading model on every v2 situation clean, a family
name in a tool result a harness error, echoes and diagnostics; `test_world_v2.py`'s Puma tests
(section 8): only the Pumas' stand-in configs carry RTL_AUTOLAND and the hand launch, and PUMA 2
sent home lands at home in AUTOLAND, disarmed, shown so by `get_fleet`, and a later orbit for it
fails with the stand-in's words in the order and the inbox's alert). The landing itself, the
disarm, the battery and the refused arm are the stand-ins' own tests (`behavior/plane.rs`,
`tests/contract.rs`); the bridge's arm failure with the autopilot's words is `tasks/tests.rs`.
`test_engage_space.py` (section 16): the signers' and the reviewer's policies on a fake operator
API, the scripted agent's choices, the pinned snapshot (its provenance, a changed file caught,
the unpacked tree), and smoke episodes on the binary: the engagement chain end to end, an abort,
a miss with a surviving munition un-spent, a lost contact blocking until its AIS claim is
confirmed, a collect, a cover, each log replayed with its
anchors, two of them run twice and compared whole, and an episode without the options having
none of it.

## 14. Step 2: how it plugs in

- **The truth objective** is a function of the episode record; nothing in the simulator changes.
  Add the fault-handling metrics (section 9) the same way.
- **Agents** are interchangeable in the driver (`Options.agent`, `batch --agent`): `playbook`
  (default), `model` (the model watch officer, section 14.1) and `none`; an `oracle` (handed the
  truth labels explicitly, recorded as such) and `random-valid` (uniform over actions whose
  preview passes) each get only a session.
- **Operators**: train and tune with `approve-all`; evaluate with `proxy` and its presets.
- **Families and seeds**: held-out families report separately and are never tuned on; tuning
  work uses seeds 1000 and up on tuning families only (CRITERIA.md).
- **The fidelity gate repeats** for each new family once a full-stack situation of its kind exists.

### 14.1 The model watch officer (`agent = "model"`)

The question it serves: on the same frozen situations and seeds, how does the LLM watch officer
(agents/watch_officer.py's design: a routine model woken by the trigger table, a stronger model
consulted for consequential moments, the same prompt) compare with the playbook and with no AI
on section 9's metrics, and what does one episode cost. **It calls the Anthropic API and costs
money**: an episode refuses to start it without `MURMUR_EVAL_MODEL=1` unless it is handed a
client (the tests hand it a fake one: nothing in `make check` calls the API).

**What it is.** `eval/headless/officer.py`, agent id `agent-watch` (the live officer's). The live
officer's own pieces, imported, not copied:

| piece | from |
|---|---|
| when to wake, escalate, coalesce, hold back repeats; the budgets | `watch.wake.Waker`, with the `Budgets` the driver builds from `watch_officer.parse_args([])` (60 wakes an hour, 12 tool turns a wake, 6 escalations, a 60 s heartbeat doubling to 300 s while quiet, 3 s coalescing, 10 s between wakes) |
| what it reads between wakes | `watch.picture.Poller`: `get_changes` on its cursor every `poll_s` (2 s) and the nomination check while a track is declared hostile, through the session adapter |
| what it is told | `compose` (the update), `render_mine` (its own proposals), `Officer.budget_line`, `CONSULT_HEAD` and the consult note `Officer.do_wake` writes for the routine model's next update |
| the system prompt | `build_prompt`: `agents/watch/prompt.md` and the situation's brief (`eval/situation.py` `brief_text`, the title and the operator's brief, never the director's notes; a family keeps Harbor watch's) |
| models and efforts | `watch_officer.DEFAULTS["claude"]`: routine `sonnet` at `medium`, consult `opus` at `high`, as the API ids `claude-sonnet-5-5` and `claude-opus-5-5` (`API_MODELS`); a wake goes to the consult when the `Waker` escalates it and that lane is not backing off, as `Officer.do_wake` decides |
| a failed turn | `Officer.failed`: the lane backs off (30 s doubling to 900 s), the wake's changes are given back |
| the model's turn | new: `ApiBackend`, a Messages API tool-use loop in place of `watch/claude.py` (below) |

**The turn.** The tools are the bridge's MCP tools as the simulator lists them (`murmur-headless`'s
`tools` request with `defs`: the bridge's `tools/list`, the definitions an MCP client hands its
model), 18 of them; each answer goes back as the compact JSON the MCP server's text content
holds, up to the bridge's 150,000 characters. A wake is at most `max_turns_per_wake` (12) model
calls. The routine model's conversation carries over from wake to wake, as the CLI's long-lived
`claude -p` session does: the driver keeps the messages and sends them again with each call,
append-only (thinking blocks back unchanged), cached at the end of the system prompt and at the
last block (`cache_control`). A consult is a fresh conversation (the system prompt and its
update), as the CLI's consult process is, and the routine model is told what it did. Each call
sets `output_config.effort`; `thinking` is left out (adaptive on both models). `max_tokens` is
16,000 a call. A tool outside the bridge's list is answered as the locked-down CLI answers it
(no such tool).

**Simulated time.** The officer is the episode's `agent` process: it comes on at `ai_start_s`
plus the drawn first-tick offset (the playbook's), polls every 2 s of simulated time, and asks
the `Waker` when to wake as `Officer.run` does. A wake runs between two simulator steps: every
tool call is a request at the current instant (the simulator refuses one that moves its clock),
and each wake reads the simulator's clock before and after and fails the episode if it moved
(`clock` in the wake's record; tested). The world waits for the model: the wall time a wake took
is recorded (`wall_s`, `perf.model_wall_s`), never simulated. That is the **paused** clock, the
default; the **charged** clock (section 14.1.1) makes a turn cost simulated time.

**What differs from the live officer** (`make situation NAME=... AI=claude`):

| live | headless `model` | effect |
|---|---|---|
| the world moves on during a turn (10 to 60 s of tool calls and thinking) | the clock stops for a wake (paused; on the charged clock it moves by each call's charge, section 14.1.1) | the model is never late for being slow, and acts on a picture as old as its last read, not older; `min_gap_s` and the heartbeat count from a wake's start in both, so a wake can come 10 s of situation time after the last one where live it would come after a long turn ended |
| Claude Code (`claude -p`, the user's subscription), its session resumed | the Messages API (the SDK's own credentials, billed per token), the conversation kept by the driver | the same conversation; Claude Code may also compact a long session (not reached in 12 minutes) and may add lines of its own to the context |
| tools named `mcp__murmur__<tool>`; whether the CLI adds the bridge's MCP server instructions (`mcp::INSTRUCTIONS`) under `--system-prompt-file` is not checked | the bare tool names (the prompt names them so); the server instructions not sent | |
| `--max-turns 12` (the CLI's count) | at most 12 model calls a wake | a long wake may end at another point |
| usage limits (`watch/limits.py`): wait for the reset or stop | none; an API error after the SDK's two retries is a failed turn (`stop: error`) | the lane backs off on the simulated clock |
| the CLI's handling of a refusal | no server-side fallback by default (`MURMUR_EVAL_WATCH_FALLBACKS=1` turns it on): a refusal ends the wake, recorded `stop: refusal` | |
| `--shadow-playbook` in situation runs | not run: the playbook is its own arm on the same seed | |
| `--max-budget-usd`, the CLI's estimate | the cost guard (below) | |

**The cost guard** is eval/model_agent.py's `CostGuard` and `Ledger` (`WatchGuard`): before each
call, the worst case of that call (its request at 2 characters a token and its whole
`max_tokens`, at the list price of the model it goes to; with fallbacks on, at the dearest model,
twice) is checked against the episode's cap (`MURMUR_EVAL_MAX_USD`, default 3.00) and reserved
in the batch's ledger (`MURMUR_EVAL_BATCH_MAX_USD`, default 20.00; the batch puts
`model-ledger.json` in its directory and every worker settles into it under its file lock);
after it, what it billed at list prices is settled. Unlike the storyline arm, which prices
every cache read as fresh input, this arm settles cache reads ($0.20 a million tokens on both
models) and cache writes (1.25 times input) at their own rates: its conversation is cached on
purpose, and pricing it as fresh would stop a batch at a fraction of its real spend. Every model
the officer can use must have a price row (an unknown one is refused before the episode starts).
**A call the guard refuses ends its wake as `cost_guard`, the officer stops (every later call
would be refused too), and the episode is marked `budget_stopped`**, never recorded as the
model choosing to do nothing (bd murmur-cv2's lesson from the storyline arm, not fixed there).
The batch leaves budget-stopped episodes out of every metric and lists them apart, with their
cost.

**Errors.** An API error after the SDK's two retries (a connection error, a timeout, an
overload, a server error) fails the wake as the CLI's error turn does: what it was told is
given back and that model's lane backs off (`Officer.failed`, on the simulated clock); the
episode is kept and its errors counted (`errors`, the aggregate's `with_errors`). A request the
API refuses as such (a 4xx other than 408, 409 and 429: bad parameters, credentials, an unknown
model) or one the SDK cannot make stops the officer at once: every later call would fail alike.
That episode is `model_failed`, left out of every metric and listed apart, as a budget-stopped
one is. A failed call's reservation counts as spent (what it billed is unknown), as the guard
counts it for the storyline arm.

**The record** (section 10): `model` holds the configuration, the budgets, the guard's summary,
the totals (wakes, consults, model and tool calls, proposals, tokens: fresh input, cache write,
cache read, output; US dollars, wall seconds, stops, `budget_stopped`) and every wake: its
simulated time and clock check, reason, why, lane (`routine` or `consult`), model, effort, the
update it was given, every tool call (name, arguments, an excerpt of the answer, error), every
model call (request characters, tokens, cost, served model, wall seconds, stop reason), the
final text (2,000 characters), the stop. Its proposals are `agent_log` lines (`source` model)
with the play category each maps to and what the picture showed of its target, read by the
driver as `agent-watch` at the instant before the proposal went in (`get_tracks`, `get_fleet`
compact, `get_inbox`; reads are not logged by the bridge). The provenance adds the models,
efforts, fallbacks, `max_tokens` and the system prompt's and tool definitions' hashes.

**The metrics** come out the same for `model`, `playbook` and `none`: metrics.py judges the
playbook's proposals by play name and target; each model proposal is mapped to the play category
it is equivalent to by what it does, and its target matched to truth by the playbook's rule
(labels.py `associate`, from the track's position as the picture showed it then):

| the model proposed | maps to | target |
|---|---|---|
| an orbit centred within max(its radius, 500 m) of a track shown, or, near none, of where a moving track will be on its course within 180 s (a look placed ahead of a fast mover; `lead_s` in the record), its vehicle not flying the model's own look on that track | `isr.investigate` | the nearest such track |
| the same, its vehicle flying the model's own look on that track (that order not failed, superseded or requeued, the vehicle not recovering on its own) | `isr.recentre` | that track |
| a hold or rtl of a vehicle shown with a `not_moving` or `task_failed` alert (an rtl of one also under the low-reserve line maps to low reserve) | `safety.stuck` | `alert:<id>` |
| a hold or rtl of a vehicle shown under the low-reserve line (25 %, the shipped watch policy's reserve plus 5) | `safety.low_reserve` | `battery:<uid>` |
| a hold or rtl of a vehicle flying the model's own look, no fault shown | `isr.release` | that look's track |
| anything else: a patrol, an orbit near no track, a track, collect, cover, engage | `unjudged:<task>` | none |

A second track within 1.5 times the nearest's distance makes the aim `ambiguous`, judged as an
ambiguous playbook target is (not legitimate). `look_covered` and `time_to_look` count
`isr.investigate`, `false_looks` any `isr.*` category on a contact that needs no look, `precision`
the human-tier proposals it can judge (an unjudged one is listed with its kind); `by_play`
counts every proposal by category so none is dropped. `unfaithful` and `objected` read the
playbook's evidence record and count playbook proposals only; a re-proposal after a rejection
counts for the model unless its rationale carries an evidence record naming a material change,
which a model's prose never does (read the rationales before quoting it). Human-tier cards per
operator-minute and approval load (`human_cards`, `outcomes`, `approval_latency_s`) are the same
functions of the audit log for every agent.

**What one episode costs, estimated** before any paid run, from the sizes the fake client saw
with the real prompt, tools and picture (Harbor watch seed 1, a model that reads the four tools
the prompt names at every wake and proposes one look): system prompt 5,195 characters; tool
definitions 30,889; an update about 800 (at most about 1,200); the reads about 15,700 a wake
(`get_tracks` 11,300, `get_fleet` compact 2,200, `get_intents` mine 1,700, `get_inbox` 540); a
first request of 36,800 characters, a last of about 311,000 after 15 wakes; 31 model calls (2 of
them one consult on Opus, 37,000 and 53,000 characters) and 4.9 million request characters in
all, nearly all of them cache reads. At about 3 characters a token (JSON-heavy; an assumption):
about 1.6 million cache-read tokens ($0.33), about 120,000 tokens written to the cache ($0.35),
and the output (thinking and text: unknown until measured, $0.15 to $0.50 at 500 to 1,500 tokens
a call): **roughly $0.8 to $1.2 an episode**, less if the model reads less. The real figures are
in each record (`model.totals`) and the aggregate (`model_cost`).

**Determinism.** The world is deterministic given (situation, seed, calibration, operator); the
model is not. Two runs of one seed can differ in what the model proposes and when, and
everything downstream of a proposal (the operator's decisions, the vehicles' tasks) differs with
it, while the playbook and `none` arms repeat exactly. What tells runs apart and lets one be read
again: each wake's update, tool calls and answer excerpts, final text, served model and token
counts; the provenance (commit, binary, prompt and tool hashes, models, efforts); the audit log
(`--keep-audit`). To see the model's own variance, run the same seeds more than once, into
separate `--out` directories, and compare seed by seed.

#### 14.1.1 The latency-charged clock (scoring v2, SCORING-V2.md 4.2)

`Options.clock = "charged"` (default `paused`) makes the agent's thinking cost simulated time.
A model call's charge is `a + b_in * input tokens + b_out * output tokens + c * tool calls in its
answer` (input tokens: fresh, cache write and cache read; output: thinking included); a call
the API failed is charged `a`; a call never sent (the cost guard, the wake's last call used, a
leak) nothing. The playbook is charged `playbook_tick_s` per tick. The constants are
`eval/headless/latency.toml`, **placeholders** (a = 2 s, b_in = 2e-5 s, b_out = 0.02 s, c =
0.5 s, 0.2 s a tick) until they are fitted to measured API wall times on dev-build episodes
(SCORING-V2.md section 11, stage 1 item a) and frozen; each charged record carries them
(`provenance.clock`), and `clock.condition` and `v2.condition.clock` say `charged` (a paused
record has none of these keys, so it is byte-identical to before: tested on three fixed seeds).

**How a turn is suspended and resumed.** A wake's turn is a `TurnRun` (officer.py) driven one
model call at a time. At the wake's time t0 the officer composes the update and makes the first
call at once (wall time only); the answer is not used yet: the officer's next event is at
`t1 = t0 + charge`, rounded up to the simulator's millisecond grid. The officer returns to the
episode's loop, which runs everything scheduled before t1 in its usual order (the operator, the
script's faults and proposers, the truth and picture samplers, the officer's own poller, which
keeps adding changes to the `Waker` as the live poll task does). At t1 the answer's tool calls
run, in order, at t1: they read the world as it stands then, and a proposal reaches the bridge
then (its audit-log creation is t1). The next call is made at t1 on the conversation as it now
holds, its answer due at t2; and so on. The turn ends when an answer without tool calls is due;
only then does the `Waker` give a new wake (one turn at a time, as live). An answer due after
the episode's end is never applied: the wake is recorded `episode_end`. A playbook tick reads
the picture at its start (the engine's own `read`) and runs the rest of the tick (plan, previews,
proposals, withdrawals) `playbook_tick_s` later, on those reads; its polls wait for it.

**Why it cannot reorder events or read the future.** A turn is an ordinary event of the `agent`
process in the loop's queue, ordered by (time, process priority, order scheduled) like every
other; the loop refuses an event at an instant already passed. A call is only ever made on what
the conversation holds at the time it is made, and every tool result in it was read at or before
that time, so no answer depends on a later state of the world; its tool calls run only when the
loop reaches its charged time, after every event before it. The charge depends only on the
tokens the API reports and the number of tool calls, never on wall time, so with the same
answers (the fake client, tested) a charged episode repeats exactly. Each wake records its
calls' times, charges and answer times (`api_calls[].t`, `charge_s`, `t_answer`), when each tool
call ran (`tool_calls[].t`), the turn's end (`t_end`) and its total charge.

**The leakage scan** (SCORING-V2.md 4.4 and 7; `eval/headless/v2/leakage.py`), scoring v2 only:
the system prompt, the tool definitions, every update and every tool result are scanned for
family names, run names, seeds in names and the scorer's field names before the model can
receive them; a hit stops the officer before that input is sent (`stop: leakage`) and the
episode is a `harness_error` (`v2.leakage` holds the hits). The model's own words are scanned as
diagnostics only; the bridge echoing them back (a rationale) is recorded as an echo, not a hit.

## 15. How to run

    make build && cargo build --release -p murmur-headless
    .venv/bin/python -m eval.headless.batch --situation harbor-watch --seeds 1-200 --workers 8 --speed
    .venv/bin/python -m eval.headless.batch --families fast-unknown decoy link-cut-mid-look low-battery --seeds 1-30 --workers 8
    .venv/bin/python -m eval.headless.batch --families two-at-once stuck-vehicle approval-flood --seeds 1-30 --workers 8
    make headless FAMILIES="fast-unknown decoy" SEEDS=1-30 WORKERS=8

    # G1: the full-stack runs (about 13 minutes each, one at a time, on the assigned port block),
    # then the gate against fresh headless episodes of the same seeds
    # (each run with MURMUR_LINKEMU_SEED = its seed, set by the runner; the calibration: seeds 101-106)
    .venv/bin/python -m eval.headless.fullstack run --seeds 11 12 13 14 15 16 17 18 19 20 --out logs/headless-g1/gate2
    .venv/bin/python -m eval.headless.gate g1 --full logs/headless-g1/gate2 --seeds 11-20 --out logs/headless-g1/g1-rerun.json --keep logs/headless-g1/gate2-headless

Records go to `eval/results/headless/<batch>/` unless `--out` says otherwise; the SATCOM look
cache to `logs/headless/space-cache/` (one file per set of element sets and link profiles).

**The model watch officer** (section 14.1) calls the Anthropic API and is billed: it needs
`MURMUR_EVAL_MODEL=1`, the Anthropic SDK in `.venv` (`pip install anthropic`) and the SDK's
own credentials (`ANTHROPIC_API_KEY`, or an `ant auth login` profile). Each episode is capped by
`MURMUR_EVAL_MAX_USD` (default 3.00), the batch by `MURMUR_EVAL_BATCH_MAX_USD` (default 20.00)
through `<out>/model-ledger.json`; give each batch a fresh `--out` (a reused one keeps its
ledger's spend). Model episodes wait on the network, so workers can exceed the cores.

    cargo build --release -p murmur-headless

    # one episode, the smoke test (about $1 by the estimate in 14.1; capped at $2)
    MURMUR_EVAL_MODEL=1 MURMUR_EVAL_MAX_USD=2 MURMUR_EVAL_BATCH_MAX_USD=2 \
        .venv/bin/python -m eval.headless.batch --situation harbor-watch --seeds 1 --agent model \
        --workers 1 --keep-audit --out eval/results/headless/model-smoke

    # the comparison: Harbor watch and two tuning families, seeds 1 to 3, the three agents on the
    # same situations and seeds (9 model episodes, about $7 to $11; capped at $15 for the batch)
    MURMUR_EVAL_MODEL=1 MURMUR_EVAL_BATCH_MAX_USD=15 \
        .venv/bin/python -m eval.headless.batch --situation harbor-watch --families fast-unknown decoy \
        --seeds 1-3 --agent model playbook none --workers 9 --keep-audit --out eval/results/headless/model-compare
    # the same through make
    MURMUR_EVAL_MODEL=1 MURMUR_EVAL_BATCH_MAX_USD=15 make headless SITUATION=harbor-watch \
        FAMILIES="fast-unknown decoy" SEEDS=1-3 AGENT="model playbook none" WORKERS=9

    # overrides: another model or effort, no consult lane, the server-side refusal fallback
    MURMUR_EVAL_MODEL=1 .venv/bin/python -m eval.headless.batch ... --agent model \
        --model-config '{"effort": "high", "escalation": false}'
    MURMUR_EVAL_WATCH_MODEL=claude-opus-5-5 MURMUR_EVAL_WATCH_FALLBACKS=1 ...

What to read: `aggregate.json`'s `groups` (one per situation or family and agent: `harbor-watch`,
`harbor-watch [model]`, `harbor-watch [none]`, each family likewise; never pooled), `paired`
(each family and seed, agent by agent: look covered, time to look, false looks, precision,
human-tier cards and their rate, the model's dollars and wakes), each model group's
`model_cost`, `budget_stopped` and `model_failed`, and the batch's `model` (the ledger, the
total, the budget-stopped and failed episodes, which no metric includes, and the episodes kept
although a wake's call failed). A model episode's record is
`<situation>-model-s<seed>.json`; its `model.wakes` say what it was told, read, proposed and
said at each wake.

**Engagements, review and space** (section 16; v1 batches; the scripted signers, reviewer and
agent; the engagement effect drawn; the Golden Gate's pinned element sets):

    .venv/bin/python -m eval.headless.batch --situation hostile-act-engagement --agent scripted \
        --signers '{"preset": "scripted"}' --review '{"preset": "confirm-declarations"}' \
        --effects '{"p_hit": 0.8}' --seeds 1-10 --keep-audit --out eval/results/headless/engage
    .venv/bin/python -m eval.headless.batch --situation harbor-watch --agent scripted --space-snapshot golden-gate \
        --scripted '{"engage": null, "cover": {"at_s": 60}}' --seeds 1-3 --out eval/results/headless/cover

## 16. Engagements, review and space (world v3: H2 and H3, murmur-1z6.3)

Status: **built** 2026-10-23 (docs/WORLD-V3-PLAN.md section 4). Everything here is off unless an
option asks for it: `Options.signers`, `review`, `scripted` (with `agent = "scripted"`),
`effects` and `space_snapshot` in episode.py, or the batch's `--signers`, `--review`,
`--scripted`, `--effects` and `--space-snapshot` (v1 only). An episode without them runs exactly
as before and its record has none of their keys (the golden holds; tested). No rule of the
decision engine changed: the simulator and its scripted operators exercise what the engine and
the bridge already do, through the same REST paths and MCP tools a person and an agent use, and
the bridge logs every act as the live stack logs it.

**Scripts, not judgment, and never human data.** The signers, the reviewer and the scripted
agent each follow stated rules; every note and rationale they leave says "scripted" and "not a
person" (the agent's "not a person, not a model"). Headless runs are dev runs: no passwords and
no passkeys (credentials mode is out of scope here).

### 16.1 The engagement chain

| step | who | how (the paths a person or an agent uses) |
|---|---|---|
| a hostile act | the contacts scenario's `[[act]]` | truth on the simulation channel; our sensors observe it and the engine declares the track hostile by rule (rules 9, `ident.act`, `ident.declared`) |
| the review | the reviewer (`review.py`) | `GET /api/review`, then `POST /api/tracks/{id}/confirm` (`ident.confirmed`) |
| the nomination | the scripted agent (`scripted.py`, `agent-scripted`) | MCP `get_tracks` (affiliation hostile), `get_nominations` (none stands on it), `get_nomination_options` (at most once a second, as the tool allows), `propose_intent` task engage with the first platform that passes every check |
| two signatures | the signers (`signers.py`, `op-2` and `op-3`) | `GET /api/inbox` (the nomination's `engage` view), `POST /api/intents/{id}/picture` (a token), then `POST .../approve` with `{"token"}` |
| dispatch, the lost-link preload, its verifies | the bridge, unchanged | parameter reads and writes on the stand-in over the emulated link (`engage.preload` intent, set, verify) |
| the effect | the engine releases it (`engage.effect`); the bridge queues it on the simulation channel; the headless contacts apply it | `Options.effects` (below) |
| the assessment | the engine, on the tracks our sensors keep | `engage.assessed`: ceased when the target left the picture, persists when it is still seen |
| what is owed back | the engine and the bridge | an expended platform's own values closed at the release (`restore.closed`, expended); after an abort or a stop, the fallback and `engage.preload` restore; after an un-spend, `platform.restore` |

**The effect** (`effects = {"p_hit", "spend_expended", "survives_miss"}`; crates/murmur-headless
`contacts.rs`). The effect stays abstract: a point, a radius, a time. Whether it takes the
intended contact out of play is a draw, a uniform from the episode seed and the intent
(splitmix64, `effect_draw`) compared with `p_hit` (default 1). A hit is applied exactly as the
contacts process applies it (every contact inside the radius flagged, out of play unless its
scenario says `persists`); a miss touches nothing inside the radius. A platform whose catalog
says `expended_by_effect` (Switchblade, FPV) leaves the world at the effect with
`spend_expended` (default true): its stand-in stops stepping, sends nothing and answers nothing,
so its link goes silent; with `survives_miss` one whose effect missed flies on, and the bridge's
rules 15 alert (spent, still heard) follows once the engagement is over. Every effect, its draw,
whether it hit and whether the platform left are in truth's `effects` and the record's
`effects.applied` (evaluation only: nothing of it reaches the bridge's API or the MCP tools).
Without `effects` an effect is applied as the contacts process applies it and the platform
flies on, as before.

**The signers** (`Options.signers = {"preset": ..., ...}`). eval/proxy.py's
`EngagementSigners` on simulated time, its own `next_signer` deciding who signs next (never the
proposer, never twice, only while the engine's live picture passes): a look at the inbox every
`period_s` (3 s), and for each nomination the next signer looks, reads for `read_s` (3 s), reads
the nomination again and signs on that token only if the same signer would still sign; the next
signer follows at once. A signature the engine refuses (409: the plan shown no longer holds) is
recorded and the signer looks again. Draws from `random.Random("signers:<seed>")`.

| preset | what |
|---|---|
| `scripted` (default) | EngagementSigners exactly: every nomination whose live picture passes is signed |
| `deliberate` | `read_s` 20, `period_s` 10: the frozen plan must hold longer between the signatures |
| `cautious` | `decline_if` `accepted_lost`, `accepted_unknown`, `changed`: a reading signer declines a nomination that rests on a lost contact or a vehicle of ours accepted as lost, or whose checks are worse than at nomination, saying which (`POST .../reject`) |
| `decline` | the first eligible signer looks, reads and declines every nomination |
| `absent` | nobody answers: every nomination waits until it expires |
| `abort` | signs, then aborts every engagement still dispatched 20 s after its dispatch (`POST .../abort`): the fallback and the restore |
| `mode = "rates"` | each nomination's fate drawn once: signed with `p_sign`, declined with `p_decline`, else left to expire, the first look after a lognormal latency |

**The reviewer** (`Options.review = {"preset": ..., ...}`; operator `op-auto`, engagement
authority `op-2`). It looks every `attend_s` (5 s; lognormal with `attend_sigma`), reads each
item `read_s` (3 s) and acts once per item:

| item | presets that act on it | path |
|---|---|---|
| a rule declaration awaiting an operator | `confirm-declarations` (default) and `accept-all` confirm; `skeptic` revokes | `POST /api/tracks/{id}/confirm`, `.../revoke` |
| a review flag in force | `accept-all` confirms, `skeptic` dismisses; `flags = "declare"` declares the flagged track hostile | `POST /api/review/{flag}/resolve`, `POST /api/tracks/{id}/declare` |
| a lost contact a waiting nomination counts (`engage.lost_open`) | `accept-all` accepts it for that engagement (again after a reset of the signatures) | `POST /api/intents/{id}/accept_unknown` `{track, since}` |
| a live track carrying a lost contact's AIS | `accept-all` confirms it, only when the engine names no blocker | `POST /api/review/lost/{track}/confirm` `{by_track}` |
| a platform spent but still heard | `unspend = "unspend"` | `POST /api/restores/unspend` |

`absent` reviews nothing. Situations' `[autoplay] confirm_rule_declarations` and `grant_agents`
(eval/proxy.py) are not read by the headless operator (section 7); the reviewer's default
preset does the first. The approve-all operator leaves engagement nominations to the signers (a
plain approve is refused for one).

**The scripted agent** (`agent = "scripted"`, `Options.scripted = {"engage": {...},
"collect": {...}, "cover": {...}, "period_s"}`): every `period_s` (5 s) from the AI's start, it
nominates on each track declared hostile when no nomination on it stands and the form's dry run
passes a platform (`effect_radius_m` 75, `window_s` 600, `fallback` return, `uids` to limit it,
at most `max_nominations` 3 per track). It is a stand-in so the chain runs end to end, not an
agent under evaluation, and v1's metrics do not judge engagements, collects or covers (H7).

### 16.2 Space on the simulated clock

**The loader.** The bridge's space segment loads at the episode's start inside the
simulation's runtime (`sim_loader`, section 2.1), so its pinned time shift and the engine's
`space.loaded` are the episode's, never a real-time moment's; the 30 s look at the element sets
runs on the simulated clock and the answer cache ages on it. Every space answer (passes,
tasking options, a collect's or a cover's query) is computed off the runtime as live
(`spawn_blocking`) at the simulated instant it is asked: the paused clock does not move while it
runs, so a collect's answer is computed at its check's own time and rules 13's `computed_t`
equals it (the 5 s bound holds trivially); a cover is prepared outside the lock as live
(`Decision::lock_with_space`). Rules 11's re-check after a reload never fires on a pinned
snapshot (nothing changes on disk), and a cancel goes through `POST /api/intents/{id}/cancel`
as live.

**The pinned element sets.** `sim/headless/space/<world>/` (golden-gate today): the catalog and
named areas as committed when it was made, every element-set file the catalog's queries read,
gzipped, beside its own `.meta.json`, and `provenance.json` (each file's SHA-256 and size
uncompressed, the element sets' source and fetch times, how and when it was made, and its `id`:
one SHA-256 over every file's name and digest). Golden Gate's: CelesTrak GP JSON imported into
`sim/space/cache` with `murmur-space import` on 2026-10-02 (11,451 satellites in 11 classes;
the June scenario is time-shifted +109 days, crates/murmur-space README "Epoch honesty"), id
`970ff616e5fbd4cc...`, about 0.7 MB in git. `eval/headless/space.py` unpacks it once per id, every
digest checked, to `logs/headless/space-snapshots/<world>-<id>/` (`sim/space/...` and a link to
this checkout's `sim/platforms.toml`), and an episode with `space_snapshot` points both the
bridge (`MURMUR_ROOT`) and the emulator's SATCOM (`space_root`, with its own look cache) at it,
so a space episode depends on the commit alone. Without it the emulator reads `sim/space/cache`
as before (an untracked input, hashed in the record's provenance). To pin another world's:

    .venv/bin/python -m eval.headless.space snapshot --world <name> --cache sim/space/cache \
        --note "<where the element sets came from>"
    .venv/bin/python -m eval.headless.space verify --world <name>

**The tools and the tasks.** `get_satellite_passes` and `get_satellite_tasking_options` answer
headless as live (the bridge's own `/api/space/*` routes through its MCP server). The scripted
agent's `collect` (`at_s`, `area`, `capabilities`, `window_s`, `rank`, `min_lead_s`) reads the
passes and the ranked options and proposes a collect with the first option at least
`min_lead_s` ahead; it waits for a human (the `satellite-collect` tier), the operator decides it
like any card, and the bridge's simulated executor runs it on the scenario clock (`tasked`,
`collected`, `product_ready`, then done). Its `cover` (`area`, `window_s`, `service`,
`min_level`) runs on its own (`coverage-query`): `watching`, the predicted outages or the
fewest in view, `ended`.

**Replay.** `murmur-replay <log> --anchors <log>.anchors` re-decides every collect and cover
from the logged answers; with `--space <root>/sim/space/constellations.toml
<root>/sim/space/cache` (the unpacked snapshot) it re-runs every query on the same segment.

### 16.3 The smoke episodes (seed 1, 2026-10-23)

**Engagement end to end** (`hostile-act-engagement`, the scripted agent, the `scripted`
signers, `confirm-declarations`, `p_hit` 1; about 1.5 s of computer time for 720 s): the boat's
first act at 205.8 s, observed by HUNTER 1's radar, EO/IR and sonar and REAPER 1's radar and
EO/IR, declared hostile by rule at 206.0 s (attribution 0.98); `op-auto` confirmed it at 211.0 s
(read from 208.0 s). At 206.2 s the form showed BLADE 1 not passing (target uncertainty); at
211.2 s it passed and the agent nominated it. `op-2` looked at 216 and 219 s and was refused both
times (409: the plan shown no longer held, the boat's velocity estimate still settling after it
stopped), signed at 225 s; `op-3` signed at 228 s and BLADE 1 was dispatched. The preload's
write-ahead at 229 s and its set at 230 s (BLADE 1's own values are the `return` preload:
`FS_GCS_ENABL` 1, `FS_LONG_ACTN` 1, `FS_LONG_TIMEOUT` 5), verifies every 6 s from 311 s; the
effect released at 351.0 s, its debt closed at once (`restore.closed`, expended); the draw hit,
the boat left the picture and BLADE 1 the world; `engage.assessed` ceased at 367.0 s (the point
covered 15 s by HUNTER 1's and REAPER 1's radar). 237 events; `eval/metrics.py`'s invariants,
the two-person rule among them, all zero; replay: every decision reproduced exactly, anchors OK.

**Abort** (`abort` signers): `op-2` aborted 21 s after the dispatch: `engage.stopped` (cause
operator, fallback return), then `engage.preload` restore, confirmed. **Miss** (`p_hit` 0,
`survives_miss`, the reviewer's `unspend`): the boat still seen 3.8 s after the effect,
`engage.assessed` persists; BLADE 1 flew on, `op-2` un-spent it at 359 s (`platform.unspent`)
and its own values went back at 360 s (`platform.restore`, confirmed). **A lost contact and its
AIS claim** (the test adds to the scenario a trawler with AIS that goes dark at 5 s near the
holding area and the same AIS back at 240 s; the reviewer's `accept-all`): its track dropped at
186 s with its AIS (`track.dropped`), and from the declaration's confirmation at 211 s the form
named it blocking BLADE 1's keep-out (`lost_contact:T-0008`); the new track claimed it at 241 s
(`lost.claimed`), `op-auto` confirmed the claim at 249 s (`track.reacquired`), the lost contact
retired, and once the claimant's own young track settled the agent nominated at 286 s and the
chain ran on to the effect.

**Collect** (`harbor-watch` for 9,000 s, the snapshot; about 20 s): 15 imaging windows; three
taskable options, all Capella SAR (03:00 local: no optical scene in the dark); the agent chose
CAPELLA-20 (NORAD 68485) for 11:25:23Z and proposed it at 61.2 s; `op-auto` read and approved it
at 70.0 s; tasked, collected at 5,124 s, product ready at 8,724 s, done. Replay with `--space`:
three queries recomputed, identical. **Cover** (GPS over a 5 km circle at the bridge for 600
s): auto at 61.2 s, watching (9 to 10 satellites at the worst of 9 sample points, PDOP at most
1.67), fewest in view 9 at 10:06:01Z, ended at 10:11:01Z, done. Every one replays exactly, and the
engagement and the cover, each run twice on one seed, gave the same record twice.

### 16.4 Not done, or found

- **Boat and UGV engagements** (`magura`, `ugv_armed`) are refused headless until the land and
  water grid lands (H1): the domain rule answers "not evaluated", which blocks a consequential
  engagement. Nothing here depends on the domain: the effect, the signers, the reviewer and the
  agent run the same for them once it answers. Not tested yet. (The grid has since landed:
  a situation with `[headless] domain_rule = "grid"` gets its world's, section 17.2; Harbor
  watch and every v1 and v2 situation keep "not evaluated".)
- **The preload writes what was there** for BLADE 1 (its own lost-link values are the `return`
  preload), so the set and the restore exercise the path, not a change of values; an FPV with
  fallback `abort` (ArduCopter `FS_GCS_ENABLE` 7) would change them.
- **No review flag, and no lost contact to accept for a waiting nomination,** arises in a smoke
  episode: the reviewer's work on them is tested on a fake operator API. (A lost contact blocks
  the form, so an agent cannot nominate past it; only one dropped while a nomination waits
  unsigned can be accepted for it, a timing the families must make.) Situations that raise them
  are the families' work (F15, F16).
- **A silent platform still senses for a few seconds**: the bridge's sensing model keeps a
  vehicle's sensors on until its link goes stale (`tracks::observers`), so a munition that left
  at the effect may still report the target for its stale age (seen in the miss: `3:eo_ir` at
  +3.8 s). That is the sensor model's (A1's), not changed here.
- The engagement, collect and cover outcomes are not yet labels or metrics (H7, C1).

## 17. World v3: worlds, land and water, faults and timelines

Built 2026-10-23 for world v3's H1, H4 and H5 (docs/WORLD-V3-PLAN.md section 4; bd
murmur-1z6.2). **Every v1 and v2 situation runs and records exactly as before**: Harbor watch
pins the simulator as it was (`[headless] domain_rule = "not_evaluated"`, `currents = "none"`),
every family is built on it, and a spec that says nothing of world v3 has none of its keys.
Shown on 2026-10-23: the golden episode's digest, the null fidelity test and the charged clock's
byte-identity pass unchanged; and 144 episodes run at the commit before this work and after it
(Harbor watch and the seven v1 families on seeds 1-4 with the playbook and `none`; the ten v2
situation kinds on seeds 1-2 with the playbook, the oracle, `null` and `none`, scoring v2) gave
the same record and the same audit log, episode by episode (the build commit, the change feed's
per-process nonce and the log's own hashes masked, as the golden masks them).

### 17.1 Worlds (H5)

A world is a file per map, `sim/worlds/<name>.toml` (format: `sim/worlds/README.md`; read by
`eval/world.py`), naming what was the Golden Gate's alone in the code: the default fleet, the
operator's areas (a directory, the bridge's `MURMUR_AREAS`), the tidewater scenario and start,
the baked grids and the box and spacing they were baked on, the current field, the links file
and the ground station, named reference points, the operating box, the mission's circle (the
labels' centre, radius and grace) and the ranges a situation's conditions are drawn from (time,
visibility, wind, sea state: `World.draw_conditions`, recorded, its wind in our vehicles' water;
nothing else reads them before the sensor model of world v3's A1). `golden-gate.toml` holds the
constants it replaced, unchanged (tested). A situation names its world (`world = "golden-gate"`,
the default) and takes its fleet, `at` and tidewater scenario from it unless it names its own.

What the simulator takes from it (`situations.py` `process_spec` and `world_spec`): the surface
grid for the emulator's terrain; the links file, written into the episode's directory with the
world's ground station when the file's differs (the link emulator has one station: a world
naming two is refused); `MURMUR_AREAS` when its areas are not `sim/areas` (a world's own areas
go in a directory of their own: every Golden Gate stack loads all of `sim/areas`). Labels read
the mission from the record's world (`labels.py` `mission_of`; a record without one is the
Golden Gate's). The families, v2's labels and the oracle read the Golden Gate world's points,
mission and operating box. A world v3 record carries `world` (its name, domain rule, currents,
conditions); before world v3 there is no such key.

### 17.2 Land and water (H1)

The bridge's domain rule reads a baked land/water grid when no tidewater URL is set
(`MURMUR_LAND_WATER_GRID`; `crates/murmur-bridge/src/decision/terrain.rs` `Terrain::with_grid`):
tidewater's water depth (water level minus elevation) on the world's 20 m grid at its bake time.
A waypoint is the depth there, bilinear between the four samples around it (water above 0, land
otherwise, outside coverage next to a sample without data, as tidewater outside its DEM); an
engagement's effect point is the nearest grid sample within `TERRAIN_NEAR_M`, refused when the
samples that near disagree (a shoreline), as with tidewater's cache. The engine logs what it
read, so replay is unchanged. A situation's `[headless] domain_rule = "grid"` (the default)
gives the simulator's bridge its world's grid; a boat's route over Crissy Field then fails
("is not water; seahunter needs water"), over the channel passes, and a UGV's the other way
round (tested). The tide stays at the bake (Golden Gate: 10:00Z on 14 June, 0.44 m).

### 17.3 The bake tool

`eval/headless/bake_world.py <world> --url <tidewater serve>` bakes a world's surface grid,
land/water grid (one pass at its `sampled_at`) and current field (hourly over its window) from
the world's tidewater scenario, each with a provenance file, on the world's box (without one,
the scenario's own bounds), and prints the wind's range at the mission's centre for the world's
conditions. Rebuilt on 2026-10-23 from tidewater 3a531c9, the Golden Gate's surface grid came out
byte for byte the grid baked on 2026-10-09 (SHA-256 9153e1a4...), so it and its provenance stay
as they were; its land/water grid (583 KB) and current field (325 KB, 50 x 65 points, 73 hours,
fastest 2.29 m/s) are new; the wind at the bridge over the window, 2.98 to 7.76 m/s, is the
world's `wind_mps`.

### 17.4 Faults and timelines (H4)

**The timeline.** A situation's `[[step]]`s run in the simulator (`situations.py` `timeline`,
`script.py` `step` items) as the live storyline runs them, on the simulated clock: `propose`
(vehicles as the C2 lists them; a centre that is a place, a track as the C2 shows it, or a
vehicle; tried every 2 s for `retry_s`; `agent-scripted` over MCP), `flood`, `link` (the
emulator's own event), `declare` (op-planner). A `note` is the panel's and is left out (Harbor
watch's two notes leave its script empty, as before); an `engage` step is recorded as skipped
(the headless engagement chain is world v3's H2); the `[storyline] proposer = "operator"` is not
in the simulator yet (recorded as an error).

**The faults** (situation step kinds the live runner refuses; `crates/murmur-headless/src/sim/
faults.rs` says what each is modelled on): `gnss` (a jammer: no fix inside its footprint by J/S,
the navigation filter loses its position and each firmware's EKF failsafe acts, Rover holding,
the C2 seeing no fix and no position source; a spoofer: the believed position walks off the
truth with the flags good; both the stand-ins' own GNSS model, which also reads the events
files' `gnss` tables now, as the live engine reads `MURMUR_EVENTS`), `battery` (a share of the
pack gone at once, or the draw times a rate), `reboot` (the autopilot silent for its boot,
default 3 s, then a fresh one: time since boot from 0, stream rates, failsafes and filter new,
parameters, stored mission and pack kept; on the surface disarmed in its boot mode, in the air
ArduPilot's watchdog restore; the bridge logs `vehicle.rebooted` once a message with the time
since boot comes, about 10 to 30 s later), `stop` (as the families' stuck vehicle), `current`
(a uniform current in place of the world's for a while). A fault's vehicles are selected in the
fleet as planned, heard or not. `sim/situations/fault-drill.toml` runs each once; its smoke
episode and one test per fault are `eval/headless/tests/test_world_v3.py`; ten seeds with the
playbook and with `none` ran every step without an error.

**The water.** `[headless] currents = "world"` (the default) gives the episode its world's
water (`water.rs`): contacts step with the current at their position (only a drifting one moves
with it, as with tidewater), and our vehicles that take conditions (boats, subs, the drifter, a
UGV, a sailboat, a blimp) get a tidewater-shaped sample about once a second, as the stand-in
engine's poller gives them: elevation and water level from the surface and land/water grids
(the depth under a keel, the ground under a UGV), the current from the field (bilinear in space,
linear in time, held outside its window; surface current only), the drawn wind. Without water a
contact steps with no current, as before.

**What is left out.** The pack's used mAh is carried through a reboot (ArduPilot's monitor
counts from 0 again and would report a full pack); a cell lost shows as used charge, not as a
voltage sag; the tide is held at the grids' bake; the current is the surface's at every depth;
the link emulator has one ground station; the GNSS sky stays `Sky::LEGACY` (no constellation
geometry in the simulator, as before).

    .venv/bin/python -m eval.headless.batch --situation fault-drill --seeds 1-10 --agent playbook none
    .venv/bin/python -m eval.headless.bake_world golden-gate --url http://127.0.0.1:9570 --out /tmp/rebake
