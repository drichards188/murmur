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
- Identity: the `X-Murmur-Agent` header on every request, or, within an MCP session, the
  `clientInfo.name` sent with `initialize`. The id must be listed in `MURMUR_AGENTS`
  (comma-separated; default `agent-planner,agent-watch`). This is placeholder authentication,
  like the operator's `X-Murmur-Operator` header: anyone who can reach the port can claim a
  listed id. Each id gets its own engine port; its proposals carry it as their source.

### The scripted agent (no model)

`agents/scripted_agent.py` connects with the official Python SDK (`mcp>=2`, in
`requirements.txt`), prints the fleet with every fact's age, previews and proposes a hold on a
routine vehicle (auto tier: dispatched at once) and an orbit for an aircraft around its own
position (for the stand-in fleet that is outside the approved area: human tier, waits in the
inbox), then follows both with `explain_intent`:

```
make up FLEET=standins-only            # or make goldengate
.venv/bin/python agents/scripted_agent.py --follow 120
curl -X POST http://127.0.0.1:8765/api/intents/<id>/approve -H 'X-Murmur-Operator: op-1'
```

`--task auto|human|both`, `--uid N --task orbit|hold|rtl` to task a given vehicle,
`--withdraw` to withdraw a proposal still pending at the end, `--url` and `--agent` for a
second stack or a second agent id.

### A real model

Any MCP client that speaks streamable HTTP and can set a header works:

- **Claude Code**: `claude mcp add --transport http murmur http://127.0.0.1:8765/mcp
  --header "X-Murmur-Agent: agent-planner"`, then ask it to read the fleet and propose. The
  server's instructions (sent at `initialize`) tell the model what it can and cannot do and
  how to treat stale facts.
- **Other clients** (Claude Desktop, claude.ai connectors, the Claude API's MCP connector)
  need a reachable URL and cannot always set custom headers. Options: a local proxy such as
  `mcp-remote` with `--header "X-Murmur-Agent: agent-planner"`, or a reverse proxy that adds
  the header; or list the client's own `clientInfo.name` in `MURMUR_AGENTS` (every instance of
  that client then acts as that agent, which is the placeholder-auth caveat above).

Don't call an external model API from tests; the scripted agent is the test harness. Phase G
(evaluation) runs agents against seeded scenarios with the same tools.

## Tools

Reads (every value carries `as_of` and `age_s`; vehicle facts carry `stale` against the
rules' freshness limit for that vehicle's link):

| tool | what |
|---|---|
| `get_fleet`, `get_vehicle` | identity, platform limits and tasks, position, nav readiness and source, battery, home, link health, current task |
| `get_platforms` | the catalog: limits, link class, `armed`, tasks per kind, fleet members |
| `get_areas` | approved and no-go areas as `[lon, lat]` polygons |
| `get_policy` | the risk-tier policy and rules configuration in force |
| `get_intents` | intents of the run (filter by state, or `mine`) with checks, policy, approval, history |
| `get_inbox` | pending approvals (why each needs a human, when it expires) and alerts |
| `get_recent_events` | vehicle events the audit log recorded: mode changes, failsafes, status text |
| `get_satellite_passes` | imaging access windows over an area (read-only) |
| `get_satellite_tasking_options` | ranked collection options and exclusions (read-only) |

Proposing:

| tool | what |
|---|---|
| `preview_intent` | dry run: the checks and the policy tier now, no intent created (logged as `agent.action intent.preview`); for a collect without `chosen`, a tasking query whose space answer lists the ranked options |
| `propose_intent` | task + uids + params + constraints + **required** rationale; auto tier dispatches, human tier waits in the inbox. Vehicle tasks (orbit, patrol, hold, rtl, track) and satellite requests: `collect` (area, window, capability, `chosen` from the options) and `cover` (area, window, service), which go through the same `decision::space` path as an operator's |
| `explain_intent` | the checks, the tier, why a human, who decided and how, history, tasking or space run, a plain summary |
| `withdraw_intent` | the agent's own intents only, while they wait |

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
