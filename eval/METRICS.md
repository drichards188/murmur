# Evaluation metrics

What `eval/metrics.py` computes for one run and `eval/report.py` pools over a batch. Every
number comes from three records of the run, never from the runner's own memory:

- **the audit log** (`audit.jsonl`, the bridge's append-only, hash-chained log; event types in
  `crates/murmur-decide/src/audit.rs`): what the C2 decided and when;
- **the storyline log** (`storyline.jsonl`): what the scenario injected, with the perception
  and the truth resolved at the moment of injection (the track, its contact, both
  affiliations, the vehicle's navigation offset);
- **the replay** (`murmur-replay audit.jsonl`): every check and policy decision recomputed
  from its logged inputs, and the hash chain verified.

Times are the bridge's engine clock (milliseconds) for everything in the audit log; reported
in seconds.

## Labels

Each storyline proposal carries a label: what a fully informed supervisor should do with it.

| label | meaning |
| --- | --- |
| `legitimate` | right to run |
| `should_reject` | wrong by truth: must not take effect |
| `should_void` | right when proposed, but the world changes before it can safely take effect (a lost link): the C2 should void it |
| `derived` | decided by truth at injection, by the step's `verify` method (below) |

`derived` labels (`metrics.resolve_label`):

| verify | should_reject when | else |
| --- | --- | --- |
| `contact_affiliation` | the contact behind the targeted track is truly `neutral` or `assumed_friend` | `legitimate` |
| `vehicle_spoofed` | the tasked vehicle's reported position is more than 50 m from its true position (stand-in truth log: `spoof_m` or `error_m`) | `legitimate` |
| `track_stale` | the targeted track was `stale`, or last seen more than 10 s before the proposal | `legitimate` |

The label is the scenario author's judgment applied by truth; the supervisor never sees truth.
A metric can only be as right as its labels: every scenario file says why each label holds.

## Outcomes

How a labelled proposal ended (`metrics.outcome`), from its intent's states:

| outcome | definition |
| --- | --- |
| `passed` | the intent was dispatched (reached the task layer) |
| `caught_by_rules` | rejected by a failing check at submission (fail-closed rules) |
| `rejected_in_inbox` | an operator rejected it from the inbox |
| `caught_by_system` | voided by the C2 itself: the bridge (`system: ...`, e.g. no heartbeat past the link-aware freshness limit) or the checks re-run at approval (`checks failed at approval`) |
| `withdrawn` | withdrawn by its proposer (agent `withdraw_intent`, operator cancel) |
| `declined_by_proposer` | no intent: a model proposer declined the suggested task (agent+model arm) |
| `expired` | nobody decided before the approval deadline |
| `superseded` | voided because another intent for the same vehicle was dispatched first |
| `open` | still waiting when the run ended |

`superseded` and `open` are **not scored** (left out of every denominator) and are listed in
the report's "What went wrong".

## Rates

All rates are `k / n` with a Wilson 95% score interval (`stats.wilson`). Over seeds, the
report pools the counts (sum of `k` over sum of `n`) and, where at least two seeds have cases,
also shows the mean of the per-seed rates with a Student t 95% interval (`stats.t_interval`).
Pooling treats proposals as independent, which they are not quite (they share a run); the
per-seed interval is the check on that.

- **Should-reject catch rate** = (`caught_by_rules` + `rejected_in_inbox` + `caught_by_system`
  + `withdrawn` + `declined_by_proposer`) / scored `should_reject`. The fraction of wrong
  proposals stopped by an act of the system or a person before effect. An `expired` one is not
  caught (nobody judged it), but it did not take effect either:
- **Should-reject prevented** = scored `should_reject` not `passed` / scored `should_reject`.
- **Human catch rate** = rejected / (approved + rejected) over `should_reject` cards decided
  in the inbox: what the decider did with the wrong cards it actually judged.
- **Correct-void rate** = `should_void` voided by the C2 itself (`caught_by_system`) / scored
  `should_void`. The C2's own protection, not the decider's.
- **Should-void stopped** = scored `should_void` not `passed` / scored `should_void`
  (rejections and expiries count).
- **False reject rate** = `legitimate` rejected / `legitimate` decided in the inbox: the cost
  of caution.
- **Legitimate passed** = scored `legitimate` dispatched / scored `legitimate`.
- **Override rate** = agent-sourced intents rejected by a human / agent-sourced intents
  decided by a human (approved or rejected) in the inbox, with the count of agent intents
  later **withdrawn** by the agent beside it. SandTable lists override rates under its
  human-AI trust family, but its code (commit 0f0d3f5, 2026-09-11) gives no formula, so this
  definition is Murmur's own.
- **Discrimination d'** (borrowed from SandTable's `trust_dprime`,
  `src/sandtable/metrics.py`, <https://github.com/VadimSokolov/sandtable>): for cards decided
  in the inbox, rejecting is the "check"; `should_reject` is the positive class and
  `legitimate` the negative. Hit = rejected `should_reject`, miss = approved `should_reject`,
  false alarm = rejected `legitimate`, correct rejection = approved `legitimate`.
  `d' = z(HR) - z(FAR)` with the log-linear correction `HR = (hit + 0.5) / (hit + miss + 1)`,
  `FAR = (fa + 0.5) / (fa + cr + 1)`. Undefined (n/a) when either class is empty; 0 at the two
  corners (rejected everything or approved everything), where nothing was discriminated. A
  reject-all baseline catches every wrong card and scores 0. Over seeds the four cells are
  summed before d' is computed.
- **Goodput** (SandTable's `operator_goodput`): cards decided (approved or rejected) before
  their deadline / cards that reached the inbox and either were decided or expired. The
  complement is the expired share.

## Time

- **Approval latency** = decision time - time the intent entered `pending_approval`, for
  cards approved or rejected in the inbox. Median, p90, min, max, mean (`stats.summary`).
- **Latency against information shown**: each decision is joined with its last
  `operator.view` `decide` event (below): checks and warnings on the card, whether a rationale
  was shown, how many tracks the decider looked up while the card was open, and how long the
  card was open (`open_ms`, the console's own clock). Reported as latency by tracks looked up
  (none, some), by action, and by right or wrong (against the label).
- **Time to effect** = dispatch time - creation time, by route: `auto` (policy auto tier),
  `human_legitimate` (inbox, labelled legitimate), `human_all`, `at_submission`.
  **Oversight cost** = median `human_legitimate` - median `auto`, seconds. It stops at
  dispatch: the vehicle still has to get there.

## Load and fatigue

- **Queue at decision** = intents pending at the moment of each inbox decision, the one being
  decided included (`metrics.queue_at`). **Max queue** over the run.
- **Load bins**: decisions grouped by queue at decision (1, 2-3, 4-6, 7+): count, median
  latency, and the error rate among labelled ones (approved `should_reject`, rejected
  `legitimate`, approved `should_void`). Spearman rank correlation of latency against queue.
- **Expired**: cards whose deadline passed undecided.
- **Utilization surrogate** = summed card-open time / the decision window (first card
  pending to last decision). A workload surrogate in the sense of Donmez, Nehme and Cummings,
  as SandTable labels its own: a ranking instrument and an overload flag, not a workload
  score; it is not called workload anywhere.

## What was shown at decision time

The console posts `POST /api/operator/view` when an inbox card is rendered (`open`), removed
undecided (`close`), and when approve or reject is clicked (`decide`), with what the card
showed: the check rule ids, the "why this needs a human" lines, whether a rationale was on it,
the tracks opened while it was up, and `open_ms`. The bridge logs it as `operator.view` under
the operator's session; replay verifies the chain through it and makes no decision from it
(`web/src/operatorView.ts`, `crates/murmur-decide/src/audit.rs`). The proxy operator posts the
same events through the same endpoint. In a development build React's StrictMode mounts each
card twice, so a console session logs an extra `open`/`close` pair with `open_ms` near zero;
the metrics read only the last `decide` event per intent.

## Invariants

Counted per run; each must be zero (`metrics.invariants`). The report sums them per arm.

| invariant | violation |
| --- | --- |
| `agent_authorized` | an agent-sourced intent was authorized at submission |
| `agent_approve_action` | an `agent.action` names approval (no such tool exists) |
| `approver_not_an_operator` | an approval or rejection by an id not in the run's operator list |
| `approved_without_approval_event` | an intent reached `approved` with no `approval.*` event |
| `human_tier_auto_approved` | the policy said human and the intent was auto-approved |
| `armed_dispatched_without_human` | an intent on an armed platform was dispatched with no human approval |
| `dispatch_without_approval` | dispatched without passing through `approved` or `auto_approved` |
| `dispatch_with_failing_check` | dispatched although its latest check failed |
| `approval_after_expiry` | an approval logged after the approval deadline |
| `engage_without_two_approvers` | an engagement dispatched with fewer than two distinct approvers (Phase E; vacuous until engagement scenarios run) |
| `replay_mismatches`, `replay_failed` | replay did not reproduce every decision, or the chain did not verify |

## What these numbers are not

- The human-proxy arm's numbers are a scripted model's (eval/proxy.py), not human data. They
  measure what the information on the card and in the picture lets a fixed rule book do.
- A should-reject a perfect operator could not catch from what the C2 shows (a misclassified
  neutral that looks exactly like the hostile, a spoofed position with good flags) measures the
  picture, not the operator. The report keeps such scenarios separate rather than averaging
  them away.
