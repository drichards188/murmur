# Scoring v3: the specification (draft)

Status: **draft, 2026-10-23** (bd murmur-1z6.6, first part; docs/WORLD-V3-PLAN.md sections 6 and 7,
approved by the owner). Nothing here is frozen and nothing is pre-registered: this draft defines the
needs and labels across every domain, the first seven situation families and the anchors the
feasibility checks need, reports the first dev check (section 11), and lists the definitional choices
the owner has to make (section 9). The ladder and the perturbations are C2's; the validation, the
pre-registration and the blind check are D's. **Scoring v2 is frozen**: its labels (version 2), its
families, its oracle and every v1 and v2 golden and record are unchanged; v3 is built beside it
(`eval/headless/v3/`), importing v2's code where it is unchanged.

## 0. Coverage (the standing rule)

Every v3 report and results file opens with this statement, filled from its own batch.

| | what the dev check of section 11 covered |
|---|---|
| worlds | golden-gate-v3 and san-diego (sim/worlds/), each with its world's background traffic; the scale world is not used yet |
| vehicles by domain | Golden Gate v3: 20 (air 8, surface 7 counting the 3 drifting buoys, subsurface 2, ground 3 counting the antenna tracker); San Diego: 24 (air 10, surface 9, subsurface 2, ground 3) |
| sensors | the physical sensing model (docs/SENSING.md): EO/IR with pointing (gimbals, rings, fixed cameras), radar, AIS, ESM, active and passive sonar, terrain and horizon |
| contact domains | the traffic's surface, air and ground contacts, and the families' elements: surface (F1), air (F2, F3), subsurface (F4, F5), ground (F6) |
| conditions | drawn per situation from each world's ranges: start (so day, night or twilight), visibility, wind, sea state; F5 moves the start into the night with p 0.7, F2 (version 2 on) into the day |
| tasks | agents' orbits (the movers' tasks are orbit, patrol, hold and rtl; `track` is the antenna tracker's); collect, cover and engagements are defined (2.8-2.10), not exercised by these families |
| operator, clock | the approve-all operator, the paused clock, one replicate per situation |

**Shortcuts, named**: no engagement, review, space, link or fault families yet (F7-F17); the basic
oracle is a feasibility check, not a ceiling; the picture says which sensors agree with a track's
identity, not which reached identification (2.2, choice 5).

### 0.1 Seeds

| role | situation seeds |
|---|---|
| dev (build, the dev checks) | 5000-5499 |
| dev-validate | 5500-5999 |
| model pilot | 6600-6699 |
| test | from 7000, counts set by a power analysis on dev data |

The v2 ranges (1000-3099) are v2's and are never reused.

## 1. The mission

Every agent receives the mission of its world as its situation brief, verbatim (eval/headless/v3/
mission.py `mission_text`; its SHA-256 is in each situation's stated truth). For San Diego:

> Identify every contact of unknown, suspect or hostile affiliation that comes within 3 km of the bay
> entrance off Ballast Point, in time: one on the surface within 60 seconds of its coming within 3 km,
> one in the air within 30 seconds, one on the ground within 60 seconds, and one under the water
> within 300 seconds. A contact counts as identified when the picture shows its kind at a confidence of
> 0.6 or more without an identity conflict: from EO/IR, or its own true AIS, for a contact on the
> surface; from EO/IR for one in the air or on the ground; from sonar for one under the water, where
> its class (submerged, or a person in the water) is enough. Keep every vehicle above its reserve.

The Golden Gate's names the Golden Gate Bridge. Labels derive from it.

## 2. Instruments

Each label carries its instrument's name and version (`v3.*`, version 3; v2's where reused).

### 2.1 Contact needs, per domain

A contact whose truth affiliation is unknown, suspect or hostile is a **contact need** when it first
comes within the world's mission circle (`[mission]` of sim/worlds/<world>.toml: the protected point
and 3 km). `t_need` is the first truth sample inside; the deadline `D = t_need + grace(domain)`.
**Not evaluable**: D later than the episode's end minus 10 s. **Eligible**: evaluable and not a
sensing error.

| domain | grace ("in time") | why |
|---|---|---|
| surface | 60 s | v2's; a fast boat at 10 to 20 m/s covers 600 to 1,200 m of the 3 km, a fifth to two fifths |
| air | 30 s | a small drone at 15 to 30 m/s covers 450 to 900 m; a light aircraft at 45 to 60 m/s reaches the centre in about a minute, so v2's 60 s would accept an identification over the protected point |
| ground | 60 s | a car at 8 to 15 m/s covers 500 to 900 m, a walker 80 m |
| subsurface | 300 s | a passive sonar identifies after 180 s of contact and an active one recognises after 60 s (docs/SENSING.md), so no sonar answers in v2's 60 s; a UUV at 1 to 2.5 m/s covers 300 to 750 m in 300 s, a diver at 0.3 to 0.7 m/s 90 to 210 m: the same share of the radius as a fast boat's 60 s, or less |

The rule behind the column: a fifth of the radius at the domain's threat speed, floored at the
domain's sensor's own shortest classification time, rounded to a round number. The 60 s v2 used is
wrong for a submarine or a diver (no sonar classifies in it) and too lenient for an aircraft.

**Sensing error.** In the physical model a wrong call is drawn once per observer, sensor, contact and
level (identification at the scenario's misclassification rate, 0.05 in both worlds; recognition at
twice it; crates/murmur-bridge tracks/physical/mod.rs `label_at`, ported in labels.py `wrong_call`).
A need is a sensing error, excluded as v2 excludes it, only when every vehicle carrying the domain's
sensor has its draw set at the deciding level, so no look could resolve it: rare by construction. A
need **poisoned** by one wrong observer (its look at the deciding level, at 0.3 or more, puts the track
in conflict) stays eligible: whether that observer looked is the episode's, and the dev check names it
as the loss's cause (`misidentified:<platform>`).

### 2.2 Identity resolved

c is **resolved at t** when the track the tracker associates with c at t shows, in the picture sample
at t: its true kind (the identification level), or for a subsurface contact its true family
(`submerged` for a UUV or a submarine, `person` for a diver; the recognition level, choice 3);
confidence 0.6 or more; no identity conflict; and `supported_by` holding the domain's sensor:

| domain | supporting sensor |
|---|---|
| surface | `eo_ir`, or `ais` when the contact truly broadcasts AIS |
| air | `eo_ir` |
| ground | `eo_ir` |
| subsurface | `sonar` |

**Served**: resolved at some t ≤ D. **Pre-served**: resolved at `t_need`. **t_served**: the first
resolved sample at or after `t_need`; **t_identified**: when the stretch of resolved samples that
served it began (v2's).

**What the picture cannot say** (choice 5). `supported_by` names every sensor whose look agrees with
the shown identity at any level (a detection's size class, a recognition's family, an
identification's kind); the picture does not say which sensor reached identification. Air, ground and
subsurface needs are exact (only EO/IR identifies in the air and on the ground, only sonar under the
water). A surface contact can also be identified by a passive sonar (a buoy, after 180 s of contact;
it hears a fast boat tens of kilometres off), and an EO/IR camera that only detected it would then
make the need look served by EO/IR. F1 is drawn so that cannot happen by the deadline (its boat
reaches the circle at most 100 s after it appears; 100 + 60 < 180); a family that cannot be drawn so
needs the tracker to say each look's level (a small read-only addition to `Track::json` in
crates/murmur-bridge/src/tracks/, not made here: another agent is changing that code).

### 2.3 Contribution

v2's, with the need's own sensors: a dispatched action **contributed** to need c if its vehicle raised
its hits on the track associated with c, with a sensor that supports c's domain (2.2; AIS for a true
broadcaster on the surface), while that vehicle flew that order (its own run), before c was
identified (`t_identified`), and c is served. An order its vehicle never ran contributes nothing.

### 2.4 Fault needs

v2's, unchanged (eval/headless/v2/labels.py `fault_needs`, version 2): a low reserve opens at 35 %
of the true battery and is met by the vehicle on the ground at home before 20 %. No v3 family scripts
a fault yet (F11-F13).

### 2.5 Action labels: the moving kinds

Orbit, patrol, hold and rtl (the movers' tasks), and `track` (the antenna tracker's), by v2's
precedence and rules, which carry v2's lessons (SCORING-V2.md 0.5 item 2):

1. **refused** (counted apart);
2. **harmful**: (a) an orbit or patrol sending a vehicle under the reserve away from home; (b) a
   vehicle taken off an open, unserved eligible need its domain sensor was raising its hits on, the
   need then unserved by D, unless the order points the vehicle at it (**harm judged by geometry**:
   the contact's truth positions over the next 300 s come within 1 km of the order's geometry; a
   `track` order points at the contact its track names);
3. **useful** (the first order that contributed to an eligible need the paired `none` replicate did
   not serve; or the rtl that brought a low vehicle down at home), **follow-up** (later contributors),
   **duplicate** (it contributed only to needs the `none` replicate also served: neither credit nor
   cost);
4. **fault response**: a hold or rtl on a faulted vehicle that earned no label of effect;
5. **redundant**: its aimed need was already served or credited when it was created;
6. **useless**: anything else, reported twice (justified on the picture, about the world's centre,
   or not).

**Credit only effects**; **no credit after the need is met** (`t_identified`); the label of effect
outranks an open fault (kept as `also_fault`). Aim: by hits of any classifying sensor (EO/IR, sonar)
while the order ran, else by geometry within 1 km, ambiguous with a second within 1.5 times; a
`track` order without hits aims at its track's contact.

### 2.6 The picture sampler

v2's (`murmur-picture/1`), every 1 s, written under the record's `v3` key (`Options.scoring = "v3"`).

### 2.7 Blind labels

v2's design (2.7, 0.5's re-judge rules), run on v3 after its validation passes (lesson 5); the judge's
world description is rebuilt per world. Not built yet.

### 2.8 Nominations (engagement), judged against truth

An agent's `engage` intent, in this precedence (labels.py `nomination_label`):

| label | when |
|---|---|
| refused | rejected at submission |
| harmful | its track named a contact of neutral or friend truth affiliation at its creation, or an applied effect had one inside its radius (by the contacts' own record of the effect) |
| effective | an effect was applied and took a contact of hostile truth out of play |
| missed | an effect was applied on a hostile contact without taking it out |
| unwarranted | its target's truth affiliation is not hostile: unknown or suspect truth gives no ground to engage |
| unexecuted | a hostile target, but no effect was applied (declined, expired, aborted, superseded) |

Nominations are counted per situation-hour by label; `harmful` joins the hard limits (3.7). Built
and tested on fixtures; no v3 family nominates yet (F15).

### 2.9 Review decisions, judged against truth

Each decision of the reviewer (docs/HEADLESS.md 16.1; the record's `review.decisions`) is `right`,
`wrong`, `no_effect` (the bridge refused it) or `unjudged` (labels.py `review_labels`):

| item | right when |
|---|---|
| a rule declaration (confirm or revoke), a flag (confirm, declare or dismiss) | confirming or declaring a contact of hostile truth; revoking or dismissing one that is not |
| an AIS claim (confirm) | the claimant's contact is the lost track's own; `unjudged` while review.py's decisions do not name the claimant's track (choice 9) |
| a lost contact accepted for an engagement | by truth the contact had left the world, or is hostile |
| an un-spend | the platform's own effect did not take it out of the world |

Built and tested on fixtures; no v3 family raises review work yet (F15, F16).

### 2.10 Collect and cover (defined; built with F14)

- **collect** (a satellite collect over an area): **useful** when its product is ready before the
  need it was asked for (a contact need, or a stated gap need) is due, and nothing of ours would have
  covered that area by then (the paired `none` replicate's picture); **redundant** when the area was
  already covered then; **late** when the product came after; **useless** otherwise.
- **cover** (watching GNSS or SATCOM coverage over an area): **useful** when the area had a predicted
  outage in its window that the C2 then avoided (an order moved or held); **useless** when no outage
  was predicted.

Until F14 builds the space needs, collect and cover actions are labelled `unjudged`, counted apart and
left out of every rate of quality.

## 3. Metrics

v2's section 3 carries over, each from v3's labels (eval/headless/v3/metrics.py wraps v2's
`episode_metrics`): need served (3.1), action quality per situation-hour (3.3), cards and burden
(3.4), fault needs met (3.5), sensing (3.6), constraints (3.7), team and policy (3.8), cost (3.9).
Changes:

- **3.1b need served per domain**: served over eligible needs of each domain, beside the pooled share;
- **3.2 time to serve**: censored at each need's own grace (v2's 60 s for all); RMST per domain;
- **3.11 nominations** per situation-hour by label; **3.12 review decisions** by label;
- `unjudged` actions counted apart.

## 4. Conditions

v2's: the paused clock is the primary condition; the mission verbatim to every agent; no leakage (the
run id opaque, `ep-<12 hex>`, and the operator's notes naming no situation: `EpisodeSpec.opaque`);
the same world for every agent of a situation (its seed draws its contacts, sensing and conditions).

## 5. Anchors

| anchor | does |
|---|---|
| `none` | no agent |
| `random-valid` | v2's (agents_v2.py): every 30 s one orbit drawn uniformly over the idle vehicles and shown tracks whose preview passes |
| `oracle-v3` | the basic v3 oracle (eval/headless/v3/oracle.py): privileged (the contacts file, our vehicles' truth, the tracker's association, the sensing model's predictions); for each need, the two vehicles carrying the domain's sensor that can be in reach of the contact earliest (the model's identification range, recognition under water, for the platform at its working altitude under the drawn conditions; their transit at cruise), sent to orbit where they meet it, the centre kept only where the model sees the contact from it and, for a line-of-sight or mesh datalink, where the link map says the orbit and the transit keep the link; it follows a contact it met but has not identified |

The basic oracle leaves the high aircraft (MQ-9B, RQ-4) on their standing orders: an orbit takes them
to their catalog altitude (7,600 and 18,000 m) and their one camera is shared by every track within
tens of kilometres. It sends two vehicles per need (under water, one active sonar never reaches 0.6
alone; above it, see 11.3). The ladder of handicapped oracles and the perturbations are C2's.

## 6. Families

Each family on both worlds (`v3-<family>-<gg|sd>`), drawn with continuous draws from
`random.Random("v3:<family>:<world>:<seed>")` (eval/headless/v3/families.py, its docstring has every
range), on the world's watch situation as the base (its fleet on its opening orders, its traffic, its
grids, currents and element sets), the mission as the brief, 900 s (F4, F5: 1,200 s).

| | family | domain | what it adds |
|---|---|---|---|
| F1 | fast boat, no AIS | surface | a small fast boat, unknown, 10 to 18 m/s, from the sea (p 0.6) or the bay (0.4), holding 0.8 to 2.4 km out (p 0.7) or running through; it appears 15 to 100 s before the circle (2.2) |
| F2 | unknown drone inbound (version 3) | air | a small drone, unknown, 12 to 25 m/s, 60 to 150 m above the surface, from 3.8 to 5.5 km out on bearings over the strait or the bay to a target inside the circle, where it hovers; by day only |
| F3 | low aircraft, transponder off | air | a light aircraft, unknown, no emitter, 45 to 60 m/s, 150 to 400 m above the ground, a straight pass within 1.5 km of the centre |
| F4 | UUV in the channel (version 2) | subsurface | a UUV, unknown, 1 to 2.5 m/s, 5 to 15 m deep, down the bay's lane toward the channel, holding 0.5 to 2 km out |
| F5 | diver at a pier | subsurface | a diver, unknown, 0.3 to 0.6 m/s, 2 to 4 m deep, from 200 to 450 m off a pier where our boats lie to 20 to 40 m off it; night p 0.7 |
| F6 | vehicle at a restricted shore | ground | a car or truck, unknown or suspect, 6 to 12 m/s, along a shore road to a restricted point inside the circle, where it parks |
| F18 | quiet | | the traffic only: no need; the cost of acting |

Every route keeps to its medium on the world's own baked grids (tested: tests/test_v3_families.py).
**F4 version 2** brings every UUV from inside the bay: version 1 brought 60 % from the sea, past the
channel's buoys and HUNTER 1's patrol, whose looks recognised it before its need opened in 5 of 6
`none` pilot episodes (11.2). **F2 version 2** is by day only and **version 3** comes over water or
low ground only (11.4).

## 7. Episode classes

v2's section 7.

## 8. Statistics

v2's section 8, with lesson 4 written in: the per-family guard is judged with Holm's correction across
its cells, decided before any run.

## 9. Definitional choices for the owner

Each was made to build and run the dev check; each has an alternative.

1. **Per-domain grace** (2.1): surface 60 s, air 30 s, ground 60 s, subsurface 300 s. *Alternative*:
   one grace for every domain (v2's 60 s, which no sonar can meet), or a per-contact grace from its
   own speed (the time it takes to cover 600 m). **Recommend** the per-domain values: they are what
   agents can be told in one sentence and they follow one stated rule.
2. **Air needs open at the same 3 km circle.** *Alternative*: a larger circle for air (a light
   aircraft crosses 3 km in a minute). **Recommend** the same circle with the 30 s grace for now;
   revisit with F3's numbers.
3. **Subsurface at the recognition level** (2.2): the true family (`submerged`, a diver's `person`)
   from sonar is enough. In the physical model only a passive sonar identifies (after 180 s) and only
   the buoys, which take no orders, carry one; active sonars never identify and one active sonar's
   confidence never reaches 0.6 (it tends to it), so two looks must combine. *Alternative*: the owner's
   literal identification level, under which no vehicle an agent can task answers a subsurface need
   (F4 and F5 would test only where the buoys drift). **Recommend** recognition, and a model question
   for A1: whether Sea Hunter's hull sonar should also listen (its MS3 is active and passive).
4. **Surface support** (2.2): EO/IR, or AIS from a true broadcaster; a buoy's passive identification
   of a surface contact does not count. *Alternative*: any classifying sensor (then F1 is served by
   the buoys, which hear a fast boat tens of kilometres off, in nearly every episode). **Recommend** as
   specified.
5. **What the picture cannot say** (2.2): `supported_by` lists agreement at any level. Resolved by
   drawing F1 so a buoy cannot identify by the deadline. *Alternative*: expose each look's level in
   the picture (`Track::json`; a read-only tracker change, tracks/ being another agent's now).
   **Recommend** the tracker change after the parallel tracker work lands, then drop F1's constraint.
6. **A need poisoned by one wrong observer stays eligible** (2.1). *Alternative*: exclude any need a
   wrong-drawn observer looked at (but that depends on the episode, so needs would differ between
   agents of one situation). **Recommend** as specified.
7. **Nominations of a contact of unknown or suspect truth are `unwarranted`**, not harmful (2.8).
   *Alternative*: harmful (no contact the scenario did not make hostile should be engaged).
   **Recommend** unwarranted, with its own rate, and harmful kept for neutral or friend targets and
   collateral.
8. **The oracle leaves the MQ-9B and RQ-4 on station** and sends two vehicles per need (5).
   *Alternative*: let it re-task them (their camera is the longest-ranged), or send one. **Recommend**
   as built for feasibility; the ladder (C2) decides its own.
9. **An AIS claim's decision is `unjudged`** until review.py records the claimant's track (2.9).
   *Alternative*: change review.py's decision record now (it changes records with review on).
   **Recommend** the change with F16.
10. **F4 version 2 comes from inside the bay only** (6). *Alternative*: keep the sea approach and
    accept that standing sonar serves it. **Recommend** version 2.
11. **F5's night** (p 0.7 by the family) and **F2 by day and night** as the world draws them. 
    *Alternative*: F2 by day only (11.3: no camera identifies a small drone at night unless it passes
    within about 200 m). **Recommend** deciding with F9 (night and fog), which tests the night itself.

## 10. Gates (dev)

1. **v2's 10.1 world conditions, per family and world**: `none` serves at most 50 % and the oracle at
   least 80 % of eligible needs. Reported; a failure is examined, never tuned away on the seeds that
   showed it.
2. **Lesson 3, single cause**: per family and agent (`none`, the oracle), the share of lost needs the
   most frequent cause explains (eval/headless/v3/causes.py: `not_tracked`, `misidentified:<platform>`,
   `lost:<platform>`, `no_look@<light>`, `looked:<platform>@<light>(<level shown>)`), whole and by its
   platform or gap alone (`looked:corsair` whatever the light). Over 50 % of at least 3 losses, by
   either, flags the family for redesign before validation; `none`'s `no_look` is the room the family
   leaves an agent and is counted apart (choice 13).
3. **Two vehicle types per need**: per need, the platform types the sensing model says can answer it
   under its drawn conditions (the oracle's ranges, at least 150 m for a camera and 35 m for a sonar),
   among the vehicles that take orders.
4. Lessons written in (WORLD-V3-PLAN 7): coverage first (0); at least two dev batches audited before
   any rule is pre-registered, every judged cell leaning the wrong way in both examined; Holm across a
   family's guard cells; one validation, then the paid blind check.

## 11. The first dev check (2026-10-23)

**Coverage**: section 0's, on dev seeds 5000-5029 (F2's later versions on fresh dev seeds 5030-5059
and 5060-5089); every family on both worlds; agents `none`, `random-valid` and `oracle-v3`; one
replicate, the approve-all operator, the paused clock; conditions drawn per situation (by local hour,
fog under 2 km visibility: 176 day, 199 night, 31 twilight, 14 fog among the 420 situations of the
main batch). 1,260 episodes in the main batch and 180 for each later F2 version, no harness error;
with the 3-seed pilot (252 episodes), about 1,900 dev episodes in all, their records deleted after
this summary (the batch and report commands are in docs/HEADLESS.md 18). Need served is pooled over
the family's eligible needs (one per situation; F4 had 3 not evaluable, its UUV's deadline past the
episode's end). Model results only.

### 11.1 Need served, per family and world

| family | world | `none` | `random-valid` | oracle | 10.1 | types able (min) | top cause of the oracle's losses |
|---|---|---|---|---|---|---|---|
| F1 fast boat | Golden Gate | 18/30 (60 %) | 18/30 | 23/30 (77 %) | both fail | 7 | `looked:seahunter` 43 % of 7 |
| F1 fast boat | San Diego | 16/30 (53 %) | 15/30 | 19/30 (63 %) | both fail | 7 | `misidentified` 36 % of 11 |
| F2 drone v1 | Golden Gate | 1/30 (3 %) | 1/30 | 4/30 (13 %) | oracle fails | 2 | `no_look@night` 54 % of 26 |
| F2 drone v1 | San Diego | 0/30 | 0/30 | 5/30 (17 %) | oracle fails | 2 | `no_look@night` 36 % of 25 |
| F2 drone v2 (day) | Golden Gate | 1/30 | 0/30 | 4/30 (13 %) | oracle fails | 4 | `no_look@day` 38 % of 26 |
| F2 drone v2 (day) | San Diego | 0/30 | 0/30 | 11/30 (37 %) | oracle fails | 4 | `no_look@day` 42 % of 19 |
| F2 drone v3 | Golden Gate | 0/30 | 1/30 | 13/30 (43 %) | oracle fails | 3 | `no_look@day` 18 % of 17 |
| F2 drone v3 | San Diego | 0/30 | 0/30 | 19/30 (63 %) | oracle fails | 4 | `looked:puma` 55 % of 11 (coarse) |
| F3 low aircraft | Golden Gate | 12/30 (40 %) | 10/30 | 16/30 (53 %) | oracle fails | 6 | `looked:corsair` 71 % of 14 (coarse) |
| F3 low aircraft | San Diego | 16/30 (53 %) | 17/30 | 22/30 (73 %) | both fail | 6 | `looked:corsair` 62 % of 8 (coarse) |
| F4 UUV v2 | Golden Gate | 26/29 (90 %) | 26/29 | 26/29 (90 %) | `none` fails | 2 | `misidentified:buoy` 67 % of 3 |
| F4 UUV v2 | San Diego | 23/28 (82 %) | 23/28 | 23/28 (82 %) | `none` fails | 2 | `misidentified:buoy` 80 % of 5 |
| F5 diver | Golden Gate | 20/30 (67 %) | 20/30 | 22/30 (73 %) | both fail | 3 | `misidentified:seahunter` 38 % of 8 |
| F5 diver | San Diego | 30/30 | 30/30 | 30/30 | `none` fails | 3 | none lost |
| F6 shore vehicle | Golden Gate | 3/30 (10 %) | 5/30 | 13/30 (43 %) | oracle fails | 6 | `no_look@day` 24 % of 17 |
| F6 shore vehicle | San Diego | 15/30 (50 %) | 13/30 | 20/30 (67 %) | oracle fails | 6 | `looked:corsair@day` 20 % of 10 |
| F18 quiet | both | no need | | | | | |

"Types able" is the fewest platform types, among those that take orders, that the sensing model
says could answer a need of the family under its drawn conditions (10.3): every need had at least two.
**Time to serve**: most served needs were pre-served (identified before they entered the circle), so
the oracle's median is 0 s in F1, F2, F3, F4 and F6 San Diego; 12 s in F6 Golden Gate; 151 s and 62 s
in F5 (sonar's minutes).

**10.1 holds in no family** with needs. **Lesson 3** (10.2, by the whole key or the platform or gap
alone, `none`'s `no_look` apart) flags, as the report (`dev_v3.md`) lists them:

| family | flag | redesign |
|---|---|---|
| F1 | none | |
| F2 v1 | the oracle's `no_look` 88 % (Golden Gate, `@night` 54 %) and 64 % (San Diego) | version 2, by day; version 3, over water (11.4) |
| F2 v2 | none | |
| F2 v3 | San Diego: the oracle's `looked:puma` 55 % of 11 | none possible in the family (11.3) |
| F3 | `looked:corsair` 62 to 71 %, `none` and the oracle, both worlds | none possible in the family (11.3: the rings detect and recognise an aircraft they cannot identify at that range, and the gimbals that could are 11.3's) |
| F4 v2 | `misidentified:buoy`, 67 % of 3 and 80 % of 5 | version 2 (after the pilot); no further change in the family (11.2) |
| F5 | Golden Gate: `none`'s `looked:buoy` 100 % of 10 | none possible in the family (11.2) |
| F6 | Golden Gate: the oracle's `no_look` 53 %; San Diego: `looked:seahunter` 53 % (`none`), `looked:corsair` 60 % (the oracle) | none possible in the family (11.3; the shore roads' terrain) |

Where the cause is the sensing model's or the world's, a family drawn around it would be tuned to the
quirk (W3); those are reported for A1 and B2 (11.2, 11.3), not redesigned here.

### 11.2 Standing sensors serve the subsurface needs

`none` served 86 % of F4 (45 of 57 before the need opened) and 83 % of F5, and the oracle served what
`none` served in F4 and two more in F5: the three buoys and HUNTER 1's patrol recognise the UUV and
the diver before an agent can act, in both worlds and from either approach (F4 version 1 from the sea,
version 2 from the bay). The mechanism is the sonar's dwell rule (crates/murmur-bridge/src/tracks/
physical/mod.rs `sonar`): a run of detections survives 30 s gaps, and a look's confidence grows with
the run's length alone (passive 0.8 (1 - e^(-d/120)), active 0.6 (1 - e^(-d/60))), so a buoy that hears
a diver in 5 to 8 % of seconds 1.2 to 1.7 km off (the model's own per-second Pd at Horseshoe Bay)
gains confidence as fast as one that hears it every second. Until the confidence follows the share of
seconds heard (A1's model) or the worlds move their buoys (B2's), F4 and F5 leave an agent no room.

### 11.3 Cameras near a contact often do not identify it in time

The oracle's vehicles met their contact and the track still showed only its detection or recognition
at the deadline in most of its F2, F3 and F6 losses (`looked:<platform>(detected|recognised)`). The
payload manager (tracks/physical/mod.rs `eo` and `steer`) explains it: one draw per look decides
detection, recognition and identification for the whole look, so a look whose draw falls between the
identification and recognition probabilities recognises and never identifies until the contact
leaves the field; a lock lasts at most 10 s; and the engine's `looked` and `identified` maps are keyed
by contact alone, shared by every camera, so one camera's failed cue keeps every camera off that
contact for 60 s and one identification (even a wrong one) for 300 s. These limit any agent, the
oracle included, in the air and ground families. They are the cameras', not the tracker's: the
tracker associates by contact id, so the air tracker defect being fixed in parallel does not limit
these numbers; its camera fix may change them.

### 11.4 Drones (F2's three versions)

Version 1 (the world's day and night): no camera identified a drone at night (0 of 25 situations,
every agent; the Puma's LWIR identifies one at about 200 m, the FVR-90's at 390 m, the airship's at
580 m). Version 2 (day only): from over the Marin Headlands, Lands End or Point Loma's ridge the drone
flies 130 to 440 m above the sea, our small aircraft fly at 150 m and no order names an altitude: the
oracle could send nothing in 24 of those 28 situations. Version 3 (by day, from bearings over the
strait or the bay): the oracle served 53 %, `none` and `random-valid` 0 to 2 %. Its routes over water
or ground under 60 m: 29 of 44 served (Golden Gate 13 of 22, San Diego 16 of 22); those that still
cross a shore's high ground to reach a target behind it (the headlands' edge to the Horseshoe Bay
pier, the Presidio's to Fort Point; up to 270 m): 3 of 16. Its remaining losses over low ground are
11.3's. **Next for F2** (choice 12): draw its routes over ground under 60 m only (version 4), on the
next dev batch.

### 11.5 Other findings

- **Surface rings by day**: the Corsairs', Sea Hunters' and the Saildrone's fixed 360 degree cameras
  identify a fast boat within about 1.8 km by day (0.6 at night): F1's `none` served 22 of 28 by day
  and 6 of 25 at night. F1 fails 10.1 narrowly (`none` 57 % pooled).
- **Wrong calls poison needs**: 5 % per observer at identification and 10 % at recognition; a wrong
  look at 0.3 or more puts the track in conflict for good (F1 San Diego: 4 of the oracle's 11 losses).
  The port of the draw agrees with the simulator: in 60 `none` episodes every one of the 89 tracks
  shown with its contact's confusion had a wrong-drawn EO/IR or sonar observer among those that looked.
- **random-valid** serves what `none` serves (its few orbits rarely meet a need) and costs 4 to 6
  useless orders an episode; in F18 the oracle orders nothing.
- **The labels**: the oracle's orders are mostly `useless` or `duplicate` (it sends two vehicles and
  follows its contact; the standing sensors often identify first), `useful` where its look identified a
  contact `none` missed (none in F4, 19 in F2 v3 San Diego): the labels credit effects, as v2's do.

### 11.6 What next (for C2 and D)

The ladder and perturbations (C2) are worth building on F1, F2 v3, F3 and F6, where the oracle beats
`none` by 13 to 53 points; F4 and F5 wait for 11.2. 10.1 cannot hold anywhere until 11.3 is examined
with A1: the oracle's ceiling is the cameras'. Two dev batches are needed before any rule is
pre-registered (lesson 2); this is the first.
