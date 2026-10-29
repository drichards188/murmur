# Helping someone use Murmur

Read this part first. If the person is using or trying Murmur (explain it, set it up, run
something, watch it, give orders), follow it and skip "Developing Murmur" below. If they want
to change Murmur's code, read "Developing Murmur" and CLAUDE.md (the developer's notebook).

## What it is

An open-source command-and-control testbed: one operator supervising about a hundred simulated
air, ground, surface and undersea vehicles at the Golden Gate (real tide, current, wind and
harbor traffic), with AI watch officers that propose orders. It exists to measure whether human
judgment over the decisions that matter stays meaningful at that scale. Every order is checked
by a deterministic engine and routed by policy (out at once, or wait for a person); an
engagement needs a hostile declaration, a nomination and two human signatures. Say plainly that
it is **a working draft**: it runs end to end, but the evaluation is still being validated, so
there are **no findings to quote yet**. Nothing in it is a weapon or connected to anything real.

## 1. Check the machine

`make doctor` (or `./scripts/doctor.sh` without make): one line per check, `OK`, `MISSING`
(with the command that fixes it) or `WARN`; exit 1 if anything is missing. Prerequisites:
Linux or WSL2 (macOS untested), git, curl, [uv](https://docs.astral.sh/uv/), Rust (rustup),
Node 22.18+ with npm (nvm: `nvm install 22`), on Ubuntu `sudo apt install build-essential
pkg-config libssl-dev`, and about 8 GB of disk. Run a `sudo` line only with the person's
go-ahead.

## 2. Set up (once)

`make setup` takes about 30 minutes. Tell the person why: some 19 minutes is downloading the
Golden Gate's real conditions from NOAA (the bay's current model alone is about 730 MB), the
rest building ArduPilot's simulator (cloned to `~/ardupilot`) and Murmur's Rust binaries. Run
it detached and look in now and then; rerunning is safe (it keeps what it fetched and built):

```bash
mkdir -p logs && setsid nohup make setup > logs/setup.out 2>&1 < /dev/null &
tail -n 5 logs/setup.out     # ends "done. run: make start"; a failure's last lines say why
```

Then `make doctor` should end with `doctor: ready`.

## 3. Start something

| what | start | console | bridge (REST, MCP) |
|---|---|---|---|
| the menu, the person picks | `make start` | http://localhost:5173 | 8765 or 9265 |
| the open world | `make goldengate` | http://localhost:5173 | 8765 |
| a situation | `make situation NAME= OPERATOR= AI=` | http://localhost:5673 | 9265 |

A person who wants to click: `make start`, open http://localhost:5173, pick, Start (End stops
it). To start a chosen scenario yourself, detach it so it outlives your shell, then wait:

```bash
mkdir -p logs
setsid nohup make situation NAME=hostile-act-engagement OPERATOR=autoplay AI=playbook \
  > logs/situation.out 2>&1 < /dev/null &
.venv/bin/python scripts/wait_ready.py --port 9265     # "ready: ..." and exit 0: tell them to look
```

The open world: `make goldengate` the same way (`logs/goldengate.out`), then `--port 8765` (22
vehicles, 30 to 60 s). On exit 1 (`failed`, `timed out`) the end of the `.out` file says why. A
situation runs its set time (12 minutes), then keeps its stack up for inspection until `make
down`. If the menu is already running (`curl -s localhost:8760/launcher/api/state` answers),
start through it instead so the person sees it there; it wants the console's Origin header:

```bash
curl -s -X POST localhost:8760/launcher/api/start -H 'Origin: http://localhost:5173' \
  -H 'Content-Type: application/json' \
  -d '{"scenario": "hostile-act-engagement", "operator": "autoplay", "ai": "playbook"}'
```

Ready when the state's `current.stage` is `running`; `POST /launcher/api/end` (same headers)
ends it; `{"scenario": "open-world"}` takes no operator or AI.

## What to suggest first

1. **The tutorial** (`make start`, then the glowing Tutorial at the top of the menu; or `make
   situation NAME=tutorial`, console on 5673): for anyone new, about 8 minutes, no account. A coach
   card in the console walks the person through the engagement chain as it happens: a fast boat
   fires on one of ours, the engine declares it hostile by rule, the person confirms that in the
   Review list and nominates an engagement with REAPER 1 themselves, op-2 and op-3 (scripts, since a
   proposer never signs their own nomination) sign, then the strike and its assessment. Its modes
   are fixed (live, no AI). Every human decision in it is the person's: you start it and point
   them at the card, nothing more.
2. **`hostile-act-engagement`, `OPERATOR=autoplay AI=playbook`**: no account, nothing to do but
   watch, about 8 minutes for the whole engagement chain (a fast boat fires on a ship about 3:30
   in, the engine declares it hostile, a nomination, two signatures by scripts standing in for
   people, the strike, its assessment by 7 to 8 minutes). The Situation panel (top left) narrates.
3. **`harbor-watch`, `OPERATOR=live`**: the person at the console as `op-1`; an unknown fast
   boat near the bridge, the AI proposes, the person decides. Its AI defaults to Claude through
   their own CLI login; `AI=playbook` if they have neither Claude Code nor Codex.
4. **The open world**: 22 platform types, real conditions and traffic, nothing scripted.

More in sim/situations/README.md. `AI=claude` or `codex` runs the person's own CLI and login;
Murmur never sees their credentials.

## Watch and explain

Read-only REST, no identity needed in a dev run, on the bridge port above: `/api/state`
(vehicles), `/api/tracks` (contacts as our sensors see them), `/api/inbox` (what waits for a
person, alerts, the queue), `/api/intents` (every order and its state), `/api/review`
(declarations to confirm, flags), `/api/situation` (the panel: brief, phase, what to watch),
`/api/links`, `/api/stats`. Logs: `logs/audit/<run id>.jsonl` (the audit log), a situation's
`logs/situations/<run id>/situation.jsonl`. Explain in the console's terms (panel, Inbox).

For richer reads (`get_tracks`, `get_inbox`, `get_changes`, `get_nominations`,
`explain_intent`; agents/README.md lists them) connect to the bridge's MCP server as
`agent-planner`, the id for the person's own agent on every stack. The tools appear in a new
session (`claude --continue` or `codex resume` picks this one up). Claude Code:

```bash
claude mcp add --transport http murmur http://127.0.0.1:8765/mcp --header "X-Murmur-Agent: agent-planner"
claude mcp add --transport http murmur-situation http://127.0.0.1:9265/mcp --header "X-Murmur-Agent: agent-planner"
```

Codex, in `~/.codex/config.toml` (and the same as `murmur-situation` on 9265):

```toml
[mcp_servers.murmur]
url = "http://127.0.0.1:8765/mcp"
http_headers = { "X-Murmur-Agent" = "agent-planner" }
```

## Acting for the person: the boundary

You may start and stop scenarios, read and explain, and, when the person asks, propose ordinary
orders (orbit, patrol, hold, rtl, track, identify) with MCP `preview_intent` then
`propose_intent` as `agent-planner`, with a plain rationale. The policy sends routine ones out;
the rest wait in the console's Inbox for the person (in a situation, an agent's orbit, patrol
or track always waits). "Orbit the bridge" is `task` orbit, their `uids`, `center`
`[-122.4783, 37.8199]` (mid-span, `[lon, lat]`), `radius_m`. Without MCP, tell the person how
from the console (select vehicles, Orbit, click the map, Send).

**You never make the human decisions.** Never approve or reject an Inbox item, declare a track
hostile or confirm or revoke a declaration, resolve a review flag, accept a lost contact or a
blocker "as behaving", sign, stop or release an engagement, write off or un-spend a platform,
or enter a password or passkey. Those decisions are what Murmur exists to study, and they stay
with the person at the console. In a dev run (the default) the bridge believes whichever
operator a request names (`X-Murmur-Operator`), so the code will not stop you: this rule must.
Never send that header and never POST to the bridge's `/api` as an operator (an operator's
order counts as that person's own approval). If asked, say why not and point to the card in
the console. In autoplay the scripted operators (`op-auto`, `op-2`, `op-3`) decide, and in the
tutorial `op-2` and `op-3` sign the person's nomination; that is
the situation's script, not you.

## Stop

`make down` stops the stack this checkout started (open world or situation); End in the menu
does the same; Ctrl-C in `make start`'s terminal stops the menu (one you started detached:
`kill $(cat logs/launcher.pid)`, which ends its scenario first). `make doctor` then shows the
ports free.

## When it goes wrong

- **Ports in use** (doctor `WARN`, "something already answers on ..."): usually an earlier
  Murmur stack: `make down`, wait a few seconds, retry; something else (another Vite app on
  5173): stop it. A situation refuses to start while a stack answers on its ports.
- **Node too old**: `nvm install 22`, then rerun `make setup` (it reruns `npm install`).
- **Build fails**: `openssl-sys` is `libssl-dev pkg-config`; a missing compiler in ArduPilot's
  `waf` lines is `build-essential`. An existing `~/ardupilot` without Murmur's patches is never
  rewritten: doctor's `WARN` says how to patch it, or move it aside and rerun setup
  (`ARDUPILOT_HOME` points elsewhere).
- **Interrupted setup**: rerun `make setup`; a half-made `~/ardupilot.partial` starts over.
- **WSL**: WSL2 only; clone under the Linux home, not `/mnt/c`; the Windows browser reaches
  http://localhost:5173.
- **Vehicles slow or missing**: SITL takes 30 to 60 s; `logs/sim.log`, `logs/bridge.log`.

## Deeper questions

README.md (what, why, how it fits), docs/PRODUCT.md (the console's purpose and users),
sim/situations/README.md (situations, who is who), docs/DECISIONS.md (what was decided and
why), agents/README.md (MCP tools, the AI watch officers), docs/PLAN.md (the phases).

# Developing Murmur

For changing Murmur's code. CLAUDE.md is the developer's notebook (commands, gotchas,
conventions). The maintainer tracks work in bd (below); a contributor without bd works on a
branch and opens a pull request.

This project uses **bd** (beads) for issue tracking. Run `bd onboard` to get started.

## Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work atomically
bd close <id>         # Complete work
bd dolt push          # Push beads data to remote
```

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

<!-- BEGIN BEADS INTEGRATION -->
## Issue Tracking with bd (beads)

**IMPORTANT**: This project uses **bd (beads)** for ALL issue tracking. Do NOT use markdown TODOs, task lists, or other tracking methods.

### Why bd?

- Dependency-aware: Track blockers and relationships between issues
- Version-controlled: Built on Dolt with cell-level merge
- Agent-optimized: JSON output, ready work detection, discovered-from links
- Prevents duplicate tracking systems and confusion

### Quick Start

**Check for ready work:**

```bash
bd ready --json
```

**Create new issues:**

```bash
bd create "Issue title" --description="Detailed context" -t bug|feature|task -p 0-4 --json
bd create "Issue title" --description="What this issue is about" -p 1 --deps discovered-from:bd-123 --json
```

**Claim and update:**

```bash
bd update <id> --claim --json
bd update bd-42 --priority 1 --json
```

**Complete work:**

```bash
bd close bd-42 --reason "Completed" --json
```

### Issue Types

- `bug` - Something broken
- `feature` - New functionality
- `task` - Work item (tests, docs, refactoring)
- `epic` - Large feature with subtasks
- `chore` - Maintenance (dependencies, tooling)

### Priorities

- `0` - Critical (security, data loss, broken builds)
- `1` - High (major features, important bugs)
- `2` - Medium (default, nice-to-have)
- `3` - Low (polish, optimization)
- `4` - Backlog (future ideas)

### Workflow for AI Agents

1. **Check ready work**: `bd ready` shows unblocked issues
2. **Claim your task atomically**: `bd update <id> --claim`
3. **Work on it**: Implement, test, document
4. **Discover new work?** Create linked issue:
   - `bd create "Found bug" --description="Details about what was found" -p 1 --deps discovered-from:<parent-id>`
5. **Complete**: `bd close <id> --reason "Done"`

### Auto-Sync

bd automatically syncs with git:

- Exports to `.beads/issues.jsonl` after changes (5s debounce)
- Imports from JSONL when newer (e.g., after `git pull`)
- No manual export/import needed!

### Important Rules

- ✅ Use bd for ALL task tracking
- ✅ Always use `--json` flag for programmatic use
- ✅ Link discovered work with `discovered-from` dependencies
- ✅ Check `bd ready` before asking "what should I work on?"
- ❌ Do NOT create markdown TODO lists
- ❌ Do NOT use external issue trackers
- ❌ Do NOT duplicate tracking systems

For more details, see README.md and docs/QUICKSTART.md.

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds

<!-- END BEADS INTEGRATION -->
