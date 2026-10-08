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

## Keep-outs against engagements (rules 6, 9, 12, 14; out of view, 16)

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
| air (four variants) | one light aircraft within 4 km, loitering (circling 500 to 1500 m at 30 to 55 m/s) or flying straight past at that speed, 150 to 1500 m up, added to the mixed traffic (the surface traffic unchanged), tracked on our radar alone or every sensor: the worst case, no ADS-B and no height-finding radar |
| buffer (four variants) | copies of the shipped policy with `keep_out_m` 100 or 50 (shipped: 200), on the radar or all-sensors tracker, mixed traffic; the truth labels keep the shipped line |
| our vehicles | the observing USV (Sea Hunter class) 1 to 3 km out, holding or straight at 2 to 6 m/s; with chance 0.1 another USV lost (last report 15 to 300 s old, 2 to 8 km out) |
| engaging platform | for each nominal time to effect (30, 45, 60, 90, 120, 180, 240, 300 s) one platform drawn (magura 12 m/s or switchblade 28 m/s) placed (time - 20 s dispatch delay) x cruise away; and each platform at 0.5, 1, 2 and 3 km; bearing uniform |
| nomination | by an operator, effect radius 75 m, fallback return, no window; the shipped policy (keep-out 200 m beyond the radius) |

**Tracks.** In the real-tracker variants every protected contact's last ten minutes are played
second by second through the bridge's own sensing model and IMM tracker
(`eval/keepout_probe` compiles `crates/murmur-bridge/src/tracks/{model,sensing,truth}.rs`
unchanged, by path), against contacts without AIS: `radar` is the observing USV's catalog
radar alone (2D, 30 m + 1% of range per fix), the pessimistic sensor case, not the general one;
`all` is every sensor it carries (radar, EO/IR, sonar). The live stack, with three to five
observers carrying radar and EO/IR, gave non-AIS circling boats a velocity in 97 to 100% of
samples (1-sigma 6 to 9 m), better than either. `ais` gives the tracker every sensor and every
contact an AIS broadcast (the best case). `model` is the first measurement's stated model of the
tracker's reports with the scenario runner's noise (position off by its own 1-sigma, velocity
off by 0.5 m/s per axis, no velocity in the first 7 s, an average velocity once tracked 45 s, a
manoeuvre when the true velocity changed beyond the bridge's threshold within its memory).
Every contact stays within the USV's sensors, so the bridge's drop of a track unseen for 180 s
never fires, except in the out-of-view variants (below); a contact never detected (a new one in
its first second) has no track and is not protected.

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
| needed | a block with a protected contact truly inside the keep-out (the effect radius plus 200 m, the shipped policy's own line, in every variant) at the effect time, from the engine's predicted point and time against the generated motion |
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

Model results from simulated geometry, not human data, not field data. Commit 7ad80be, clean
tree, master aab563f merged (the probe's output byte-identical across the merge);
`python -m eval.run keepout --seeds 1 2 3 4 5 6 7 8 9 10 --situations 100`: 27 variants,
172,000 situations (`radar`, `all`, `model` and `ais` 24,000 each over every behaviour and
density; 15 mixed-traffic variants 4,000 each; 8 live variants 2,000 each), 11.5 million
contacts, 2,544,000 nominations each decided under rules 6, 9 and 12; 0 mismatches, 0 errors,
every nomination previewed. About 16 minutes on 12 processes. Rates in percent; at 1,000
situations a cell every interval is within 3 points (radar, mixed, 0.1 per km^2, 120 s:
rules 6 35, 32-38; rules 12 50, 47-53).

Blocked on tracks, mixed traffic, rules 6 / 9 / 12, the bridge's tracker on our USV's radar
alone (the pessimistic sensor case) and on every sensor it carries, and perfect knowledge
(rules- and sensor-independent):

| per km^2 | tracker | 30 s | 60 s | 120 s | 180 s | 300 s | perfect knowledge (keep-out; over the release window) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 0.03 | radar | 2 / 3 / 4 | 5 / 6 / 7 | 11 / 15 / 17 | 24 / 32 / 35 | 58 / 64 / 68 | 1; 1-2 |
| 0.03 | all | 2 / 3 / 3 | 2 / 5 / 6 | 8 / 13 / 16 | 17 / 25 / 31 | 46 / 59 / 66 | |
| 0.1 | radar | 8 / 10 / 11 | 14 / 19 / 22 | 35 / 45 / 50 | 59 / 71 / 76 | 95 / 98 / 99 | 2-3; 4 |
| 0.1 | all | 6 / 9 / 10 | 10 / 17 / 21 | 26 / 40 / 47 | 48 / 64 / 73 | 87 / 95 / 98 | |
| 0.3 | radar | 18 / 22 / 24 | 36 / 45 / 48 | 72 / 83 / 88 | 93 / 97 / 98 | 100 | 6-7; 9-11 |
| 0.3 | all | 15 / 19 / 22 | 27 / 37 / 42 | 57 / 75 / 83 | 85 / 94 / 98 | 100 | |
| 1 | radar | 48 / 57 / 62 | 74 / 84 / 89 | 99 / 100 / 100 | 100 | 100 | 18-22; 29-33 |
| 1 | all | 40 / 51 / 57 | 65 / 79 / 86 | 96 / 99 / 100 | 100 | 100 | |

By behaviour at 0.1 per km^2, 120 s, rules 6 / 9 / 12 (perfect knowledge 2 to 3%):

| tracker | stationary | transiting | circling | manoeuvring | new | mixed |
| --- | --- | --- | --- | --- | --- | --- |
| radar | 18 / 18 / 18 | 18 / 19 / 19 | 37 / 63 / 63 | 68 / 84 / 84 | 77 / 77 / 97 | 35 / 45 / 50 |
| all | 15 / 15 / 15 | 15 / 16 / 16 | 23 / 55 / 55 | 51 / 79 / 79 | 60 / 60 / 97 | 26 / 40 / 47 |
| model | 14 / 15 / 15 | 14 / 16 / 16 | 13 / 34 / 34 | 14 / 51 / 51 | 50 / 50 / 97 | 16 / 26 / 35 |

What the tracker reported against the truth (about 52,000 sampled contacts each):

| tracker | steady contacts: velocity, its error (median / p90), 1-sigma | circling: velocity, error, manoeuvre seen | manoeuvring: velocity, error, manoeuvre seen | new: velocity |
| --- | --- | --- | --- | --- |
| radar | 98%, 0.94 / 1.8 m/s, 30 m | 86%, 1.8 m/s, 62% | 66%, 2.8 m/s (8.6 at p90), 58% | 43% |
| all | 99%, 0.63 / 1.3 m/s, 15 m | 93%, 1.3 m/s, 82% | 76%, 1.6 m/s (7.5), 87% | 68% |
| ais | 100%, 0.05 / 0.13 m/s, 2 m | 100%, 0.2 m/s, 99% | 100%, 0.06 m/s, 100% | 100% |
| model (assumed) | 100%, about 0.6 m/s, 8 to 25 m | 100% | 100% | 84% |

Needed or conservative, and misses, mixed traffic (pooled over densities and times; clustered
intervals; the line is the shipped keep-out):

| tracker | rules | blocks | needed (keep-out) | severe (circle) | missed at the keep-out line | missed in the circle | missed over the window (keep-out / circle) |
| --- | --- | --- | --- | --- | --- | --- | --- |
| radar | 6 | 16,542 | 14 (13-15) | 1.1 | 37 of 15,458 | 2 | 278 / 30 |
| radar | 9 | 18,179 | 13 | 1.0 | 10 of 13,821 | 0 | 151 / 15 |
| radar | 12 | 18,932 | 13 | 0.9 | 8 of 13,068 | 0 (< 0.023%) | 120 / 11 |
| all | 6 | 14,642 | 16 (15-17) | 1.2 | 63 of 17,358 | 3 | 362 / 47 |
| all | 9 | 17,132 | 14 | 1.0 | 2 of 14,868 | 0 | 159 / 20 |
| all | 12 | 18,312 | 13 | 1.0 | 2 of 13,688 | 0 (< 0.022%) | 129 / 13 |
| model | 6 / 12 | 11,673 / 16,337 | 20 / 15 | 1.5 / 1.1 | 63 / 0 of 15,663 | 2 / 0 | 450 / 52; 173 / 15 |
| ais | 6 / 12 | 10,861 / 15,949 | 22 / 15 | 1.6 / 1.1 | 66 / 0 of 16,051 | 2 / 0 | 465 / 47; 201 / 18 |

By behaviour, rules 6 passed with someone in the circle 2 to 13 times per 10,000 to 21,000
passes (circling: radar 3, all 8, model 7; manoeuvring: 2, 3, 13), and with someone inside the
keep-out line 56 to 245 times in circling or manoeuvring traffic; rules 9 and 12 never with
someone in the circle, and 0 or 1 at the keep-out line. Of the blocks, 10 to 31% were needed at
the keep-out line in every variant (1 to 2% in the effect circle).

Conservative blocks, mixed traffic, rules 12, the component deepest / alone (percent): on the
radar alone, no velocity 58 / 19, young 19 / 5, manoeuvre 17 / 8; on every sensor, no velocity
37 / 10, young 31 / 7, manoeuvre 26 / 11 (23% would have passed without the rules 9 and 12
regions); under the stated model, young 53 / 15, manoeuvre 22 / 9. A track without a velocity is
a 25 m/s disc: on one radar alone that region drives most conservative blocks, less so with more
sensors, and the live stack's several observers gave velocities far more often.

The longest time to effect with a pass on tracks at least 50% / 90% of the time, mixed traffic,
rules 12 (bootstrap 95%), and the distance that is for a holding target:

| per km^2 | radar | all | magura within, radar / all (50%) | switchblade within, radar / all (50%) |
| --- | --- | --- | --- | --- |
| 0.03 | 235 s (224-245) / 81 s (69-94) | 244 s (234-254) / 85 s (74-96) | 2.6 / 2.7 km | 6.0 / 6.3 km |
| 0.1 | 120 s (114-127) / <30 s | 128 s (120-134) / 31 s (<30-36) | 1.2 / 1.3 km | 2.8 / 3.0 km |
| 0.3 | 62 s (58-66) / <30 s | 69 s (66-73) / <30 s | 0.5 / 0.6 km | 1.2 / 1.4 km |
| 1 | <30 s / <30 s | <30 s / <30 s | | |

The 200 m buffer (copies of the policy with `keep_out_m` 100 and 50; mixed traffic; the misses
counted at the shipped line):

| tracker | keep_out_m | rules 12 at 30 / 60 / 120 s, 0.1 per km^2 | rules 6 at 30 s, 0.1 | rules 12 at 30 s, 0.3 | 90% pass, rules 12, 0.1 | missed at the shipped keep-out line, rules 6 / 12 | missed in the circle, rules 6 / 12 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| radar | 200 | 11 / 22 / 50 | 8 | 24 | <30 s | 37 / 8 | 2 / 0 |
| radar | 100 | 8 / 19 / 46 | 5 | 18 | 36 s | 118 / 42 | 3 / 0 |
| radar | 50 | 7 / 17 / 45 | 4 | 16 | 40 s | 215 / 105 | 5 / 0 |
| all | 200 | 10 / 21 / 47 | 6 | 22 | 31 s | 63 / 2 | 3 / 0 |
| all | 100 | 7 / 17 / 43 | 4 | 16 | 39 s | 154 / 27 | 4 / 0 |
| all | 50 | 5 / 16 / 41 | 3 | 13 | 44 s | 299 / 113 | 5 / 0 |

The buffer is a lever about as large as better sensors (radar alone to every sensor: 50 to
47% at 120 s; 200 to 50 m: 50 to 45%), mainly at short times to effect, where it halves rules 6
at 30 s. A smaller buffer passes more nominations with someone inside the shipped line (rules
12: 8 to 105 on the radar) though none with someone in the effect circle under rules 9 or 12;
whether the buffer may shrink is a decision about the effect model's margin, which this does
not measure.

The live run's question (one boat circling within 1 or 2 km of the target, followed for a
while, mixed traffic around, a magura at 1, 1.5 and 2 km), rules 12 (rules 9 within a point or
two) and rules 6:

| circling boat within | per km^2 | radar | all | ais | model | perfect knowledge |
| --- | --- | --- | --- | --- | --- | --- |
| 1 km | 0.03 | 87 / 91 / 94 (r6 40 / 51 / 59) | 92 / 96 / 98 (35 / 44 / 52) | 77 / 93 / 97 (31 / 38 / 43) | 84 / 95 / 98 (32 / 38 / 44) | 8-9 |
| 1 km | 0.1 | 92 / 96 / 98 (54 / 70 / 82) | 95 / 98 / 99 (48 / 61 / 72) | 84 / 96 / 99 (41 / 50 / 58) | 88 / 98 / 100 (42 / 52 / 60) | 8-10 |
| 2 km | 0.03 | 54 / 77 / 87 (22 / 33 / 45) | 46 / 73 / 88 (15 / 27 / 39) | 26 / 43 / 62 (11 / 20 / 27) | 29 / 46 / 67 (10 / 20 / 28) | 2-3 |
| 2 km | 0.1 | 73 / 90 / 96 (41 / 59 / 75) | 66 / 87 / 94 (31 / 47 / 63) | 45 / 64 / 83 (20 / 30 / 45) | 48 / 68 / 85 (21 / 32 / 47) | 4 |

A boat circling within 1 km blocks 77 to 100% of these nominations under rules 9 and 12 for
every tracker, as the live run's 77 of 77; within 2 km it depends on the tracker (26 to 83% with
AIS, 54 to 96% on the radar alone).

Air traffic, the worst case (one light aircraft within 4 km, no ADS-B, no height-finding radar;
the engine counts it in plan), 120 sampled aircraft each:

| variant | aircraft with a velocity | its reported 1-sigma | rules 12 at 30 / 60 / 120 s, 0.03 per km^2 |
| --- | --- | --- | --- |
| radar, loitering | 0 | 46 m | 67 / 100 / 100 |
| radar, straight | 1 | 31 m | 73 / 100 / 100 |
| all sensors, loitering | 2 | 24 m | 65 / 100 / 100 |
| all sensors, straight | 50 | 16 m | 41 / 64 / 70 |

The cause is not turning: on 2D radar fixes the bridge's air tracker keeps the velocity's
uncertainty above the 3 m/s it reports at, straight or turning, so the keep-out counts the
aircraft anywhere within 100 m/s of where it was; EO/IR helps a straight one, not a loitering
one. The air tracker's constants (initial velocity uncertainty, process noise) were scaled from
the surface ones and never checked against truth, so these rows say what the bridge does today,
not what a validated air tracker would. (10-07: the air tracker was calibrated, a 2D radar's fix
of an aircraft made honest, and rules 14 added; below, "The air tracker against truth".)

Sensitivity, mixed traffic at 0.1 per km^2 (other densities in the report):

| variant | rules 6 at 30 / 60 / 120 / 180 s | rules 12 | perfect knowledge | 50% / 90% pass, rules 12 | needed share, rules 12 | missed at the line, rules 6 / 12 | in the circle, rules 6 / 12 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| radar | 8 / 14 / 35 / 59 | 11 / 22 / 50 / 76 | 3 / 3 / 2 / 3 | 120 / <30 s | 13 | 37 / 8 | 2 / 0 |
| all | 6 / 10 / 26 / 48 | 10 / 21 / 47 / 73 | 3 / 3 / 2 / 3 | 128 / 31 s | 13 | 63 / 2 | 3 / 0 |
| model | 5 / 8 / 16 / 28 | 8 / 16 / 35 / 59 | 3 / 3 / 2 / 3 | 158 / 38 s | 15 | 63 / 0 | 2 / 0 |
| ais | 5 / 7 / 15 / 25 | 7 / 15 / 34 / 55 | 3 / 3 / 2 / 3 | 165 / 44 s | 15 | 66 / 0 | 2 / 0 |
| lanes (radar) | 12 / 21 / 43 / 67 | 13 / 23 / 48 / 73 | 7 / 8 / 7 / 8 | 124 / <30 s | 26 | 9 / 1 | 0 / 0 |
| lanes-model | 11 / 17 / 34 / 57 | 12 / 20 / 40 / 65 | 7 / 8 / 7 / 8 | 144 / <30 s | 29 | 11 / 1 | 0 / 0 |
| new0 (radar) | 6 / 13 / 30 / 53 | 9 / 18 / 40 / 67 | 2 / 3 / 3 / 3 | 142 / 34 s | 15 | 29 / 2 | 0 / 0 |
| new15 (radar) | 8 / 18 / 41 / 70 | 13 / 29 / 62 / 87 | 3 / 4 / 3 / 3 | 98 / <30 s | 13 | 24 / 2 | 0 / 0 |
| new25 (radar) | 10 / 20 / 48 / 78 | 16 / 34 / 72 / 94 | 3 / 3 / 3 / 2 | 84 / <30 s | 11 | 12 / 1 | 0 / 0 |
| vel1 (model) | 5 / 8 / 16 / 28 | 8 / 17 / 37 / 61 | 3 / 3 / 2 / 3 | 153 / 36 s | 15 | 66 / 0 | 2 / 0 |
| sigma60 (model) | 6 / 9 / 16 / 28 | 9 / 18 / 37 / 60 | 3 / 3 / 2 / 3 | 154 / 33 s | 14 | 56 / 0 | 2 / 0 |
| buffer100 (radar) | 5 / 11 / 32 / 56 | 8 / 19 / 46 / 74 | 3 / 3 / 2 / 3 | 128 / 36 s | 13 | 118 / 42 | 3 / 0 |
| buffer50 (radar) | 4 / 10 / 30 / 54 | 7 / 17 / 45 / 73 | 3 / 3 / 2 / 3 | 131 / 40 s | 13 | 215 / 105 | 5 / 0 |
| air (radar) | 70 / 100 / 100 / 100 | 71 / 100 / 100 / 100 | 3 / 3 / 2 / 3 | <30 / <30 s | 8 | 0 / 0 | 0 / 0 |

Velocity error 1 m/s and track sigma 15 to 60 m barely move the model's rates (3 points or
less). The sensors (radar alone to every sensor: 50 to 47% at 120 s, 35 to 26% under rules 6),
the share of new tracks (40 to 72% from none to a quarter), the layout and the buffer move them;
one aircraft seen only by our radar decides every nomination near it.

The keep-out on our vehicles blocked at most 1% up to 120 s and 6% (6-7) at 300 s, the lost
vehicle in about a quarter of those. The region sizes the engine gives a 15 m track (rules 12,
2-sigma included): along a velocity 164 m at 60 s, 388 m at 120 s, 664 m at 180 s, 1,342 m at
300 s; a circling 5 m/s track's manoeuvre region 390, 750, 1,110, 1,830 m; a 10 m/s zigzag's
690, 1,350, 2,010, 3,330 m; a young or new surface track's 1.5, 3.0, 4.5, 7.5 km (an aircraft's
no-velocity region at 100 m/s four times that). The 8 km field holds every region up to about
310 s of span; only the few nominations whose actual time to effect ran longer (1,560 of
192,000 more than 5 s off the nominal, at most 357 s) can lose a young region beyond it.

**What these numbers can support** (model results, unreviewed):

- Blocks climb steeply with the time to effect and with traffic density, for every tracker:
  launching closer is the strongest lever (at 0.1 contacts per km^2, rules 12 passes half the
  time at about 2 minutes to effect, a magura within about 1.2 km, and nine times in ten only
  around 30 s).
- Rules 9 and 12 never let a nomination through with someone in the effect circle, in any
  variant at the shipped buffer; rules 6 did, 2 to 13 times per 10,000 to 21,000 passes
  depending on traffic and tracker. At the keep-out line rules 9 and 12 cut the misses from 37 to
  66 to 0 to 8 per 13,000 to 16,000 passes of mixed traffic.
- A boat circling within 1 km of the target blocks nearly everything under rules 9 and 12 (77 to
  100%, every tracker), matching the live run.
- Most blocks come from uncertainty about where contacts will be, not from contacts being
  close: perfect knowledge would block 2 to 3% of these nominations at 0.1 per km^2 (7 to 8% in
  lanes) where the engine blocks 15 to 76% at 60 to 180 s. That gap is the price of uncertainty
  as the engine models it, not evidence that the constants are too large for real boats.
- Aircraft seen only by our 2D radar (no ADS-B, no height-finding radar) block engagements
  within their reach.
- The levers for fewer blocks are launching closer, better tracks (more sensors), how young
  tracks are handled, and the buffer, which is about as large a lever as better sensors, mainly
  at short times to effect. Whether the buffer may shrink is a decision about the effect model's
  margin, which this does not measure.

**What they cannot support:**

- That missing velocity estimates drive surface blocks in general: that holds for one USV's
  radar alone and depends on the sensors.
- That the buffer is not a lever.
- Any absolute rate for a real place, or any loosening: judge a loosening by the misses at the
  keep-out line it brings, under the real tracker (a 50 m buffer took them from 8 to 105 on the
  radar, none in the circle).
- Anything about the plan's re-checks while signatures wait, the release's own check (over the
  60 s window more contacts move in, which it is meant to catch), or what operators do when
  blocked (wait, reposition, accept a lost vehicle, give up).

Not measured either: how often such situations arise, more than one observing vehicle, land
and channels, air traffic beyond one aircraft. Out-of-view contacts and dropped tracks are
measured next.

### Out of view (rules 16, murmur-533 and its review; 10-07, unreviewed)

The question: the tracker drops a track unseen for 180 s, and before rules 16 the keep-out
stopped counting it then, and counted it by its course alone while it was unseen but not yet
dropped. Rules 16 counts a contact out of view (unseen 10 s, live or lost) from its last fix
anywhere at its domain's top speed (25 m/s on the water, or its own bound if faster), a lost
one with no cap until an operator with engagement authority accepts it for one engagement or
an operator confirms a track carrying its AIS as it (DECISIONS.md, "A dropped protected contact
stays in the keep-out" and its review). What does each choice miss, and what does holding it
cost?

**How.** The variants `oov` and `oov-all` take the mixed traffic above (radar alone; every
sensor) and send 30% of the contacts out of view (`masked_from_s`: no sensor detects them from a
uniform time 30 to 900 s before the nomination, whatever the range, as in a radar shadow, clutter
or a dark small boat; each was tracked 60 to 600 s before that), while they keep moving as their
behaviour says; since the review a quarter of those also speed up after their drop (from a
uniform time up to 240 s after it, 12 to 25 m/s on a uniform course: `oov_speedup_share`, before
which every behaviour but sprint-and-stop kept one speed for good). The bridge's own tracker
(eval/keepout_probe) drops each track at the first second it has been unseen 180 s, and hands the
facts then to the engine as its lost contact under rules 16 (rules 14: gone). `oov-near` is the
review's case: the traffic in view and one contact within 1 km of the target going out of view
the same way. Densities 0.03, 0.1, 0.3 (near: 0.03, 0.1) per km^2, the times to effect of the
grid, seeds 1 to 10, 300 situations per cell: 72,000 evaluations each for `oov` and `oov-all`,
48,000 for `oov-near`, every one decided by the engine under rules 14 and 16; 0 mismatches, 0
errors. The policies, all on the same evaluations: `today` (rules 14), `held` (rules 16 as
shipped), `stale_course` (held, but a live track out of view by its course: before the review's
item 4), `own_speed` (a lost contact by a live track's regions and anywhere at its own speed
bound, its speed and 2-sigma of its velocity error or the fastest it went, and a live track out
of view by its course: the second version, shipped before the review), `heading` (a lost
contact by a live track's regions alone: the first version), `held600` (held, but a lost contact
only until 600 s after its drop), `near3km` (held, but a lost contact only with its last fix within
3 km of the effect point), `accepted` (every lost contact accepted, or cleared by any rule: every
one here is within the observer's nominal sensor coverage, 40 km of radar, so "clear the water
our sensors covered with no detection" clears them all; live tracks out of view still count as
shipped). Commit e4f58af, clean, about four minutes; `python -m eval.run keepout --variants oov
oov-all oov-near --seeds 1 2 3 4 5 6 7 8 9 10 --situations 300`. The `own_speed` rows reproduce
the first measurement's `held` (commit bd51261: 90% blocked in `oov`, 99% in `oov-near`) on the
new traffic. Run again at commit 25b466f (clean, 10-08) after the review of the merged branch:
the engine now judges a lost contact even when its id is in the picture (the tracker never
reuses one), and the probe no longer skips a lost contact whose contact is tracked again under
the same key, so an earlier drop of a contact back in view now counts as well. Every number in
the two tables below came out the same (the pooled rows of all three variants and the blocked
percentages by time to effect), 0 mismatches and 0 errors: in these variants a masked contact
never comes back into view, and no contact that came back after a drop changed a verdict.

Pooled over densities and times to effect (blocked and needing an acceptance: percent of
nominations, Wilson 95%; misses: passes with a protected contact truly inside the keep-out line or
the effect circle at the effect time, then in the circle within the 60 s release window; in
brackets, those by a contact out of view and dropped; then at the line by one out of view but not
yet dropped, and by one that sped up after its drop):

| variant | policy | blocked | needs an acceptance | passes | missed at the line | in the circle | in the circle over the window | at the line, not dropped yet | at the line, sped up |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| oov (radar) | today | 51 (51-52) | 0 | 35,109 | 161 (160) | 7 (7) | 68 (49) | 0 | 25 |
| | held | 95 (95-95) | 30 (29-30) | 3,616 | 1 (0) | 0 | 0 | 0 | 0 |
| | stale_course | 94 | 30 | 4,281 | 1 (0) | 0 | 0 | 0 | 0 |
| | own_speed | 90 | 27 | 7,085 | 1 (0) | 0 | 2 (0) | 0 | 0 |
| | heading | 87 | 24 | 9,214 | 1 (0) | 0 | 2 (0) | 0 | 0 |
| | held600 | 93 | 28 | 4,827 | 1 (0) | 0 | 0 | 0 | 0 |
| | near3km | 76 | 11 | 17,184 | 17 (16) | 1 (1) | 8 (4) | 0 | 4 |
| | accepted | 65 | 0 | 25,066 | 82 (81) | 4 (4) | 30 (24) | 0 | 13 |
| oov-all | today | 49 | 0 | 37,026 | 169 (169) | 8 (8) | 74 (58) | 0 | 29 |
| | held | 95 | 31 | 3,709 | 0 | 0 | 0 | 0 | 0 |
| | stale_course | 94 | 31 | 4,391 | 0 | 0 | 0 | 0 | 0 |
| | own_speed | 89 | 28 | 7,563 | 1 (1) | 0 | 2 (0) | 0 | 1 |
| | heading | 87 | 25 | 9,549 | 1 (1) | 0 | 2 (0) | 0 | 1 |
| | held600 | 93 | 29 | 4,954 | 0 | 0 | 0 | 0 | 0 |
| | near3km | 75 | 12 | 17,752 | 14 (14) | 1 (1) | 8 (4) | 0 | 4 |
| oov-near | today | 47 (47-48) | 0 | 25,220 | 1,295 (1,284) | 86 (86) | 271 (260) | 3 | 43 |
| | held | 100 (100-100) | 51 (51-52) | 0 | | | | | |
| | stale_course | 99 | 51 | 641 | 0 | 0 | 0 | 0 | 0 |
| | own_speed | 99 | 51 | 641 | 0 | 0 | 0 | 0 | 0 |
| | heading | 99 | 51 | 705 | 2 (2) | 0 | 1 (1) | 0 | 0 |
| | held600 | 92 | 43 | 3,790 | 145 (145) | 11 (11) | 35 (33) | 0 | 0 |
| | near3km | 84 | 36 | 7,494 | 376 (376) | 30 (30) | 133 (127) | 0 | 5 |

Blocked on tracks by time to effect, today / held / own_speed (perfect knowledge, someone truly
inside the keep-out, 1% at 0.03, 2 to 3% at 0.1, 6 to 7% at 0.3 per km^2):

| per km^2 | 30 s | 60 s | 120 s | 180 s | 300 s |
| --- | --- | --- | --- | --- | --- |
| oov 0.03 | 5 / 79 / 61 | 8 / 81 / 64 | 19 / 84 / 71 | 35 / 88 / 79 | 66 / 94 / 90 |
| oov 0.1 | 16 / 100 / 95 | 26 / 100 / 97 | 52 / 100 / 98 | 74 / 100 / 99 | 97 / 100 / 100 |
| oov 0.3 | 41 / 100 / 100 | 60 / 100 / 100 | 88 / 100 / 100 | 98 / 100 / 100 | 100 / 100 / 100 |
| oov-near 0.03 | 17 / 100 / 98 | 21 / 100 / 98 | 31 / 100 / 99 | 46 / 100 / 99 | 76 / 100 / 99 |

The HTML report has the same tables (`eval/keepout_report.py`).

**The acceptance burden over a run** (`python -m eval.lost_burden --seeds 1 2 3 4 5 6 7 8 9 10
--situations 20`, commit e4f58af; 9,000 nominations, 18,000 evaluations; the model is the module's
docstring: the same field and traffic, each contact going out of view at a rate per contact-hour,
lost from its drop on and never confirmed or accepted for good, no live traffic but the target).
The lost contacts in force at a nomination, and how many of them it needs accepted (mean over the
nominations they block; in brackets the largest), two hours in:

| losses per contact-hour | 0.03 per km^2 | 0.1 per km^2 | 0.3 per km^2 |
| --- | --- | --- | --- |
| 0.1 | 1.3 in force; 71% blocked; 1.8 to accept (5) | 4.1; 98%; 4.2 (10) | 12.0; 100%; 11.9 (22) |
| 0.3 | 3.4; 93%; 3.7 (10) | 11.8; 100%; 11.8 (23) | 35.6; 100%; 35.6 (54) |
| 1.2 (the oov variants' rate) | 14.6; 100%; 14.5 (32) | 46.3; 100%; 46.2 (81) | 140.2; 100%; 140.1 (200) |

and at 1.2 per contact-hour and 0.1 per km^2: 4.4 in force a quarter of an hour in, 11.1 at half
an hour, 23.2 at an hour, 35.9 at an hour and a half; every one blocks every nomination once its
reach (25 m/s from its last fix) passes the field's 8 km, about five minutes after it.

**What these numbers can support** (model results, unreviewed):

- Dropping a contact from the keep-out when its track ages out misses people: with a contact out
  of view within 1 km of the target, rules 14 passed 1,295 of 25,220 nominations (5.1%) with it
  truly inside the keep-out line at the effect time, 86 (0.3%) with it inside the effect circle;
  in traffic where 30% of contacts go out of view, 160 of 35,109 at the line by a dropped contact
  and 7 in the circle. Holding it as shipped took those to none by a dropped, a not yet dropped
  or a sped-up contact in all three variants (the one remaining miss at the line in `oov` is a
  live track in view, a prediction miss of the keep-out on live tracks). With 3,616 passes in
  `oov` and none of them missing a contact out of view, the rate is below 0.08% (95%, rule of
  three); in `oov-near` the shipped policy blocked every nomination, so there is nothing to
  measure a miss on.
- The review's two points, measured: counting a lost contact at its own speed bound
  (`own_speed`, the second version) missed one in `oov-all`, at the line, a dropped contact that
  sped up after its drop (1 of 7,563 passes); the top speed missed none. In `oov-near` that
  version's 641 passes were all with the near contact out of view but not yet dropped, judged by
  its course as a live track (none with it dropped: its disc blocked them all), so they say
  nothing about its disc; the first measurement's "0 of 647" was the same (not evidence that the
  own-speed disc held). Counting a live track out of view like a lost one (item 4) removed those
  641: blocks 99 to 100% in `oov-near`, and 94 to 95% in `oov`.
- The cost: blocks from about half of these nominations (`today`) to 95% in this traffic and
  100% with a contact out of view within 1 km; the top speed costs 4 to 6 points over the own
  speed (90 to 95% in `oov`; at 0.03 per km^2 and 30 s, 61 to 79%), the stale tracks 1 more. 30%
  of all nominations then pass only with an operator's acceptance of a lost contact (51% in
  `oov-near`); with every lost contact accepted 65% are still blocked (51% `today`), the 14 points
  between them mostly live tracks out of view, which no acceptance clears: only their being seen
  again, or their drop and then an acceptance. Over a run the lost contacts pile up: two hours in, from one or two in calm
  water to 140 at the oov variants' loss rate and 0.3 per km^2, and every nomination needs each
  of them accepted (the acceptance is per engagement, the reach has no cap): at that rate the
  acceptance is no longer a judgement an operator can make one contact at a time.
- None of the cheaper policies is sound against the truth: forgetting a lost contact 600 s after
  its drop let 145 nominations through with the near contact inside the line and 11 inside the
  effect circle; counting it only near the effect point, 376 and 30; clearing it where our sensors
  cover the water with no detection clears every one here (each was lost inside the radar's
  nominal 40 km), and so misses as rules 14 does. That last one is not an artefact: a track ages
  out within coverage only when the sensors stopped seeing a contact that was there, so their
  coverage is no evidence of its absence. What lowers the burden soundly is evidence of where the
  contact is: seen again (a track carrying its AIS, confirmed by an operator; one without AIS stays
  lost beside its new track), another sensor that sees it, or an operator who knows.

**What they cannot support:** any rate for a real place (the 30% share, the 30 to 900 s, the
quarter that speeds up and its 12 to 25 m/s on a uniform course, and the loss rates of the burden
are stated guesses; masking is all-or-nothing and permanent, one observer, open water); anything
about a contact faster than the top speed (none here goes over 25 m/s, so the shipped policy
cannot miss one by construction: the measurement shows what a policy below the generator's speeds
misses, not how often real contacts outrun 25 m/s); how often a sped-up contact heads for the
target (the course is uniform, so few do: the own-speed miss is one in thousands of passes, a
rate set by that assumption); how often operators would accept or confirm, or how carefully;
anything about the release's own check (a lost contact's region keeps growing to the release,
which these do not run); or the AIS claims (no contact here comes back into view; the claims are
tested in scenarios 88, 93 and 94 and the bridge's tests, not measured).

### The air tracker against truth (10-07, murmur-oho; model results, unreviewed)

Model results from the simulation, never field data. What changed (DECISIONS.md, "The air
tracker is calibrated against truth" and "An aircraft counts anywhere it could turn or speed up
to"): a 2D radar plots an aircraft at its slant range and the tracker reads the plot as the
bearing line, no farther out (before, the sensing model centred the fix on the aircraft's true
ground range, which needs its height); a track is ranged only while a position fix, or another
radar's bearing across this one by 30 degrees or more, came within 10 s, and is the bearing
segment again otherwise, with no velocity; the filter keeps the full covariance; the air
constants are calibrated (steady and manoeuvring 0.2 and 20 m^2/s^3, were 0.4 and 80; the 3
m/s velocity gate judged and kept); and rules 14 counts an aircraft anywhere it reaches from
its speed bound (its speed, two of its velocity's sigmas and a 7 s lag at 4.5 m/s^2), speeding
up at 4.5 m/s^2 to 100 m/s, every region of an aircraft 110 m wider for the lag. A review of
this change the same day found an unranged track sliding kilometres along a radar's bearing
and the first aircraft region missing 3 to 20 s ahead; these numbers are after its fixes.

**Calibration** (`tracks::tests::calib_air`; the held-out set: seeds 221-250, 13 motions, 30
seeds each, 300 s, scored from 30 s, run once with the constants frozen; before: verbatim
copies of the sensing model and tracker at efd332f on the same truth and draws, its keep-out
under rules 12; after: rules 14). The order of events, which moved, is in DECISIONS.md and the
test's module doc: three earlier sets were held out in turn and became tuning data (161-190 was
run on the code before the review, 191-220 failed its run). Per class and sensor mix, before to
after: the truth within two sigmas when updated (mean normalized error, 2 expected); the share
of samples with a velocity (its mean normalized error when given); a keep-out 3 / 10 / 30 / 60
/ 100 s ahead holding the aircraft, before and after; after, the tracker alone (rules 13's
regions, without the aircraft region) missing the aircraft 3 / 10 s ahead (its share of
samples, and by how far at most); the share of effect points within 5 km of the aircraft a
keep-out 60 s ahead blocks (a proxy of the nominations it denies):

| class | sensors | within 2 sigma (NEES) | velocity (its normalized error) | keep-out held, before | after | the tracker alone missed, after | blocked 60 s |
| --- | --- | --- | --- | --- | --- | --- | --- |
| straight | radar + EO/IR | 96.2 (1.17) to 97.9 (1.11) | 17% (0.22) to 38% (0.23) | 99.6 / 100 / 100 / 100 / 100 | 100 / 100 / 100 / 100 / 100 | 0.9% (20 m) / 0.0% (0 m) | 77 to 90 |
| straight | all sensors | 96.1 (1.19) to 97.7 (1.08) | 32% (0.24) to 41% (0.25) | 99.5 / 100 / 100 / 100 / 100 | 100 / 100 / 100 / 100 / 100 | 0.6% (130 m) / 0.0% (0 m) | 69 to 90 |
| one turn | radar + EO/IR | 95.1 (1.28) to 95.8 (1.19) | 44% (0.50) to 73% (0.71) | 98.9 / 98.4 / 95.7 / 92.2 / 89.8 | 100 / 100 / 100 / 100 / 100 | 1.9% (128 m) / 2.9% (367 m) | 76 to 93 |
| one turn | all sensors | 95.0 (1.27) to 95.2 (1.25) | 73% (0.46) to 82% (0.60) | 98.3 / 97.3 / 93.5 / 87.8 / 83.8 | 100 / 100 / 100 / 100 / 100 | 2.0% (78 m) / 2.8% (285 m) | 64 to 93 |
| speed change | radar + EO/IR | 94.6 (1.33) to 95.7 (1.20) | 9% (1.19) to 37% (1.66) | 99.2 / 98.7 / 98.1 / 98.9 / 100 | 100 / 100 / 100 / 100 / 100 | 2.1% (84 m) / 3.1% (175 m) | 85 to 90 |
| speed change | all sensors | 94.7 (1.31) to 96.3 (1.17) | 43% (0.91) to 48% (1.31) | 98.4 / 97.5 / 95.3 / 96.1 / 96.1 | 100 / 100 / 100 / 100 / 100 | 1.8% (73 m) / 3.0% (151 m) | 76 to 90 |
| hover and go | radar + EO/IR | 94.9 (1.29) to 95.6 (1.19) | 24% (1.31) to 58% (1.26) | 99 / 98.3 / 95.6 / 92.8 / 91.6 | 100 / 100 / 100 / 100 / 100 | 1.7% (88 m) / 2.8% (234 m) | 84 to 95 |
| hover and go | all sensors | 94.4 (1.34) to 94.7 (1.28) | 71% (0.78) to 79% (1.04) | 98.4 / 97.5 / 93.5 / 89.1 / 87 | 100 / 100 / 100 / 100 / 100 | 1.9% (65 m) / 2.7% (206 m) | 68 to 95 |
| approach and break | radar + EO/IR | 95.1 (1.28) to 96.0 (1.17) | 5% (5.25) to 31% (3.63) | 99.3 / 99 / 98 / 97.7 / 97.7 | 100 / 100 / 100 / 100 / 100 | 2.0% (236 m) / 3.1% (906 m) | 76 to 82 |
| approach and break | all sensors | 95.5 (1.26) to 95.8 (1.16) | 38% (1.19) to 52% (1.47) | 98.4 / 97.1 / 93.1 / 87.4 / 83.9 | 100 / 100 / 100 / 100 / 100 | 2.0% (196 m) / 3.3% (812 m) | 65 to 81 |
| far transit | radar + EO/IR | 96.2 (1.18) to 97.8 (1.29) | 0% (0.16) to 20% (0.22) | 100 / 100 / 100 / 100 / 100 | 100 / 100 / 100 / 100 / 100 | 0.6% (19 m) / 0.0% (0 m) | 78 to 86 |
| far transit | all sensors | 96.4 (1.18) to 98.2 (1.16) | 16% (0.23) to 34% (0.24) | 99.7 / 100 / 100 / 100 / 100 | 100 / 100 / 100 / 100 / 100 | 0.7% (22 m) / 0.0% (0 m) | 72 to 86 |
| route | radar + EO/IR | 94.0 (1.37) to 94.1 (1.26) | 0% (0.13) to 16% (1.42) | 100 / 100 / 100 / 99.9 / 99.9 | 100 / 100 / 100 / 100 / 100 | 0.4% (97 m) / 0.0% (48 m) | 89 to 90 |
| route | all sensors | 93.5 (1.42) to 93.7 (1.30) | 13% (1.45) to 34% (1.47) | 99.7 / 100 / 100 / 100 / 99.8 | 100 / 100 / 100 / 100 / 100 | 1.0% (116 m) / 0.0% (0 m) | 85 to 90 |
| loitering | radar + EO/IR | 87.5 (1.87) to 90.3 (1.64) | 0% (0.00) to 0% (0.00) | 100 / 100 / 100 / 100 / 100 | 100 / 100 / 100 / 100 / 100 | 0.0% (0 m) / 0.0% (0 m) | 99 to 99 |
| loitering | all sensors | 86.9 (1.93) to 88.1 (1.84) | 0% (9.16) to 1% (10.98) | 99.7 / 99.7 / 99.7 / 99.9 / 99.9 | 100 / 100 / 100 / 100 / 100 | 0.4% (85 m) / 0.4% (205 m) | 98 to 99 |
| random turns | radar + EO/IR | 91.1 (1.60) to 91.6 (1.50) | 3% (2.92) to 14% (4.72) | 99.8 / 99.8 / 99.4 / 99.1 / 99.1 | 100 / 100 / 100 / 100 / 100 | 0.9% (122 m) / 1.5% (360 m) | 96 to 98 |
| random turns | all sensors | 91.5 (1.59) to 92.3 (1.47) | 22% (1.85) to 27% (3.19) | 99.3 / 98.9 / 97.8 / 96.8 / 96.8 | 100 / 100 / 100 / 100 / 100 | 1.3% (109 m) / 2.0% (333 m) | 90 to 98 |
| weave | radar + EO/IR | 89.0 (1.75) to 89.4 (1.73) | 0% (0.00) to 0% (0.00) | 100 / 100 / 100 / 100 / 100 | 100 / 100 / 100 / 100 / 100 | 0.0% (0 m) / 0.0% (0 m) | 88 to 90 |
| weave | all sensors | 89.2 (1.76) to 90.3 (1.64) | 0% (24.99) to 0% (41.74) | 99.9 / 99.9 / 100 / 100 / 100 | 100 / 100 / 100 / 100 / 100 | 0.1% (67 m) / 0.1% (144 m) | 88 to 90 |

Two samples of some 300,000 are behind the "100"s: a keep-out 3 s ahead missed an aircraft
without a velocity once in "straight, all sensors" (by 20 m: 10 km out, a lone EO/IR fix
ranged it after a stretch on the bearing segment, and the next 10 s of two nearly parallel
radars' bearings drew a velocity of 140 m/s from it, the truth 75: 3.5 sigmas off) and once
in "route, all sensors" (by 6 m: a turn the tracker had not taken in when EO/IR lost it, 7
sigmas off). Every sample with a velocity held at every span. The test pins those
two as found. On the tuning sets (101-220, 48 motions) a keep-out 3 to 100 s ahead held every
sample. One USV's 2D radar alone: before, the fix sat on the aircraft's true ground range
(information the radar does not have) and the track looked like a surface one (1-sigma 31 to
60 m, 87 to 97% within two sigmas); after, the track is the bearing segment within the slant
range, 1-sigma 1.0 to 3.5 km, every sample within two sigmas (the truth at most 1.7 sigmas out,
NEES 1.31 to 1.97), no velocity either way, the keep-out holding it every time, blocking 94.5
to 100% of the effect points within 5 km (was 79 to 99). The calibrated tracker under rules 13 (its own regions,
`AIR_RULES=13` on the table, the tuning sets) held a first turn 60 s ahead 86 to 93% of the
time, a route 85 to 92%, a helicopter that hovers and goes 89 to 93%, a break away 83 to 86%,
and 3 s ahead 97.5 to 98.6% in those classes: the reason for rules 14. The first rules 14
region (the policy's 2 m/s^2 read as a bound, no lag) missed 3 to 20 s ahead (the review: a
helicopter leaving its hover 1.2 to 2.1% of samples, by up to 199 m), and the review's
unranged slide missed by up to 5.5 km at 3 s (seeds 131 and 153 of round one, now a regression
test). The surface calibration (`tracks::tests::calib`) passes unchanged: its held-out classes
86.7 to 97.5% within two sigmas (were 85.2 to 97.3), NEES 1.03 to 1.90 (1.05 to 2.02).

**The keep-out measurement's air variants** (`python -m eval.run keepout --variants air
air-straight air-all air-all-straight --seeds 1 2 3 4 5 6 7 8 9 10 --situations 100 --workers
6`, 4,000 situations each, 64,000 evaluations each under rules 6, 9, 12 and 14, 0 mismatches, 0
errors; before at efd332f, after at 27f93b5), mixed surface traffic plus one light aircraft
within 4 km, 120 sampled aircraft each:

| variant | aircraft with a velocity | its 1-sigma (median) | blocked at 30 / 60 / 120 s, 0.03 per km^2: before (rules 12) | after, rules 12 | after, rules 14 | missed at the keep-out line, before r12 / after r12 / after r14 |
| --- | --- | --- | --- | --- | --- | --- |
| radar, loitering | 0 to 0 | 46 m to 1.3 km | 67 / 100 / 100 | 100 / 100 / 100 | 100 / 100 / 100 | 0 of 1,042 / no pass / no pass |
| radar, straight | 1 to 0 | 31 m to 1.2 km | 73 / 100 / 100 | 100 / 100 / 100 | 100 / 100 / 100 | 0 of 909 / no pass / no pass |
| radar and EO/IR, loitering | 2 to 0 | 24 m to 25 m | 65 / 100 / 100 | 65 / 100 / 100 | 70 / 100 / 100 | 0 of 1,171 / 0 of 1,150 / 0 of 996 |
| radar and EO/IR, straight | 50 to 97 | 16 m to 17 m | 41 / 64 / 70 | 15 / 31 / 42 | 73 / 100 / 100 | 0 of 6,143 / 2 of 10,316 / 0 of 875 |

At 0.1 contacts per km^2 the last row reads 40 / 67 / 77 before, 19 / 38 / 62 under rules 12
after, 76 / 100 / 100 under rules 14. The regions of an aircraft at 50 m/s followed ten minutes
(15 m 1-sigma), at 30 / 60 / 120 s: along its course 385 / 1,080 / 3,045 m around the predicted
point (rules 12 and 14); rules 14's aircraft region 3,110 / 6,110 / 12,110 m around where it
is; without a velocity 3,140 / 6,140 / 12,140 m (rules 12: 3,030 / 6,030 / 12,030). The
straight aircraft's conservative blocks under rules 14 are the aircraft region's (deepest in
85%; without it 33% would have passed). The variants' straight aircraft never turns, so these
labels cannot show the turns the calibration does. The 2 misses under rules 12 with the
calibrated tracker were one situation's circling boat inside the keep-out line (not the
circle), which the aircraft's wider region had blocked by chance before and blocks under rules
14. Before the review's fixes, rules 13's region (no lag) blocked the last row 46 / 100 / 100.

**Live** (the engagement demo, stand-ins, `make goldengate GG_FLEET=engage-demo
CONTACTS=sim/scenarios/golden-gate-engage.toml` with `MURMUR_EVAL_TRUTH=1` on this agent's own
ports; `scripts/track_truth.py --settle 60 --seconds 600 --domain air`, one sample a second
against the truth: the sightseeing light aircraft, 55 m/s at 450 m, a 29 km loop with turns at
6 deg/s, seen by the Corsair-class and Magura-class radars, both 2D, and EO/IR at the edge of
its range): before (efd332f) a velocity in 88 of 600 samples (15%; 12% of the distinct truth
messages), NEES 1.18, 95.9% within two sigmas, 1-sigma 34 m (median); before the review's fixes
(83c0e3d) in 109 of 600 (18%), NEES 1.38, 92.9%, 1-sigma 50 m; after (27f93b5) in 85 of 600
(14%), NEES 1.33, 98.0%, 1-sigma 112 m. After, 272 of the 600 samples had the aircraft ranged
by the 2D radars alone (the bearing segment, 1-sigma 0.3 to 4.1 km, no velocity: the radars'
bearings cross it by less than 30 degrees, or not within 10 s of each other); of the other 328,
26% had a velocity (the turns, and EO/IR beyond 5 km detecting a 0.6-signature aircraft about
half the time), as before the fixes (26% of 412). The truth was at most 2.8 sigmas from the
estimate (3.3 before the fixes). The issue's live figures (24 to 25%, and 16% over 192 s in
murmur-533) came from other runs; how they were sampled is not recorded.

**What these numbers can support** (model results, unreviewed): one 2D radar does not locate an
aircraft whose height it does not measure beyond its bearing and the slant range, so a keep-out
cannot do better than count it anywhere on that segment and give it no velocity; with EO/IR the
calibrated tracker gives a straight-flying aircraft a velocity a third to 70% of the time,
honest; under rules 13 that velocity let a keep-out miss an aircraft that turns or speeds
up, a few percent of samples seconds ahead and up to a sixth a minute ahead, which rules 14
closes on every set but for two tracker tails in the held-out set's 300,000 samples 3 s ahead,
at the cost of an aircraft with a velocity blocking about as far as one without at every span.
**What they cannot support**: that velocity is the lever for aircraft (altitude is: an aircraft
above the keep-out's ceiling clears it, which needs a height-finding radar or a transponder),
any figure for a real radar or aircraft, that the region is a bound outside these motions (its
lag is measured on them, not derived; a dive is not modelled), or anything about a turning
aircraft's displayed velocity (it lags the turn; the constant-velocity models have no turn
model). The rules 9 identification measurement does not move: it plays its situations through
`murmur-scenarios`, whose acts are its own (truth plus seeded noise) and whose authors are
boats, so the bridge's placing of an act by a radar-only aircraft (`observe_act`, now the
segment's middle with its spread, as the tracker reads that fix) is not on its path.

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
