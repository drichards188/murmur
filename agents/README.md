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
  default `agent-planner,agent-watch,agent-playbook`). Anyone who can reach the port can claim a listed id.

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
.venv/bin/python agents/policy_agent.py --watch 120 --near=-122.478,37.818 --radius-km 4
```

The evaluation's real-model arm (`eval/model_agent.py`, opt-in and cost-capped; see
`eval/README.md`) relays these same tools to a Claude model through the Anthropic API.

### The playbook engine (no model; the free option, the baseline, the shadow)

`agents/playbook/` is Murmur's deterministic AI agent (murmur-t8m, step 1): a library of named,
versioned **plays** (a trigger read from facts, a few checks, one proposal, its withdraw
condition, its material change, its rationale template) run on a 2 s tick against one read of
the world, with an **arbiter** that guards the operator's inbox and a **ledger** that remembers
every card and how the operator answered it. It is what `AI=playbook` runs in a situation (as
`agent-playbook`), the baseline a model is to be measured against (the evaluation's arm moves
to it in step 2; `policy_agent.py` and `eval/agent.py`'s `AgentPolicy` are unchanged), and the
shadow beside the model watch officer. What "useful" means, frozen before its first live run,
is `agents/playbook/CRITERIA.md`; the design and where it departs from the research is in
`docs/DECISIONS.md` ("The playbook engine").

```
make goldengate        # or any stack (agent-playbook is in the bridge's default MURMUR_AGENTS)
.venv/bin/python agents/playbook_agent.py --url http://127.0.0.1:8765/mcp --config playbook.toml \
    --situation-log logs/playbook.jsonl --minutes 30
```

**Each tick** reads `get_fleet` (compact), `get_tracks`, `get_inbox`, `get_intents` (mine: the
newest 50, and `explain_intent` for any older card of ours the ledger still holds open) and
`get_changes` since its cursor (the platform catalog and the policy once a session; while a link
is lost, that vehicle's full facts for its `lost_after_s`, and `explain_intent` once for an order
of someone else's a gap filler would take over; after a truncated change feed, `get_nominations`
for the engagements it would have reported; when a track is worth a look, one `get_link_coverage`
for the predicted link of the orbit each free sensor aircraft would fly, which `isr.investigate`
ranks on). None of these is logged. At every start and
change-feed reset the ledger is checked against the bridge, whatever `run` says (a resumed run
keeps its intent ids; a fixed run id over a restart does not): a card is kept only if
`explain_intent` shows the same intent, ours, with the same task and vehicles; memory per vehicle
only if the same platform and callsign are under the uid; memory per track is dropped across a
bridge restart (live track ids follow detection order), and a card carried over it whose target
is a track id has an unknown target (pending, it is withdrawn, "track ids restarted with the
bridge"; running, it is left alone). What is dropped is said in the panel (`ledger.verified`),
and the ledger as it was is kept beside (`playbook-ledger.<run>.json`). Only an answer drops a
card ("unknown intent", or not ours): a timeout or a dropped session aborts the check, leaves the
ledger as it was, says so (`ledger.unverified`), and the engine acts on nothing until a check
completes. Ended cards are checked only across a restart, and pruned once nothing can use them.
The tick's time is the
oldest `t` among them. Between ticks it reads the change feed every 0.5 s and ticks at once on a
change that wakes it (one of its cards decided, a link lost or back, a new track, an affiliation,
a declaration, an alert). Then, a pure function of the reads, the config and the ledger:

1. **beliefs**: contacts of interest (inside a watch area, or a closest approach to a protected
   point), CPA and TCPA along the track's velocity, which of our sensors see each track, vehicle
   health (link, navigation, GPS glitch, position, battery), free vehicles, inbox pressure,
   engagements in play. A vehicle is **free** when healthy, its navigation not suspect, its
   battery at least the low-reserve line plus 10 points, idle (no order, or a failed one: a
   vehicle on someone's running order is theirs), not recovering on its own (any return,
   landing, docking or surfacing mode of the bridge's mode tables: `get_fleet` has no failsafe
   field, so the mode is the sign), and nothing of ours, no other proposer's card waiting in the
   inbox, and no engagement claims it.
2. **plays** (each threshold's reason in `defaults.toml` and CRITERIA.md):

   | play | class | trigger | action | ends or withdraws |
   |---|---|---|---|---|
   | `isr.investigate` | P2 | a track shown unknown or suspect (not declared), not stale, existence confidence at least 0.5, inside a watch area or closing on a protected point (CPA within 457 m inside 10 min), and its classification under 0.6 (unless an EO/IR or sonar of ours detected it in the last 10 s and the classification rose by 0.02 in the last 30 s, for at most 120 s per track), faster than 8 m/s, or an identity conflict | an orbit by the free unarmed sensor aircraft (version 2: those whose predicted link holds for the orbit first, from one `get_link_coverage` call a tick for every free one, a link not known counting as holding; then preferred platform, ETA, battery, uid; the rationale gives the predicted link and whom it was chosen over for theirs, and when none holds the look still goes and says so) on the track carried to now, radius max(1.1 x min turn, a quarter of its imaging range) | shown neutral or assumed friend at 0.5 or more; unseen 60 s; out of the watch (with hysteresis); its sensor unfit |
   | `isr.recentre` | P2 | an orbit of ours on a track, the track more than half the radius off centre | the same vehicle, a new orbit on the track; once per vehicle per 60 s | as investigate |
   | `isr.release` | P2 | an orbit of ours (a look or an armed overwatch) running whose premise is gone | a hold, or the order of ours the vehicle was flying when ours superseded it, if the premise of the play that first made that order still holds | |
   | `isr.armed_overwatch` | P2, human | off unless a situation enables it: a suspect at affiliation confidence 0.5 or more closing on a protected point, no track shown neutral or assumed friend within the radius | an armed orbit by a free armed platform whose navigation is not suspect | confidence under 0.5; a protected track inside the radius; stale |
   | `link.cover_gap` | P3 | a vehicle on an orbit or patrol silent for 2 x its `lost_after_s` | the nearest free capable vehicle takes over the route or centre | the link back (a waiting filler withdrawn; a running one kept) |
   | `link.withdraw_pending` | P3 | a card of ours waiting for the operator (pending approval) for a vehicle whose link is lost (one approved and queued for the link is the queue's) | withdraw: "link lost at <time>" | |
   | `nav.suspect_exclude` | P3 | navigation not ready, off GNSS or a GPS glitch flagged (now or within 60 s) | the vehicle excluded from tasking; an armed card of ours on it pending approval withdrawn, and until murmur-su0 lands an approved or queued one too, from that state only | |
   | `safety.low_reserve` | P0 | battery under the policy's reserve plus 5 points, link up, not recovering on its own | rtl | not sent again while someone else's order newer than our rtl runs on the vehicle (an operator's, or another agent's the policy or a person approved; the rationale says which), nor while its order reads null after our rtl; once more if the battery falls under the policy's own reserve, said so |
   | `safety.stuck` | P0 | a `not_moving` or `task_failed` alert in our reads for 30 s (from the first read that showed it), link up, not recovering on its own, not holding | hold, quoting the bridge's detail | the vehicle not given the task it was stuck on again until the alert clears and 300 s more |
   | `hygiene.withdraw_stale` | P4 | a card of ours pending approval whose play's withdraw condition holds | withdraw, with the reason | |

3. **arbiter**: one total order (class, minus score, play, target, uid); an engagement in play on a
   vehicle (a nomination pending, approved, under way or in its assessment) holds every ISR, link
   and hygiene order for it, and the panel says what the playbook would have done (`withheld`),
   while a P0 command goes on: the bridge makes it a human card that stops the engagement if
   approved, and the rationale says so; a cooldown in force holds a candidate unless its play
   finds a material change (a rise in suspicion, a new declaration, a closest approach halved by
   half the alert); a back-off holds its key after a failing preview, a rule-book veto, a refusal
   at submission or an order of ours that failed, and its vehicle after a failed order unless it is
   P0 (10 s, doubling to 160 s: retried, never dropped, never every tick; a retry names the
   failure); a play and target the budget held after a human-tier preview waits for the next
   token or a decision on a card of ours (at most the approval wait plus 30 s); one claim per vehicle (a look offers its first three
   free vehicles, so a taken one falls to the next); at 6 or more pending cards in the inbox only
   P0 and P1 propose.
4. **preview** first (at most 3 a tick), then the **inbox budget**: the preview says the tier, and
   only a human-tier answer needs a token (one per 30 s, burst 2) and fewer than 2 of ours
   pending; P0 is exempt by class, neither held nor charged. A card the budget holds is not
   previewed again until the next token is due or a card of ours waiting then is decided.
5. **the rule book's veto**: `eval/evidence.py` (v2, frozen) on the preview's checks and, for an
   armed task, the tracks as read: anything it objects to is never proposed.
6. **propose** with the rationale.
7. **follow up**: its cards still pending approval whose premise is gone are withdrawn, with the
   reason and `if_state: pending_approval`, so an approval that came meanwhile stands (the bridge
   refuses the withdrawal, `withdraw.refused`); a card a person approved (approved, queued) whose
   premise went is said once in the panel (`left`) and left to the operator, except, until
   murmur-su0 lands, an armed one on a vehicle whose navigation is suspect: withdrawn from its state
   only, "withdrawn (armed, nav suspect, murmur-su0)".

**Rationales** are rendered by a fixed template per play from an **evidence record** built only
from values the tick read (each fact with its age, the derived CPA, TCPA and ETA, the thresholds,
the alternatives considered, the rule book's verdict, the preview's tier, the withdraw condition,
and inside a cooldown the material change), named by play and version. The record rides after the
line `--- evidence record (playbook, JSON) ---` (`playbook/rationale.py` composes and parses it);
the console shows the text and folds the record behind a toggle, on a card proposed by an agent
id it knows runs the playbook (`agent-playbook`) only, so no other proposer can hide text under
the fold. **The ledger** (JSON beside the
log, `playbook-ledger.json`) keeps every card by (play, target, vehicle): a rejection cools that
play on that target for 10 minutes whatever the vehicle (the operator's note is the only reason
channel, so the conservative reading), an expiry 2 minutes and halves the play's score for it, a
void 30 s for the key, a withdrawal of ours 30 s: for that play on that target whatever the
vehicle when the premise about the target went, for that vehicle alone when it was for the
vehicle's sake (its link, its navigation, its battery, recovering). An order of ours that failed backs off its key and its vehicle; one superseded by
someone else's order records that order (`safety.low_reserve` reads it); a refusal at submission
backs off and is logged with the bridge's reason, never cooled (no person answered it). A restart
adopts its cards from their records, the armed flag included.
**The config** is the shipped `defaults.toml` under a situation's `[playbook]` table
(`sim/situations/README.md`); its hash goes to the bridge as the MCP client's version at connect,
so the audit log's `agent.action session.connected` reads `client agent-playbook playbook-1 config
harbor-watch#<hash>`, and `run.start` in its log carries the whole config.

Its situation log (`--situation-log`, the watch officer's `{t, kind, text}` lines, `"source":
"playbook"`): `run.start` (the config and its hash), `play.status` (a play's status once it has
held 3 ticks and differs from the last one logged: active, ready, or not ready with the blocking
constraint), `proposal` (a refusal at submission with its reason), `withdrawal`,
`withdraw.refused`, `decision` (how the operator answered a card, and the cooldown it set),
`withheld` (what an engagement in play kept it from doing), `left` (an approved card of ours whose
premise went), `ledger.verified` (what checking the ledger against the bridge dropped),
`ledger.unverified` (a check that did not finish, and why),
`feed.truncated`, `error`, `run.stop` (ticks and compute per
tick). The panel shows the status, proposal, withdrawal, withheld, left and decision lines.

**Shadow mode** (`watch_officer.py --shadow-playbook --shadow-config <file>`): the same engine
beside the model, through the driver's session, on its own 2 s clock: it reads only (no preview,
no proposal, no withdrawal; it reads no intents of its own), logs what it would propose as `shadow`
lines with the rationale and record, what it would withdraw as `shadow.withdraw`, a would-be card
past the policy's approval wait as `shadow.lapsed` (then cooling as expired), and each play's
status as `shadow.status`. It never previews (a preview would be logged under the model's agent
id), so it estimates each would-be card's tier from `get_policy` and `get_areas`
(`playbook/tier.py`, said in every line): a human-tier one is held as pending, claims its vehicle
and spends the budget as the live engine's would; an auto-tier one is held as running until its
premise goes. The estimate walks the policy's tiers and applies the bridge's floors (an
unverified check, an engagement in play, a human-approved order waiting in the queue: human); it
does not model a check that would fail, the leg home and the transit (the compact read carries no
home), unverified outcomes other than reserves without an endurance figure, or the rules version
gates (`playbook/tier.py` lists them).

**Checked live** (stand-ins, 10-09, on a port block of their own; every audit log replayed exactly
with its anchors, every proposal's text faithful to its record by `agents/playbook/fidelity.py`):
Harbor watch on autoplay three times (the look on the fast boat proposed 177 to 179 s in, the
tick that first saw it closing on the bridge, then a recentre about 33 s later; the connect line in
the audit log reads `client agent-playbook playbook-1 config harbor-watch#6ddb1a6035e3`); Harbor
watch live twice, op-1 rejecting the first card with a note over the operator API (the first run
re-proposed 4 s later on a closest approach that had wandered from 192 m to 79 m, which is why
the closest approach is now material only when it halves by half the alert; the second held its
cooldown to the end of the run, previewing nothing more on that track); and a scratch variant
with scripted link cuts (PUMA 2 cut: `link.cover_gap` proposed PUMA 1 on PUMA 2's station orbit
7 s after the cut; PUMA 1 cut while that card waited: `link.withdraw_pending` withdrew it 4 s
after). Compute per tick about 0.3 ms at 4 vehicles live and 4 ms at 100 (synthetic read), against
the 50 ms target. What each run found and changed is in CRITERIA.md's dated section.

**Checked live after fix round 1** (10-09; both audit logs replayed exactly with their anchors,
every proposal faithful): Harbor watch on autoplay (`situation-harbor-watch-20261009T175910Z`):
the look on the fast boat at 177.6 s, as before (its trigger now "closing on the Golden Gate
Bridge; fast", REAPER 1's EO/IR having the boat), a recentre at 210.8 s, 2 previews for 2
proposals and 11 status lines in 12 minutes; the recentre's upload failed on PUMA 1's lossy link
("mission upload unconfirmed") and the ledger backed off its key and PUMA 1 (10 s), the look it
superseded read as ours, no back-off; both Pumas then went home on their own failsafes, which the
look's status names. And a scratch variant with the low-reserve line raised to 99 %
(`situation-harbor-retask-20261009T181358Z`, operator live): rtl for both Pumas at 93 s; op-1
sent PUMA 1 back on station over the operator API (i6) at 128 s, and for the remaining 410 s,
its battery falling from 98 % to 95 %, under the line throughout, the playbook sent it nothing
("i6, someone else's order after our rtl i4: not sent again while it stands"). In the same run a
scratch client (agent-scripted) withdrew with `if_state: pending_approval` an intent op-1 had just
approved (refused, 409, nothing logged; the audit log holds only the other withdrawal) and one
still waiting (voided).

**Checked live after fix round 2** (10-09; replayed exactly with anchors, every proposal
faithful): Harbor watch on autoplay (`situation-harbor-watch-20261009T192358Z`), the same story as
after round 1: the look at 177.4 s (trigger "closing; fast", the classification rising under
REAPER 1's EO/IR), a recentre at 210.5 s whose upload failed on PUMA 1's lossy link, 2 previews
for 2 proposals. A live check of a P0 card on an engaging platform was tried on a scratch
situation (`situation-harbor-engage-20261009T193718Z`: the engagement demo fleet, op-1 declaring
the fast boat hostile, a scratch agent to nominate): no armed platform passed the nomination
checks in its 400 s (identity under 0.60, the effect point outside the engagement zone, a neutral
fishing boat in the keep-out), so no engagement was in play and the playbook's five rtls went
out by policy. That behaviour rests on its tests and the bridge's own (mcp tests, "engagement in
play").

**Checked live after fix round 3 and the merge of master** (10-09,
`situation-harbor-watch-20261009T200944Z`, autoplay; replayed exactly with anchors, both proposals
faithful): the look at 178.9 s and the recentre at 210.5 s as before. This time PUMA 1's link
dropped with the recentre approved: it waited in the bridge's queue (said once, `left`, while
PUMA 1 went home on its own failsafe), came back to the inbox after the queue's hold, and the
playbook then withdrew it from pending approval, the sensor being the reason ("PUMA 1 is
recovering on its own"), so the cooldown it left is PUMA 1's on T-0008 alone, not the pair's.

Not seen by any play yet, said so: fewer than six satellites (the compact fleet read has no
satellite count) and a spoof that keeps every navigation flag good (no navigation-integrity fact
exists; PLAN.md). The engagement plays, `ident.review_support` and `isr.reacquire_lost` are step 3.
Tests: `agents/tests/test_playbook_*.py` (recorded and synthetic reads, a fake session; no bridge,
no model).

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
budget is spent, or on a usage limit (below). `--shadow-playbook` also runs the playbook
engine beside the model, reading only, and logs what it would have proposed (`--shadow-config`:
its config, a situation's `[playbook]` table; "The playbook engine", shadow mode).

The CLI runs in a run directory: by default a fresh private temporary one. `--run-dir` takes
a new or empty directory only, with no `CLAUDE.md`, `AGENTS.md`, `.claude` or `.git` in it or
above it (a CLI would read them; your own `~/.claude` is allowed, `--setting-sources
project` keeps it out); a directory it creates is made private (0700), one that exists is
left as it is. Paths are compared resolved, so a home or a temporary directory reached
through a symlink is recognised. A `CLAUDE.md` or `AGENTS.md` in your home directory itself
makes every run directory under your home refused (the CLI would read it), which is one
reason the default is a temporary directory: if your `TMPDIR` is under such a home, pass
`--run-dir` somewhere else.

### In a situation: the launch contract

A situation (`sim/situations/README.md`, `make situation NAME=<name> AI=...`) starts its AI as a
subprocess once the fleet is on station (at the situation's `ai_start_s`), and stops it at the
end of the situation (SIGTERM to its process group, SIGKILL 8 s later). The runner survives the
AI exiting early, crashing, or never doing anything; the panel shows its status.

Before the stack starts, the runner asks the chosen CLI whether it is logged in (`claude auth
status`, JSON `loggedIn` and `authMethod`; `codex login status` where the installed Codex has
it, else the check is skipped with a note) and keeps only yes or no and the kind of login; a
missing or logged-out CLI stops the run there, saying how to fix it (`claude` then `/login`, or
`codex login`) and that `AI=playbook` is the free option. Nothing falls back silently. The watch
officer then runs its own preflight (the CLI's version and login, and for Codex the features
check below) and, if that fails, logs `preflight.failed` with the fix and exits 2; the run goes
on without an AI and the panel says so.

`AI=claude` or `AI=codex` runs:

```
.venv/bin/python agents/watch_officer.py --backend claude|codex --url <bridge>/mcp \
    --agent agent-watch --brief <brief file> --situation-log <path> --until <unix time> \
    --shadow-playbook --shadow-config <playbook.json> --shadow-config-name <situation> \
    [--codex-home <dir>]
```

| argument | what |
| --- | --- |
| `--backend` | `claude`: the user's own Claude Code (`claude -p`); `codex`: the user's own Codex CLI (`codex exec`). Shown in the panel as "Claude" or "ChatGPT (Codex)". |
| `--url` | the bridge's MCP endpoint (`http://127.0.0.1:<MURMUR_HTTP_PORT>/mcp`), streamable HTTP; a dev run, so the agent names itself with `X-Murmur-Agent` |
| `--agent` | `agent-watch` (listed in the run's `MURMUR_AGENTS`) |
| `--brief` | a Markdown file with the situation's title and brief: what a watch officer is told (appended to its system prompt). Never the director's notes, the timeline or the watch items |
| `--situation-log` | the watch officer's own JSON-lines log ("The situation log", below): every line has `t` (unix s), `kind` and `text`, one short line for a person. The runner tails it (read-only) and shows the lines meant for a person in the panel as the AI's notes: the model's note after each turn (`turn`, shown as "note"), why it was woken (`wake`, "woken"), and its errors, waits and stop; the plumbing (`bridge.connected`, `backend.start` and the like) stays in the file, which the runner keeps with the run |
| `--until` | unix time at which to stop on its own (the situation's end) |
| `--shadow-*` | run the playbook engine beside the model on the situation's `[playbook]` table (`playbook.json` in the AI's directory), reading only, and log what it would have proposed and withdrawn (`shadow`, `shadow.withdraw`, `shadow.lapsed`, `shadow.status` lines; not shown in the panel) |
| `--codex-home` | passed only when the user's environment sets `MURMUR_CODEX_HOME` (Setup, Codex step 3), since the runner strips every `MURMUR_*` variable from the AI's environment |

The runner starts it in an empty directory of its own for this run, outside the repository and
outside the run directory (`/tmp/murmur-<run id>-ai-*`), holding only `brief.md` and
`playbook.json` (the situation's `[playbook]` table, for the shadow);
`--situation-log` names a file in that directory, and the runner keeps a copy with the run and
removes the directory at the end. The watch officer's CLI then runs in a run directory of its
own, another fresh private temporary directory, which passes the check above. Its environment
is the runner's minus every `MURMUR_*` variable (no simulation token, no agent key from
elsewhere), so the user's own CLI login works as it does in their shell. Its stdout and stderr
go to `agent.log` in the run directory.

**MCP only.** A situation hides things from the watch (the director's notes, the timeline to
come, the runner's logs; with `reveal` off the bridge strips them from `GET /api/situation`
too), and a model with a shell or file tools could read them from the checkout. So the watch
officer runs its model with the bridge's MCP tools and nothing else, and does not start when
it cannot (each in detail under "How it runs each backend"):

- Claude: `claude -p --strict-mcp-config --mcp-config <its own config naming only the bridge's
  /mcp as server "murmur", with the X-Murmur-Agent header> --tools "" --allowedTools
  "mcp__murmur__*" --disallowedTools Agent --setting-sources project --permission-mode dontAsk
  --permission-prompts none`, in its empty run directory (no built-in tools, no other MCP
  server, no user or repository settings, nothing asked). A tool the CLI refused is logged; a
  tool that ran stops the run.
- Codex: `codex exec` with every feature `codex features list` shows whose name says it runs a
  shell or a command, runs model-written code, reads or writes files, or reaches beyond the
  bridge disabled with `--disable <name>` on every exec (`shell_tool`, `unified_exec`,
  `js_repl`, `code_mode` and the rest: 32 of the 52 on 0.116), the list then checked to show
  each of them off and to list `shell_tool` and `unified_exec` at all, or the backend refuses
  to start; only the Murmur MCP server configured; an item outside the lockdown, or of a type
  it does not know, stops the run. The read-only sandbox and the approval policy `never` stay on
  as a second fence, but they are not MCP-only on their own: with a shell the model could
  still run commands that read the disk.

What it holds to besides: agents propose and never approve (no approve tool exists, and the
watch policy sends its tasking to the operator); every proposal carries a rationale naming the
facts and their ages; it reads only the MCP tools, never the bridge's HTTP API (`GET
/api/situation` is the runner's display for the console, and the bridge serves truth to no
agent: its evaluation hook, `MURMUR_EVAL_TRUTH`, is off in a situation). The bridge keeps an
idle MCP session 15 minutes and Claude Code holds none (below), so a quiet watch keeps its
tools; the runner's own scripted agents connect per act. With `REVEAL=1` the director's notes
and the timeline to come are on the bridge for anyone who asks, so a run with `REVEAL=1` and a
model AI is marked "not evidence about the AI (truth was served on the bridge)" in
`situation.jsonl`, the panel and the summary. What it did is in the audit log (`agent.action`,
then its intents' own events), which the panel reads too.

`AI=playbook`: `agents/playbook_agent.py --url <bridge>/mcp --agent agent-playbook --situation-log
<path> --until <unix time> --config <playbook.json> --config-name <situation>`, the playbook engine
on the situation's `[playbook]` table (deterministic, no model, no account; shown as "Playbook
(deterministic)"; "The playbook engine" above). In the same directory, with the same situation
log contract: its play statuses, proposals, withdrawals and decisions are the lines the panel
shows. `AI=none`: no AI ("None").

### How it runs each backend

- **Claude Code**: one long-lived `claude -p` process with stream-json in and out, one user
  message per wake, resumed with `--resume <session id>` after a crash, a limit or a bridge
  restart (within the run; the session id is in the situation log and, for a person who
  wants to open the conversation afterwards with `claude --resume` in that directory, the
  run directory's `session.json`). Locked down:
  `--mcp-config` (the bridge only) with `--strict-mcp-config`, `--setting-sources project`
  in an empty run directory outside any repository (your CLAUDE.md, hooks and plugins stay
  out), `--tools ""` (no built-in tools), `--allowedTools "mcp__murmur__*"`,
  `--disallowedTools Agent`, `--permission-mode dontAsk --permission-prompts none` (anything
  else is refused, never asked), `--system-prompt-file` (`agents/watch/prompt.md` and the
  brief), `--max-turns` per wake, `ENABLE_TOOL_SEARCH=false`, `CLAUDE_CODE_MAX_RETRIES=2`.
  Never `--bare` (it ignores the subscription login) and never `bypassPermissions`.
  A tool outside the Murmur server that the CLI refused is the lockdown working: logged as
  `lockdown.refused` and the turn goes on. Only the CLI's two refusals, both seen live,
  count: an error result that starts "No such tool available" (the model asked for `bash`
  to read a saved answer; `agents/tests/fixtures/claude_bash_refused.jsonl` is that stream)
  or "Permission to use X has been denied" (`dontAsk`). Any other result, an error from a
  tool that ran and failed included (the CLI wraps those in the same `<tool_use_error>`
  tag), interrupts the turn and stops the run (`lockdown.breach`).
  Routine model `sonnet` at effort `medium`.
- **Codex**: one `codex exec --json` per wake, then `codex exec resume <thread id>`, the
  update on stdin. A read-only sandbox blocks writes and the network, not reads: a shell
  left on could read the whole disk, and a breach found in the event stream comes after the
  command ran. So before any turn the backend runs `codex features list` (the effective
  feature states with this run's configuration; no model call), disables every listed
  feature whose name says it runs commands or model-written code, reads or writes files, or
  reaches beyond the bridge (`shell_tool`, `unified_exec`, `js_repl`, `code_mode`, the patch,
  apps, plugins, agents, hooks, skills, memories, image, search and others: 32 of the 52 that
  0.116 lists) with `--disable <name>` on every exec, and refuses to start unless the list
  then shows each of them off and lists `shell_tool` and `unified_exec` at all (a renamed
  shell feature cannot be confirmed off). Confirmed on the installed 0.116.0 without a model
  or a login: the `codex features list` output and its effective state; `--disable
  <FEATURE>` (and `-c features.<name>=false`, its documented equivalent) on `codex`, `codex
  exec` and `codex exec resume`, each disabled feature then listed false; an unknown name
  refused (`Unknown feature flag`). Needing 0.161: `--ignore-user-config`, `--ignore-rules`
  and `--strict-config` (not in 0.116's `codex exec --help`), the other `-c` keys below
  (0.116 rejects `agents.enabled`, so its feature check refuses to start, as it should), and
  whether the bridge's tools are offered directly with code mode off (the backend no longer
  sets `features.code_mode.direct_only_tool_namespaces`: code mode runs model-written code,
  so it is disabled). Locked down further by `-c` configuration: `--ignore-user-config --ignore-rules
  --strict-config`, read-only sandbox, `approval_policy="never"`, the bridge as the only MCP
  server (required, its tools approved and exposed directly), shell, web search, apps,
  plugins, sub-agents, browser and computer use, image generation and viewing, the patch
  tool, goals, memories, hooks, tool suggestions, skills and project docs off, the prompt as
  `developer_instructions`. Each key is marked in `agents/watch/codex.py` with its source
  (the research handed to this work, or inferred); `--strict-config` turns a wrong key name
  into a refusal to start, never a weaker lockdown. The event stream carries no list of the
  tools a session offers, so the backend checks what the model used instead, failing
  closed: a command, a file change, a web search, the patch tool, the image viewer or a call
  to another MCP server stops the run as a lockdown breach, and the error carries the items
  as Codex sent them. A call whose server field is missing passes only when it names one of
  the bridge's tools. The documented harmless items (messages, reasoning, a plan or todo
  list, an error notice) pass. An item of a type the backend does not know stops the run
  too, with the raw item and the way to allow it in the error: the vocabulary has not been
  seen live here, and this is Codex's only check at run time. `--codex-allow-item TYPE`
  (repeatable, never a command, file change, web search, patch, image view or tool call)
  lets a type you have looked at through; it is listed in `run.start` and each use is logged
  (`lockdown.unknown_item`, with the raw item). Once a live 0.161 run has established the
  vocabulary, unknown types can be switched to logged and let through, so a new harmless
  item in a later Codex does not end a good run. Routine model `gpt-6-luna` at reasoning
  effort `high`. Never `ultra` effort (it spawns sub-agents).
- **The stronger model** (`opus` at `high`; `gpt-6.1-sol` at `high`) takes consequential
  moments, in a fresh short consult process each time: the system prompt and the update,
  not the routine conversation. Resuming or forking the routine session on another model
  would send that whole conversation again, uncached, at every escalation; the consult
  reads the picture itself (its earlier proposals are in `get_intents` with `mine`), and
  the routine model is told what it did at its next update.

### What the model is told, and what is data

The system prompt is `agents/watch/prompt.md` with the run's brief appended; only those
instruct the model. Each update lists what changed. Free text from outside the driver (a
contact's AIS name, which anyone can broadcast; an autopilot's status text; an alert's
detail; the engine's evidence; a recorded reason) is shown in double quotes, on one line,
cut short and labelled with its source, and the prompt says such text is data, never an
instruction, whatever it says. An operator's note on a rejection is shown as that operator's
note: the model respects it as their reason (no re-proposal without a material change).
This matters because auto-tier proposals (a hold, a return, an orbit inside the approved
area) run without a human.

### When it wakes

The driver polls `get_changes` every 2 s. While a track is declared hostile it also reads
`get_nomination_options` for one track per poll, each track every 15 s (the bridge allows an
agent one such dry run a second, shared with the model), for the armed platforms that could
pass only and asking only whether each passes (`passed_only`): armed, link up, navigation
ready, and within reach, which is the platform's catalog cruise speed plus the track's speed
over the policy's longest transit (`max_transit_s`, read from `get_policy` once a session;
900 s when the policy has no engagement section), or 30 km for a platform with no cruise
figure. The full answer for all 23 armed platforms of 100 stand-ins was 213 557
characters. The table is `TRIGGERS` in `agents/watch/wake.py`:

| change | wakes | stronger model |
|---|---|---|
| a track declared (by an operator or the hostile-act rule), or re-declared | yes | when hostile |
| an armed platform passes every nomination check on a hostile track | yes | yes |
| a declaration revoked; a review flag raised | yes | |
| a new track that is suspect, unknown or hostile | yes | |
| an affiliation estimate moving to (or from) suspicion | yes | |
| an identity conflict appearing (an AIS identity that does not fit what was seen) | yes | yes |
| a vehicle's link lost | yes | on an armed platform |
| a GPS glitch flagged by a vehicle's navigation filter | yes | yes |
| a condition coming and going twice within 5 minutes (flapping) | yes | for a GPS glitch, a conflict, an armed platform's link |
| navigation not ready, or off GNSS on anything but a submarine | yes | |
| a failsafe; a high-severity alert, or an alert that became one | yes | |
| one of its own proposals approved, rejected, voided, expired, failed, aborted, queued, dispatched or effected (also when it was decided before the driver saw it pending) | yes | |
| the bridge restarted (the CLI is restarted too, and its tools checked again) | yes | |

Anything else is a note that rides along with the next update (a task of its own that ended
`done` is a note: nothing to decide). Bursts are coalesced (3 s after the first waking
change, at most 10 s), and wakes are at least 10 s apart. A condition that came and went
once inside one batch (a 2-second link flap, an alert that cleared itself) stays a note; the
second time within 5 minutes it is flapping, which wakes (intermittent glitching is what
jamming and spoofing look like). One subject (a track, a vehicle, an alert, a flag, one of
its intents in one state) wakes the model at most once in 5 minutes and escalates at most
once in 30, so one contact cannot spend the run's escalations or the hour's wakes. A subject
is the item and its instance: a declaration by its number (a re-declaration is new), an
affiliation by the value it moved to. A repeat inside the 5 minutes is held back: one per
item (the kind and its track, vehicle, alert, flag or intent), the newest, with a count,
listed in the update as held back. When the interval ends it wakes the model by itself,
shown as the latest of N repeats. Any newer change of the item that is not held back
supersedes it, whatever its value and whether or not it wakes (navigation back on GNSS, an
estimate gone to neutral): the held one is then told as a note just before it, never later
as a trigger, and a clear of its condition makes it a note too. A subject that clears (a
link back, an alert cleared, a declaration revoked) is released, so its next raise wakes at
once, unless it is flapping; it escalates again only if that was its first come-and-go in
the 30 minutes. Of the run's escalations, two are kept for the engagement chain (a hostile
declaration, a nomination becoming possible, a lost link on an armed platform with an
engagement in play); the others share the rest, and one subject escalates at most twice a
run, so spoofs, glitches and links coming and going (an armed boat at the edge of radio
range, or an adversary choosing the period) cannot spend what a hostile declaration needs.
Notes kept for the next update are bounded (1000; the oldest go first, a condition that came
and went with both its halves, and the update says how many). A change the driver cannot
judge (malformed) is set aside and logged (`change.dropped`); the watch goes on. A
heartbeat wakes it every 60 s without a trigger, doubling up to 300 s while nothing new
arrives (autopilot chatter, vehicles joining and repeats are not news), and never takes the
last quarter of the hour's wakes, which stay for triggers; a heartbeat that finds a waking
change still coalescing wakes for that change, escalated if it escalates.

### Budgets and limits

`--max-wakes-per-hour` (60; an escalation is not held back by it), `--max-turns-per-wake`
(12 tool turns), `--max-escalations` (6 a run: two kept for the engagement chain, at most two
for one subject), `--max-wakes` (400 a run), and for Claude
`--max-budget-usd` (the CLI's own cost estimate, summed across restarts; on a subscription it
is an estimate, not a bill). `--turn-timeout-s` (300) interrupts a turn that runs long. A
turn that fails (a crash, or an error result such as an overloaded API after the CLI's own
retries) gives its wake back: the changes are told again at the next wake (a failed opening
update, as the opening) and an escalation it spent is returned. The model that failed
then waits: 30 s after one failure, doubling with each failure in a row up to 15 minutes
(`--error-backoff-s`, `--error-backoff-max-s`, logged as `backoff` with the model), and a
success or a usage limit ends the wait. The routine model and the senior consult wait
separately: while the routine model waits, an escalating wake still goes to the consult;
while the consult waits, escalations go to the routine model. The run stops only on routine
turns that go on failing after a wait at that ceiling (reason `errors`) or on more than
three crashes in ten minutes (`crashes`). A crashed turn reports no cost, so the budget does
not count what it spent (its `backend.crashed` line says so).

Neither CLI waits for a usage limit to reset. Claude Code ends the turn with "You've hit
your session limit · resets 3:45pm" (weekly, Opus and Sonnet variants too) and its stream's
`rate_limit_event` gives the exact reset; Codex fails the turn with "You’ve hit your usage
limit. Try again at 3:42 PM." (or a date, or "later"). Only the CLI's own words count (its
result, its synthetic error message, its stderr), never the model's prose. The driver logs
`limit.hit` with the parsed reset and, with `--on-limit wait` (the default), sleeps until the
reset plus a minute when that comes before `--until`, never sooner than a floor that doubles
with each limit in the hour (`--limit-floor-s`, 60 s, up to 30 min), then resumes the
session; otherwise it stops. A limit on the stronger model only sends later escalations to
the routine model until its reset.

### The situation log

One JSON object per line (`--situation-log`, appended; every line has `"source":
"watch_officer"`). Every line has `t`, `iso`, `run`, `kind` and `text`, one short line for a
person; the situation runner shows the lines meant for a person in the console's Situation
panel (`PANEL_KINDS` in `eval/situation_run.py`: the model's note after each turn, why it was
woken, errors, waits, limits, its start and stop). `agents/watch/situation.py` lists the
kinds. The ones to read:

- `wake`: `n`, `reason` (start, changes, heartbeat), `why`, `escalated`, `model`, `update`
  (exactly what the model was told) and `changes` (the records behind it);
- `turn`: `n`, `model`, `tool_calls` (name, input, a result excerpt, `is_error`), `text`
  (its note for the operator), `proposed` (intent ids), `usage`, `cost_usd`, `duration_s`,
  `stop` (success, max_turns, timeout, limit, budget, error, lockdown), `context_tokens`
  (Claude: the largest context one model call of the turn read, input plus cache read and
  cache write tokens; Codex reports only the turn's sum of input tokens, an upper bound),
  `refused`, `unknown_items`;
- `shadow`, `limit.hit`, `limit.resume`, `backend.crashed`, `backend.restart`, `backoff`,
  `lockdown.refused`, `lockdown.breach`, `lockdown.unknown_item`, `change.dropped`,
  `budget.deferred`, `budget.exhausted`, `run.start`, `run.stop`.

`context_tokens` shows how fast a session's context grows: every answer a tool gave stays in
the conversation, so the prompt steers the model to read narrowly (`get_fleet` with
`compact`, `uids` for the few vehicles it acts on).

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
out (from a run directory under /tmp; not checked from one under the home directory),
`dontAsk` refusing a tool outside the allow rule, `${VAR}` expansion in the MCP config's
headers, `--max-turns` per message (an exceeded one ends that turn, the process lives), the
stream-json interrupt (ends the turn, the process lives; SIGINT ends both), `--max-budget-usd`
(per process, after each turn), `--resume` (same session, the conversation intact,
`total_cost_usd` from zero again). `--safe-mode` would keep the user's settings out too,
but it also drops the `--mcp-config` server, so it is not used. Claude Code speaks MCP
2026-07-28, which the bridge serves without a session: a call after 35 s of silence went
through with the bridge's idle limit set to 20 s (`MURMUR_MCP_IDLE_S`), so its quiet spells
between wakes cost nothing. Clients on an older protocol version (the Python SDK negotiates
2025-11-25) hold a session that the bridge drops after 15 minutes idle (it was five minutes,
rmcp's default; `MURMUR_MCP_IDLE_S`; the driver's own reads come every 2 s), and one agent
id holds at most eight, counted from each session's initialize (the one used least recently
with no request in flight is closed; sessions their client closed do not count, and with
every one busy the cap waits rather than cut a request off; sessions opened by a claimant
that names no listed agent share one capped bucket, and an opening request over 1 MB is
refused with 413). Claude Code saved tool answers over about 50 000 characters to a file
and gave the model a preview, which a model with no file tools cannot read (the nomination
dry run on a hostile track, 61 KB): the bridge's tools now declare
`anthropic/maxResultSizeChars` and the backend sets `MAX_MCP_OUTPUT_TOKENS`, and the whole
answer arrived. The figure, 150 000 characters, comes from full reads on 100 stand-ins
(`scripts/scale_fleet.py 100`: 23 armed, 14 tracks; no model): `get_fleet` 131 650
characters (32 494 with `compact`), `get_tracks` 38 945, `get_nomination_options` 213 557
for every armed platform and 4 980 for two. A full fleet read at 100 comes whole; the dry
run over every armed platform of a large fleet does not, and the tool says to pass `uids`.
Live smoke runs: the model
read the picture and proposed with rationales, a killed CLI was resumed, a SIGTERM stopped it
cleanly, an operator's rejection woke it without a re-proposal, and a hostile declaration
woke it. In a situation (Harbor watch, `OPERATOR=autoplay AI=claude`, Claude Code 2.1.295,
the full 720 s): 14 wakes and one consult on the stronger model, two orbit proposals (a close
look at the cutter whose sonar class disagreed with its AIS, and one on the fast boat 3.7 km
out and closing) that op-auto approved, the compact fleet and filtered tracks read, the notes
in the Situation panel, about $0.75 by the CLI's estimate, and the audit log replayed exactly
with its anchors.

Not verified: Codex, entirely (the CLI installed here is 0.116.0): the configuration keys
marked inferred in `agents/watch/codex.py`, the order of `exec resume`'s arguments, SIGINT
ending a turn at the turn cap, and the 0.161.0 event shapes. The Codex event stream carries no
tool list to check before a turn (the backend checks what was used, after it). A real usage
limit was not hit (the messages and `rate_limit_event`'s `rejected` status follow the
documentation). The stronger-model consult was not run live (stand-in only). Both backends are
tested against stand-in CLIs (`agents/tests/`, in `make check`) that print the documented
events, limits, errors and crashes. No test calls a model.

## Tools

Reads (every value carries `as_of` and `age_s`; vehicle facts carry `stale` against the
rules' freshness limit for that vehicle's link):

| tool | what |
|---|---|
| `get_fleet`, `get_vehicle` | identity, platform limits and tasks, position, nav readiness and source, the GPS glitch flag (`nav.gps_glitch`, with its age: the navigation filter rejects the fix, as a jammed or spoofed receiver looks), battery, home, link health, current task. `get_fleet` takes `uids` (only those) and `compact` (one short entry per vehicle: identity, mode, position and its age, navigation, GPS glitch, battery, link, current task, and which facts are stale, the GPS glitch flag's own included; about a quarter of the size, 32 494 characters for 100 vehicles against 131 650) |
| `get_tracks` | contacts that are not ours, as our sensors see them (perception only): position and its uncertainty carried to now, existence confidence, the identity estimate, the affiliation estimate with its reason, any declaration and review flags, the sources, and `lost` (dropped contacts every engagement keep-out still counts). `identity.kind` comes with `identity.level`, how specific it is: `detect`, only a size class (`small_air`, `large_surface`, ...) or, from ESM alone, the domain (`aircraft`, `vessel`, `ground_vehicle`, `submerged`); `recognise`, a family (`small_uas`, `aircraft`, `small_craft`, ...); `identify`, a kind (`unknown_uas`, `fishing_vessel`, ...), from a sensor's identification or, with none, the AIS identity. `identity.confidence` is that level's: `small_air` at 0.99 is a sure size class, not an identified contact. `identity.conflict` is a call that disagrees and is not yet outweighed (two agreeing looks as sure as it outweigh it), or the AIS identity contradicted by what our sensors settle on (docs/SENSING.md, identity fusion) |
| `get_platforms` | the catalog: limits, link class, `armed`, tasks per kind, fleet members |
| `get_areas` | approved, no-go, engagement-zone and no-strike areas as `[lon, lat]` polygons |
| `get_policy` | the risk-tier policy and rules configuration in force |
| `get_intents` | intents of the run (filter by state, or `mine`) with checks, policy, approval, history |
| `get_inbox` | pending approvals (why each needs a human, when it expires), alerts, the queue, and the identification review (`review`: rule declarations to confirm, flags, lost-contact claims; read-only) |
| `get_changes` | what changed since a cursor, oldest first (a track change carries its summary, the identity with its `identity_level`): tracks new, changed affiliation, declared or re-declared (with what it replaced), confirmed, revoked, an identity conflict appearing, changing (with what it was) or clearing, stale, dropped; review flags; vehicles joined (and already lost or glitching), link lost or back, navigation changed, GPS glitch flagged or cleared; alerts raised, changed (kind or severity) and cleared; intents created or changed state (`mine` marks yours; a rejection of yours carries the operator's note, kept to 1000 characters and shown only to the agent that proposed it); vehicle events. Read-only, not logged. A lean digest compared between reads, at most one new digest a second whatever the number of readers (any agent's read refreshes the shared feed), so a condition that came and went between two reads is not reported. A cursor is `<feed>:<n>`: a cursor of an earlier bridge answers `reset` |
| `get_recent_events` | vehicle events the audit log recorded: mode changes, failsafes, status text |
| `get_satellite_passes` | imaging access windows over an area (read-only) |
| `get_satellite_tasking_options` | ranked collection options and exclusions (read-only) |
| `get_nominations` | engagement nominations (Phase E): checks, preview, signatures needed and given, the picture held since approval, the effect, a stop, the assessment |
| `get_link_coverage` | the predicted link, before a vehicle goes anywhere (docs/LINK-COVERAGE.md; read-only, not logged, never a check): the link emulator's own geometry (range from the ground station, terrain line of sight on the emulator's 250 m cells and 10 m altitude bands, SATCOM satellites of the class in view, an AUV's depth gate), with no random draw and no scripted event (a jammer, a cut). For a vehicle (`uid`: its own link profile, its position, the altitude its orders use) or a link class or profile (`class`: `los`, `mesh`, `satcom`, `satcom_leo`, `satcom_iridium`, `satcom_ku_geo`, `acoustic`, `tether`; `alt_m` above sea level, or `above_surface`): at `point` or `points` (`state` up, degraded from 20 % loss, down from 95 % or where the emulator blocks; `p_loss`; `reason`: `range`, `terrain`, `no_sat`, `submerged`, `surfaced` or the link's own `loss`); along a `route` (from the vehicle with `uid`; `loop` for a patrol); for an `order` as `preview_intent` takes it (each vehicle's transit, task path and return as the decision engine lays them out: per leg its stretches up, degraded and down, `first_down` (where, why, for how long at the vehicle's cruise speed) and `trip_down` (the first stretch down at least as long as the failsafe), and for the order `failsafe_s` (the profile's `lost_link_s`), `p_failsafe` (the chance the failsafe trips on the way and in five minutes on station, heartbeat by heartbeat with the predicted loss), `holds` (that chance at most 10 %) and a `sentence`: "PUMA 2 would lose its link 1.2 km along the transit leg (behind terrain from the ground station), for about 70 s; its failsafe trips after 5 s."); or a grid over `bbox` [w, s, e, n] at `res_m` (one letter a cell for the state and the reason, and the loss; an agent's grid holds at most 10 000 cells, coarser otherwise, and says so). `queries` asks up to 64 at once. Terrain comes from tidewater (`MURMUR_TIDEWATER_URL`) or a baked surface grid (`MURMUR_SURFACE_GRID`; the headless simulator's world), each cell's answer kept for good; without either, terrain is not checked and `sources.terrain` says so. HTTP: `GET` and `POST /api/links/coverage` |
| `get_identify_options` | rules 21: the vehicles that could be sent to identify a track (`track_id`; `uids`, `standoff_m` optional), ranked, passing first and soonest there first: each one with an EO/IR, whether an identify order for it passes its checks now (`failing` in words), its orbit's standoff from its camera's identification reach (`standoff_m`, `reach_m`), the time to get there (`transit_s`) and, for one that passes, its predicted link along the way (`link`: `holds`, `p_failsafe`, `sentence`). Read-only and not logged, at most one call a second per agent. HTTP: `POST /api/identify/options` |
| `get_nomination_options` | the nomination form's dry run for a declared-hostile track: each armed platform's checks and preview now, and `keep_outs`: which track or vehicle of ours blocks (or nearly blocks) the keep-out, the region that blocks and by how much, and what would clear it (the effect times, and for a holding target the distances, at which every keep-out holds on the picture now; a later nomination if everything keeps behaving as now). Read-only and not logged, at most one call a second per agent; listed by uid, not ranked; no keep-out is relaxed (murmur-12e). `uids` limits it to a few platforms: for every armed platform of a large fleet the answer is too large for a model to read whole (213 557 characters for 23); `passed_only` answers only whether each passes, without the checks, preview and keep-out explanation. From rules 19 each live track's item also has `behaving`: whether an operator with engagement authority may accept it as behaving now (in view, with a velocity) and whether that would clear its block (`clears`, the accepted path's `region`, `reason`). An agent can never accept one: no tool takes it, and a `propose_intent` carrying `accept_behaving` is refused (403). A nomination the track blocks is rejected when proposed; only a person can nominate with the acceptance, or accept a blocker that appears while a nomination waits unsigned |

Proposing:

| tool | what |
|---|---|
| `preview_intent` | dry run: the checks and the policy tier now, no intent created (logged as `agent.action intent.preview`); for a collect without `chosen`, a tasking query whose space answer lists the ranked options; for a vehicle task, `link_coverage`: the predicted link along each vehicle's route, as `get_link_coverage` answers an order (information only: computed after the decision and outside its lock, never logged, no check or rule reads it; the console's dry run carries the same) |
| `propose_intent` | task + uids + params + constraints + **required** rationale; auto tier dispatches, human tier waits in the inbox. Vehicle tasks (orbit, patrol, hold, rtl, track; rules 21 identify: one vehicle, `track_id`, optional `standoff_m`, flown as an orbit around the track that follows it until the track is identified, lost or out of time, then the vehicle goes back to the order it was on; under the situations' watch policy an agent's waits for the operator as its orbits do) and satellite requests: `collect` (area, window, capability, `chosen` from the options) and `cover` (area, window, service), which go through the same `decision::space` path as an operator's |
| `explain_intent` | the checks, the tier, why a human, who decided and how, history, tasking or space run, a plain summary |
| `withdraw_intent` | the agent's own intents only, while they wait; with `if_state` (e.g. `pending_approval`) only if the intent is in that state at that moment, else 409 and nothing withdrawn or logged |

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
