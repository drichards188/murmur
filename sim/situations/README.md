# Situations

Pre-built, pre-tasked scenarios you start with one command, to watch the C2 work and an AI
respond: with you live at the console, or with a scripted operator on autoplay.

```
make situation NAME=harbor-watch                                  # you at the console, AI claude
make situation NAME=harbor-watch OPERATOR=autoplay AI=playbook    # watch it run, no account needed
make situation NAME=harbor-watch AI=codex REVEAL=1                # ChatGPT (Codex), director's view
make down                                                         # the stack stays up after a run
```

| variable | values | default |
| --- | --- | --- |
| `NAME` | a file here, without `.toml` | `harbor-watch` |
| `OPERATOR` | `live` (you decide at the console, as `op-1`) or `autoplay` (a scripted operator, `op-auto`, decides) | the file's `operator` |
| `AI` | `claude`, `codex`, `playbook` or `none` | the file's `ai` (`claude`) |
| `COSIGNER` | `you` or `scripted`: who makes an engagement's second signature in live mode | the file's `cosigner` (`you`) |
| `REVEAL` | `1` shows the director's notes and the timeline still to come in the panel; `0` hides them | the file's `reveal` (`0`) |

A situation runs on its own port block, so it never meets the default stack, a second stack
beside it, the evaluation's blocks (UDP 35000-36999, HTTP 10600-10699) or the host's ephemeral
range: vehicles UDP 33000+, bridge UDP 34000+, bridge 9265, link emulator 9266, console 5673,
tidewater 9270, SITL instances from 160 (`SITUATION_PORTS` in `eval/situation.py`). Each
`scripts/up.sh` port variable (`MURMUR_BASE_PORT`, `MURMUR_HTTP_PORT` and the rest) overrides its
port; an exported `MURMUR_TIDEWATER_URL` is not passed on (the situation's tidewater runs on its
own port). The runner prints the console's address (`http://127.0.0.1:5673` by default); the
Situation panel is over the map's top left.

## Situations

| file | status | what |
| --- | --- | --- |
| `harbor-watch.toml` | runs | Four of ours on station at the Golden Gate, normal traffic, and an unknown fast boat without AIS that comes in from the west and holds near the bridge. The AI should notice it and propose a sensor orbit; the watch policy puts that in front of the operator. Two opening orders wait for your decision at the start (the C2 does not know those vehicles' endurance). 12 minutes at rate 1. |
| `comms-cut-gnss-spoof.toml` | stub (TODO) | Title and brief only. |
| `hostile-act-engagement.toml` | stub (TODO) | Title and brief only. |
| `approval-flood.toml` | stub (TODO) | Title and brief only. |

The runner refuses a stub (`todo = true`).

## The AI, and your own subscription

`AI=claude` runs the Claude watch officer (`agents/watch_officer.py --backend claude`) through
your own Claude Code CLI (`claude -p`); `AI=codex` runs it through your own Codex CLI (shown as
"ChatGPT (Codex)"); `AI=playbook` runs the deterministic `agents/policy_agent.py --watch`
(no model, no account); `AI=none` runs no AI. The watch officer itself is built separately: until
it lands, `agents/watch_officer.py` is a placeholder that says so and exits, and the situation
goes on without an AI.

**Murmur never stores, reads, proxies or prompts for anyone's model credentials.** It only
invokes the CLI you installed and logged into yourself, with your own subscription or key.
Before the stack starts the runner asks that CLI whether it is logged in (`claude auth status`,
or `codex login status` where the installed Codex has it) and keeps only yes or no and the kind
of login, never an account, an email or a key. If the CLI is missing or not logged in, the run
stops before anything starts and says how to fix it (`claude`, then `/login`; or `codex
login`), and that `AI=playbook` is the free option. It never falls back silently.

The AI gets the situation's title and brief (a file), the bridge's MCP endpoint and its agent id
(`agent-watch` for Claude and Codex, `agent-planner` for the playbook), and nothing else: not the
director's notes, not the timeline, not the watch items, no `MURMUR_*` variable. It starts in an
empty directory of its own outside the repository (only `brief.md` in it), and the watch officer
runs its model with the bridge's MCP tools only (no shell, no file tools, no other settings), so
it cannot read what the situation hides from the checkout. Agents propose; they never approve
(no approve tool exists). The launch contract is in `agents/README.md`. A run with `REVEAL=1`
serves the director's notes on the bridge, so with Claude or Codex it is marked "not evidence
about the AI (truth was served on the bridge)" in `situation.jsonl`, the panel and the summary.

## Operator modes

- **live**: you are `op-1` at the console (a dev run: the "Acting as" switcher names who acts;
  `op-1`, `op-2` and `op-3` hold engagement authority). `op-1` is only ever you: no script acts
  as `op-1`. An engagement needs two distinct signers, never its proposer: in a dev run you can
  sign twice through the switcher (`cosigner = "you"`), or let a scripted operator (`op-2` or
  `op-3`) add the second signature after your first, never the first, and only on the plan your
  signature froze, once per nomination (`cosigner = "scripted"`).
- **autoplay**: the evaluation's proxy operator model (`eval/proxy.py` `ProxyOperator`, its
  `[autoplay]` parameters) decides as `op-auto`, and two scripted operators (`op-2`, `op-3`,
  `EngagementSigners`) sign engagement nominations the way the engine requires it: each asks for
  the picture first and quotes its token, reads the nomination again before it signs, never the
  proposer, only while the engine's live picture passes; they never refuse. Their notes in the
  audit log say "scripted"; the panel says "operator: autoplay (op-auto), scripted". They are
  scripts, never evidence about people.

Who is who in a situation's run:

| id | who |
| --- | --- |
| `op-1` | you, at the console (live); never a script |
| `op-setup` | the setup script: issues the opening orders |
| `agent-setup` | the setup script as an agent: proposes an opening order that would otherwise be authorized by op-setup's own submission (below) |
| `op-auto` | the autoplay operator (autoplay only) |
| `op-2`, `op-3` | the scripted engagement signers (autoplay), or the scripted cosigner (live, `cosigner = "scripted"`) |
| `op-planner` | the timeline's second station (`declare` steps; proposals with `[storyline] proposer = "operator"`) |
| `agent-scripted` | the timeline's proposals (MCP) |
| `agent-watch`, `agent-planner` | the AI: Claude or Codex, or the playbook |

Every scripted act says so in the audit log: an opening order's rationale begins "situation
setup (scripted, not a person)", a timeline proposal's or declaration's "situation timeline
(scripted, not a person)", a scripted signature's or decision's note "scripted". The runner
lists the scripted ids it used in `situation.jsonl`, the panel and the summary.

**A script never authorizes a human tier.** Each opening order is first an operator's dry run as
`op-setup` (`POST /api/intents/preview`: the checks and the policy decision now, nothing created,
nothing logged). Where op-setup's submission would be its own approval (a human tier whose
`submitter_authorizes` is true: an armed platform, leaving the approved area, an endurance the
C2 does not know), the order is proposed by `agent-setup` instead, and it waits in the Inbox for
you (live) or `op-auto` (autoplay); otherwise `op-setup` submits it and the policy routes it as
any order. An opening order for an armed platform is refused when the file is read. In live
mode the timeline's operator proposer (`op-planner`) refuses, the same way, a proposal its
submission would authorize.

Situations run as dev runs for now (operator ids asserted by header, logged `auth: dev`,
excluded from any human-control claim); the runner refuses `MURMUR_AUTH=credentials`. In a dev
run the bridge has no mark for a scripted operator: which ids the runner drove is in
`situation.jsonl` and in every note they left.

## What the runner does

1. Reads the situation and checks, before anything starts: a dev run, the AI's CLI installed
   and logged in, and no stack answering on its ports (the bridge, console, emulator and
   tidewater ports, the vehicle and bridge UDP ranges, this checkout's `logs/up.pid`). If one
   does, it refuses and tells you to `make down` first; it never stops another stack.
2. A fresh station: removes the bridge's restore pointer `logs/audit/.last-run-<http port>` and
   resume pointer `logs/audit/.resume-<http port>.json` for this port and sets `MURMUR_RESUME=0`,
   and says so (as the evaluation does: nothing of an earlier run carries into this one).
3. Starts the stack with `scripts/up.sh`, in its own session so it outlives the runner: the
   situation's fleet, contacts, events (merged into one file, `events.toml` in the run directory),
   policy, tidewater scenario and scenario clock; a roster of
   `op-1,op-2,op-3,op-auto,op-setup,op-planner` (`MURMUR_OPERATORS`), engagement authority for
   `op-1,op-2,op-3`, agents `agent-watch,agent-planner,agent-scripted,agent-setup`; this run's id
   (`MURMUR_RUN_ID=situation-<name>-<UTC time>`) and its own simulation-channel token. No truth
   endpoint (`MURMUR_EVAL_TRUTH` is off).
4. Waits for the stack, takes the situation clock's zero, waits until every vehicle is
   navigation-ready (`ready_timeout_s`; it goes on with the rest after that, saying which were
   not), then issues the opening orders under `op-setup`, each after its dry run (above).
5. Starts the autoplay operator or the scripted cosigner, the timeline, and at `ai_start_s` the
   AI (in its own directory). Every 2 s it posts the status document to the bridge for the panel.
   A background task that fails, or a scripted signature that fails, is recorded
   (`problem` in `situation.jsonl`) and shown in the panel; the situation goes on.
6. At `duration_s` (or Ctrl-C, SIGTERM, SIGHUP) it stops its own children (the AI's whole
   process group, even after its leader exited; the autoplay), leaves the stack
   running for inspection, and prints a summary: the audit log, the `murmur-replay` command (run
   it after `make down`, when the log is closed and its shutdown anchor written), the opening
   orders, the AI's proposals with their state and who decided, the scripted decisions, the
   tracks. `make down` stops the stack.

Everything it did goes to `logs/situations/<run id>/situation.jsonl` (one JSON object per line:
`start`, `fresh_station`, `stack`, `zero`, `phase`, `ready`, `order` (with its dry run and who
issued it), `ai_start`, `ai_action`, `ai_note`, `autoplay`, `step`, `timeline`, `problem`,
`end`), beside `events.toml`, `up.log` (the stack's output), `agent.log` (the AI's output),
`agent.jsonl` (the watch officer's own notes, copied from its directory at the end) and
`summary.txt`. The audit log is the bridge's, as for any run.

## The panel and the status document

The runner posts one document to `POST /api/sim/situation` (the stack's simulation token, the
same guard as `/api/sim/contacts`); the console reads `GET /api/situation` and shows the panel
only while there is one: title, brief, the modes, the clock (elapsed and left), what to watch for
as each item comes due, the AI's status and activity (its proposals from the decisions feed with
their state and who decided; its `agent.action` events from the audit log; the watch officer's
notes), the scripted operators' decisions, the timeline done so far, what went wrong in the
runner, and when revealed what is still to come and the director's notes. Its clock runs on the
console's own clock from each status's arrival; a runner that stops posting (10 s without a
status while it starts or runs) is shown as "not reporting".

It is display only (`docs/DECISIONS.md`, "Situation status is display only"): the bridge keeps it
in memory and nowhere else; it never reaches the decision engine, the world snapshot, the tracks,
the MCP tools or the audit log. With `reveal = false` (a blinded run) it carries no director's
notes, nothing of the timeline still to come and no step or watch item marked `director = true`,
and the bridge strips the same again from whatever it is sent (`reveal` not true: no
`director`, no `timeline.upcoming`, no `watch_upcoming`, no entry marked `director`). Watch
items without `director` are shown as they come due whatever `reveal` says, so keep them free of
answers (what to look at, not what it is) or mark them `director = true`.

## The format

`sim/situations/<name>.toml` extends the evaluation's spec (`eval/spec.py`): the same fleet,
contacts, events and policy files, the same `[[step]]` kinds run by the same executor
(`eval/storyline.py`), and what a watch needs on top. Parsed by `eval/situation.py` (unknown keys
are refused).

```toml
title = "Harbor watch"
brief = """What a real watch officer would be told: the operator reads it in the panel and
the AI is given it (title and brief only)."""
director = """Truth and intent, for whoever runs the situation: shown only when revealed."""
todo = false                                  # true: a stub (title and brief only); not runnable

fleet = "sim/fleets/harbor-watch.toml"        # sim/fleets format (stand-ins start in seconds)
contacts = "sim/scenarios/harbor-watch.toml"  # crates/murmur-contacts format; optional
events = []                                   # link and GNSS event files, merged; optional
policy = "sim/situations/policies/watch.toml" # default sim/policy.toml
tidewater = "golden-gate-2026-06"             # a scenario in ~/tidewater/scenarios, a path, or "none"
at = "2026-06-14T10:00:00Z"                   # the scenario clock (RFC 3339)
rate = 1
duration_s = 720                              # situation time, from the stack's start
ai_start_s = 45                               # the AI comes on watch then (default 30)
ready_timeout_s = 180                         # how long the fleet may take to be ready
seed = 1                                      # the autoplay operator's draws

operator = "live"                             # live | autoplay
ai = "claude"                                 # claude | codex | playbook | none
cosigner = "you"                              # you | scripted (live mode, engagements)
reveal = false                                # director's notes and what is to come in the panel

[[order]]                                     # opening orders, once the fleet is ready: op-setup's
                                              # (never for an armed platform; see above)
id = "gate-patrol"
task = "patrol"                               # orbit | patrol | hold | rtl | track
vehicles = { callsign = "MARLIN 1" }          # {callsign}, {platform (, index, all)}, {uids = [..]}
route = [[-122.4720, 37.8135], [-122.4720, 37.8250]]   # orbit: center = [lon, lat], radius_m
note = "Patrol the channel east of the bridge"          # the order's rationale in the log

[[step]]                                      # the timeline: eval/storyline.py's step kinds
id = "fast-boat-enters"
at_s = 150
kind = "note"                                 # note | propose | flood | declare | link | engage
text = "A fast boat without AIS enters from the west (truth)"   # what the panel says when done
director = true                               # shown only when revealed

[[watch]]                                     # what to watch for, shown as each comes due
at_s = 240
text = "How the C2 labels a contact without AIS."   # what to look at, never the answer
director = false                              # true: shown only when revealed

[playbook]                                    # AI=playbook: agents/policy_agent.py --watch
near = [-122.4783, 37.8199]                   # the watch point
radius_km = 3
every_s = 10
platform = "puma"                             # the sensor platform it tasks (default: any unarmed aircraft)

[autoplay]                                    # the scripted operator: eval/proxy.py presets and keys
preset = "default"

[storyline]
proposer = "agent"                            # storyline proposals as agent-scripted (MCP), or
                                              # "operator": as op-planner (a second station, REST)
```

**One clock.** Every `at_s` (steps, watch items), `duration_s` and `ai_start_s` counts from the
stack's start: the link emulator's, which `scripts/up.sh` starts first. The events files' `at_s`
count on that clock too (the emulator's own; the stand-ins' GNSS events from their start, a
second or two later), so the existing files under `sim/link-events/` and `sim/events/` work
unchanged. The contacts process starts with the bridge a few seconds after the emulator, and a
contact's `start_s` counts from it. The fleet is usually ready and on its opening orders some 5
to 40 s in (stand-ins faster than SITL); timeline steps due before then run right after the
orders.

**Steps.** The storyline's kinds and selectors (`eval/storyline.py`'s docstring), with these
differences: a proposal needs no truth label or `reason` (nothing scores it); tracks are chosen
as the C2 shows them (`{track = {id = ...}}` or `{track = {select = ...}}`), never by contact
(that needs the evaluation's truth hook, which a situation leaves off); `link` steps append to
the run's own events file; `engage` steps nominate through the storyline's proposer and are
signed by the scripted signers in autoplay, or by you live. `text` and `director` are the
situation's own. Each kind takes its own keys only (a `declare` step never names its operator:
it is op-planner's), every `at_s` is before `duration_s`, and a value of the wrong type is
refused with the key named.

**Policies.** `policies/watch.toml` is `sim/policy.toml` with one tier before the default,
`agent-tasking`: an AI's orbit, patrol or track waits for the operator, however routine
(`eval/tests/test_situation.py` holds it to the shipped policy). Name `sim/policy.toml` to let
the shipped policy dispatch an agent's routine orbit itself.

Tests: `.venv/bin/python -m pytest eval/tests/test_situation.py` (the format, the status
document and what a blinded one hides, the launch contract, the AI's own directory and
environment, its process group, the preflight, the port block and check, the audit tail, the
watch policy, who acts as whom and the opening orders' dry run, the scripted signers against the
bridge's shapes, concurrent timeline proposals); the bridge's side in `cargo test -p
murmur-bridge situation` (blinding, the operator's dry run, what an agent can read).
