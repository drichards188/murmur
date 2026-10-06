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
  how to treat stale facts.
- **Other clients** (Claude Desktop, claude.ai connectors, the Claude API's MCP connector)
  need a reachable URL and cannot always set custom headers. Options: a local proxy such as
  `mcp-remote` with `--header "Authorization: Bearer $KEY"`, or a reverse proxy that adds
  the header. (On a dev bridge, listing the client's own `clientInfo.name` in
  `MURMUR_AGENTS` also works: every instance of that client then acts as that agent.)

Don't call an external model API from tests; the scripted agent is the test harness. Phase G
(evaluation, `eval/`) runs agents against seeded scenarios with the same tools.

## Tools

Reads (every value carries `as_of` and `age_s`; vehicle facts carry `stale` against the
rules' freshness limit for that vehicle's link):

| tool | what |
|---|---|
| `get_fleet`, `get_vehicle` | identity, platform limits and tasks, position, nav readiness and source, battery, home, link health, current task |
| `get_platforms` | the catalog: limits, link class, `armed`, tasks per kind, fleet members |
| `get_areas` | approved, no-go, engagement-zone and no-strike areas as `[lon, lat]` polygons |
| `get_policy` | the risk-tier policy and rules configuration in force |
| `get_intents` | intents of the run (filter by state, or `mine`) with checks, policy, approval, history |
| `get_inbox` | pending approvals (why each needs a human, when it expires) and alerts |
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
