# Murmur

Open-source C2 testbed: one operator supervising ~100 autonomous air, ground, surface and
subsurface nodes, built to measure whether human judgment over consequential decisions stays
meaningful. Decision and command, not weapons.

## Start of a session

1. Read `docs/PLAN.md` (the plan, phase by phase) and `docs/DECISIONS.md` (what was decided
   and why; don't relitigate those).
2. `bd ready` for open work (see AGENTS.md for bd usage). Done: R (Rust core), C (intents, rules,
   policy, audit, inbox), C2 (contacts, sensing, tracks), F (MCP for agents). Current: **E**
   (engagement decision chain) and **G** (evaluation harness).
3. Sibling repo `~/tidewater` (real-world conditions). ArduPilot at `~/ardupilot`, branch
   `tidewater-water-depth`.

## Run and verify

- `make goldengate` runs the default fleet (one of each of 22 platform types) at the Golden
  Gate with tidewater conditions; `make up FLEET=<name>` for other fleets; `make down` stops.
  Console http://127.0.0.1:5173, bridge http://127.0.0.1:8765 (`/api/state`, `/api/tasks`,
  `/api/platforms`, `/api/stats`; MCP server for agents at `/mcp`, header `X-Murmur-Agent`,
  ids in `MURMUR_AGENTS`, or `Authorization: Bearer <key>` in a credentials run;
  `agents/README.md`). Vehicles ready in ~30-60 s. The scenario clock is `MURMUR_AT` plus
  elapsed x `MURMUR_RATE`; a rate other than 1 without `MURMUR_AT` is refused by every process
  (murmur-drd: there is no shared start).
- A run is **dev** by default: header ids, the console's "Acting as" switcher, a red DEV pill,
  logged `auth: dev`, never evidence about people, no credential file needed.
  `MURMUR_AUTH=credentials`: operators sign in with their own password (create once with
  `target/release/murmur-auth add op-1 --engage`, password on stdin; file
  `~/.config/murmur/credentials.json` or `MURMUR_CREDENTIALS`), agents use keys
  (`murmur-auth agent-add`), and every engagement signature re-enters the password. Study mode
  always uses credentials and marks its scripted operators. Replay a credentials log with
  `--anchors`, or it is not evidence.
- Passkeys (rules 10, `PASSKEY_SINCE`, policy schema 7): the console must be opened at
  http://localhost:<MURMUR_WEB_PORT> (RP id `localhost`; 127.0.0.1 gets a pointer and password
  only; `MURMUR_PASSKEY_RP_ID` for a real domain). Register from the operator menu (password
  re-entered) or by `murmur-auth passkey-invite <op>` (one-time `#enroll=` link); also
  `murmur-auth passkeys`, `passkey-remove`, `import`. `[approval] step_up_factor` and
  `login_factor` in the policy. A study (`MURMUR_STUDY=<participant>`; eval `eval.run enroll`,
  `eval.run study --credentials`) refuses to start without the participant's passkey;
  `MURMUR_STUDY_PASSWORD_STEP_UP=<why>` overrides, logged, and the run is not evidence. Each
  passkey challenge is derived from its act, single use, 120 s; replay re-verifies every
  assertion from the log.
- Live path is Rust: `crates/murmur-bridge`, `crates/murmur-standins`. `scripts/up.sh` builds
  their release binaries (`make build` does too) and, with a scenario, starts `tidewater serve`
  (reused if one answers) and `tidewater drive`, and stops them with the stack. It writes
  this run's `sim/run/fleet.json` (`swarm.py --manifest-only`) before the emulator and the
  bridge start, in both link modes; the bridge logs which manifest it read and its age
  (flagged past a minute, a previous run's).
- `make check` (typecheck, lint, py_compile, `cargo clippy` and `cargo test` for the workspace;
  toolchain in `~/.cargo/bin`). Tidewater: `cd ~/tidewater && .venv/bin/pytest`.
- `scripts/measure.py` measures bridge and stand-in load; `scripts/scale_fleet.py N` writes an
  N-node stand-in fleet. Logs in `logs/`, SITL and stand-in logs in `sim/run/`.
- Links go through `crates/murmur-linkemu` by default (`MURMUR_LINKEMU=0` bypasses):
  profiles `sim/links.toml`, events `make goldengate EVENTS=sim/link-events/golden-gate.toml`,
  log `logs/linkemu.log`, PID `logs/linkemu.pid`; the bridge reads `sim/run/bridge-fleet.json`
  and serves the emulator's view at `/api/links[/{uid}]`.
- Ports: vehicles UDP 14560+i -> linkemu -> bridge UDP 15560+i (`MURMUR_BRIDGE_BASE_PORT`,
  default base + 1000); linkemu stats HTTP :8766 (`MURMUR_LINKEMU_PORT`); bridge :8765; console
  :5173; tidewater serve :8770; SITL SERIAL1 TCP 5762+10*instance (tidewater driver). A second
  stack beside the default one: set `MURMUR_BASE_PORT`, `MURMUR_BRIDGE_BASE_PORT`,
  `MURMUR_LINKEMU_PORT`, `MURMUR_HTTP_PORT`, `MURMUR_WEB_PORT`, `MURMUR_TIDEWATER_PORT`,
  `MURMUR_SITL_INSTANCE_BASE` (header of `scripts/up.sh`). `MURMUR_WEB_PORT` also tells the
  bridge which console origin to accept; the session cookie is named per HTTP port.

## Gotchas (each cost real time)

- **Never `pkill -f <pattern>` with a pattern that appears in your own command line**: it kills
  your shell (exit 144). Use `make down` (PID file `logs/up.pid`; it sends TERM, because an
  `up.sh` started in the background ignores SIGINT).
- Background tasks in this harness stop after 2 hours: restart with `make goldengate`, or start it
  detached (`setsid nohup make goldengate > logs/goldengate.out 2>&1 &`) or in your own terminal.
- Foreground `sleep` is blocked: wait with `until <check>; do sleep 2; done`.
- Global git `core.autocrlf=true` (WSL): repos rely on `.gitattributes` (LF). A fresh ArduPilot
  clone needs `-c core.autocrlf=false` or its python shebangs break.
- The Playwright browser renders WebGL in **software** (SwiftShader): 3D fps there is
  meaningless (use `?perf` on a real GPU). After many reloads it loses the WebGL context:
  `browser_close` and reopen.
- Dev-only globals for debugging the console: `window.murmurMap`, `murmurBridges`, `murmurPerf`.
- MapLibre is pinned to **v5** (deck.gl 9.4 breaks on v6). Don't upgrade.
- After changing SITL code or its param table, rebuild all firmwares:
  `cd ~/ardupilot && PATH=~/murmur/.venv/bin:$PATH ./waf plane copter rover sub blimp antennatracker`.
- ArduPilot stream-rate params are `MAV1_*` here (not `SR0_*`). Pass SITL one defaults file, not a
  comma list: ArduPilot drops `MAV1_*` from an earlier file in the list (murmur-yjq; `swarm.py` merges).
- In SITL, stop vehicles with forces the IMU feels, never by zeroing velocity or clamping
  position (the EKF diverges).
- tidewater is installed editable in `.venv`; a scenario needs `tidewater fetch` once.
- `murmur-replay` accepts only the current decision-rules version by default: replay older logs with
  `--rules N` / `--min-rules N` (versions and changelog in `crates/murmur-decide/README.md`). The bridge
  writes run-time anchors to `logs/anchors/<run>.anchors`; verify with `murmur-replay <log> --anchors <file>`.
  Engagements need two distinct signers with engagement authority: in a dev run
  `MURMUR_ENGAGE_OPERATORS` (each also in `MURMUR_OPERATORS`, or the bridge refuses to start; both
  lists are logged as the run's roster); in a credentials run the credential file's engagement flag
  (`murmur-auth engage <id> on`). Scenarios: `murmur-scenarios` (`sim/scenarios/engage/`).
- Stand-ins each take one ephemeral UDP port (`net.ipv4.ip_local_port_range`, ~4,000 here,
  shared with everything else on the host): that, not CPU, caps a stand-in fleet near 4,000.
- A stack uses two UDP ranges now (vehicle base + N and bridge base + N): pick both clear of
  every other stack's ranges, including a large load test's (a 3,000-node test spans 3,000
  ports from its base). A bridge that finds a port taken drops it for good.
- Keep fixed port bases (vehicle, bridge) below the ephemeral range: stand-ins and clients draw
  from it, and a fixed base inside it collides at random. It can change between boots: check
  `sysctl net.ipv4.ip_local_port_range` (37795-41890 as of 2026-10-04; was 39160-43255).
- Eval stacks own UDP 35000-36999 and HTTP 10600-10699 (`eval/ports.py`, fixed): keep other
  live stacks off them. Each eval run starts as a fresh station (`eval/stack.py` removes the
  bridge's `.last-run-<port>` before and after; `run.json` `restore_carry`, murmur-kvb).
- Rules 5 queue: an order for a vehicle the link can't carry it to now waits in the Queue
  (console button; `GET /api/inbox` `queued`), it is not an error; one whose link drops
  before the vehicle started it comes back there (`intent.requeued`). A human's approval is
  held 120 s at most (`[queue] human_expire_s`), then back to the inbox. An order whose start
  went out unconfirmed (op `unconfirmed`) may be running: it is only followed if the vehicle
  runs it, else a human decides; a vehicle that runs one no longer in force raises a
  `may_be_running` alert. Open intents carry into the next run only after a crash (the
  pointer `logs/audit/.resume-<port>.json` that a clean stop, `make down`, deletes; within 15
  min) or with `MURMUR_RESUME=<log>`, onto the same vehicles (platform, callsign, home within
  200 m, from `<log>.fleet.json`) and a log matching its anchors; without those files nothing
  carries unless `MURMUR_RESUME_UNVERIFIED=1`. Replay such a log with its predecessor beside
  it (or `murmur-replay --prev`). `MURMUR_RESUME=0` turns it off.
- Rules 6 engagements preload the approval's fallback as the platform's own lost-link action
  and owe its own values back afterwards (`engage.preload`, inbox alert `restore_pending`).
  `logs/audit/.last-run-<http port>` names the last run's log: at start the bridge carries
  the values that log still owed (`platform.restore`) onto the same vehicle only. Unlike
  `.resume-<port>.json` it survives a clean stop and is rewritten at every start, so the two
  can name different logs. An engagement itself never carries over or queues.
- Rules 7 (`QUEUE_FOLLOWUPS_SINCE` in `crates/murmur-decide/src/rules.rs`; feature gates use
  named constants, never literal versions, so a renumber is one line): a vehicle start, or a
  follow of an order it already runs, is logged before it goes out (`op.state` with
  `ahead`). A mission for a platform still owed its own lost-link values waits in the Queue
  (or is refused, saying why, for a kind the policy never queues); a hold or rtl goes at
  once, and the restore goes the moment the platform is heard. An order queued, dispatched
  or refused for an engaging platform supersedes the engagement (no fallback).
  `murmur-scenarios --rules 6` shows what rules 6 did. Live track ids follow detection order:
  a driver should find a track by its identity, not its id.
- Rules 9 (`RULE_ID_SINCE`, policy schema 6): the engine declares hostile by rule on an
  observed hostile act against ours clearly attributed to one track, and flags unsure cases
  for review; effect release and accepting a lost friendly stay human. `[identification]` in
  `sim/policy.toml` (`rule_declaration` on, `nominate_unconfirmed` off, `corroboration`);
  acts are `[[act]]` in contacts scenarios; review at `GET /api/review`. Live demo:
  `make goldengate GG_FLEET=engage-demo CONTACTS=sim/scenarios/golden-gate-ident.toml` (the
  Makefile sets `MURMUR_CONTACTS` from `CONTACTS=`; an exported `MURMUR_CONTACTS` is ignored).
  Eval: `.venv/bin/python -m eval.run ident --seeds 1 2 3 4 5 6 7 8 9 10 --situations 20
  --untracked-share 0.2 0.5` (model results: read METRICS.md's caveats before quoting).
- `POST /api/sim/contacts` needs `X-Murmur-Sim-Token` equal to the bridge's
  `MURMUR_SIM_TOKEN`; `up.sh` and eval give it to the bridge and contacts only. To inject
  truth by hand, set it in both.
- Rules 11 (`SPACE_FOLLOWUPS_SINCE`): a dispatched collect or cover can be cancelled
  (`POST /api/intents/{id}/cancel`; the engine logs what it meant: a pass under way cannot
  be recalled). The bridge reloads `sim/space/cache` when it changes (every 30 s, keeping the
  session's pin) and re-checks every running space task outside the lock (`rechecked`, or
  `missed` with fresh options and an inbox alert; a collect whose pass has come is never
  re-checked). A cover is judged at the centroid and boundary samples (the least covered
  point); the engine never computes a cover itself (`Decision::lock_with_space` prepares it
  outside the lock), so long covers don't hold the lock.
- Rules 12 (`REVIEW_FOLLOWUPS_SINCE`): a start records what it carried in `run.started`
  (`carried_from`: each earlier log, its head, what it owed); replay finds those logs beside
  the log or by `--prev`. A `.last-run-<port>` that is empty, garbled or unreadable is a
  failed carry (alerted), never a failed start; the pointer is rewritten whole at every start.
  A failed carry holds missions for the armed platforms its `<log stem>.fleet.json` names
  until read or written off (`POST /api/restores/write_off` with a note, or the console's
  Write off). Carried lost-link values hold a mission on the vehicle they were preloaded on:
  only platform, callsign or sysid say another vehicle under a uid, never its home. A
  platform spent by its own effect (catalog `expended_by_effect`) takes no order or
  nomination and its values are closed (`restore.closed`). A track with no mean velocity yet
  counts at its domain's top speed in keep-outs. A credentials run fixes scripted operators
  at start. To crash a bridge for a carry test: `kill -9` the PID that is the run id's
  suffix in `logs/audit/<run-id>.jsonl`, never `pkill -f`.
- Rules 13 (`SPACE_REPLAY_SINCE`): every decision on a space answer logs when it was computed
  (`computed_t`, engine time); a collect's answer must be within 5 s of its check and asked at
  that time (the `space_query` rule, so replay recomputes it; `PREPARED_SPACE_MAX_AGE_MS`); a
  re-check is logged at the clock read its "has the pass come" guard made; the engine refuses
  a segment pinned to another time shift mid-run (`EngineError::SpaceSegment`) and replay
  flags a pin change between two `space.loaded`.
- Keep-out explanation (murmur-12e; read-only, not logged, no rules change): the nomination
  form and `POST /api/engage/options` (`keep_outs`) say which track or vehicle blocks, which
  region, the shortfall, and what would clear it ("within X m", "between X and Y m", or a wait
  "if it keeps behaving as now"; scanned on a 0.25 s grid); agents read it with
  `get_nomination_options` (one call a second). It shares the checks' own per-item judgement
  (`engage::track_standing`, `friendly_standing`, `circle_holding`): changing those changes
  decisions. The dry run fills what the engine fills (`Engine::dry_run_inputs`). More
  pictures: `MURMUR_KO_SEED=N MURMUR_KO_WORLDS=1500 cargo test -p murmur-decide --test keepout`.
- Keep-out measurement: `.venv/bin/python -m eval.run keepout --seeds 1 2 3 4 5 6 7 8 9 10
  --situations 100` (every variant, ~16 min; `--variants radar all` for less). `eval/keepout_probe`
  is a workspace member that compiles the bridge's `tracks/{model,sensing,truth}.rs` by path, so
  `make check` lints and tests it with them. Model results: read METRICS.md "Keep-outs against
  engagements" before quoting (radar alone is the pessimistic sensor case; the air tracker's
  constants are unvalidated).
- The tracker is a two-model IMM; changes must pass `tracks::tests::calib`. Its held-out seeds
  (66-95) are not for tuning: write a new held-out set if you tune on them.
- This host's wall clock (WSL) stepped back 0.5 to 0.9 s every ~32 s for the first two hours or
  so after boot (monotonic up to 2 % fast against Windows), then stopped. The bridge times
  everything on `clock::wall` (the monotonic rate, steered within ~2 s of the wall clock at
  5 % at most, never back; the engine reads it as `clock::BridgeClock`; anchors record UTC
  beside it); `clock::utc` only for dates another clock reads (CoT, the resume pointer,
  credential-file times); `clock::mono` for durations. The tracker dates fixes by the truth
  message's clock (`Detection::measured_t`). Tests: `clock::test_clock` (`step_utc`, `pin`,
  `pin_drifting`, `restart`; per thread). Stepped stand-in tests read `Link::capture`; test
  vehicles use `Vehicle::seeded` (murmur-dup, murmur-ek3).
- The bridge's task-layer timeouts follow each link's measured round trip (`LinkTiming` in
  `crates/murmur-bridge/src/link.rs`; floors 1 s per ACK, 1.5 s per mission item, as before on
  fast links) and its telemetry plan follows the link class (`crates/murmur-bridge/src/
  telemetry.rs`: HIGH_LATENCY2 every 5 s on Iridium-class links). A task to an AUV under water
  still crosses a 300 bps modem: expect tens of seconds per exchange, and ArduPilot's own 8 s
  mission-upload timeout makes uploads there unlikely to finish. `MURMUR_TELEMETRY=uniform`
  restores the one-plan-for-all for A/B runs.

## Conventions

- bd for all task tracking; the pre-commit hook exports issues to `.beads/issues.jsonl`.
- Commit and push to the local remote (`~/git/murmur.git`) when work lands.
- No AI attribution in commits; no em dashes in prose (global CLAUDE.md).
