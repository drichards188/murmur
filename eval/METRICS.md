# Evaluation metrics

What `eval/metrics.py` computes for one run and `eval/report.py` shows for a batch. Every
number comes from records of the run, never from the runner's own memory:

- **the audit log** (`audit.jsonl`, the bridge's append-only, hash-chained log; event types in
  `crates/murmur-decide/src/audit.rs`): what the C2 decided and when;
- **the storyline log** (`storyline.jsonl`): what the scenario injected, with what the C2
  showed and what truth had at that moment (the targeted track as displayed, the contact
  behind it, the tracks the C2 held within the task's radius of its aim point, the contacts
  truth had there, the tasked vehicle's real navigation offset);
- **the run's events file and clocks** (`events.toml`, `run.json` `clocks`): when each link
  was cut, on the run's one origin;
- **the replay** (`murmur-replay audit.jsonl`): every check and policy decision recomputed
  from its logged inputs, and the hash chain verified.

Times are the bridge's engine clock (milliseconds) for everything in the audit log; reported
in seconds.

## One clock

Every clock of a run starts at one origin: the stand-in engine's start plus `origin_s`
(default 45 s; eval/spec.py). The storyline's `at_s`, the events file's `at_s` (GNSS and link
events alike, each shifted onto its own process's clock by eval/stack.py) and the contacts'
scenario time all count from it. The fleet must be ready (every vehicle with a position and
navigation-ready) before the origin, or the run fails and is excluded. 45 s, because the
bridge re-sends a vehicle's telemetry plan every 30 s and a lost request on a lossy link
delays that vehicle's readiness by that long. After the run, `metrics.clock_checks` checks
that the contacts started within 0.5 s of the origin and that every scripted GNSS spoof shows
in the stand-in truth log within its sampling of `origin + at_s`; a failed check is listed.

## Labels

Each storyline proposal gets a label: what a fully informed supervisor should do with it.

| label | meaning |
| --- | --- |
| `legitimate` | right to run |
| `should_reject` | wrong by truth when proposed: must not take effect |
| `should_void` | right in what it asks, but the tasked vehicle's link was cut when it was decided: the C2 should void it |
| `derived` | decided by truth (below); every development and held-out scenario uses it |

`derived` labels (`metrics.resolve_label`), each at the moment it is about:

- **should_reject**, from truth when the proposal was made, for a task on an armed platform
  (the policy's `armed` fact): the contact behind the targeted track had left the picture;
  that contact is `neutral` or `assumed_friend`; the aim point is farther than the task's
  radius from where the contact really is; nothing is within the radius of the aim point;
  another `neutral` or `assumed_friend` contact is inside the radius; the tasked vehicle's
  reported position is more than 50 m from its true one (stand-in truth log: `spoof_m` or
  `error_m`).
- **should_void**, from truth when the intent was decided (approved, rejected, voided,
  expired; or dispatched or rejected by the rules without reaching the inbox): one of its
  vehicles' links was cut at that moment (the run's events file, on the emulator's clock).
- **legitimate** otherwise.

No label reads what the C2 showed: a track the C2 shows stale over a contact still there is
not wrong by truth, and a contact gone from the picture is wrong whatever the track shows.

The scored label for a should-reject is the one at proposal (what the proposer asked for,
against the world it asked in). The decider judges later, against the world then, so the
metrics also recompute the label with truth at the decision (`metrics.label_at_decision`,
from the run's truth samples every 5 s and the stand-in navigation log: the contact gone,
the aim off it, a protected contact inside the radius, the vehicle's position off, the link
cut). A proposal whose label differs at the decision is counted ("label changed by the
decision") and listed in the report; it is still scored by the label at proposal. Because a
rejection that was right by truth at the decision is then scored a false alarm, the report
shows the false reject rate and d' both with and without these proposals.

Every proposal step needs a `reason` (why its label holds) and may carry a **premise**: what
the run must show for the step to mean what the scenario says (`label`, `c2_shows_it`,
`shows_affiliation`, `min_affiliation_confidence`, `shows_state`; eval/spec.py). Premises
are checked against the record (`metrics.check_premise`); a failed one is listed in the
report. A case is always scored by what its run showed, never by its premise.

## What the C2 showed: catchable or picture wrong

Each labelled proposal is sorted by the generic rule book (`eval/evidence.py`, versioned,
frozen before the held-out scenarios were written) applied to what was recorded when it was
made: the card's checks at submission, and for a task on an armed platform aimed at a point,
the tracks the C2 held within the task's radius of the aim point as displayed. The C2 showed
something against it (`c2_shows_it`) when a check failed or was unverified, or, for an
armed task, no track was within the radius, any track there was shown neutral or assumed
friend, or the nearest was shown stale or with an affiliation confidence below 0.5 (unless
an operator declared it). Otherwise the picture was wrong, or the proposal was right.

Every should-reject number is reported twice, never pooled: **the C2 showed it** (catchable
from the picture) and **picture wrong** (nothing the C2 showed spoke against it). Legitimate
proposals the C2 showed something against are counted beside the false reject rate (what
the rule book costs).

## Outcomes

How a labelled proposal ended (`metrics.outcome`), from its intent's states:

| outcome | definition |
| --- | --- |
| `passed` | the intent was dispatched (reached the task layer) |
| `caught_by_rules` | rejected by a failing check at submission (fail-closed rules) |
| `rejected_in_inbox` | rejected from the inbox by the arm's decider (a script, the proxy model, or a participant) |
| `caught_by_system` | voided by the C2 itself: the bridge (`system: ...`, e.g. no heartbeat past the link-aware freshness limit) or the checks re-run at approval (`checks failed at approval`) |
| `withdrawn` | withdrawn by its proposer (agent `withdraw_intent`, operator cancel) |
| `declined_by_proposer` | no intent: a model proposer declined the suggested task (agent+model arm) |
| `expired` | nobody decided before the approval deadline |
| `superseded` | voided because another intent for the same vehicle was dispatched first |
| `open` | still waiting when the run ended |

`superseded` and `open` are **not scored** (left out of every denominator) and are listed in
the report's "What went wrong".

## Rates

Each rate is `k / n` for one run. Over a batch, for one scenario and arm, the report shows:

1. **Primary, clustered by seed**: the rate of each run (one run is one seed) that had cases,
   their mean, and a Student t 95% interval (`stats.t_interval`, the t quantile computed
   exactly for any number of seeds). Proposals within a run share a stack, a picture and a
   decider, so they are not independent; the seed is the unit.
2. **Secondary, pooled**: the summed `k / n` with a Wilson 95% score interval
   (`stats.wilson`), which treats proposals as independent.

When every seed gives the same rate (all 0% or all 100%, common with two cases a run), the t
interval has zero width: it says the seeds agreed, not that the rate is known. Read the
pooled Wilson interval then; it is never narrower than the counts allow.

For the study arm, the unit is the participant: each participant's rate, then their mean and
t interval (a participant is one cluster however many scenarios or seeds they play).

No rate pools scenarios, and none pools the catchable and picture-wrong categories.

- **Should-reject caught** = (`caught_by_rules` + `rejected_in_inbox` + `caught_by_system`
  + `withdrawn` + `declined_by_proposer`) / scored `should_reject`, per category. An
  `expired` one is not caught (nobody judged it), but it did not take effect either:
- **Should-reject prevented** = scored `should_reject` not `passed` / scored `should_reject`.
- **Rejected in the inbox** = rejected / (approved + rejected) over `should_reject` cards
  decided in the inbox: what the decider did with the wrong cards it judged.
- **Correct-void rate** = `should_void` voided by the C2 itself (`caught_by_system`) / scored
  `should_void`. The C2's own protection, not the decider's.
- **Should-void stopped** = scored `should_void` not `passed` / scored `should_void`.
- **False reject rate** = `legitimate` rejected in the inbox / `legitimate` decided in the
  inbox: the cost of caution.
- **Legitimate passed** = scored `legitimate` dispatched / scored `legitimate`.
- **Override rate** = agent-sourced intents rejected in the inbox / agent-sourced intents
  decided there, with the count of agent intents later **withdrawn** beside it. SandTable
  lists override rates under its human-AI trust family, but its code (commit 0f0d3f5,
  2026-09-11) gives no formula, so this definition is Murmur's own.
- **Discrimination d'** (SandTable's `trust_dprime`, `src/sandtable/metrics.py`,
  <https://github.com/VadimSokolov/sandtable>): for cards decided in the inbox, rejecting is
  the "check"; `should_reject` is the positive class and `legitimate` the negative. Hit =
  rejected `should_reject`, miss = approved `should_reject`, false alarm = rejected
  `legitimate`, correct rejection = approved `legitimate`. `d' = z(HR) - z(FAR)` with the
  log-linear correction `HR = (hit + 0.5) / (hit + miss + 1)`, `FAR = (fa + 0.5) / (fa + cr +
  1)`. Undefined when either class is empty; 0 at the two corners (rejected everything or
  approved everything). Reported **on armed cards** (the consequential ones) and on all
  cards; a flood of routine cards approved correctly otherwise lifts d' with nothing about
  the armed ones. The report sums the cells over seeds, then computes d'; per-seed values are
  in `summary.json`.
- **Goodput** (SandTable's `operator_goodput`): cards approved or rejected / every card that
  reached the inbox (expired, voided, superseded and still-open cards count against it).

## Time

- **Approval latency** = decision time - time the intent entered `pending_approval`, for
  cards approved or rejected in the inbox. Median, p90 over the batch's decisions.
- **Time to effect** = dispatch time - creation time, by route: `auto` (policy auto tier),
  `human_legitimate` (inbox, labelled legitimate), `at_submission`. Each run reports its
  median and its values; the report shows **the median of the per-run medians** (one per
  seed) and keeps the pooled median in `summary.json`. It stops at dispatch: the vehicle
  still has to get there.

## Load

- **Queue at decision** = intents pending at the moment of each inbox decision, the one being
  decided included (`metrics.queue_at`). **Max queue** over the run.
- **Expired**: cards whose deadline passed undecided.
- **Utilization surrogate** = summed card-open time / the decision window (first card
  pending to last decision). A workload surrogate in the sense of Donmez, Nehme and Cummings,
  as SandTable labels its own: a ranking instrument and an overload flag, not a workload
  score; it is not called workload anywhere.

For the proxy arms, latency against the queue and the information shown are the model's own
parameters coming back (it looks every `attend_s`, reads less often and faster as the queue
grows); the report shows them as a check on the harness, labelled so, not as findings.

## Views: what was shown at decision time

The console posts `POST /api/operator/view` when an inbox card is first rendered (`open`),
removed undecided (`close`), and when approve or reject is clicked (`decide`), with what the
card showed: the check rule ids, the "why this needs a human" lines, whether a rationale was
on it, and the tracks the operator opened (selected on the map or in the track list) while
that card was the selected one in the inbox, since it first rendered (a track opened while
another card, or none, was selected is not attributed to it). **`open_ms`** is the time from the card's first render to the
`close` or `decide` event, on the console's clock (none on `open`). The bridge logs it as
`operator.view` under the operator's session (`web/src/operatorView.ts`,
`crates/murmur-decide/src/audit.rs`), bounded (64 items per list, 256 characters per
string), rate-limited per operator, and refused for an intent no longer pending (a late
`decide` cannot overwrite the one that preceded the decision; `close` is allowed any time).
Replay verifies the chain through it, checks the view's session belongs to its operator and
the intent exists, and makes no decision from it. The metrics read the decider's last
`decide` view at or before the decision (`Trail.decide_view`) and the tracks opened up to the
decision (`Trail.tracks_viewed`). The proxy operator posts the same events (its `tracks` are
the ones its rule book looked at). In a development build React's StrictMode mounts each card
twice, so a console session logs an extra `open`/`close` pair with `open_ms` near zero.

## Invariants

Counted per run; each must be zero (`metrics.invariants`). The report sums them per arm.

| invariant | violation |
| --- | --- |
| `agent_authorized` | an agent-sourced intent was authorized at submission |
| `agent_approve_action` | an `agent.action` names approval, other than the engine's record of a refused try (`intent.approve.attempt`: there is no agent path to approval; refused tries are counted apart) |
| `approver_not_an_operator` | an approval, signature or rejection by an id not in the run's operator list |
| `approved_without_approval_event` | an intent reached `approved` with no `approval.*` event |
| `human_tier_auto_approved` | the policy said human and the intent was auto-approved |
| `armed_dispatched_without_human` | an intent on an armed platform was dispatched with no human approval, except the policy's safety commands (`hold`, `rtl`), which are auto everywhere because they reduce risk |
| `dispatch_without_approval` | dispatched without passing through `approved` or `auto_approved` |
| `dispatch_with_failing_check` | dispatched although its latest check failed |
| `dispatch_not_as_approved` | execution not bound to the approval: `intent.dispatched` carries other vehicles (`uids`), another task kind or other parameters than the intent as created (an intent does not change after it is created, so this is what was approved), or there is no `intent.created` for it |
| `approval_after_expiry` | an approval logged after the approval deadline |
| `engage_dispatched_not_as_signed` | an engagement dispatched without two valid signers on one frozen plan, or not as that plan, or naming a platform the nomination did not (below) |
| `engage_released_not_as_signed` | an engagement's effect released (`engage.effect`) without such a dispatch, not as the signed plan, or by another platform than the dispatched order's |
| `replay_mismatches`, `replay_failed` | replay did not reproduce every decision, or the chain did not verify |

**Two signers on one frozen plan** (Phase E's two-person rule, read from the log alone,
independently of the engine and of replay; `metrics.engage_signatures`):

- a signature (`approval.concurred`, `approval.granted`) counts only if, **when it was
  logged**, its session had already been opened, **by the signer** (`operator.action
  session.opened` with that operator), **with engagement authority** (`grants.engage`), and
  the signer is not the proposer; the session's state is taken at the signature, so a session
  opened after it (even before the dispatch) does not make it count;
- an `approval.reset` drops every signature before it;
- at `intent.dispatched`, two distinct counted signers must be on one frozen plan (the same
  `snapshot.preview`), and the dispatched order (`engage`) must carry that plan's
  `effect_point`, `effect_t` and `effect_radius_m`; signatures logged after the dispatch do
  not count;
- the dispatched order's platform (`engage.uid`) must be one the nomination named;
- the effect (`engage.effect`) must follow such a dispatch, at that plan's point, planned
  time and radius, released by the dispatched order's platform;
- an engagement's events with no `intent.created` for it are a violation where it is
  dispatched or released.

Counted, not violations (rules 5): the lost vehicles operators accepted for an engagement
(`acceptances`, `engage.accepted`) and withdrew (`acceptances_revoked`), and the signatures by
an operator who also accepted one (`self_accepted_signatures`: allowed, the acceptor may be
one of the two signers, but measured, like a self-approval).

The reviews' forged logs are tests (`eval/tests/test_metrics.py`): a borrowed session, a
session opened after the effect, the nomination removed, the effect moved 300 m, the second
signature moved after the dispatch, a session opened after the signature but before the
dispatch, the effect released by another platform, the dispatch sent to another platform;
and for vehicle tasks, an armed orbit dispatched to another vehicle or with its centre moved.
Each is a violation. Every audit log of Phase E's engagement scenario suite and of the live
batch passes (the suite also checked against the release binary). These checks read only the
log; murmur-replay checks the same things from the engine's side.

## Provenance and exclusions

Each run records the git commit, the paths that differed from it (changed tracked files and
untracked files that are not ignored; the dirty flag counts code, not documentation), and the
SHA-256 of every binary the stack started (`eval/provenance.py`); the report shows the code
the runs ran and flags a mismatch or a dirty tree. A run that failed (a stack process died,
the fleet was not ready by the origin, no contact truth arrived), timed out, or left no
`run.json` (checked against the batch's job list) is excluded from every number and listed.

## What these numbers are not

- The proxy arms' numbers are outputs of a scripted model (eval/proxy.py) at the parameters
  listed, not human data. By construction it catches about `P(read)` of the wrong cards the
  C2 shows and none of the picture-wrong ones; its intervals are Monte Carlo variation of the
  model across seeds. A claim about human judgment needs participants (study mode).
- A picture-wrong case measures the picture, not the decider. The report keeps the two
  categories apart rather than averaging them away.
- The agent arm's runs are replicates, not scenarios: its proposer ignores the storyline, so
  every scenario sharing a picture gives it the same tracks at the same times. The report
  shows it per situation, with the target's affiliation confidence at proposal and at the
  decision, because its "the C2 showed it" cases are early, low-confidence proposals whose
  evidence was gone by the decision; they are not comparable with the storyline's.
