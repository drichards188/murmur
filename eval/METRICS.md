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

A study run counts at all only when it is evidence about its participant
(`metrics.auth_of`, `human_control_evidence`): a credentials run whose log replayed with its
anchors verified, the participant (`op-study`) signed in as a person (their own password or
passkey), and, from rules 10 (passkeys), the log naming the study (`run.started`'s roster
`study`, its participant), no override that allowed password step-ups, and every
consequential act of the participant's (a signature, an acceptance of a lost vehicle, a
hostile declaration or confirmation where the policy steps them up) quoting a passkey step-up
(`participant_steps`; a passkey registration's password is not a decision). Only the
participant's own acts count; the scripted operators' never. A run that falls short is listed
as excluded with what was missing; a log of earlier rules (no passkeys) is a pilot.

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

Counted, not violations (rules 6): the lost vehicles operators accepted for an engagement
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

## Identification: rule against manual (rules 9)

`eval/ident.py` (`python -m eval.run ident`, and a step of every batch) generates
identification situations per seed and per untracked-author share (seeded geometry from
stated distributions: a hostile author, not tracked with probability `untracked_share`, run at
0.2 and 0.5 by default, who acts against our patrol USV at 10 s, a second time at 40 s with
probability 0.2; none to three contacts 15 to 400 m from it: a fishing boat shown neutral at
0.3 to 0.9 or not classified, an AIS ferry, a friendly patrol boat, another hostile; each
holding or under way as `MOTION` states (the author holds with probability 0.4, else 2 to 8
m/s; fishing boats 0.5, else 1 to 4 m/s; the ferry 6 to 10 m/s; the patrol boat 0.3, else 3
to 8 m/s; another hostile as the author; courses uniform); each track's 1-sigma 8 to 25 m;
one or two of our USVs observing, each its own fix with a radar's error at its range; an
armed aircraft). The files name no policy: each arm is the shipped policy changed only by its
flags (`manual`: rule off; `rule`; `rule-nominate`: `nominate_unconfirmed`;
`rule-corroborated`: `corroboration = "second_observation"`; `rule-corroborated-nominate`).
Every arm has the same scripted operator (`scenario::ReviewProxy`: a look every 20 s median,
6 s to read, reads 80% of items, slips 5%; the one track within two combined sigmas of the
fix, its victim excluded, not shown neutral or friendly at 0.5 or more, is the author, else it
can't say; a rule declaration it skims is confirmed unread) and a nominator (every track
declared hostile nominated as soon as the policy lets it, retried every 10 s; two signers
sign 5 and 8 s after it waits, and again while their signature does not stand; they never
refuse).

| measure | definition |
| --- | --- |
| author declared | the hostile author's track declared hostile, by the rule or the person |
| missed | a tracked hostile author nobody declared |
| false hostile / wrong author | a hostile declaration of a contact that is not hostile / of another hostile |
| C2 showed it / picture wrong | a wrong declaration is *C2 showed it* when, at the act, another track had 0.1 or more of the attribution or the declared track was shown neutral or assumed friend at 0.5 or more; else *picture wrong*. Never pooled |
| revocations (of a right one) | the person's revocations of rule declarations, and of those, the ones of the author's |
| items | what was put before the person: every act without the rule; every rule declaration and flag with it (the proxy's tally) |
| decisions | one per item the person got to the end of, whatever it decided: declared, confirmed (read or skimmed), revoked, dismissed, or read and decided nothing. Counted alike in every arm |
| consequential | of the decisions, declarations, confirmations and revocations |
| confirmations (unread) | confirmations of rule declarations, and of those, the ones the proxy skimmed (a model assumption: a skimmed card is approved) |
| agreement | in the manual arm, the person's declarations that named the track the rule would have declared (its best candidate at the shipped threshold, not shown neutral or friendly at 0.5), and the acts the rule would have declared on |
| effects on the author / on a contact not hostile | effects released on the hostile author's track / on a contact that is not hostile, each such case listed with what the C2 showed |
| act to declaration, act to effect | seconds from the first act to the author's first declaration, and to the first effect; medians |

What these are not: model results from scripted proxies on generated situations, never human
data. What they cannot say, stated: the proxy's procedure is close to the rule's own, so their
agreement is built in (the agreement row measures it) and the manual arm says nothing about
how accurately a person identifies; a case the rule gets wrong the proxy mostly gets wrong
too. With confirmation required each rule declaration still costs a person one decision: the
shipped policy moves the decision from declaring to confirming, it does not remove it. The
rule's latency advantage, and what nomination before confirmation saves, are the proxy's look
interval and reading time. The signers never refuse, so a declaration the C2 should not have
made is carried to an effect: "no effect on a contact that was not hostile" holds or not
with the untracked share (at 0.5 every arm released one, listed in the report). The
situations come from stated distributions, not from the world; the proxy's procedure,
parameters and slip rate are assumptions; the tracks are the scenario runner's (truth plus
seeded noise), not the bridge's tracker. Scenarios 72-78 illustrate the rule and run in the
engagement suite; they are not part of this measurement.

## Keep-outs against engagements (rules 6, 9, 12)

`eval/keepout.py` (`python -m eval.run keepout`, and a small one in every batch) measures how
often the keep-outs block the nomination of an engagement on a genuinely hostile target, and
why. It generates situations from stated distributions, decides each with the engine's own
nomination checks under rules 6, 9 and 12, and labels each decision from the generated truth.
The question it serves (murmur-12e): a live run saw 77 of 77 nominations blocked while the
platform was 1 to 2 km out near a circling tour boat; is that the keep-outs working, or making
engagements near ordinary traffic impossible?

**Situations** (seeded; paired: a seed, density and index give the same target, contact
positions, our vehicles and platform placements under every behaviour, only the motion
differs):

| part | distribution |
| --- | --- |
| target | holding in ez-gate-west (eval/ident.py's box), declared hostile by an operator, 1-sigma 8 to 25 m |
| protected contacts | uniform over 8 km around the target, a Poisson count at 0.03, 0.1, 0.3 or 1 per km^2 (0.03 is about the 7 tracks of the 10-06 live run; 1 a busy harbour, a guess); neutral or unknown, none declared; surface |
| behaviour | all contacts one of: stationary; transiting (3 to 12 m/s, a uniform course); circling (100 to 600 m radius, 2 to 8 m/s); manoeuvring (a zigzag, 0.6: 30 to 90 s legs 30 to 60 degrees either side of a course at 3 to 10 m/s; or sprints and stops, 0.4: 5 to 12 m/s for 30 to 90 s, up to 90 degrees of turn each time, 20 to 60 s stopped); new (tracked 0 to 45 s, holding 0.4 or transiting 0.6); or mixed (0.30, 0.40, 0.10, 0.15, 0.05 of those, a guess) |
| our vehicles | the observing USV 1 to 3 km out, holding or straight at 2 to 6 m/s; with chance 0.1 another USV lost (last report 15 to 300 s old, 2 to 8 km out) |
| engaging platform | for each nominal time to effect (30, 45, 60, 90, 120, 180, 240, 300 s) one platform drawn (magura 12 m/s or switchblade 28 m/s) placed (time - 20 s dispatch delay) x cruise away; and each platform at 0.5, 1, 2 and 3 km; bearing uniform |
| nomination | by an operator, effect radius 75 m, fallback return, no window; the shipped policy (keep-out 200 m beyond the radius) |

**What the tracker reports** for each contact (the facts the engine reads; a stated model of
the bridge tracker, `crates/murmur-bridge/src/tracks/model.rs`, with the scenario runner's
noise): its position off by its own 1-sigma (8 to 25 m) per axis; a velocity off by 0.5 m/s
per axis (the scenario runner's `--vnoise`; the Phase G value that passed), none in its first
7 s (a guess at the fixes a 3 m/s velocity sigma takes); an average velocity over the last
minute once tracked 45 s (from two position estimates, each with its noise); a manoeuvre when
its true velocity, sampled every 10 s since the track began, changed by more than
max(2 m/s, the chi-square 13.8 test on two estimates at 0.5 m/s) within 300 s, remembered
300 s, with the fastest speed in the memory plus two sigmas; when the track began. Not
modelled: noise-driven manoeuvre flags on steady tracks, filter lag after a turn, track breaks
and re-initialized tracks, land, channels, air traffic.

**What decides.** `eval/keepout_probe` (its own Cargo workspace, read-only use of
murmur-decide) runs `rules::evaluate` on `WorldSnapshot::for_intent_with` for each evaluation
and rules version: the checks `Engine::submit` runs at a nomination. For the keep-out on
tracks it also lists every protected track's regions (`engage::track_reach`) and applies the
keep-out's own comparison to each region alone, giving each **component**'s margin (its
region's near edge outside the keep-out, negative inside): `current` along the velocity (rules
6), `mean` along the average velocity (rules 9), `manoeuvre` anywhere at the fastest it went
(rules 9), `young` anywhere at the domain's top speed in a track's first 45 s (rules 12),
`no_velocity` anywhere at the top speed before a velocity exists (rules 6). The regions
together must give the engine's verdict for every evaluation (`mismatch` otherwise; 0 in every
run so far).

| measure | definition |
| --- | --- |
| blocked (tracks) | `keep_out_tracks` failed at the nomination |
| blocked (ours) | `keep_out_friendly` failed; by the lost vehicle or by one predicted along its velocity |
| needed | a block with a protected contact truly inside the effect circle (the radius) at the effect time, the engine's predicted point and time against the generated motion |
| conservative | any other block: *buffer* (a protected contact inside the keep-out, outside the circle) or *clear* (nobody inside the keep-out) |
| missed | a pass with a protected contact truly inside the effect circle at the effect time |
| deepest | of a conservative block, the component whose region reaches deepest into the keep-out |
| alone | of a conservative block, the component without whose regions it would have passed, every other region as it was (several components can be inside at once, so the shares need not add up) |
| without rules 9 and 12 regions | a conservative block that would have passed with the `mean`, `manoeuvre` and `young` regions left out |
| time to effect for 50% (90%) | the longest nominal time to effect at which the keep-out on tracks still passes at least half (nine tenths) of the time, linearly interpolated on the grid |

**Intervals.** A cell (density, behaviour, time to effect, rules) has one evaluation per
situation, and situations are independent draws: Wilson 95%. The per-seed rates' Student t
interval is kept beside it in `summary.json` (`tracks_seed_t`); they agree. Shares pooled over a
situation's evaluations (needed, components, missed) treat the situation as a cluster: Wilson
on the effective sample size (the design effect of the ratio). The thresholds' intervals are a
percentile bootstrap over situations. The keep-out on our vehicles counts each field once (it
does not depend on the contacts' behaviour).

### First measurement (10-06, unreviewed)

Model results from simulated geometry, not human data, not field data. Commit 913d457, clean
tree; `python -m eval.run keepout --seeds 1 2 3 4 5 6 7 8 9 10 --situations 100`: 24,000
situations (4,000 fields x 6 behaviours), 1.73 million contacts, 384,000 evaluations each
under rules 6, 9 and 12; 0 mismatches, 0 errors, every evaluation previewed; 40 s on 12
processes. Rates in percent with Wilson 95% intervals; 1,000 situations per cell.

Blocked by the keep-out on tracks, mixed traffic:

| per km^2 | rules | 30 s | 60 s | 120 s | 180 s | 300 s |
| --- | --- | --- | --- | --- | --- | --- |
| 0.03 | 6 | 2 (1-3) | 2 (1-3) | 4 (3-6) | 7 (6-9) | 23 (21-26) |
| 0.03 | 9 | 2 (1-3) | 3 (2-4) | 8 (6-9) | 13 (11-15) | 34 (32-37) |
| 0.03 | 12 | 3 (2-4) | 4 (3-6) | 11 (9-13) | 21 (19-24) | 48 (45-51) |
| 0.1 | 6 | 5 (4-7) | 8 (6-10) | 16 (14-18) | 28 (25-31) | 62 (59-65) |
| 0.1 | 9 | 7 (6-9) | 13 (11-15) | 26 (23-28) | 44 (41-47) | 79 (77-82) |
| 0.1 | 12 | 8 (6-10) | 16 (14-19) | 35 (32-38) | 59 (56-62) | 92 (90-93) |
| 0.3 | 6 | 13 (11-15) | 19 (17-22) | 38 (35-41) | 63 (60-66) | 95 (93-96) |
| 0.3 | 9 | 16 (14-18) | 26 (24-29) | 55 (52-58) | 81 (78-83) | 99 (98-99) |
| 0.3 | 12 | 19 (17-22) | 33 (30-36) | 69 (66-72) | 92 (90-94) | 100 (99-100) |
| 1 | 6 | 35 (32-38) | 52 (49-55) | 82 (80-85) | 96 (95-97) | 100 (100-100) |
| 1 | 9 | 44 (41-47) | 68 (65-70) | 95 (93-96) | 99 (98-100) | 100 (100-100) |
| 1 | 12 | 51 (48-54) | 79 (76-81) | 99 (98-100) | 100 (100-100) | 100 (100-100) |

By behaviour, rules 6 / 9 / 12 (intervals within about 3 points at these sizes):

| per km^2 | time to effect | stationary | transiting | circling | manoeuvring | new | mixed |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 0.1 | 60 s | 8 / 8 / 8 | 7 / 8 / 8 | 6 / 15 / 15 | 6 / 20 / 20 | 22 / 22 / 67 | 8 / 13 / 16 |
| 0.1 | 120 s | 14 / 15 / 15 | 14 / 16 / 16 | 13 / 34 / 34 | 14 / 51 / 51 | 50 / 50 / 97 | 16 / 26 / 35 |
| 0.1 | 180 s | 24 / 27 / 27 | 25 / 27 / 27 | 25 / 54 / 54 | 26 / 74 / 74 | 76 / 76 / 100 | 28 / 44 / 59 |
| 0.3 | 60 s | 17 / 18 / 18 | 17 / 18 / 18 | 16 / 37 / 37 | 17 / 51 / 51 | 48 / 48 / 96 | 19 / 26 / 33 |
| 0.3 | 120 s | 35 / 38 / 38 | 35 / 38 / 38 | 34 / 69 / 69 | 33 / 86 / 86 | 87 / 87 / 100 | 38 / 55 / 69 |
| 0.3 | 180 s | 58 / 61 / 61 | 57 / 61 / 61 | 57 / 89 / 89 | 57 / 98 / 98 | 98 / 98 / 100 | 63 / 81 / 92 |

Needed or conservative, pooled over densities and times (clustered intervals): under every
rules version and behaviour 1 to 2% of blocks were needed and 98 to 99% conservative; in 9 to
22% of blocks a protected contact was inside the 200 m buffer (but not the circle), in the rest
nobody was inside the keep-out. Missed: rules 6 passed 7 circling and 13 manoeuvring evaluations
(of about 21,000 passes each) and 2 mixed with a protected contact truly inside the effect
circle at the effect time; rules 9 and 12 passed none. Those 20 were bought with about 13,000
more blocks in those two behaviours (circling 10,913 to 16,062, manoeuvring 10,978 to
18,875), nearly all conservative.

Conservative blocks, the component deepest / alone (percent):

| behaviour | rules | conservative | current | mean | manoeuvre | young | no velocity | without rules 9 and 12 regions |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| stationary | 9 | 11,270 | 50 / 4 | 50 / 4 | | | | 4 |
| circling | 9 | 15,866 | 11 / 1 | 9 / 1 | 81 / 24 | | | 32 |
| manoeuvring | 9 | 18,675 | 3 / 1 | 1 / 0 | 95 / 32 | | | 42 |
| new | 6 | 18,584 | 9 / 8 | | | | 91 / 47 | |
| new | 12 | 26,041 | 0 / 0 | | | 84 / 29 | 16 / 3 | 29 |
| mixed | 6 | 11,499 | 71 / 68 | | | | 29 / 7 | |
| mixed | 12 | 16,161 | 9 / 1 | 9 / 1 | 22 / 8 | 51 / 13 | 10 / 2 | 29 |

The longest time to effect with a pass on tracks at least 50% / 90% of the time, mixed traffic
(bootstrap 95%), and the distance that is for a holding target at the platform's cruise speed:

| per km^2 | rules | 50% | 90% | magura within (50% / 90%) | switchblade within |
| --- | --- | --- | --- | --- | --- |
| 0.03 | 6 | >300 s | 203 s (189-219) | >3.4 / 2.2 km | >7.8 / 5.1 km |
| 0.03 | 12 | >300 s | 110 s (98-124) | >3.4 / 1.1 km | >7.8 / 2.5 km |
| 0.1 | 6 | 257 s (247-268) | 79 s (63-95) | 2.8 / 0.7 km | 6.6 / 1.6 km |
| 0.1 | 9 | 199 s (190-208) | 47 s (37-56) | 2.2 / 0.3 km | 5.0 / 0.8 km |
| 0.1 | 12 | 158 s (151-165) | 38 s (32-46) | 1.7 / 0.2 km | 3.9 / 0.5 km |
| 0.3 | 6 | 150 s (144-156) | <30 s | 1.6 / <0.1 km | 3.6 / <0.3 km |
| 0.3 | 12 | 88 s (83-93) | <30 s | 0.8 / <0.1 km | 1.9 / <0.3 km |
| 1 | 12 | <30 s | <30 s | | |

The live run's geometry (a magura 1 to 2 km out, circling traffic): blocked 27 and 56% at 0.1
per km^2 and 61 and 90% at 0.3 under rules 9 and 12 (rules 6: 10 and 27%, 29 and 59%). The
keep-out on our vehicles blocked at most 1% up to 120 s and 6% (6-7) at 300 s, the lost vehicle
in about a quarter of those. The region sizes the engine gives a 15 m track (rules 12,
2-sigma included): along a velocity 164 m at 60 s, 388 m at 120 s, 664 m at 180 s, 1,342 m at
300 s; a circling 5 m/s track's manoeuvre region 390, 750, 1,110, 1,830 m; a 10 m/s zigzag's
690, 1,350, 2,010, 3,330 m; a young or new track's 1.5, 3.0, 4.5, 7.5 km.

What these show, read plainly: under the stated distributions the time to effect and the
density decide most of it, and the platform's speed only through the time it gives. Rules 6
alone blocks a third of nominations at 120 s at 0.3 contacts per km^2 even when every
contact holds still or keeps its course, because a steady track's region along its velocity
grows with the span (the 1 m/s velocity error and 0.2 m/s^2 manoeuvre noise). Rules 9's
manoeuvre region multiplies the rate in circling or manoeuvring traffic by two to four at one
to three minutes at 0.1 per km^2 (less where the rate nears 100%), and rules 12's young region raises it wherever new tracks appear, by an amount that
follows the share of new tracks (5% in the mix, a guess). Almost every block is conservative:
nobody truly inside the effect circle. Rules 9's regions caught the few cases rules 6 missed.

What they do not show: how often such situations arise (densities, behaviours and mix are
assumptions); real tracker behaviour (a model of its reports); what the plan's re-checks
while signatures wait would add (more blocks, not fewer); air traffic, land and channels;
whether traffic converging on the target (a harbour mouth) would make more blocks needed; what
operators do when blocked (wait, reposition, accept a lost vehicle, give up), which no part of
this measures.

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
