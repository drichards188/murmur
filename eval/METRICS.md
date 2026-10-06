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
why. It generates situations from stated distributions, makes the protected contacts' tracks
with the bridge's own tracker (or a stated model of it), decides each nomination with the
engine's own checks under rules 6, 9 and 12, and labels each decision from the generated
truth. The question it serves (murmur-12e): a live run saw 77 of 77 nominations blocked while
the platform was 1 to 2 km out near a circling tour boat; is that the keep-outs working, or
making engagements near ordinary traffic impossible?

**Situations** (seeded; paired: a seed, density and index give the same target, contact
positions, our vehicles and platform placements under every behaviour, only the motion
differs):

| part | distribution |
| --- | --- |
| target | holding in ez-gate-west (eval/ident.py's box), declared hostile by an operator, its track the generator's in every variant (1-sigma 8 to 25 m: the good track an engagement needs) |
| protected contacts | over 8 km around the target, a Poisson count at 0.03, 0.1, 0.3 or 1 per km^2 (0.03 is about the 7 tracks of the 10-06 live run; 1 a busy harbour, a guess); neutral or unknown, none declared; surface; uniform, or (lanes) 70% in two lanes each passing 0 to 1.5 km from the target on a uniform course, 100 m wide, transiting along it either way at 3 to 12 m/s |
| behaviour | all contacts one of: stationary; transiting (3 to 12 m/s, a uniform course); circling (100 to 600 m radius, 2 to 8 m/s); manoeuvring (a zigzag, 0.6: 30 to 90 s legs 30 to 60 degrees either side of a course at 3 to 10 m/s; or sprints and stops, 0.4: 5 to 12 m/s for 30 to 90 s, up to 90 degrees of turn each time, 20 to 60 s stopped); new (tracked 0 to 45 s, holding 0.4 or transiting 0.6); or mixed (0.30, 0.40, 0.10, 0.15, 0.05 of those, a guess; the new share 0, 15 or 25% in the sensitivity variants, the others scaled) |
| air (one variant) | one light aircraft loitering within 4 km: circling 500 to 1500 m at 30 to 55 m/s, 150 to 1500 m up, added to the radar variant's mixed traffic (the surface traffic unchanged) |
| our vehicles | the observing USV (Sea Hunter class) 1 to 3 km out, holding or straight at 2 to 6 m/s; with chance 0.1 another USV lost (last report 15 to 300 s old, 2 to 8 km out) |
| engaging platform | for each nominal time to effect (30, 45, 60, 90, 120, 180, 240, 300 s) one platform drawn (magura 12 m/s or switchblade 28 m/s) placed (time - 20 s dispatch delay) x cruise away; and each platform at 0.5, 1, 2 and 3 km; bearing uniform |
| nomination | by an operator, effect radius 75 m, fallback return, no window; the shipped policy (keep-out 200 m beyond the radius) |

**Tracks.** In the primary variant (`radar`) every protected contact's last ten minutes are
played second by second through the bridge's own sensing model and IMM tracker
(`eval/keepout_probe` compiles `crates/murmur-bridge/src/tracks/{model,sensing,truth}.rs`
unchanged, by path): the observing USV's catalog radar (2D, 30 m + 1% of range per fix)
against contacts without AIS. `ais` gives the tracker every sensor the USV carries and every
contact an AIS broadcast (the best case). `model` is the first measurement's stated model of the
tracker's reports with the scenario runner's noise (position off by its own 1-sigma, velocity
off by 0.5 m/s per axis, no velocity in the first 7 s, an average velocity once tracked 45 s, a
manoeuvre when the true velocity changed beyond the bridge's threshold within its memory).
Every contact stays within the USV's sensors, so the bridge's drop of a track unseen for 180 s
never fires (a dropped track stops counting in keep-outs: murmur-533); a contact never detected
(a new one in its first second) has no track and is not protected.

**What decides.** The probe runs `rules::evaluate` on `WorldSnapshot::for_intent_with` for
each evaluation and rules version: the checks `Engine::submit` runs at a nomination. For the
keep-out on tracks it also lists every protected track's regions (`engage::track_reach`) and
applies the keep-out's own comparison to each region alone, giving each **component**'s margin
(unrounded): `current` along the velocity (rules 6), `no_velocity` anywhere at the domain's top
speed before a track has a velocity (rules 6), `mean` along the average velocity (rules 9),
`manoeuvre` anywhere at the fastest it went (rules 9), `young` anywhere at the top speed in a
track's first 45 s (rules 12). The regions together must give the engine's verdict for every
evaluation (`mismatch` otherwise; 0 in every run).

| measure | definition |
| --- | --- |
| blocked | `keep_out_tracks` failed at the nomination (the keep-out on our vehicles is reported apart) |
| perfect knowledge | the share of the same nominations with a protected contact truly inside the keep-out at the effect time: what a keep-out that knew where everyone would be would block |
| needed | a block with a protected contact truly inside the keep-out (the effect radius plus 200 m, the policy's own line) at the effect time, from the engine's predicted point and time against the generated motion |
| severe | a needed block with someone inside the effect circle (the radius) |
| conservative | any other block: nobody inside the keep-out |
| missed | a pass with a protected contact inside the keep-out (severe: the circle) at the effect time |
| over the release window | the same counting anyone inside at any time from the effect time to 60 s after (`late_s`: a holding target may be released that late); the release's own check (not run here) is meant to catch what moves in later |
| deepest / alone | of a conservative block, the component whose region reaches deepest into the keep-out / without whose regions it would have passed (several can be inside at once) |
| time to effect for 50% (90%) | the longest nominal time to effect at which the keep-out on tracks still passes at least half (nine tenths) of the time, interpolated on the grid |
| aircraft | the engine counts an aircraft in plan (our radar gives no altitude); the labels count it in 3D (above the keep-out's height, the radius plus 200 m, it is outside it) |

**Intervals.** A cell (variant, density, behaviour, time to effect, rules) has one evaluation per
situation, and situations are independent draws: Wilson 95% (the per-seed Student t interval is
kept in `summary.json`, `tracks_seed_t`; they agree). Shares pooled over a situation's
evaluations (needed, components, missed) weight the grid's densities and times uniformly and
treat the situation as a cluster: Wilson on the effective sample size (the ratio's design
effect); per-time counts are in `blocks_by_tte`. A miss count of zero carries its one-sided 95%
upper bound, 1 - 0.05^(1/n) over n passes (wider counted per situation). The thresholds'
intervals are a percentile bootstrap over situations. The keep-out on our vehicles counts each
field once.

### Measurement (10-06, unreviewed)

Model results from simulated geometry, not human data, not field data. Commit a306d96, clean
tree; `python -m eval.run keepout --seeds 1 2 3 4 5 6 7 8 9 10 --situations 100`: 13 variants,
108,000 situations (`radar`, `model` and `ais` 24,000 each over every behaviour and density;
`lanes`, `lanes-model`, `new0`, `new15`, `new25`, `vel1`, `sigma60` and `air` 4,000 each over
mixed traffic; `live` and `live2` 2,000 each), 7.5 million contacts, 1,676,000 nominations each
decided under rules 6, 9 and 12; 0 mismatches, 0 errors, every nomination previewed; 378 of the
radar variant's 1.73 million contacts never tracked (new ones in their first second). About ten
minutes on 12 processes. Rates in percent; at 1,000 situations a cell every interval is within
3 points (mixed at 0.1 per km^2, 120 s, rules 12: 50, 47-53).

Blocked on tracks, mixed traffic, the bridge's tracker on our radar (primary), rules 6 / 9 / 12,
and perfect knowledge (rules-independent):

| per km^2 | 30 s | 60 s | 120 s | 180 s | 300 s | perfect knowledge (keep-out; over the release window) |
| --- | --- | --- | --- | --- | --- | --- |
| 0.03 | 2 / 3 / 4 | 5 / 6 / 7 | 11 / 15 / 17 | 24 / 32 / 35 | 58 / 64 / 68 | 1; 1-2 |
| 0.1 | 8 / 10 / 11 | 14 / 19 / 22 | 35 / 45 / 50 | 59 / 71 / 76 | 95 / 98 / 99 | 2-3; 4 |
| 0.3 | 18 / 22 / 24 | 36 / 45 / 48 | 72 / 83 / 88 | 93 / 97 / 98 | 100 | 6-7; 9-11 |
| 1 | 48 / 57 / 62 | 74 / 84 / 89 | 99 / 100 / 100 | 100 | 100 | 18-22; 29-33 |

By behaviour, the same tracker, rules 6 / 9 / 12 (perfect knowledge 2 to 3% at 0.1 per km^2,
6 to 8% at 0.3):

| per km^2 | time to effect | stationary | transiting | circling | manoeuvring | new | mixed |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 0.1 | 60 s | 8 / 9 / 9 | 8 / 8 / 8 | 15 / 29 / 29 | 30 / 44 / 44 | 37 / 37 / 68 | 14 / 19 / 22 |
| 0.1 | 120 s | 18 / 18 / 18 | 18 / 19 / 19 | 37 / 63 / 63 | 68 / 84 / 84 | 77 / 77 / 97 | 35 / 45 / 50 |
| 0.1 | 180 s | 32 / 33 / 33 | 32 / 34 / 34 | 65 / 88 / 88 | 93 / 98 / 98 | 97 / 97 / 100 | 59 / 71 / 76 |
| 0.3 | 60 s | 20 / 20 / 20 | 19 / 20 / 20 | 36 / 63 / 63 | 66 / 82 / 82 | 74 / 74 / 96 | 36 / 45 / 48 |
| 0.3 | 120 s | 41 / 44 / 44 | 42 / 44 / 44 | 75 / 94 / 94 | 97 / 99 / 99 | 99 / 99 / 100 | 72 / 83 / 88 |

What the tracker reported against the truth (the radar variant, about 52,000 sampled contacts):
stationary and transiting contacts 98% with a velocity, its error 0.94 m/s median (1.8 at p90),
position 25 m off, reported 1-sigma 30 m; circling 86% with a velocity, error 1.8 m/s (3.9),
a manoeuvre seen on 62%; manoeuvring only 66% with a velocity, error 2.8 m/s (8.6), a manoeuvre
seen on 58%; new tracks 43% with a velocity. The stated model assumed about 0.6 m/s, 8 to 25 m,
every established track with a velocity and 84% of new ones. With AIS every track had a
velocity, errors 0.05 to 0.2 m/s and a 2 m 1-sigma. The loitering light aircraft never had a
velocity (0 of 120 sampled): the tracker withholds it while a track turns.

Needed or conservative, and misses (pooled over densities and times; clustered intervals):

| variant | behaviour | rules | blocks | needed (keep-out) | severe (circle) | needed over the window | missed (keep-out) | missed (circle) | missed over the window (keep-out / circle) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| radar | mixed | 6 | 16,542 | 14 (13-15) | 1.1 | 21 | 37 of 15,458 | 2 | 278 / 30 |
| radar | mixed | 9 | 18,179 | 13 (12-14) | 1.0 | 20 | 10 of 13,821 | 0 | 151 / 15 |
| radar | mixed | 12 | 18,932 | 13 (12-14) | 0.9 | 19 | 8 of 13,068 | 0 (< 0.023%) | 120 / 11 |
| radar | circling | 6 | 17,012 | 14 | 1.1 | 21 | 96 of 14,988 | 3 | 402 / 45 |
| radar | circling | 9 and 12 | 20,935 | 12 | 0.9 | 19 | 0 of 11,065 (< 0.027%) | 0 | 26 / 2 |
| radar | manoeuvring | 6 | 21,551 | 12 | 0.9 | 19 | 56 of 10,449 | 2 | 190 / 27 |
| radar | manoeuvring | 9 and 12 | 23,682 | 11 | 0.8 | 18 | 1 of 8,318 | 0 | 21 / 0 |
| radar | stationary, transiting | 6, 9, 12 | 12,400 to 12,700 | 20 to 21 | 1.8 to 1.9 | 20 to 35 | 0 or 1 | 0 | 0 to 557 |
| model | circling | 6 | 10,913 | 22 | 1.7 | 30 | 186 of 21,087 | 7 | 747 / 87 |
| model | manoeuvring | 6 | 10,978 | 21 | 1.7 | 32 | 241 of 21,022 | 13 | 794 / 108 |
| model | mixed | 6 | 11,673 | 20 | 1.5 | 29 | 63 of 20,327 | 2 | 450 / 52 |
| model | every behaviour | 9 and 12 | | 10 to 23 | 0.7 to 2.0 | 15 to 38 | 0 (< 0.014 to 0.05%) | 0 | 0 to 554 / 0 to 62 |
| lanes (radar) | mixed | 6 / 12 | | 28 / 26 | | | 9 / 1 | | 412 / 351 |

Conservative blocks under the bridge's tracker, the component deepest / alone (percent): mixed
traffic at rules 6, no velocity 87 / 42 and current 13 / 11; at rules 12, no velocity 58 / 19,
young 19 / 5, manoeuvre 17 / 8, current and mean 3 / 0 to 1, and 14% would have passed without
the rules 9 and 12 regions; circling at rules 9, no velocity 58 / 12 and manoeuvre 40 / 20;
manoeuvring at rules 9, no velocity 77 / 20 and manoeuvre 22 / 9. Under the stated model, where
almost every track has a velocity, the manoeuvre region dominated instead (circling 80 / 27,
manoeuvring 95 / 36) and the young one in mixed traffic (53 / 15).

The longest time to effect with a pass on tracks at least 50% / 90% of the time, mixed traffic,
the bridge's tracker, rules 12 (bootstrap 95%), and the distance that is for a holding target:

| per km^2 | 50% | 90% | magura within (50% / 90%) | switchblade within |
| --- | --- | --- | --- | --- |
| 0.03 | 235 s (224-245) | 81 s (69-94) | 2.6 / 0.7 km | 6.0 / 1.7 km |
| 0.1 | 120 s (114-127) | <30 s (<30-34) | 1.2 / <0.1 km | 2.8 / <0.3 km |
| 0.3 | 62 s (58-66) | <30 s | 0.5 / <0.1 km | 1.2 / <0.3 km |
| 1 | <30 s | <30 s | | |

(rules 6: 274, 157, 83 and 32 s for 50% at the four densities.)

The live run's question (`live`, `live2`: one boat circling within 1 or 2 km of the target,
followed for a while, mixed traffic around, a magura at 1, 1.5 and 2 km; the bridge's tracker):

| circling boat within | per km^2 | rules 6 at 1 / 1.5 / 2 km | rules 9 and 12 | perfect knowledge (keep-out; over the window) |
| --- | --- | --- | --- | --- |
| 1 km | 0.03 | 40 / 51 / 59 | 87 / 91 / 94 | 8-9; 12-14 |
| 1 km | 0.1 | 54 / 70 / 82 | 92 / 95-96 / 98 | 8-10; 13-15 |
| 2 km | 0.03 | 22 / 33 / 45 | 53-54 / 76-77 / 86-87 | 2-3; 5 |
| 2 km | 0.1 | 41 / 59 / 75 | 70-73 / 88-90 / 95-96 | 4; 7 |

Sensitivity, mixed traffic at 0.1 per km^2 (other densities in the report and `summary.json`):

| variant | blocked at 60 / 120 / 180 s, rules 6 | rules 12 | perfect knowledge | 50% pass, rules 12 | needed share, rules 12 | missed (keep-out), rules 6 / 12 |
| --- | --- | --- | --- | --- | --- | --- |
| radar (primary) | 14 / 35 / 59 | 22 / 50 / 76 | 3 / 2 / 3 | 120 s | 13 | 37 / 8 |
| model | 8 / 16 / 28 | 16 / 35 / 59 | 3 / 2 / 3 | 158 s | 15 | 63 / 0 |
| ais | 7 / 15 / 25 | 15 / 34 / 55 | 3 / 2 / 3 | 165 s | 15 | 66 / 0 |
| lanes (radar) | 21 / 43 / 67 | 23 / 48 / 73 | 8 / 7 / 8 | 124 s | 26 | 9 / 1 |
| lanes-model | 17 / 34 / 57 | 20 / 40 / 65 | 8 / 7 / 8 | 144 s | 29 | 11 / 1 |
| new0 (radar) | 13 / 30 / 53 | 18 / 40 / 67 | 3 / 3 / 3 | 142 s | 15 | 29 / 2 |
| new15 (radar) | 18 / 41 / 70 | 29 / 62 / 87 | 4 / 3 / 3 | 98 s | 13 | 24 / 2 |
| new25 (radar) | 20 / 48 / 78 | 34 / 72 / 94 | 3 / 3 / 2 | 84 s | 11 | 12 / 1 |
| vel1 (model) | 8 / 16 / 28 | 17 / 37 / 61 | 3 / 2 / 3 | 153 s | 15 | 66 / 0 |
| sigma60 (model) | 9 / 16 / 28 | 18 / 37 / 60 | 3 / 2 / 3 | 154 s | 14 | 56 / 0 |
| air (radar, one aircraft) | 100 / 100 / 100 | 100 / 100 / 100 | 3 / 2 / 3 | <30 s | 8 | 0 / 0 |

Velocity error 1 m/s and track sigma 15 to 60 m barely move the model's rates (3 points or
less). The tracker (the bridge's on one radar against the model: 50 against 35% at 120 s), the
share of new tracks (40 to 72% from none to a quarter) and the layout move them a lot, and one
loitering aircraft decides every nomination near it.

The keep-out on our vehicles blocked at most 1% up to 120 s and 6% (6-7) at 300 s, the lost
vehicle in about a quarter of those. The region sizes the engine gives a 15 m track (rules 12,
2-sigma included): along a velocity 164 m at 60 s, 388 m at 120 s, 664 m at 180 s, 1,342 m at
300 s; a circling 5 m/s track's manoeuvre region 390, 750, 1,110, 1,830 m; a 10 m/s zigzag's
690, 1,350, 2,010, 3,330 m; a young or new surface track's 1.5, 3.0, 4.5, 7.5 km (an aircraft's
no-velocity region at 100 m/s four times that). The 8 km field holds every region up to about
310 s of span; only the few nominations whose actual time to effect ran longer (1,560 of
192,000 more than 5 s off the nominal, at most 357 s) can lose a young region beyond it.

**What this shows, in plain words, for the decision.**

- Robust in every variant: blocks climb steeply with the time to effect and with traffic
  density. Launching closer is the strongest lever: at 0.1 contacts per km^2 a nomination
  under rules 12 passes half the time at about 2 minutes to effect (a magura within about
  1.2 km) and nine times in ten only under about 30 s.
- Robust: rules 9 adds blocks mainly in circling or manoeuvring traffic, and rules 12 near
  young tracks. Rules 9's regions caught every release rules 6 allowed with someone truly
  inside the effect circle, and nearly every one with someone inside the keep-out (radar,
  mixed: 37 rules 6 misses at the keep-out, 8 at rules 12, none in the circle).
- Robust: most blocks come from not knowing where contacts will be, not from contacts being
  close. Perfect knowledge would block 2 to 3% of these nominations at 0.1 per km^2 (7 to 8% in
  lanes); the engine blocks 15 to 76% at 60 to 180 s. That gap is the price of uncertainty as
  the engine models it, not evidence that the constants are too large for real boats. Of the
  blocks, 10 to 31% were needed at the keep-out line (1 to 2% in the effect circle).
- So the levers for fewer blocks are shorter prediction spans, better track quality (a track
  without a velocity is a 25 m/s disc: under the bridge's tracker on one radar that region, not
  the rules 9 or 12 ones, drives most conservative blocks), and how young tracks are handled,
  not the 200 m buffer.
- Not robust: every absolute rate. The tracker, the share of new tracks, the layout and air
  traffic each move them by tens of points; one loitering aircraft without a velocity estimate
  blocks every nomination within reach.
- The nomination check is a screen: over the 60 s release window more contacts move in (radar,
  mixed, rules 12: 120 passes with someone inside the keep-out by then, 11 in the circle), which
  the release's own check is meant to catch.

What it cannot support: calling blocks unnecessary, loosening a keep-out, or any absolute rate
for a real place. Any loosening should be judged by the misses at the keep-out level it brings,
under the real tracker. Not measured: how often such situations arise, more than one observing
sensor, land and channels, air traffic beyond one aircraft, out-of-view contacts and dropped
tracks, the plan's re-checks while signatures wait, and what operators do when blocked (wait,
reposition, accept a lost vehicle, give up).

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
