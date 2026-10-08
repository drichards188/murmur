# Agents: AI agents via MCP (Phase F)

The bridge serves an MCP server at `/mcp` (streamable HTTP, built on the official Rust SDK,
`rmcp`) through which AI agents read the world model and propose intents. Agents decide,
they don't drive (`docs/DECISIONS.md`): **no approve tool exists**, and every proposal goes
through the same decision engine as an operator's command, as an agent-sourced intent.
Auto tiers dispatch; human tiers wait in the operator's inbox, which shows the agent's id and
rationale. Everything is in the audit log (`agent.action` events for connect, preview,
propose and withdraw, then the intent's own events), and replay checks that no agent-sourced
intent was ever approved without a human.

Source: `crates/murmur-bridge/src/mcp/` (the server), `crates/murmur-bridge/src/decision/agent.rs`
(the agent side of the decision layer, through `murmur_decide::AgentPort` only).

## Connecting

- Endpoint: `http://127.0.0.1:8765/mcp` (the bridge's HTTP port; `MURMUR_HTTP_PORT`).
- Identity (rules 8), in a credentials run (`MURMUR_AUTH=credentials`): the agent's own key,
  `Authorization: Bearer <key>` on every request.
  Issue one with `target/release/murmur-auth agent-add agent-planner` (printed once on
  stdout; the credential file keeps only its SHA-256, compared in constant time);
  `agent-rotate` replaces it, `agent-remove` revokes it, both at once on a running bridge. A
  request without a valid key is refused (401) before any MCP session exists; an
  `X-Murmur-Agent` header naming another agent than the key's is refused too. The audit log
  names the agent and the key's id (`mma_<id>_...`), never the key. Each agent gets its own
  engine port; its proposals carry its id as their source. A key is not a login: on an
  operator's endpoint it is a 401, and no tool reaches an approval either way.
- Dev run (the default: `MURMUR_AUTH` unset or `dev`; logged as such and excluded from any
  human-control claim): the
  `X-Murmur-Agent` header, or within an MCP session the `clientInfo.name` sent with
  `initialize`, names the agent, which must be listed in `MURMUR_AGENTS` (comma-separated;
  default `agent-planner,agent-watch`). Anyone who can reach the port can claim a listed id.

### The scripted agent (no model)

`agents/scripted_agent.py` connects with the official Python SDK (`mcp>=2`, in
`requirements.txt`), prints the fleet with every fact's age, previews and proposes a hold on a
routine vehicle (auto tier: dispatched at once) and an orbit for an aircraft around its own
position (for the stand-in fleet that is outside the approved area: human tier, waits in the
inbox), then follows both with `explain_intent`:

```
make up FLEET=standins-only            # or make goldengate (a dev run)
.venv/bin/python agents/scripted_agent.py --follow 120
# an operator approves in the console, or over the API:
curl -X POST http://127.0.0.1:8765/api/intents/<id>/approve -H 'X-Murmur-Operator: op-1'
```

In a credentials run (`MURMUR_AUTH=credentials make up ...`) the agent needs its key and the
operator their own session:

```
export MURMUR_AGENT_KEY=$(target/release/murmur-auth agent-add agent-planner)   # once
.venv/bin/python agents/scripted_agent.py --follow 120
curl -c jar -X POST http://127.0.0.1:8765/api/auth/login -H 'Content-Type: application/json' \
  -d "{\"operator\": \"op-1\", \"password\": \"$PASSWORD\"}"
curl -b jar -X POST http://127.0.0.1:8765/api/intents/<id>/approve
```

(An approval that steps up, such as an engagement signature, also carries the password again,
`"password"`; where the policy asks for a passkey (`[approval] step_up_factor = "passkey"`,
rules 10, a study always) only a person's passkey, at the console on localhost, signs it: an
agent holds no passkey and never authorizes either way.)

`--task auto|human|both`, `--uid N --task orbit|hold|rtl` to task a given vehicle,
`--withdraw` to withdraw a proposal still pending at the end, `--url`, `--agent` and `--key`
for a second stack or a second agent.

### The policy agent (no model; the evaluation's proposer)

`agents/policy_agent.py` is two things. As a library, `AgentSession` wraps an MCP session as
one agent id with typed helpers (fleet, tracks, preview, propose, withdraw, explain); a
proposal the rules refuse comes back as its intent with `refused`, not as an exception. The
evaluation harness (`eval/agent.py`) proposes through it. As a program, it watches the tracks
and proposes one sensor orbit per new suspect or unknown track near a point, with a rationale
naming the facts and their ages:

```
.venv/bin/python agents/policy_agent.py --watch 120 --near -122.478,37.818 --radius-km 4
```

The evaluation's real-model arm (`eval/model_agent.py`, opt-in and cost-capped; see
`eval/README.md`) relays these same tools to a Claude model through the Anthropic API.

### A real model

Any MCP client that speaks streamable HTTP and can set a header works (the key from
`murmur-auth agent-add`, below as `$KEY`):

- **Claude Code**: `claude mcp add --transport http murmur http://127.0.0.1:8765/mcp
  --header "Authorization: Bearer $KEY"`, then ask it to read the fleet and propose. The
  server's instructions (sent at `initialize`) tell the model what it can and cannot do and
  how to treat stale facts. (Claude Code 2.1.294 speaks MCP 2026-07-28, which requires
  `ttlMs` and `cacheScope` on list results; the bridge sends both since 10-08. Before that
  this Claude Code connected but offered the model no Murmur tool at all.)
- **As a watch officer that runs on its own**: `agents/watch_officer.py`, below.
- **Other clients** (Claude Desktop, claude.ai connectors, the Claude API's MCP connector)
  need a reachable URL and cannot always set custom headers. Options: a local proxy such as
  `mcp-remote` with `--header "Authorization: Bearer $KEY"`, or a reverse proxy that adds
  the header. (On a dev bridge, listing the client's own `clientInfo.name` in
  `MURMUR_AGENTS` also works: every instance of that client then acts as that agent.)

Don't call an external model API from tests; the scripted agent is the test harness. Phase G
(evaluation, `eval/`) runs agents against seeded scenarios with the same tools.

## The watch officer (an AI teammate)

`agents/watch_officer.py` runs a model as the operator's watch officer: it watches the
picture through the MCP tools, wakes the model when something worth a look changes, and the
model reads, proposes (with rationales), withdraws its stale proposals and writes a short
note for the operator. It is an agent like any other: it cannot approve or sign anything,
every proposal goes through the rules and the policy, and humans sign every effect. It may
nominate an engagement; two operators with engagement authority decide it.

**No model credentials in Murmur.** Murmur never stores, reads, proxies or prompts for a
model credential. The watch officer runs the CLI you installed and logged into yourself,
with your own Claude or ChatGPT plan, and removes the API-key variables from the CLI's
environment (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`; `CODEX_API_KEY`, `OPENAI_API_KEY`)
so that it uses your plan and not an API account. It reads only their names, to log that
they were removed.

### Setup

**Claude Code** (the default backend):

1. Install Claude Code (https://code.claude.com), version 2.1.294 or later (`claude
   --version`; `claude update`). Older versions are refused: the flags below were checked
   with 2.1.294.
2. Log in once with your Claude subscription: run `claude` and `/login`.
   `claude auth status` should say `"authMethod": "claude.ai"`; any other login is refused
   unless you pass `--allow-other-auth` (it may bill an API account).

**Codex CLI**:

1. Install it (`npm install -g @openai/codex`), version 0.161.0 or later (`codex
   --version`). Older versions are refused: they lack configuration keys the lockdown needs.
2. Log in once with your ChatGPT account: `codex login`; `codex login status` should say
   `Logged in using ChatGPT`.
3. Optional, recommended: a home of its own, so your global `AGENTS.md` and config stay out
   of the watch: `CODEX_HOME=~/.codex-murmur codex login` once, then
   `export MURMUR_CODEX_HOME=~/.codex-murmur`. Without it the CLI runs with your normal home
   (`~/.codex`), and its global `AGENTS.md` is read (`--ignore-user-config` keeps its
   `config.toml` out either way).

Check either without calling a model: `--preflight` (the CLI, its version and its login),
`--print-command` (the exact command lines).

### Running it

```
make goldengate                      # or any stack; a dev run here
.venv/bin/python agents/watch_officer.py --backend claude --url http://127.0.0.1:8765/mcp \
    --agent agent-watch --brief brief.md --situation-log logs/situation.jsonl --until <unix time>
```

`--backend codex` for Codex; `--minutes N` instead of `--until`; `--key` or
`MURMUR_AGENT_KEY` in a credentials run (the key goes to the CLI in its environment only,
never in a file or on its command line). It stops at `--until`, on SIGTERM or SIGINT, when a
budget is spent, or on a usage limit (below). `--shadow-playbook` also logs what the
deterministic policy agent (`agents/policy_agent.py`'s rule) would have proposed at each
wake, without proposing it.

### How it runs each backend

- **Claude Code**: one long-lived `claude -p` process with stream-json in and out, one user
  message per wake, resumed with `--resume <session id>` after a crash or a limit (the
  session id is in the run directory's `session.json` and the situation log). Locked down:
  `--mcp-config` (the bridge only) with `--strict-mcp-config`, `--setting-sources project`
  in an empty run directory outside any repository (your CLAUDE.md, hooks and plugins stay
  out), `--tools ""` (no built-in tools), `--allowedTools "mcp__murmur__*"`,
  `--disallowedTools Agent`, `--permission-mode dontAsk --permission-prompts none` (anything
  else is refused, never asked), `--system-prompt-file` (`agents/watch/prompt.md` and the
  brief), `--max-turns` per wake, `ENABLE_TOOL_SEARCH=false`, `CLAUDE_CODE_MAX_RETRIES=2`.
  Never `--bare` (it ignores the subscription login) and never `bypassPermissions`.
  Routine model `sonnet` at effort `medium`.
- **Codex**: one `codex exec --json` per wake, then `codex exec resume <thread id>`, the
  update on stdin. Locked down by `-c` configuration: `--ignore-user-config --ignore-rules
  --strict-config`, read-only sandbox, the bridge as the only MCP server (required, its
  tools approved and exposed directly), shell, web search, apps, plugins, sub-agents,
  browser and computer use, image generation, goals, memories, hooks, tool suggestions,
  skills and project docs off, the prompt as `developer_instructions`. Routine model
  `gpt-6-luna` at reasoning effort `high`. Never `ultra` effort (it spawns sub-agents).
- **The stronger model** (`opus` at `high`; `gpt-6.1-sol` at `high`) takes consequential
  moments, in a fresh short consult process each time: the system prompt and the update,
  not the routine conversation. Resuming or forking the routine session on another model
  would send that whole conversation again, uncached, at every escalation; the consult
  reads the picture itself (its earlier proposals are in `get_intents` with `mine`), and
  the routine model is told what it did at its next update.

### When it wakes

The driver polls `get_changes` every 2 s (and, while a track is declared hostile,
`get_nomination_options` every 15 s). The table is `TRIGGERS` in `agents/watch/wake.py`:

| change | wakes | stronger model |
|---|---|---|
| a track declared (by an operator or the hostile-act rule) | yes | when hostile |
| an armed platform passes every nomination check on a hostile track | yes | yes |
| a declaration revoked; a review flag raised | yes | |
| a new track that is suspect, unknown or hostile | yes | |
| an affiliation estimate moving to (or from) suspicion | yes | |
| an identity conflict (an AIS identity that does not fit what was seen) | yes | yes |
| a vehicle's link lost | yes | on an armed platform |
| a GPS glitch flagged by a vehicle's navigation filter | yes | yes |
| navigation not ready, or off GNSS on anything but a submarine | yes | |
| a failsafe; a high-severity alert | yes | |
| one of its own proposals approved, rejected, voided, expired, failed, queued or dispatched | yes | |
| the bridge restarted | yes | |

Anything else is a note that rides along with the next update. Bursts are coalesced (3 s
after the first waking change, at most 10 s), wakes are at least 10 s apart, a condition that
came and went inside one batch (a 2-second link flap, an alert that cleared itself) stays a
note, and a heartbeat wakes it every 60 s without a trigger (doubling while nothing at all
changes, up to 300 s) so it can look for slow developments and stale proposals.

### Budgets and limits

`--max-wakes-per-hour` (60; an escalation is not held back by it), `--max-turns-per-wake`
(12 tool turns), `--max-escalations` (6 a run), `--max-wakes` (400 a run), and for Claude
`--max-budget-usd` (the CLI's own cost estimate, summed across restarts; on a subscription it
is an estimate, not a bill). `--turn-timeout-s` (300) interrupts a turn that runs long.

Neither CLI waits for a usage limit to reset. Claude Code ends the turn with "You've hit
your session limit · resets 3:45pm" (weekly, Opus and Sonnet variants too) and its stream's
`rate_limit_event` gives the exact reset; Codex fails the turn with "You’ve hit your usage
limit. Try again at 3:42 PM." (or a date, or "later"). The driver logs `limit.hit` with the
parsed reset and, with `--on-limit wait` (the default), sleeps until the reset plus a minute
when that comes before `--until`, then resumes the session; otherwise it stops. A limit on
the stronger model only sends later escalations to the routine model until its reset.

### The situation log

One JSON object per line (`--situation-log`, appended; the situation framework may write
its own lines to the same file, ours have `"source": "watch_officer"`). Every line has `t`,
`iso`, `run` and `kind`; `agents/watch/situation.py` lists the kinds. The ones to read:

- `wake`: `n`, `reason` (start, changes, heartbeat), `why`, `escalated`, `model`, `text`
  (exactly what the model was told) and `changes` (the records behind it);
- `turn`: `n`, `model`, `tool_calls` (name, input, a result excerpt, `is_error`), `text`
  (its note for the operator), `proposed` (intent ids), `usage`, `cost_usd`, `duration_s`,
  `stop` (success, max_turns, timeout, limit, budget, error);
- `shadow`, `limit.hit`, `limit.resume`, `backend.crashed`, `backend.restart`,
  `budget.deferred`, `budget.exhausted`, `run.start`, `run.stop`.

```
jq -c 'select(.kind=="turn") | {n, model, stop, proposed, text: .text[:160]}' logs/situation.jsonl
jq -c 'select(.kind=="wake") | {n, reason, why, escalated}' logs/situation.jsonl
```

What the model proposed is also in the bridge's audit log as agent-sourced intents
(`agent.action` `intent.propose` with the rationale, then the intent's own events), and
replays like everything else.

### What was checked, and how

Against Claude Code 2.1.294 on a live stack (10-08): the stream-json message shape, the
`system/init` event (session id, MCP status, the tools the model sees: only the 18
`mcp__murmur__*` tools), `--setting-sources project` keeping the user's CLAUDE.md and hooks
out, `dontAsk` refusing a tool outside the allow rule, `${VAR}` expansion in the MCP config's
headers, `--max-turns` per message (an exceeded one ends that turn, the process lives), the
stream-json interrupt (ends the turn, the process lives; SIGINT ends both), `--max-budget-usd`
(per process, after each turn), `--resume` (same session, the conversation intact,
`total_cost_usd` from zero again). `--safe-mode` would keep the user's settings out too,
but it also drops the `--mcp-config` server, so it is not used. Three live smoke runs: the
model read the picture and proposed with rationales, a killed CLI was resumed, a SIGTERM
stopped it cleanly, and an operator's rejection woke it without a re-proposal.

Codex was not run live (the CLI installed here is 0.116.0); its flags follow the 0.161.0
documentation, and both backends are tested against stand-in CLIs (`agents/tests/`, in
`make check`) that print the documented events, limits and crashes. No test calls a model.

## Tools

Reads (every value carries `as_of` and `age_s`; vehicle facts carry `stale` against the
rules' freshness limit for that vehicle's link):

| tool | what |
|---|---|
| `get_fleet`, `get_vehicle` | identity, platform limits and tasks, position, nav readiness and source, the GPS glitch flag (`nav.gps_glitch`, with its age: the navigation filter rejects the fix, as a jammed or spoofed receiver looks), battery, home, link health, current task |
| `get_platforms` | the catalog: limits, link class, `armed`, tasks per kind, fleet members |
| `get_areas` | approved, no-go, engagement-zone and no-strike areas as `[lon, lat]` polygons |
| `get_policy` | the risk-tier policy and rules configuration in force |
| `get_intents` | intents of the run (filter by state, or `mine`) with checks, policy, approval, history |
| `get_inbox` | pending approvals (why each needs a human, when it expires), alerts, the queue, and the identification review (`review`: rule declarations to confirm, flags, lost-contact claims; read-only) |
| `get_changes` | what changed since a cursor, oldest first: tracks new, changed affiliation, declared, confirmed, revoked, in identity conflict, stale, dropped; review flags; vehicles joined, link lost or back, navigation changed, GPS glitch flagged; alerts raised and cleared; intents created or changed state (`mine` marks yours); vehicle events. Read-only, not logged. A digest of the views above compared between reads (any agent's read refreshes the shared feed), so a condition that came and went between two reads is not reported; `reset` after a bridge restart |
| `get_recent_events` | vehicle events the audit log recorded: mode changes, failsafes, status text |
| `get_satellite_passes` | imaging access windows over an area (read-only) |
| `get_satellite_tasking_options` | ranked collection options and exclusions (read-only) |
| `get_nominations` | engagement nominations (Phase E): checks, preview, signatures needed and given, the picture held since approval, the effect, a stop, the assessment |
| `get_nomination_options` | the nomination form's dry run for a declared-hostile track: each armed platform's checks and preview now, and `keep_outs`: which track or vehicle of ours blocks (or nearly blocks) the keep-out, the region that blocks and by how much, and what would clear it (the effect times, and for a holding target the distances, at which every keep-out holds on the picture now; a later nomination if everything keeps behaving as now). Read-only and not logged, at most one call a second per agent; listed by uid, not ranked; no keep-out is relaxed (murmur-12e) |

Proposing:

| tool | what |
|---|---|
| `preview_intent` | dry run: the checks and the policy tier now, no intent created (logged as `agent.action intent.preview`); for a collect without `chosen`, a tasking query whose space answer lists the ranked options |
| `propose_intent` | task + uids + params + constraints + **required** rationale; auto tier dispatches, human tier waits in the inbox. Vehicle tasks (orbit, patrol, hold, rtl, track) and satellite requests: `collect` (area, window, capability, `chosen` from the options) and `cover` (area, window, service), which go through the same `decision::space` path as an operator's |
| `explain_intent` | the checks, the tier, why a human, who decided and how, history, tasking or space run, a plain summary |
| `withdraw_intent` | the agent's own intents only, while they wait |

An engagement (Phase E) is nominated with `propose_intent`, `task = "engage"`: `track_id` (a
track declared hostile: by a human operator, or from rules 9 by the engine's rule on a
hostile act our sensors observed, which the track's `declared` names with its evidence and
whether an operator confirmed it), one armed platform in `uids`, `effect_radius_m`,
`fallback` (`abort`, `return` or `loiter`), optional `window_s`, and the rationale. The effect
is an abstract event (a point, a radius, a time). It always waits for two human operators
with engagement authority; the agent can preview it and follow it (`get_nominations`,
`explain_intent`) and withdraw it while it waits, never sign, stop or release it: no such
tool exists, and the engine's agent port has no such method. While an engagement is in play
on a vehicle (pending, approved or under way), any order the agent proposes for that vehicle
waits for a human, whatever its tier (`(engagement in play)`). An agent can't declare a
track, confirm or revoke a declaration, or resolve a review flag either: those are an
operator's, and the engine's own rule is a deterministic function of logged facts, not an
agent's call. Keeping company with a declared hostile raises a track to suspect at most.

A collect flows as: preview (or `get_satellite_tasking_options`) for the options, propose with
`chosen = {norad_id, collect_time}` and a rationale, wait in the inbox (every collect needs a
human; it expires when its pass can no longer be tasked), then the simulated run (`tasked`,
`collected`, `product_ready`) in `explain_intent` and `murmur://space/runs`. A cover commands
nothing and runs on its own.

Resources mirror the reads: `murmur://fleet`, `murmur://fleet/{uid}`, `murmur://platforms`,
`murmur://areas`, `murmur://policy`, `murmur://intents`, `murmur://intents/{id}`,
`murmur://inbox`, `murmur://events`, `murmur://space/passes`, `murmur://space/runs`.

## The guarantee, and how it is tested

- `murmur_decide::AgentPort` has no approve method and its engine reference is private;
  `HumanAuthority` cannot be built from JSON (compile-fail doctests in `crates/murmur-decide`).
- The MCP server holds `AgentPort`s only; `crates/murmur-bridge/src/mcp/tests.rs`:
  - `no_tool_implies_approval_or_control` enumerates the tool list and fails on any name
    containing an approval or control stem, any description where the tool itself approves,
    authorizes, dispatches or drives, or a writer tool not marked as such;
  - `tools_that_would_approve_or_drive_do_not_exist` calls `approve_intent`, `set_mode`,
    `arm_vehicle` and friends: unknown tools;
  - `no_tool_moves_a_pending_intent_toward_approval` proposes a human-tier intent and calls
    every tool against it: it stays pending (or voided by its own withdraw), with no
    `approval.granted`, `approval.at_submission` or `intent.dispatched` in the log;
  - the auto-tier and human-tier flows, withdraw of another agent's intent (refused), preview
    without creating an intent, staleness on facts, and the `/mcp` endpoint over HTTP with
    the agent header; every test replays its audit log.
