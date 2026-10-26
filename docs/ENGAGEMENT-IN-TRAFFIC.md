# Engagements in traffic: who decides when the keep-out blocks

Status: **design note for the owner's decision, 2026-10-25** (bd murmur-e3b, part B). No code.
Part A (the tracker keeping a velocity for steady and stationary tracks) is approved and being
built separately.

## 1. What happens

Scoring v3's F15 (eval/SCORING-V3.md section 12; dev seeds 5400-5409, the physical sensing
model, the worlds' ordinary traffic) tried to nominate an engagement on a contact that had fired
on one of ours. The engine's keep-out refused nearly every attempt:

- **Golden Gate v3:** 1,186 of 1,191 attempts blocked. An aircraft was among the blockers in
  97 % (the only one in 19 %), a ground track with no velocity in 67 %, a lost contact and our
  own vehicles in 22 % each.
- **San Diego:** all 1,445 blocked by airliners on approach that had gone out of view (lost
  contacts, counted anywhere they could have flown, with no cap).
- With a munition loitering 700 to 1,000 m off the target (a span of about 57 s), 4 attempts
  passed; none was executed.

The rules are working as designed (fail closed: what might be there counts as there). The
result is that in a realistic harbour the machine answers "no" to every engagement, and no
person can make the call. For a testbed built to measure whether human judgment over
consequential decisions stays meaningful, that is a finding about the design, not only about
the numbers.

## 2. What the rules already let a person do

- **Lost contacts and lost friendlies** (rules 6 and 16, DECISIONS.md "A lost friendly blocks
  until an operator accepts it" and "A dropped protected contact stays in the keep-out until an
  operator accepts it"): an operator with engagement authority may accept one for one
  engagement, explicitly, logged with its facts and the world's time, with the `accept_unknown`
  step-up, shown to every signer, revocable, never an agent. **San Diego's blockers are all of
  this kind**: a person could clear them today. The headless runs did not exercise it, because
  the oracle read its options and never submitted a nomination for a person to accept (the
  scripted reviewer can accept: eval/headless/review.py `accept-all`).
- **Our own vehicles:** a person moves them clear.
- **Ground tracks with no velocity:** part A should remove most of these (all 27 in the dev
  check had had a velocity and lost it).

## 3. What has no path

**Live tracks whose reachable region covers the effect** within the engagement's span: above
all, aircraft. A light aircraft or a tour helicopter 15 to 30 km away counts as able to reach
the effect point by its domain's top speed. Nothing clears it but time or geometry (wait, or
shorten the span). The keep-out explanation (murmur-12e) already says what would clear it,
including "if it keeps behaving as now".

## 4. Options

1. **Keep it as it is.** The machine stays the only judge of these blockers; F15 measures that
   engagements in traffic are effectively impossible, and is reported, not scored. Cost: none.
   Consequence: the testbed cannot study human judgment over engagements in realistic traffic.
2. **Let a person accept a specific live blocker "as it is behaving now"** (recommended). It
   mirrors the lost-contact acceptance, with one difference that keeps the machine watching:
   - The operator with engagement authority accepts one live track, for one engagement, on the
     assumption the explanation already computes: that it keeps its current course and speed.
   - The engine then judges that track by its predicted path with its uncertainty, not by its
     domain's top speed, for that engagement only.
   - **The acceptance lapses on its own** the moment the track departs from the accepted
     behaviour beyond its uncertainty, or goes out of view: the engagement is blocked again (or
     stopped, if under way), and a person must decide anew.
   - Same safeguards as today's acceptances: logged with the track's facts, the step-up,
     shown to every signer on the card and the picture token, revocable by any operator, reset
     with the plan, never an agent's.

   Why this one: it does not weaken the default (every unaccepted track still counts at top
   speed); it turns the worst-case assumption into a stated, signed human judgment that the
   machine then monitors; and replay can check every part of it from the log.
3. **Tighter reach for cooperative aircraft** (a transponder's reported altitude and route, an
   airliner on a published approach). Sound in principle, but the simulator has no ADS-B and the
   C2 no flight-plan source; it would also be a claim about how far to trust a broadcast that
   can be spoofed. Later, if at all.
4. **Count reach only at the effect time, not across the whole span.** Unsound: a track that
   passes through the effect area before the effect could still be there at it.

## 5. What option 2 would take

- A decision-rules version (a named feature gate, as CLAUDE.md requires), with replay of the
  acceptance, its lapse and every check that reads it; engine scenarios for accept, lapse,
  revoke, reset and an agent refused.
- Console: an Accept on a live blocker in the nomination form and on the card, with the
  explanation's prediction shown; the step-up.
- Headless: the scripted reviewer's policy gains "accept a live blocker as behaving now";
  F15 then scores the human decision, including the restraint cases (accepting a blocker that
  then deviates is visible and judged).
- Measurement: F15 before and after, small runs; how often an acceptance lapses, and whether
  any lapse came too late (an accepted track inside the effect circle at the effect time).

## 6. Decision asked

Choose option 1 or 2 (or another). If option 2: the build starts after part A lands, as its own
task, with the engine scenarios and the replay checks written first.
