# Murmur

**Using or trying Murmur rather than changing its code?** Follow "Helping someone use Murmur"
in AGENTS.md (loaded here) and skip the rest of this file, which is the developer's notebook.

@AGENTS.md

Open-source C2 testbed: one operator supervising ~100 autonomous air, ground, surface and
subsurface nodes, built to measure whether human judgment over consequential decisions stays
meaningful.

## Start of a session

1. Read `docs/PLAN.md` (the plan, phase by phase) and `docs/DECISIONS.md` (what was decided
   and why; don't relitigate those).
2. `bd ready` for open work (see AGENTS.md for bd usage). Done: R (Rust core), C (intents, rules,
   policy, audit, inbox), C2 (contacts, sensing, tracks), F (MCP for agents). Current: **E**
   (engagement decision chain) and **G** (evaluation harness).
3. `tidewater/` (real-world conditions; its own Python package, own README). ArduPilot at
   `~/ardupilot`, branch `tidewater-water-depth`: the commit `scripts/setup.sh` pins plus the
   patches `scripts/ardupilot-patches.sh` applies in order (`sim/patches/ardupilot/`,
   `tidewater/patches/ardupilot/`).

## Run and verify

- `make start` is how a person opens Murmur: the launcher (`eval/launcher.py`, API :8760, bridge
  relay :8761) serves the console at :5173 with a main menu and runs one scenario at a time: the
  open world (`make goldengate` itself, default ports) or a situation (`eval.situation_run`, its
  own block), each with `MURMUR_WEB=0` (up.sh starts no console); End is `make down`'s.
- `make goldengate` runs the default fleet (one of each of 22 platform types) at the Golden
  Gate with tidewater conditions; `make up FLEET=<name>` for other fleets; `make down` stops.
  `make situation NAME=harbor-watch [OPERATOR=live|autoplay] [AI=claude|codex|playbook|none]`
  runs a pre-built situation (`sim/situations/README.md`): fresh station, opening orders, the AI
  through the user's own CLI (never their credentials), a Situation panel; it refuses ports a
  stack answers on, and leaves its stack up for `make down`.
  Console http://127.0.0.1:5173, bridge http://127.0.0.1:8765 (`/api/state`, `/api/tasks`,
  `/api/platforms`, `/api/stats`; MCP server for agents at `/mcp`, header `X-Murmur-Agent`,
  ids in `MURMUR_AGENTS`, or `Authorization: Bearer <key>` in a credentials run;
  `agents/README.md`). Vehicles ready in ~30-60 s. The scenario clock is `MURMUR_AT` plus
  elapsed x `MURMUR_RATE`, read by `murmur_space::scenario_clock` in every process:
  `MURMUR_AT` is RFC 3339 only (`2026-06-14T10:00:00Z`; the short `10:00Z` is refused), and a
  rate other than 1 without `MURMUR_AT` is refused (murmur-drd: there is no shared start).
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
  assertion from the log. A login's options are the same shape for every id (decoys drawn like
  real credentials under `<credential file>.decoy-key`, made once beside the file; a key file
  that can't be read stops the bridge: remove it to make a new one), at most four live per id,
  a request past that served the newest again (murmur-i58).
- Live path is Rust: `crates/murmur-bridge`, `crates/murmur-standins`. `scripts/up.sh` builds
  their release binaries (`make build` does too) and, with a scenario, starts `tidewater serve`
  (reused if one answers) and `tidewater drive`, and stops them with the stack. It writes
  this run's `sim/run/fleet.json` (`swarm.py --manifest-only`) before the emulator and the
  bridge start, in both link modes; the bridge logs which manifest it read and its age
  (flagged past a minute, a previous run's).
- `make check` (typecheck, lint, py_compile, `cargo clippy` and `cargo test` for the workspace,
  tidewater's tests; toolchain in `~/.cargo/bin`). Tidewater alone:
  `.venv/bin/python -m pytest -q tidewater/tests`.
- `scripts/measure.py` measures bridge and stand-in load; `scripts/scale_fleet.py N` writes an
  N-node stand-in fleet. Logs in `logs/`, SITL and stand-in logs in `sim/run/`.
- Links go through `crates/murmur-linkemu` by default (`MURMUR_LINKEMU=0` bypasses):
  profiles `sim/links.toml`, events `make goldengate EVENTS=sim/link-events/golden-gate.toml`,
  log `logs/linkemu.log`, PID `logs/linkemu.pid`; the bridge reads `sim/run/bridge-fleet.json`
  and serves the emulator's view at `/api/links[/{uid}]`. Its random draws (each vehicle and
  direction its own stream) come from the links file's `seed` (1), so every run of a situation
  meets the same draws unless `MURMUR_LINKEMU_SEED=<n>` says otherwise (`up.sh` passes it as
  `--seed`); the headless simulator runs each episode's emulator on its episode seed, and
  `eval/headless/fullstack.py` passes the same seed to the full stack it compares with
  (`docs/HEADLESS.md`).
- Ports: vehicles UDP 14560+i -> linkemu -> bridge UDP 15560+i (`MURMUR_BRIDGE_BASE_PORT`,
  default base + 1000); linkemu stats HTTP :8766 (`MURMUR_LINKEMU_PORT`); bridge :8765; console
  :5173; tidewater serve :8770; the contacts process's API :8767 (`MURMUR_CONTACTS_PORT`, with a
  contacts scenario); SITL SERIAL1 TCP 5762+10*instance (tidewater driver). A second
  stack beside the default one: set `MURMUR_BASE_PORT`, `MURMUR_BRIDGE_BASE_PORT`,
  `MURMUR_LINKEMU_PORT`, `MURMUR_HTTP_PORT`, `MURMUR_WEB_PORT`, `MURMUR_TIDEWATER_PORT`,
  `MURMUR_CONTACTS_PORT`, `MURMUR_SITL_INSTANCE_BASE`, and for thousands of stand-ins
  `MURMUR_STANDIN_SRC_BASE` (header of `scripts/up.sh`). `MURMUR_WEB_PORT` also tells the bridge which console origin to accept;
  the session cookie is named per HTTP port.
  `make situation` has its own block (UDP 33000+ and 34000+, bridge :9265, linkemu :9266,
  contacts API :9267, console :5673, tidewater :9270, SITL from instance 160; each variable
  overrides one); an eval stack's contacts API is 10660+block (`eval/ports.py`).
- Contacts API (docs/AUTONOMOUS-NODES.md 2.5, decision D1, bd murmur-y85j.4): `murmur-contacts`
  takes orders for its nodes itself (`crates/murmur-contacts/src/api.rs`: `POST /orders`, `GET
  /orders[/{id}]`, `GET /nodes`, 127.0.0.1 only), every request with the stack's
  `X-Murmur-Sim-Token`; our bridge never relays a red order. A situation's `red` step posts there
  (the runner made the token); by hand, pass your own `MURMUR_SIM_TOKEN` to `make goldengate`
  (up.sh's own is never shown). Without `MURMUR_CONTACTS_PORT` the process serves nothing.

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
  `browser_close` and reopen. A full-screen shader (the weather layer) runs at 1 to 8 fps
  there: give screenshots a long timeout. Playwright MCP writes files only under
  `~/murmur` (not the scratchpad): keep screenshots in a gitignored `logs/` dir.
- A checkout runs one stack at a time: `make start` started in a worktree shares that
  checkout's `logs/up.pid`, so a `make situation` there refuses and `make down` there stops the
  menu's scenario. Run the owner's menu and an agent's test stack from different checkouts.
- Dev-only globals for debugging the console: `window.murmurMap`, `murmurBridges`, `murmurPerf`.
- MapLibre is pinned to **v5** (deck.gl 9.4 breaks on v6). Don't upgrade.
- After changing SITL code or its param table, rebuild all firmwares:
  `cd ~/ardupilot && PATH=~/murmur/.venv/bin:$PATH ./waf plane copter rover sub blimp antennatracker`.
- ArduPilot stream-rate params are `MAV1_*` here (not `SR0_*`). Pass SITL one defaults file, not a
  comma list: ArduPilot drops `MAV1_*` from an earlier file in the list (murmur-yjq; `swarm.py` merges).
- In SITL, stop vehicles with forces the IMU feels, never by zeroing velocity or clamping
  position (the EKF diverges).
- A strike's damage reaches SITL vehicles through `sim/sitl_damage.py` (a thread of `swarm.py`,
  following the damage file; bd murmur-y85j.8): mobility sets `SIM_ENGINE_MUL` 0 and
  `SIM_ENGINE_FAIL` to the kind's propulsion outputs over the vehicle's SERIAL2 (TCP
  5763+10*instance; tidewater has SERIAL1), the `DAMAGE` STATUSTEXT goes in there from its sysid
  as component 191 and ArduPilot routes it to the bridge, and destroyed SIGSTOPs the SITL process
  (`swarm.py`'s shutdown sends CONT after TERM). A sailboat and a tracker only report. Test with
  `sim/tests/sitl_damage_live.py` (its own ports).
- tidewater (`tidewater/`) is installed editable in Murmur's `.venv` (`make setup`, with its
  `sfbofs` extra); a scenario needs `tidewater fetch` once (setup fetches the Golden Gate's),
  cached in `tidewater/data/cache/` (untracked). `TIDEWATER_HOME` points the scenarios at
  another tidewater tree. A worktree has neither: make its own `.venv` with `-e ./tidewater`, and
  symlink the main checkout's `tidewater/data/cache`.
- `murmur-replay` accepts only the current decision-rules version by default: replay older logs with
  `--rules N` / `--min-rules N` (versions and changelog in `crates/murmur-decide/README.md`). The bridge
  writes run-time anchors to `logs/anchors/<run>.anchors`; verify with `murmur-replay <log> --anchors <file>`.
  Engagements need two distinct signers with engagement authority: in a dev run
  `MURMUR_ENGAGE_OPERATORS` (each also in `MURMUR_OPERATORS`, or the bridge refuses to start; both
  lists are logged as the run's roster); in a credentials run the credential file's engagement flag
  (`murmur-auth engage <id> on`). Scenarios: `murmur-scenarios` (`sim/scenarios/engage/`).
- Stand-ins take an ephemeral UDP port each unless `MURMUR_STANDIN_SRC_BASE` is set (then
  stand-in i sends from base + i; one whose port is taken falls back to an ephemeral one,
  logged). The ephemeral range (`net.ipv4.ip_local_port_range`, ~4,000 here, shared by the whole
  host) capped a fleet near 4,000 before; set the base for thousands. The link emulator takes no
  ephemeral port (it answers the bridge from its listening port, murmur-z07i).
- A stack uses two UDP ranges (vehicle base + N and bridge base + N), three with
  `MURMUR_STANDIN_SRC_BASE`: pick each clear of every other stack's ranges, including a large
  load test's (a 3,000-node test spans 3,000 ports from its base). A bridge that finds a port
  taken drops it for good.
- WSL here runs mirrored networking (`wslinfo --networking-mode`): 127.0.0.1 is routed out
  through `loopback0`, a virtual NIC to Windows and back (`ip route get 127.0.0.1`), not `lo`.
  At a few thousand vehicles its queues drop packets (`tc -s qdisc show dev loopback0`: 37
  million of 391 million since boot; `Udp: SndbufErrors` 18,000/s with two 3,600 stacks up) and
  loopback TCP stalls for seconds (SYN retries), so HTTP calls between processes time out now
  and then; any other 127.x (127.0.0.2) stays on `lo`: `MURMUR_LINK_HOST=127.0.0.2` puts a
  stack's UDP links there. A UDP socket bound to
  0.0.0.0 and connected to 127.0.0.1 stopped getting replies at about 1,700 of them (stand-ins
  bind the loopback address now). A port Windows holds is taken in Linux too, though `ss` does
  not show it (UDP 10010 is): probe a new fixed range by binding it before trusting it.
- Keep fixed port bases (vehicle, bridge, stand-in source) below the ephemeral range: clients
  draw from it, and a fixed base inside it collides at random. It can change between boots: check
  `sysctl net.ipv4.ip_local_port_range` (39160-43255 as of 2026-10-08; was 37795-41890 on 10-04).
- Headless worlds (`sim/worlds/`, docs/HEADLESS.md 17): Harbor watch's `[headless]` pin
  (`domain_rule = "not_evaluated"`, `currents = "none"`) holds every v1 and v2 result and the
  golden: never drop it. A new world's areas go in their own directory, never in `sim/areas`
  (every Golden Gate stack loads all of it). In a worktree without `sim/space/cache` (untracked)
  the golden episode fails (SATCOM looks differ): symlink the main checkout's.
- Eval stacks own UDP 35000-36999 and HTTP 10600-10699 (`eval/ports.py`, fixed): keep other
  live stacks off them. Each eval run starts as a fresh station (`eval/stack.py` removes the
  bridge's `.last-run-<port>` before and after; `run.json` `restore_carry`, murmur-kvb).
- Situations (`sim/situations/README.md`): `op-1` is the person, never a script; scripted ids
  are `op-setup`, `op-auto`, `op-2`, `op-3`, `op-planner`, `agent-setup`, `agent-scripted`.
  op-setup submits only what its dry run (`POST /api/intents/preview`) says is auto or waits for
  someone else; the rest goes through agent-setup to the inbox. The AI starts in an empty temp
  dir, MCP tools only. A timeline `propose` step may be a lay order (`task = "lay"`, `count`,
  `live_h`, `weapon`) and an `engage` step may name its `weapon`; op-2 and op-3 sign a lay order
  as a nomination, without a picture token (`EngagementSigners.consider_lay`). Hit draws are
  seeded by the contacts' `seed` and the intent's number (`effect:<intent>:target`, a burst's
  `effect:<intent>:<burst>:target`, a mine's `mine:<intent>:<n>:<contact>:<pass>` or
  `...:run`): a weapon situation picks its seed for the default run's intent numbers.
- A stand-in UGV's altitude is its fleet `spawn` altitude plus the terrain's rise from there: spawn
  it at the DEM's height (tidewater's `terrain.npz`), or it reports 0 m and the link emulator
  judges its line of sight to the Crissy Field radios from sea level, under the hill (its mesh
  link down by terrain, its telemetry stale, every check "cannot evaluate"). The Presidio's hills
  block the radios from most of the bluffs west of the bridge's south end.
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
  acts are a node's own shots under its contacts file's `[[order]]` (docs/AUTONOMOUS-NODES.md;
  no shipped contacts file scripts an `[[act]]` any more); review at `GET /api/review`. Live demo:
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
  a segment pinned to another time shift or on another scenario map mid-run
  (`EngineError::SpaceSegment`) and replay flags either changing between two `space.loaded`.
- Rules 15 (`CARRY_FOLLOWUPS_SINCE`): a spent platform stays spent across restarts, a full
  restack with the same fleet included (`carried_from`'s `spent`; same vehicle only): an
  engagement-authority operator un-spends it with a note once the engagement that spent it is
  over (a mark carried from a log where it had not ended: once its assessment window from the
  spend is past), it was heard after that and is heard now, and the vehicle under the uid is the
  one spent (platform, callsign, sysid; `POST /api/restores/unspend`, the console's Un-spend on
  its SPENT, STILL HEARD alert, shown only then; a credentials run re-enters the password or
  passkey, asked for only after the 403 and 409 checks), and its own values are then owed back
  (missions wait for the restore). A nomination on an armed
  platform a failed carry may owe is refused (`fallback_preload`) until that log is read or
  written off. A pointer that can't be read falls back to this station's newest log by the carry
  chain (`pointer_failed`) when its chain and anchors check and no newer log naming no pointer,
  or not parsing, and no fleet or anchors file of a deleted log, could be the one named; else
  that log and every such log is a failed carry under its own name (its fleet's armed platforms
  and what it owed or had spent held) and the next start reads them. A failed carry records the fleet file
  it read (`fleet_read`); `murmur-replay` compares `<log stem>.fleet.json` when at hand.
- Keep-out explanation (murmur-12e; read-only, not logged, no rules change): the nomination
  form and `POST /api/engage/options` (`keep_outs`) say which track or vehicle blocks, which
  region, the shortfall, and what would clear it ("within X m", "between X and Y m", or a wait
  "if it keeps behaving as now"; scanned on a 0.25 s grid); agents read it with
  `get_nomination_options` (one call a second). It shares the checks' own per-item judgement
  (`engage::track_standing`, `friendly_standing`, `circle_holding`): changing those changes
  decisions. The dry run fills what the engine fills (`Engine::dry_run_inputs`). More
  pictures: `MURMUR_KO_SEED=N MURMUR_KO_WORLDS=1500 cargo test -p murmur-decide --test keepout`.
- What a person alone holds up (`decision/blocking.rs`; display only, never logged, no rules
  change): the live bridge (`main.rs`; the headless simulator does not look) looks every 2 s,
  only while a rule declaration nobody confirmed or a flag on an act against ours is in force,
  with the nomination form's dry run (75 m, the policy's window, return) on the 12 armed
  platforms nearest the track; an item on which a nomination fails `target_declared` alone is
  `blocking` on `/api/inbox`, `/api/review` and MCP `get_inbox` (only when any). The console
  shows it urgent (`WaitingStrip.tsx`, `blocking.ts`); the watch officer's driver names it in
  the update and attaches its id to the turn. A test rig that changes the policy's
  `[identification]` calls `p.sync()`, or the bridge's own rules (the dry run's) keep the file's.
- Keep-out measurement: `.venv/bin/python -m eval.run keepout --seeds 1 2 3 4 5 6 7 8 9 10
  --situations 100` (every variant, ~16 min; `--variants radar all` for less). `eval/keepout_probe`
  is a workspace member that compiles the bridge's `tracks/{model,sensing,truth}.rs` by path, so
  `make check` lints and tests it with them. Model results: read METRICS.md "Keep-outs against
  engagements" before quoting (radar alone is the pessimistic sensor case; the air tracker is
  calibrated on simulated motions only, "The air tracker against truth"). `--variants oov
  oov-all oov-near` (rules 14 against 16, a few minutes): contacts going out of sensor view;
  METRICS.md "Out of view".
- Rules 16 (`LOST_CONTACT_SINCE`): a track the tracker drops (180 s unseen) stays in every
  engagement keep-out as a lost contact (`track.dropped`) until an operator with engagement
  authority accepts it for one engagement (nomination form or the card, `accept_lost`, the
  `accept_unknown` step-up) or an operator confirms a claimant carrying its AIS
  (`lost.claimed`, review list, `POST /api/review/lost/{track}/confirm`, `track.reacquired`;
  void, `lost.reinstated`, once the claimant turns suspect, is declared or drops). A track out of view (unseen `engage::OUT_OF_VIEW_S`, 10 s, live or lost) counts anywhere
  at its domain's top speed from its last fix: anything that judges or draws one uses
  `engage::lost_reach` / `track_standing` / `track_reach_at` with the decision's time, never
  `track_reach` alone. A track's AIS is `WorldSnapshot::track_ais`, not a `TrackFacts` field: a
  new `TrackFacts` field breaks `tracks/tests/before_oho`, which builds one as it was and is
  never edited. Replay
  flags a track that leaves a picture without a `track.dropped`: a test or driver that drops a
  track hands it to `Engine::record_track_drops` first (scenarios: `drop_track`); an id is
  dropped once, and never back in a picture with another fix (a lost contact counts whatever the
  picture holds). A v16 run with drops must log `track_drop_s` in `run.started` (the bridge and
  the scenario runner do: 180; an engine test passes it in `StartCarry`) and each drop comes that
  long after the track was last seen, its fix no later than that sighting; replay flags either.
  Burden model: `python -m eval.lost_burden`.
- Rules 17 (`ROUTE_SURFACE_SINCE`, murmur-k8i item 2): the domain rule judges land or water
  every 20 m along each leg a boat, sub or UGV travels (transit, task path, return; an
  engagement's way; an rtl's), logged one letter a point (`along`), besides the waypoints. A
  mission or nomination across the wrong surface is refused; an rtl never is: it goes with a
  check `warning` and an inbox alert `path_crosses_land`. The bridge fetches the points before
  each decision and answers a moving vehicle's from the nearest cached tidewater sample within
  25 m (`TerrainOracle::sample_along`).
- Rules 18 (`VELOCITY_SIGMA_SINCE`, murmur-vni): a track's velocity regions and a young track's
  speed bound grow with the tracker's own `velocity_sigma_mps` (the policy's 1 m/s a floor), so the
  tracker may show an aircraft's velocity up to 20 m/s 1-sigma while its two models agree (the
  3 m/s gate elsewhere). Such a velocity can lag a hard turn by a few hundred metres a few
  seconds ahead; keep-outs cover it, but `get_tracks` does not yet carry the sigma.
- Rules 19 (`ACCEPT_BEHAVING_SINCE`, bd murmur-j0x): an operator with engagement authority may
  accept a live blocker as behaving now (`accept_behaving` on a nomination, `POST
  /api/intents/{id}/accept_behaving`/`revoke_behaving {track}`, the `accept_unknown` step-up);
  the keep-out counts it along its accepted path and its course now until it lapses on its own
  (out of view, or its estimate leaves the accepted prediction's 2-sigma in position or velocity:
  `engage.acceptance_lapsed`, logged before any step, look, signature or acceptance reads it). A
  waiting nomination with one is stepped every second for its lapse; anything that judges a live
  track for an engagement goes through `engage::live_standing`, never `track_standing` alone.
  The card's `engage.behaving` and the form's `keep_outs.items[].behaving` say what accepting a
  blocker would do; agents see them and can never accept. Scenarios 99-105; the headless
  reviewer's `accept-behaving` preset.
- Rules 20 (`MUNITION_FLIGHT_SINCE`, bd murmur-64z): a platform whose catalog entry has `munition
  = { name, speed_mps, range_m }` (only `[mq9b_armed]`) releases it within its slant range of the
  point (not `arrival_m`; needs the vehicle's `alt_msl_m`, else `arrival` fails), its time of
  flight before the plan's time: `effect_time`, `alignment`, the release's keep-outs and the
  window judge the strike (`engage::shot`, `strike_lead`), not now. `engage.effect`,
  `engagement.munition` and `EffectOrder` carry the flight (engine ms); the bridge hands the
  contacts process `impact_t` on its own clock and `effect::InFlight` holds the effect until then
  (live and headless); observation starts at the impact (`engage::struck`). Without a munition
  nothing changes. Scenarios 106-107; tamper `edit_munition`.
- Rules 21 (`IDENTIFY_SINCE`, bd murmur-bia): `identify` names one track and one vehicle; the
  engine flies it as an orbit around the track at its EO/IR's identification standoff
  (`identify::plan`: an eighth of the detection reach, `IDENTIFY_PX_SHARE`), moves the orbit with
  the track (`identify.step` recentred, logged before the follow goes out), ends it identified,
  lost or at its deadline, and sends the vehicle back to the order it replaced, re-checked, or a
  hold, all under the identify's own id. Its look phase never finishes on an op `done` (an orbit
  is done once established): only `Engine::step_identify` ends it. The world's `track_levels`
  (the tracker's identity level per track) is what ends it: a driver that sets a track's identity
  sets its level too. Never queued, never carried over a restart.
- Rules 22 (`ROUTE_PLAN_SINCE`, bd murmur-n6a): a transit (and a patrol's legs) the domain rule
  or the geofence would refuse is routed round it first (`murmur_decide::plan`, a 40 m grid on the
  same land/water source, logged with its grid in `route_plans`; replay plans again from it), and
  refused only with no route ("no route around ..."). The waypoints go out in `intent.dispatched`
  `via`/`path` and the task layer flies `TaskParams::via` before the task. The return home, an rtl
  and an engagement's way stay straight. The bridge fetches the grid before the lock (`prefetch`):
  a new caller of the engine with a planned kind prefetches first, or plans on an empty cache
  ("no land/water answers for the planner's grid"). `IDENTIFY_SECOND_LOOK_SINCE` (22 too): any
  live track may be sent a look. Rules 23 (`IDENTIFY_OWN_LOOK_SINCE`): it ends identified only on
  the vehicle sent's own identification after it went out (`TrackLevel::own`, by vehicle uid, the
  tracker's `own_identified_t` per observer); another vehicle's is an `identify.step` `meanwhile`
  and the order goes on. A driver that identifies a track by a vehicle's camera sets `own` for
  that vehicle (the scenario runner does for any `level_by` but `ais`). The bridge shows each as an
  inbox alert `identify_meanwhile` of severity `info` (never a badge; alerts sort high, medium,
  info).
- Rules 24 (`LOADOUT_SINCE`, bd murmur-lbw8.1): a catalog entry's `loadout` (`catalog::
  WeaponFacts`, a logged platform fact; figures in docs/research/weapons.md) replaces rules 20's
  `munition` line, which only rules 20 to 23 read. An engagement names its weapon (`weapon`, by
  name: REST, MCP and the scenario runner's `nominate`) or the engine picks the first of the
  loadout, in the catalog's order, usable against the track's domain with a round left
  (`loadout::choose`; the `weapon` check says why); the preview's `weapon` is frozen with the
  plan. A missile, bomb or torpedo is released between its minimum and maximum slant range; `self`
  is the platform (spent with it, never counted); mines are laid from rules 27 (a lay order, never
  an engagement's release; the Orca and the mine-laying UGV are armed); guns fire from rules 26. Each release
  logs `weapon` (rounds `left`, the vehicle) on the `engage.effect`; every engagement decision
  logs the `rounds` fact (`Restores`' ledger, as spent marks); `run.started`'s `carried_from`
  carries `rounds`, on the same vehicle only. Rounds left: `GET /api/loadouts`, MCP `get_fleet`'s
  `platform.loadout`, the vehicle details' Loadout row. The v1/v2 worlds keep the unarmed `mq9b`;
  live fleets and the v3 worlds fly `mq9b_armed` (no opening order for it), except
  morning-harbor-watch and hostile-act-loitering-munition (an unarmed MQ-9B is their premise).
  Scenarios that need a Magura surviving its own effect name `magura_pre_loadout`. Scenarios
  119-120; tamper `edit_rounds`.
- Damage model and rules 25 (`DAMAGE_SINCE`, bd murmur-lbw8.2, DECISIONS.md "Warheads and
  damage"): the truth side draws every strike (crates/murmur-contacts `damage.rs`): a hit from the
  weapon's catalog `effects` for the target kind (`KIND_ALIASES` maps other kinds; decide's
  `catalog::KIND_ALIASES` is the same table, a headless test checks) times the conditions'
  factors, then destroyed / mobility / firepower / undamaged, plus collateral within 4 m per cube
  root of the warhead's kg; each draw seeded by the contacts' seed and the strike (`effect:<intent>`,
  `act:<id>`). A weapon without `effects` (or no weapon: the v1/v2 worlds) stays abstract exactly
  as before. Our damaged vehicles: `sim/run/damage-<http port>.jsonl` (`MURMUR_DAMAGE_FILE`,
  emptied by the contacts process at start; an eval stack's in its run directory; a file not
  written since the stand-ins or the SITL watcher started is a previous run's, skipped, bd
  murmur-4gnm) to the stand-ins (`murmur_standins::damage`), which
  step unpowered, go silent, or report `DAMAGE mobility|firepower` STATUSTEXT every 30 s; the
  bridge makes that the `damage` vehicle fact, a `damage` event and a `damaged` alert. SITL
  vehicles act it out too (`sim/sitl_damage.py`, the SITL gotcha above). The engine reads only
  that fact (weapons out of action refuse the loadout; a `self` platform without propulsion can't
  strike) and drops it from logs of older rules; the weapon check shows the figures. Headless: an episode's `effects.p_hit`, when given, replaces the
  target's hit chance (an evaluation's knob; absent, the figures decide). Scenarios 121-122;
  tamper `drop_damage`.
- Rules 26 (`GUN_SINCE`, bd murmur-lbw8.3, DECISIONS.md "Guns: bursts from a counted magazine"):
  a gun (`kind = "gun"`, its `burst_rounds`, `rate_rpm` and `burst_interval_s`; only the armed
  UGV's M2) is usable. Its plan sends the platform to a firing position (`EngagePreview::
  fire_from`, `EngageOrder::fire_from`: the gun's range less the effect radius from the point,
  or where it stands), which reach, the domain rule and the bridge's intercept use instead of the
  effect point. Its release is burst 1 (`engage.effect`, `weapon.rounds`) and opens a firing
  window (`Engagement::fire`, `engage::firing`): each second the observation (from the last
  burst) runs first, then `engine.step_fire` ends the fire on its own (`engage.fire_ended`:
  rounds out, the approved window closed), holds the picture (`engage::picture(.., last_burst)`:
  no lateness, the target unseen holds fire rather than failing it) and, holding, ready and
  `burst_interval_s` after the last, fires `engage.burst` (rounds, left, `duration_s`), counted in
  rules 24's ledger. Burst checks: the target within the gun's range of the platform, seen since
  the last burst (`target_recency`), the line of fire clear (`line_of_fire`, 50 m either side,
  `engage::LINE_OF_FIRE_M`), alignment, uncertainty, the preload read back (re-read all through
  the window). Ceased or unknown from the last burst ends it (persists does not while it fires);
  a stop, a supersede or a failed picture ends it with `engage.stopped` and no transition (it
  stays effected and is assessed). A firing gun counts as dispatched wherever a platform is held
  (`Intent::play_state`, `engage::play_state`: engaged facts, supersede, abort, preload holding).
  The truth side draws each burst on `effect:<intent>:<burst>` (guns: 20 points a band past
  15 m/s, x0.6 in fog, x0.6 against a boat in sea state 4-5). No munition flight (2 s to 1,800 m at
  the muzzle speed, about 3 with drag [E]: inside the keep-out's margins). Scenarios 123-127; tamper `burst_after_stop`
  (`edit_rounds` covers bursts too); runner keys `cease_at_burst`, `expect_bursts`,
  `expect_fire_end`, `expect_rounds_left`.
- Rules 27 (`MINE_SINCE`, bd murmur-lbw8.4, DECISIONS.md "Mines: laid on two signatures,
  recorded, live for a set period"): `Task::Lay { points, count, live_h, weapon }` (REST and MCP
  `task = "lay"` with `route`, `count`, `live_h`, `weapon`; the runner's `lay` step) on the Orca
  (Hammerhead) or the mine-laying UGV (TM-62M), both now `armed`. `TaskKind::needs_engagement_
  authority`: signed only by `Engine::approve_lay` with an `EngagementAuthority` (the generic
  `approve` refuses it; the policy's engagement floor applies, two signers, never the proposer; no
  picture token: the order is what is signed). Checks after the vehicle rules (`mines::evaluate`):
  `platform_armed`, `weapon` (a mine of the loadout, rounds >= count, `live_h` <= its `persist_h`),
  `platform_free`, `minefield` (what the field could catch beside a declared hostile: tracks of the
  mine's `targets` domains now and on their course for `mines::TRACK_HORIZON_S`, lost contacts,
  ours and their `orders_in_play`, the layer after each mine's `arm_s`, no-strike areas). Dispatched
  as a vehicle order flown once along the line, then home (the bridge's task kind `lay`; no start
  logged ahead: rules 7 covers `queue::vehicle_task`'s kinds only); `Engine::step_lays`
  (the bridge's tick, before `follow_ops`) lays each mine the layer reached (`mines::due`:
  `LAY_AT_M`, `LAY_AHEAD_M`) as `mine.laid` (the record; spends a round in rules 24's ledger) and
  logs `minefield.inert` once a field's period is over. Fields live in `Restores` (engine and
  replay), go into decisions as `minefields` (set before the route plans read them:
  `Engine::with_fields`), carry in `run.started`'s `carried_from` (`fields`), and are no-go areas
  (`mines::field_areas`) for our vehicles of their domains in the geofence and the planner; an rtl
  across one passes with a `warning` (inbox alert `path_crosses_minefield`). The truth side:
  `murmur_contacts::mine` (contacts process and headless): `mines` in the simulation channel's
  answer, `mines_live` in the truth message (the answer then carries `ours` of those domains),
  pressure passes drawn on `mine:<field>:<n>:<who>:<pass>`, a Hammerhead's torpedo run on
  `mine:<field>:<n>:run`; ours struck come back as `impacts` (a struck report). `GET
  /api/minefields`, MCP `get_areas`' `minefields` (only when there is one), the console's map (red
  outlines with the reach rings). Scenarios 128-130; tampers `move_mine`, `hide_lay_signature`;
  runner key `expect_mines_laid`.
- Rules 28 (`MISSILE_STANDOFF_SINCE`, bd murmur-9001): a missile on a ground or surface platform
  (today the armed UGV's Javelin) gets a firing position as a gun does (`EngagePreview::
  fire_from`): where it stands when the point is within its band (minimum range plus the effect
  radius, to maximum range less it), else on the line to the point at the band's nearer edge
  (closing in or opening out); the plan's effect time is its arrival there plus the flight from
  that edge. The release is judged as any missile's (slant range in the band, the strike's time,
  keep-outs at the strike), so a UGV on land launches at a boat on the water. Aircraft and other
  kinds unchanged. Scenarios 131-133 (`tests/rules28.rs`).
- Opponent weapons (docs/OPPONENT-WEAPONS.md, bd murmur-wcvp; no rules version): a node fires its
  catalog kind's loadout (`sim/contacts.toml`) by its own fire control at an `[[order]]` (the
  contacts file, the contacts process's `POST /orders`, a situation's `red` step;
  docs/AUTONOMOUS-NODES.md 2.5). The old path (an `[[act]]`, a contact's own `weapon = {...}`)
  loads only under a top-level `scripted_acts = "<why>"` (`scenario.rs` `old_path`) and never in a
  situation, marked or not (`eval/situation.py` `_contacts_guard`; bd murmur-y85j.7). Each shot's
  weapon kind (bridge figures: `tracks::physical::weapon::weapon_signature`: `atgm`, `rocket`,
  `air_missile`, `gun`, `torpedo`, the catalog's `sense`; section 15) makes it a launch, flown by
  `murmur_contacts::weapon` to a lead point on the victim, in 3D (the truth asks the bridge where
  our victims are: `aim_at` in, `ours` out; a node's `look` asks for ours within its reach, answered
  in the same `ours`). The truth's `kind` only picks the physics. Under the physical model only (a
  legacy run senses the act as abstract): a weapon act is observed by the sensors that caught the
  launch (`observe_launch`: cameras; a torpedo's transient a listening sonar), never by radar; a
  gun's episode is one act whose bursts are each a weapon (`rounds`), reported at the first burst
  caught. The weapon's fixes go to the weapon picture (`tracks::weapons`, `W-<n>`, `weapons` on the
  tracks feed and `GET /api/tracks`), never the track picture; a gun's rounds are never fixed; a
  hit is the victim's `vehicle.event` `struck` and an inbox alert, a miss nothing (the impact's
  `hit`, the damage draw: the victim can't know rounds went past, bd murmur-aset; absent for a
  weapon without figures, reported as before), its weapon track ending lost. The picture knows a weapon by
  stages (bd murmur-ju1h, section 14): `launch` (the flash: place, bearing, and its kind when told:
  missile, rocket, muzzle, transient; nothing else), `weapon` (fixes gated together, not by truth
  id, whose measured speed passes `weapons::weapon_speed_mps` for its domain (water by sonar, air by
  a measured height) from a launch or closing on ours; a contact track that passes is taken for one
  too), `class` (only on `evidence` leaving one class: the launch's kind, speed in the threat
  library's band, `weapon_signature`'s `threat`, an identified shooter `carried_by` it, which may
  name several); target and `eta_t` derived or absent; never the type. Nothing in it may read the
  truth's weapon kind, name, speed or victim. Weapon sensing reads the payload managers' state and
  never writes it: contact detections are the same with any class in the truth (tested). Every
  live situation's attacker is an armed catalog kind fired by its own orders (bd murmur-y85j.6);
  the tutorial's boat is ordered every 90 s, and its contacts' `[training]` (a tutorial's only)
  spares HUNTER 1. A situation's attack aims at a vessel whose loss leaves the chain standing:
  air-to-air's drone fires at HUNTER 2, 3.5 km off, not HUNTER 1, whose camera the chain needs.
- Predicted link coverage (docs/LINK-COVERAGE.md, bd murmur-hei): `GET`/`POST
  /api/links/coverage`, `link_coverage` in dry runs and `preview_intent`, the MCP tool
  `get_link_coverage`, the console's Links layer and the order form's dry run (`web/src/
  coverage.ts`). Read-only, never logged, never a check; it shares the link emulator's
  own range and terrain code (`murmur-linkemu` `coverage.rs`), so prediction and simulation
  cannot disagree on geometry, but it does not predict scripted events (a jammer, a cut).
  Terrain comes from tidewater or a baked grid named by `MURMUR_SURFACE_GRID` (headless: the
  world's); without either it is not checked and the answer says so. It predicts at the
  order's altitude: a vehicle flying lower sits in another 10 m band and can lose its link
  sooner than predicted.
- Weather on the map and sensor reach (docs/WEATHER.md, bd murmur-sgok, murmur-4tq,
  murmur-vr17): `GET /api/sensing/reach` and MCP `get_sensor_reach` (read-only, never logged,
  never a check) predict each sensor's reach under the conditions where the vehicle is. They
  and the running model share one merge of scenario conditions and weather
  (`physical::Engine::conditions_at`), and the reach display and the catalog's
  `physical_reach_m` generator share one reference contact (`tracks::reach`): change either
  in one place. The decision engine still reads the clear-day `physical_reach_m` (bd
  murmur-ahss), so in fog the display shows less reach than the checks assume. The map draws
  each archived model point's region flat (the samplers' nearest-point rule, from
  `/api/weather/grid` `regions`), never a spatial interpolation; `regions.scripted` names the
  fields the timeline sets. Draw anything on the water with deck.gl at the surface: a MapLibre
  line drapes on the terrain, which under water is the seabed (heights x2), and sits visibly
  off its vehicle in a pitched view. A MapLibre image source's blob URL can stall behind the
  tile queue (the weather uses a custom WebGL layer), and terrain fog only shows from pitch
  60. A new MCP tool changes the tool lists in the headless golden and paused digests
  (`eval/headless/tests/test_episode.py`, `test_charged_clock.py`): re-take them and check that
  the code without the tool gives the old digests.
- The tracker is a two-model IMM; changes must pass `tracks::tests::calib`. Its held-out seeds
  (66-95) are not for tuning: write a new held-out set if you tune on them. Aircraft have their
  own calibration (`tracks::tests::calib_air`: held-out seeds 221-250, run once, its two misses
  pinned as found, not for tuning; tuning sets 101-220; tuning aids `air_calib_table` and
  `air_tune_grid`, ignored tests), compared with verbatim copies of
  the tracker before it (`tracks/tests/before_oho/`: never edit them). A 2D radar plots an
  aircraft at its slant range (`Detection::slant_from`): a test that reads such a fix's
  `lat`/`lon` as the aircraft's position is wrong by its height's share.
- Two sensing models (docs/SENSING.md): `physical` (the default, live and in new worlds) and
  `legacy`, chosen by the contacts scenario's `[sensing] model` or `MURMUR_SENSE_MODEL` (which
  wins); v1 and v2 worlds and the eval stacks are pinned to `legacy` by that variable
  (`process_spec`, `fullstack.stack_env`, `eval/stack.py`), never in their contacts files (the v1 generator's digests hold them byte for
  byte). Under `physical` the catalog's `range_m` is not a sensor's reach (the model reads the
  entry's other fields or placeholders from it; the decision engine's assessment reads the
  generated `physical_reach_m`, bd murmur-1u4: a change to the physical model or a sensor's
  figures that moves one fails `tracks::tests::reaches` until `cargo test -p murmur-bridge
  write_physical_reaches -- --ignored` rewrites the catalog's lines), a
  camera sees only where its payload manager points and takes seconds a look (each level builds
  up second by second at ACQUIRE's rate P²/T, T 3.4 s to detect and 1 s to recognise or
  identify, plus 1 s to settle after each slew: a test expecting it to see something within a
  few ticks fails; docs/SENSING.md), and the sun comes from the truth message's clock: a test at the tests' `T0`
  (1,000,000 s, 1970) is near dawn at the Gate, so state `sun_elevation_deg` in its conditions.
  Its held-out calibration runs (`calib_physical`, `air_calib_physical`, and its own air sets
  `air_calib_physical_held_out`, seeds 251-280, `air_calib_physical_held_out_two`, 281-310, each
  run once for murmur-7vp, `air_calib_physical_held_out_three`, 311-340, run once for
  murmur-0b8, whose manoeuvre model it did not confirm, and `air_calib_physical_held_out_four`,
  341-370, with the surface's own `calib_physical_held_out`, 371-400, run once for sensing round
  3, and 401-490 for the ground calibration (murmur-e3b; ground tuning sets 491-520); 221-490
  are spent, and 521-550 for the air velocity change (rules 18, murmur-vni), so a new physical
  check takes a fresh set from 551) are evaluations: never
  tune the sensing model or the tracker on them. Sea clutter (round 3, murmur-qlt): the grazing
  angle is the spherical earth's to the nadir, a radar's `cell_m` its range resolution, its
  `doppler` mode clears a surface contact moving along its line of sight past the clutter's band;
  an airborne radar sees a contact holding still on the sea only beyond its nadir's glare (no
  surface reach), and radars' reaches start at the IMO's 40 m minimum range. A cued camera holds
  its search pattern while each new fix keeps the contact within it (`Gimbal::pattern`). Mind `cargo test calib_physical -- --ignored`:
  the name matches every one of them (use `--exact`). A physical radar's plot carries its scan
  (`Detection::scan_s`): the tracker time-aligns such bearings and holds a ranging fix 6 s on
  them, while a plot without it (the legacy model's) takes the rules as they were; a test that
  builds a physical radar detection by hand sets it.
- The physical sensing step runs each pass's vehicles on a pool of its own (bd murmur-9730,
  `tracks/physical/par.rs`): `MURMUR_SENSE_THREADS`, else a quarter of the cores up to 8 live and
  1 headless (a batch runs a simulator per worker), said at start (`sensing: the physical step on
  N threads`). Any count gives the same detections and state: a vehicle's sensing reads `Shared`
  (grids, weather, cues, identify contacts) and writes only its own `Own`; something one
  vehicle's sensing writes and another's reads in the same pass breaks that (the cues are written
  only between the passes, in observer order). `tracks::tests::sense_par` checks it. The headless
  simulator gets only `PATH`, `HOME` and the spec's variables (`process_spec`): `eval/headless/
  sim.py` passes the caller's `MURMUR_SENSE_THREADS` on, nothing else. The Harbor watch golden
  runs the legacy model: check a change to the physical step on the scale watch's digest
  (`golden-gate-scale-watch`, agent `none`, seed 1; docs/load-test.md). The live bridge runs the
  truth handler's sensing on a blocking thread (`spawn_blocking`), the simulator in line.
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
- Headless engagements and space are opt-in (docs/HEADLESS.md section 16): without
  `Options.signers` nobody signs (the proxy operator never decides an engagement card), and
  without `Options.space_snapshot` the bridge has no space segment. A space episode reads the
  committed snapshot `sim/headless/space/<world>/`, never `sim/space/cache`: a new fetch changes
  nothing headless until `python -m eval.headless.space snapshot` pins it.
- Headless Saturation (`--situation swarm-saturation`, docs/HEADLESS.md 17.6) takes about 30
  minutes an episode at half real time, 3 GB (simulator and driver) and 350 MB of output. The
  simulator caps an API answer at 16 MB (`sim/mod.rs` `to_bytes`): its `/api/tracks` passes that
  by 450 s, and a driver that reads it fails there.

## Conventions

- bd for all task tracking; the pre-commit hook exports issues to `.beads/issues.jsonl`. bd
  can't open its database from a worktree: create and close issues from the main checkout.
  `bd dolt push` has no remote configured; the exported file in git is what is shared.
- Landing a branch while main holds someone's staged, uncommitted work: save it (`git diff
  --cached --binary` and `git stash create`), check it applies on the new tip in a temporary
  index (`GIT_INDEX_FILE=... git read-tree <tip>; git apply --cached --check`), then reset main
  to the tip and `git apply --index` it. Commit only your own paths (`git commit -- <paths>`).
- Commit and push to `origin` (GitHub) when work lands; `~/git/murmur.git` is stale.
- No AI attribution in commits; no em dashes in prose (global CLAUDE.md).
- The history is public and read by people judging the work, so write it for them:
  - A subject line of 72 characters at most, in the imperative ("Add", "Fix", "Calibrate"),
    no trailing period, saying what changed for the project rather than which file moved.
  - A blank line, then a body wrapped at 72 that says what was wrong or missing, what changed
    and why, and what it was checked against. Bug, issue and section ids belong in the body.
  - One commit per coherent change. No "WIP", "lint" or "fix typo" commits on main: fold
    them into the change they belong to before it lands.
  - Work from a branch or worktree lands on main by a squash (`git merge --squash`) with a
    written message, never a merge commit; main stays linear, and no branch or worktree name
    (`worktree-agent-...`) appears in the history.
  - A commit that test data or a document cites by hash (a fixture's `build`, a results
    note) is never rewritten after it is pushed; before a push, keep its tree identical and say
    which hash it stands for. The history was rewritten twice: on 2026-10-28 for authorship
    only (every tree identical), and on 2026-10-30 to remove a work email from file contents
    (only the `.beads` issue files and the ArduPilot patches' `From:` lines changed; short
    hashes in commit messages were updated). A hash cited in a file before 2026-10-30 is an
    old one, and `docs/COMMIT-MAP.tsv` gives the commit it names now. Only the noreply
    address appears anywhere (git, bd's Dolt store, `dolt config --global`): keep it so.
