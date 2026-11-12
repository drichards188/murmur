# The console under saturation: what one operator should see

Research note for the console (`web/`) under the Saturation situation
(`sim/situations/swarm-saturation.toml`), 2026-11-12. The owner ran it live and found the console
"an unintelligible mess of text and tags". This note names what went wrong in the owner's two
screenshots (T+3 and T+14), what the evidence says about showing many vehicles and contacts to one
person, and what to change, ranked by expected effect against cost.

Display only: nothing here changes a decision of the engine, the audit log or replay. Where a
recommendation would need command design (how groups are ordered, group behaviours, declaring or
engaging many contacts at once), it is a question for the owner in section 5, not a design.
Saturation stays a performance test; this note only asks how the console should present it.

Conventions: [n] is a source in section 6, where each carries its strength (controlled experiment,
review, standard, practitioner guidance, open source, vendor material). "Inference" marks my
reasoning; [E] marks an estimate of mine with its basis. The fleet's `[[swarm]]` tables are called
groups here: independent vehicles managed together, not coupled swarms. The console's own names
(the Swarms panel, `swarms.ts`) are kept where a file is meant.

## Bottom line

- Most of the mess is three layers drawn once per track with no budget: a label pair, a filled
  uncertainty ring and, for an aircraft without a height, a 15-dash column. Five hundred tracks
  multiply each into a flood. Lowlighting them (outline, short mark, no text) except for the few
  that matter is cheap and supported by the strongest evidence here [4][5].
- Our side floods the same way through per-vehicle estimates, lost badges and list rows. The
  console already has the groups; the lists, the command log and the estimates should be drawn
  per group, with the exceptions pulled out.
- Nobody groups the 500 hostile and unknown drones. A display-side "raid" (tracks close in
  position, velocity and kind, drawn as one object with a count) follows long practice [32][23]
  and the tracking literature [15][16][17], but it can bury a distinct contact. Saturation's own
  shooter flew in with the recon group, so exceptions must break out by rules the grouping cannot
  override.
- Whatever is lowlit, unlabelled or aggregated must be counted on screen and one action away [8].
- The decision that matters most in the scene, confirming the engine's rule declaration, has the
  weakest standing cue: its strip never appears in Saturation (section 1, P14).
- Part of the clutter is the scene's packing (83 km² for 3,602 vehicles). At realistic spacing
  overlap between groups shrinks, but list lengths, the inbox, the label budget per screen and the
  whole-area view do not, and more of what matters is off screen.

## 1. What the screenshots show

Both screenshots are in the pitched 3D view (the top view is one key away, T).

| # | What is visible | Drawn by | Why it grows with the scene |
|---|---|---|---|
| P1 | Grey smears of overlapping text: "T-0347 · alt ?" right of every track, "SM AIR (DET)" left of it | `trackLayers.ts` `tracks-ids`, `tracks-kinds` | Every track is labelled at every zoom. Only our vehicles have a declutter (`declutter.ts`: the first of two overlapping labels wins, by selection then uid, none below zoom 9); tracks, estimates, group names and order labels have none. About 500 tracks give about 1,000 strings. |
| P2 | Yellow ellipses over much of the map | `trackLayers.ts` `tracks-uncertainty` | Each track's 1-sigma ring, growing with age, filled at 8% opacity. Fills add up: [E] ten overlapping rings cover the ground at about 57% (1 - 0.92^10), thirty at about 92%. The pitched camera turns the circles into ellipses. Yellow is the unknown colour, so the map is painted "unknown". |
| P3 | Columns of dashed vertical lines (slanted near the edges), thick yellow pillars where a group hovers | `trackLayers.ts` `tracks-columns` | An aircraft with no measured height is a column of 15 dashes from the sea to 3,000 m, doubled by the height exaggeration, so it never reads as a boat (DECISIONS.md, 10-07). Right for one track; 400 unranged drones make a fence. |
| P4 | Boxed labels "EST dead reckoned if it kept its task · silent 4 m 39 s", "EST RTL on lost link · 6 m 26 s · silent 6 m 29 s", each with its region and leader line | `estimateLayers.ts` | One estimate per silent vehicle, two once its lost-link timer fires; the jamming silences 300 vehicles at once. |
| P5 | Red "LOST 98s", "LOST 222s" under symbols | `vehicleLayers.ts` `vehicle-lost` | One per stale vehicle drawn in full. |
| P6 | Group names run together ("SEARCH-8 SEARCH-7 ... SEARCH-1"), tinted outlines overlap, an outline stretched across water | `swarms.ts` `swarm-hulls`, `swarm-labels`; bridge `swarms.rs` `summary` | Labels sit at each group's mean position and are not decluttered. The outline is the convex hull of every member's position, those gone home included, and the mean can fall where no member is. |
| P7 | Number bubbles (482, 836, 1780) with domain lines and "268 unk"; individual symbols drawn over them | `clusterLayers.ts`; bridge `push.rs` (more than 800 vehicles in view) | Counts per map tile, mixing groups and domains; the red line counts every unknown track in the tile. Pinned vehicles (selected, named by a pending or queued order, or by an alert: `App.tsx` `pins`) still come in full, so selecting a group, or an order to one waiting in the Inbox, puts 100 full symbols over the bubbles. Tracks are never aggregated. |
| P8 | "ORDERED i16 orbit · not yet confirmed (sent, star..." repeated | `orderLayers.ts` | One per order not yet confirmed by telemetry: the opening orders at T+3. |
| P9 | A fleet list of identical five-line rows | `FleetPanel.tsx` | 3,602 rows of 84 px under 37 headers, about 300,000 px of scroll. Headers do not collapse; rows keep fleet order, not exceptions first. |
| P10 | A track list of about 500 "small drone (identified) · EO/IR, UNKNOWN, live" rows | `TracksPanel.tsx` | Sorted hostile, suspect, unknown, then freshest. The hostile one is first (good); the other 491 are indistinguishable, and the help paragraph takes the top of the list every time. |
| P11 | A command log listing every member of every group order | `TaskPanel.tsx` | One row per vehicle per order with its lifecycle strip. An orbit to 100 members is 100 rows; the 36 opening orders are about 3,600. Failures and supersessions sit among them. |
| P12 | Situation panel and weather legend over the left fifth of the map | `SituationPanel.tsx`, `WeatherLegend` | Fixed floating panels: not a scale problem, but they take room the map needs. |
| P13 | Inbox 13 at T+3; 56 (with 2 and 1) at T+14 | `InboxPanel.tsx` | It already folds a group's member alerts into one card and lets the person acknowledge or snooze. Still, [E] 56 open items at T+14 is about 40 per 10 minutes if they arrived over the run, four times the flood line process control uses [19]. |
| P14 | The decision that matters has the weakest cue | `WaitingStrip.tsx`, bridge `decision/blocking.rs`, `hostileAct.ts` | At T+14, T-0009 is HOSTILE BY RULE, unconfirmed, first in the track list and a red frame on the map. The act banner lasts 45 s (`BANNER_MS`). The WAITING ON YOU strip shows only a review item that alone holds up a nomination; in Saturation the reserve's loadout refuses an air track (sim/situations/README.md), so the item is never alone and the strip never shows. The standing cue is a small count on the Inbox button beside dozens of others. |

What already works and should stay: the Inbox's group folding, acknowledge and snooze; the track
list's hostile-first order; the vehicle declutter; the bridge's level of detail by view; one click
to select a group's members.

### 1.1 How much of it is the scene's packing

At the end of the live run the 3,602 vehicles sat in about 83 km² (a 4 by 29 km strip), median
nearest-neighbour spacing 14 m, many of them back home. A saturation of that size would spread
over roughly 10,000 to 60,000 km² (the coast from Bodega Bay to Monterey Bay, out past the
Farallones). [E] Evenly spread, 83 km² gives each vehicle 0.023 km² (about 150 m apart);
10,000 km² about 1.7 km apart; 60,000 km² about 4.1 km. The vehicles work in groups, though: a
picket ring is as tight as its orbit wherever it is, so spreading the scene moves groups apart
without thinning them.

[E] On the screenshots' map area (about 1,840 by 1,280 px), a 100 km square view is about 80 m per
pixel, so a 24 px symbol covers about 1.9 km; a 245 km square (60,000 km²) is about 190 m per pixel
and 4.6 km. At either size, a whole-area view draws symbols that touch or overlap. And
`declutter.ts`'s own label box (140 by 44 px) fits at most about 380 labels on that map area with
perfect packing, whatever the density.

| At realistic spacing | Problems |
|---|---|
| Shrink | Overlap between different groups' outlines and names (P6); tile bubbles mixing groups (P7), once tiles are smaller than the gaps between groups; rings, columns and estimates of different groups or raids overlapping one another (P2 to P4) |
| Do not shrink | List lengths and the command log (P9 to P11); the Inbox count (P13: link losses may grow with range); the label budget per screen; thousands of symbols in a whole-area view; overlap inside one group or one raid at the zoom where it is seen; the weak cue for the declaration (P14) |
| Grow | What is off screen: zoomed in on one group, the act, a raid or a jammed sector is tens of kilometres away |

So every recommendation below has to hold at both densities. Aggregation by group and by raid
matters more at realistic spacing, not less, because seeing a group's members means zooming in.

## 2. Principles from the evidence

**E1. Capacity is limited by attention, not by vehicle count.** A capacity model checked against a
multi-UAV simulation found that the waits caused by human interaction cut predicted capacity by up
to 67%, loss of situation awareness the main cause; with management-by-exception automation the
loss was still 36% [1]. In DARPA's OFFSET field experiment one operator commanded 174 platforms (84
air, 90 ground) by drawing tactics on the map rather than robot by robot, and the team still gave
health monitoring to a separate "swarm health engineer", one of five roles each sized to "the
amount of responsibility we found one person could handle" [30]. Inference: the display must bring
exceptions to the person; scanning 3,602 symbols is not a plan.

**E2. Aggregate displays can beat per-agent displays, at a price.** In a between-subjects
experiment (56 participants, four collectives of 200 simulated UAVs), an abstract collective
display gave better situation-awareness probe accuracy than one showing every agent (89.9% against
65.3%) and lower reported workload, with no difference in decision accuracy; its operators
abandoned the highest-value target more often (43.6% against 32.4%, not significant) [2]. Caveat:
those collectives decided together; Murmur's groups are independent vehicles, so this supports
showing groups as groups, not any particular aggregate. Survey of the field: [3].

**E3. Lowlight rather than remove, automatically rather than by hand.** Twenty-seven Navy
air-defence operators, given automation that dimmed the symbols of less threatening aircraft,
responded to threats about 25% faster (most for threats in the periphery), with little change in
which aircraft they judged threatening, and spot-checked the automation; 25 of 27 preferred it
[4]. On electronic maps, colour and intensity coding helped by separating the visual field, while
interactive decluttering cost more retrieval time than it saved [5].

**E4. Clutter can be measured** by image measures that track visual search performance (feature
congestion, subband entropy; public code) [7], and by performance, ratings and eye tracking,
best combined [6].

**E5. Aggregates must look like aggregates and say what they hold.** Keep an entity budget; an
aggregate summarises its contents, stays simple, is distinguishable from an item, may lie (averages
do), and should show "that there is more to see by expanding" [8]. "Overview first, zoom and
filter, then details on demand" [9].

**E6. Labels in practice: two levels and a priority.** Radar labels have a simple and an extended
form, the extended one on hover, a leader line to the track, automatic anti-overlap and manual
repositioning [20][21]. An open-source reimplementation of the FAA's terminal display gives a
track with no flight plan a limited datablock; a full one to a track under the user's control,
being handed to them, pointed out to them, with an active warning, or asked for; a partial one to
every other [27]. The FAA's schema ties the full block to "the track owner's display" [22]. ATAK's
open-source label manager draws labels in three priority passes, shifts or drops overlapping ones,
and skips those below a resolution threshold [25]. Inference: the label level should follow the
person's responsibility and the item's state.

**E7. Uncertainty regions are read as size.** 70% of viewers of a hurricane forecast cone said it
showed the storm growing (39% with an ensemble display) [10]; submarine officers did better with a
graphic display of uncertainty only under some conditions [11]. Inference: 500 filled yellow rings
read as "a large unknown threat here", which no one of them means.

**E8. Alarm floods have numbers.** ISA-18.2: about one alarm per 10 minutes per operator position
very likely acceptable, two the most manageable; a flood is more than 10 in 10 minutes, under 1% of
the time; priorities about 80% low, 15% medium, 5% high; shelved alarms come back after a set time
and must be listed; suppression by design differs from the operator's shelving [19]. Process
control figures: an analogy for C2, not evidence about it.

**E9. Off-screen things can be pointed at:** rings reaching into the screen edge [12], or wedges,
which were more accurate and do not overlap [13].

**E10. A grouping aid invites bias.** Imperfect aids produce omission and commission errors, and
complacency persists in experts [14]. Whatever a grouping or ranking hides needs a path back that
does not depend on the grouping being right.

**How others show a group as one object.**
- Historical: RAF plotting tables in 1940 carried one block per raid, with a raid number and, as
  the museum's image description reads them, a strength ("H01", "40+") [32].
- Air traffic control: a formation flight is controlled as a single aircraft; in a standard
  formation only the lead squawks [23].
- Tracking: group and extended-object tracking estimate a group as a whole (composition, centroid
  motion, extent), with criteria for groups that split and merge and density-based clustering to
  partition the measurements [15][16][17] (the UAV-swarm paper is a perspective with untested
  proposals). ST-DBSCAN, clustering on a spatial and a non-spatial distance together, is the
  simple version [18].
- Symbology: MIL-STD-2525's quantity amplifier (C) gives "the number of items" for equipment;
  echelon is for units [24]; I found no public swarm or raid symbol. milsymbol, Murmur's library,
  supports `quantity` and `echelon` [29].
- Open source: ATAK's Overlay Manager shows and hides categories with an eye icon that has a third
  state, "some, but not all, objects visible", and a Show All beyond the current view [26].
- Closed, publicly described: OFFSET colours agent icons by state, has a health panel, and shows
  the explored area as one coverage layer fading to magenta over twenty minutes [30]. L3Harris
  says AMORPHOUS will let "a very small number of operators" coordinate thousands, one command per
  swarm [31]: a vendor claim about command design, cited only as the field's direction.

**Gaps in the evidence.** No controlled study of declutter at thousands of tracks in a C2 display;
no public description of how a fielded counter-drone or C2 system draws a drone swarm as one track;
no public raid or swarm symbol; nothing on showing dead-reckoned estimates for many silent vehicles
at once. ISA-18.2's figures come through a vendor's summary (the standard is paywalled); the FAA's
datablock rules through an open-source reimplementation and the FAA's schema (the FAA site refused
automated reads).

## 3. Recommendations

### 3.1 Ranked

By expected effect on the operator's effectiveness against cost. Effort: S a day or two, M about a
week, L more. Details, components and what each risks hiding are in 3.3.

| Rank | Recommendation | Effort | Fixes |
|---|---|---|---|
| R1 | Lowlight, don't paint: rings, columns and estimates in full only for what matters | S | P2, P3, P4 |
| R2 | One label budget by priority across every map layer | M | P1, P4 to P6, P8 |
| R3 | Say what is hidden: counts of what is lowlit, unlabelled or aggregated, and Show all | S | guards all |
| R4 | A calm, standing pointer to every pending human step in the engagement chain | S | P14 |
| R5 | Our groups drawn as they are: core outline, detached parts, one estimate per part | M | P4 to P6 |
| R6 | Lists by exception: command log per order, fleet list by group, track list exceptions first | M | P9 to P11 |
| R7 | Raids: hostile and unknown tracks grouped on the display, exceptions break out | L | P1 to P3, P10 |
| R8 | Group-aware aggregates at low zoom instead of tile bubbles | M | P7 |
| R9 | Edge markers for exceptions off screen | M | density (1.1) |
| R10 | Inbox flood summary: arrival rate and causes on top | S | P13 |
| R11 | Panels that step aside; crowded situations open in the top view | S | P12, P2, P3 |

R1, R2, R5, R6 and R7 also lighten the page (fewer glyphs, filled polygons and DOM rows).

### 3.2 Grouping, the hard part

**(a) Our vehicles.** Each belongs to an assigned group (`[[swarm]]`), but members drift, fail, lose
their link and fly home, so a group splits in practice. Today the bridge's summary draws one
convex hull over every member and labels it at the mean (`swarms.rs` `summary`); thirty members
gone home stretch the outline over the bay and the name lands where nobody is, the "abstractions
may lie" case [8]. The console alone can do better (R5): partition each group by the state it
already holds (on the group's task or not, from the summary's `tasks`; heard or lost; damaged),
then split each state spatially (members within a set distance of one another form a part), draw
the main part's hull with the name at its medoid, and draw detached parts small, joined to it,
labelled with what they are ("PICKET-1 · 30 lost link"). The positions are in the console's feed
already (coarse for vehicles out of view, enough for a part).

What it cannot do: change membership. A detached part becoming its own group, two groups merging,
an order to "the rest of PICKET-1" are command design (question 3). Today the person corrects a
wrong picture by expanding a part and selecting its members, which the order form already takes.

**(b) Hostile and unknown tracks.** Nobody groups them. Most of the 491 unknowns at T+14 belong to
five hostile groups (the director's notes), which the person can only infer. The console alone can
cluster live tracks each second (R7): by affiliation and domain first, then kind, then position
and velocity together (a density-based neighbourhood as in ST-DBSCAN [18]), with a minimum size so
a handful of tracks never forms a raid. It draws each cluster as one object: an outline of member
positions (not of their rings), a count, a mean velocity arrow, the mix of kinds and identity
levels, the oldest fix. Names (R-1, R-2) stay stable by matching clusters to the last second's by
shared members. This is the display's grouping only: never logged, never read by the engine, not
visible to agents.

What would need the tracker or the engine (questions 1 and 2): group tracks in the tracker (a raid
count when drones are not resolved one by one: a display can only count the tracks the tracker
holds, not the drones); a raid that can be declared, accepted or engaged as one; a
keep-out that counts a raid as one region; raids in MCP so the AI watch officer and the evaluation
see the same grouping as the person.

**How the person sees and corrects a wrong grouping.** Every aggregate is marked as one (a count,
a distinct outline, E5) and expands in place. Exceptions break out by fixed rules that do not
depend on the clusterer: hostile or suspect; any declaration or review flag; attributed an act;
linked to a weapon track; selected or picked; a kind different from the raid's; a velocity that
leaves the raid's. The person can ungroup a raid, pull a track out, or merge two, as view settings
kept in this browser like acknowledge and snooze. The test case is in the scene: Saturation's
armed drone flew in with the recon group and hovered west of the bridge while the group moved on
(the director's notes). A grouping on position alone could bury it; its velocity splits it out,
and so, by T+14, does its kind ("UAS, no transponder" where the others read "small drone"). Once
it fired it is an exception whatever the clusterer says.

### 3.3 The recommendations in detail

**R1. Lowlight, don't paint (S).** Fill a track's uncertainty ring only for the focused set
(selected or hovered; hostile or suspect; declared or flagged; the track of an engagement in play),
outline the rest, and give a raid member none (the raid's outline instead). Draw an unranged
aircraft with a short "?" stem, the full 3,000 m column only when focused. Draw unknowns that are
not exceptions at lower intensity, as stale ones already are. For a silent vehicle draw only the
leading hypothesis unless it is focused. *Why:* E3, E7, the fill arithmetic in P2. *Where:*
`trackLayers.ts` (`tracks-uncertainty`, `columnDashes`), `estimateLayers.ts`. *Risks hiding:* an
unknown's uncertainty and missing height at a glance. What a nomination rests on stays where it is
decided: the keep-out explanation lists each blocking track with its region and shortfall
(`KeepOutExplain.tsx`); hover brings any ring back.

**R2. One label budget by priority (M).** One screen-space declutter over every map text layer
(callsigns, track ids and kinds, estimates, group names, order labels, lost badges), filled in
order: (1) the selected and focused, items waiting on the person, hostile, suspect, declared,
flagged, act- or weapon-linked tracks; (2) group and raid names; (3) our vehicles with an alert;
(4) the rest. A label that does not fit shows on hover. The default is a limited block ("T-0347",
a callsign); the full block (kind and level, height, sources, silent time) is for hover, selection
and priority 1. Each priority gets a zoom floor. *Why:* E5, E6; about 380 label boxes fit the map
area (1.1). *Where:* `declutter.ts` (from vehicles to all layers), the layers with text,
`MapView.tsx`. deck.gl's CollisionFilterExtension does priority collision on the GPU [28], but it
is a new package, approximate by its own docs and silent on pitched views; extending `declutter.ts`
keeps one rule in one place. *Risks hiding:* a low-priority object's label; R3 counts them.

**R3. Say what is hidden (S).** A one-line map legend, for example "Labels 61 of 4,107 · rings:
focus only · 491 tracks in 5 raids · 300 estimates in 3 groups · Show all", each item opening its
category, with ATAK's three states (all, some, none visible) [26]. Show all is a check, not the way
of working (E3). *Why:* the constraint itself; E5; shelved alarms must be listed [19]. *Where:*
beside `WeatherLegend` in `App.tsx`, counts from the layer builders. *Risks hiding:* none.

**R4. A calm, standing pointer to the person's pending steps (S).** Each rule declaration awaiting
confirmation and each flag on an act against ours gets a standing line under the top bar (for
example "D-3 · T-0009 declared hostile by rule 2 m ago · waiting for your review · Open"), styled
as pending, not urgent, with no confirm button. The act banner hands over to it when it fades;
WAITING ON YOU keeps its meaning. *Why:* P14; E8 (the high priority must be rare); PRODUCT.md, "Urgency is shown only
where something is urgent"; DECISIONS.md, confirming is never the easy act, so the line points and
the card decides. *Where:* `WaitingStrip.tsx`, `blocking.ts` (the console has `inbox.review`).
*Risks:* nothing hidden; pressure toward confirming (question 4).

**R5. Our groups drawn as they are (M).** Section 3.2 (a), plus silent members' estimates as one
region per part and hypothesis ("PICKET-1 · 100 silent · task model", "... · failsafe RTL"), one
label each; individual estimates only for a focused vehicle or one with its own alert or order.
*Why:* E2, E5 (fidelity), OFFSET's single fading coverage layer [30]. *Where:* `swarmsModel.ts`
`swarmOutlines`, `swarms.ts`, `estimateLayers.ts`; optionally a core hull in the bridge's
`swarms.rs` (display only). *Risks hiding:* one member's own estimate and timer, one click away.

**R6. Lists by exception (M; the command log S).** The command log: one card per order with its
members' states counted (say "100: 82 active · 12 acked · 4 sent · 2 failed"), only exceptions
listed (failed, superseded, unconfirmed, requeued, stuck past a set time), "show all 100" for the
rest.
The fleet list: each group collapsed to a header with the Swarms panel's counts, exceptions first
inside an open group, one-line rows past a size. The track list: exceptions first (declared,
flagged, suspect, act-linked, selected), then raids as collapsible rows (count, kind mix, where,
heading, nearest of ours), then the rest; the help paragraph behind a "?". *Why:* E1, E5, [9].
*Where:* `TaskPanel.tsx`, `FleetPanel.tsx` (`layout`, `ROW_H`), `TracksPanel.tsx`, `tracks.ts`.
*Risks hiding:* one vehicle's state in a collapsed group (the header counts it in the alert colour;
the Inbox keeps the alert). Any ranking says why an item is where it is and never changes an
affiliation (E10).

**R7. Raids (L; a first cut M).** Section 3.2 (b). Members drawn as dim dots without label, ring or
column; one raid symbol per cluster: the affiliation's frame with a count and a distinct outline so
it cannot be read as one contact (milsymbol's `quantity` can carry the count [29], though 2525
reserves it for equipment [24]). *Why:* E2, E3, the practice in section 2. *Where:* a new pure
`raids.ts` with a `raids.check.ts`, `trackLayers.ts`, `TracksPanel.tsx`. *Risks hiding:* a
distinct contact in a raid (the shooter case); a count of tracks, not drones; an outline implying a
shape. Mitigated by the break-out rules, R3 and in-place expansion; checked by section 4.

**R8. Group-aware aggregates at low zoom (M).** Zoomed out, one marker per group part and per raid
with counts by state, instead of tile bubbles; tiles only for vehicles in no group. A pinned group
(selected whole, or named by a pending group order) is drawn as its part unless one member is
focused. *Why:* a tile aggregates by a dimension the person does not think in (E5). *Where:*
`clusterLayers.ts`, `MapView.tsx`, `App.tsx` `pins`; the bridge's cluster cells could carry a group
id (`push.rs`, display only). *Risks hiding:* per-tile counts of unknowns, which raids carry.

**R9. Edge markers for exceptions off screen (M).** A wedge at the map's edge for each exception
out of view (hostile or suspect track, act, weapon, the track of a pending step, a raid closing on
ours); a click flies there. *Why:* E9; at realistic spacing the person works zoomed in and the act
happens elsewhere. *Where:* `MapView.tsx`. *Risks:* edge clutter, so exceptions only.

**R10. Inbox flood summary (S).** A line at the top of Pending: arrivals in the last 10 minutes
and what they are (in the form "38 new · 31 lost link in 3 groups since 4:00 · 4 approvals · 1
review").
*Why:* P13, E8. *Where:* `InboxPanel.tsx`. *Risks hiding:* none. The link emulator's cause
(`linkEmu.ts`) is simulation truth: not used without the owner's word (question 6).

**R11. Panels that step aside; top view when crowded (S).** The Situation panel collapses to its
header after first read; the weather legend starts collapsed; a situation with many tracks opens in
the top view (columns and ellipses flatten), 3D one key away. *Why:* [E] the panels cover about a
fifth of the map's width in the screenshots (about 370 of 1,840 px); the console is 3D only with a
top view by the owner's direction. *Risks hiding:* the brief's notes, one click away (question 7).

**At both densities.** R2's budget is per screen and R6's counts do not depend on spacing, so both
hold anywhere. R1, R5 and R7 matter across the map at the live density and inside each group or
raid at realistic spacing, where whole-area views still need R7 and R8. At the live density
hostile groups fly through our rings, so tracks are clustered apart from our vehicles. R9 matters
more as the scene spreads.

**In the small situations** (one operator, 20-odd vehicles): no groups, no tile bubbles (under
`min_fleet`, 100), raids below their minimum size never form, most tracks are in the focused set,
and the label budget is not reached, so the console looks as it does now. R3 says "nothing hidden";
R4 adds one line before the strip the tutorial already raises.

### 3.4 Not recommended

- Removing unknown tracks from the map or the list: violates the constraint and gives up what
  dimming keeps (operators checking the automation [4]).
- Layer toggles as the main declutter: hand declutter cost more than it saved [5]; keep toggles as
  an override behind R3.
- A density heat map in place of tracks: it hides identities and counts; at most a whole-area
  backdrop for our own vehicles.
- A ranking that reorders or recolours by a hidden score: if tracks are ranked, the reason is on
  the row (E10), and affiliation stays the tracker's and the person's.

## 4. How to tell it worked

Measured in Murmur's own terms, on the same situation and seed before and after (a situation
reruns the same draws), at the live density and, if one is built, a spread variant (question 8),
and on the tutorial and hostile-act-engagement to check the small situations do not regress. Only a
credentials or study run is evidence about people (a dev run never is); the owner's own runs are a
smoke test.

| Measure | In Murmur's terms | Where the number comes from |
|---|---|---|
| Time to notice the hostile act | From the act (the engine's act event) to the person first opening the shooter's track (map or list) or its review item | Needs a display event like `operator.view`: today a track opened is logged only when an inbox card is selected (question 5) |
| Time to confirm | From the act to the person's confirmation or revocation of the rule declaration (D-n) | Audit log; eval/METRICS.md "act to declaration" defines the same for the proxy |
| Time to a decision | Approval latency; review item shown to decided | METRICS.md "Approval latency"; `operator.view` |
| Missed | Review items unresolved at the end; approvals expired; act-linked tracks never opened; alerts never opened | Audit log; alerts need instrumentation |
| Wrong | Revocation of a correct declaration; confirmation of a wrong one | METRICS.md identification measures (truth labels) |
| Load | Queue at decision, max queue, utilization surrogate | METRICS.md "Load" |
| Hidden at the decision | Whether the track a decision rests on was in a raid, lowlit or unlabelled when the person decided | Needs the console to report its display state (question 5) |
| Clutter | Feature congestion and subband entropy [7] of screenshots at T+3, 7, 11 and 14; labels and symbols drawn (the dev counters) | A small script over screenshots; the authors' code is MATLAB |
| Workload | NASA-TLX after the run | Questionnaire |

What "worked" would look like, stated before measuring: notice and confirm faster or no slower; no
more missed items; no decision taken on a track that was hidden at the time; clutter measures down;
the small situations unchanged.

## 5. Questions for the owner

1. Should grouping of hostile and unknown tracks stay in the console (display only, invisible to
   agents and replay), or move into the tracker as group tracks with a raid count, visible through
   MCP to the AI watch officer and to the evaluation?
2. Should a raid ever be something a person declares, accepts or nominates as one? (Command design:
   not designed here.)
3. When one of our groups splits in practice, should a detached part become addressable (re-formed
   groups, membership changes, an order to "the rest of PICKET-1")? (Command design.)
4. The standing strip shows only a review item that alone holds up a nomination, which never
   happens in Saturation. Do you want a calm, non-urgent pointer to every pending declaration and
   flag (R4), given that confirming must never be the easy act?
5. Should the console report what it hid or aggregated, and what the person opened, as display
   events beside `operator.view`, so the evaluation can tell whether a decision rested on something
   hidden and when the act was first looked at?
6. The fleet rows show the link emulator's cause for a lost link, which is simulation truth. May
   display grouping use it, or should it come only from what the C2 can know?
7. May a crowded situation open in the top view, within the 3D-only decision?
8. Is Saturation also the display's test bed, or should the display be checked on a spread variant
   (Bodega Bay to Monterey Bay) so that packing and display are judged apart?
9. OFFSET split health monitoring off to a second person. Murmur is one operator by design: should
   group health be watched for the person by something (the engine's alerts, the AI watch officer),
   or is the person meant to carry it?

## 6. Sources

Strength, in brackets: controlled experiment; review; model; standard; practitioner guidance; open
source; vendor material; historical record.

**Research**

1. M. L. Cummings and P. J. Mitchell, "Predicting controller capacity in remote supervision of
   multiple unmanned vehicles", IEEE Trans. SMC Part A 38(2), 451-460, 2008:
   https://dspace.mit.edu/handle/1721.1/90280
   [model checked against a human-in-the-loop simulation]
2. K. A. Roundtree, J. R. Cody, J. Leaf, H. O. Demirel, J. A. Adams, "Human-collective visualization
   transparency", Swarm Intelligence, 2021: https://arxiv.org/abs/2003.10681 [controlled experiment,
   between subjects, n = 56]
3. A. Kolling, P. Walker, N. Chakraborty, K. Sycara, M. Lewis, "Human interaction with robot swarms:
   a survey", IEEE THMS 46(1), 9-26, 2016: https://eprints.whiterose.ac.uk/90476/ [review]
4. M. St. John, H. S. Smallman, D. I. Manes, B. A. Feher, J. G. Morrison, "Heuristic automation for
   decluttering tactical displays", Human Factors 47(3), 509-525, 2005:
   https://dblp.dagstuhl.de/rec/journals/hf/JohnSMFM05.html [controlled experiment, 27 Navy users]
5. M. Yeh and C. D. Wickens, "Attentional filtering in the design of electronic map displays",
   Human Factors 43(4), 543-562, 2001: https://doi.org/10.1518/001872001775870359 [controlled
   experiment]
6. N. Moacdieh and N. Sarter, "Display clutter: a review of definitions and measurement
   techniques", Human Factors 57(1), 61-100, 2015:
   https://www.scilit.net/publications/eda7cd1d0af69b4f1a27fd6f1bbb6541 [review]
7. R. Rosenholtz, Y. Li, L. Nakano, "Measuring visual clutter", Journal of Vision 7(2):17, 2007:
   https://pubmed.ncbi.nlm.nih.gov/18217832; code: https://dspace.mit.edu/handle/1721.1/37593
   [measures validated against search performance]
8. N. Elmqvist and J.-D. Fekete, "Hierarchical aggregation for information visualization:
   overview, techniques, and design guidelines", IEEE TVCG 16(3), 439-454, 2010:
   https://cs.au.dk/~elm/pdf/hieragg.pdf [design guidelines from a survey]
9. B. Shneiderman, "The eyes have it: a task by data type taxonomy for information
   visualizations", IEEE Symposium on Visual Languages, 336-343, 1996:
   https://drum.lib.umd.edu/items/cd983adb-568a-47d3-b43a-1d8d8b8c72f4/full [design guidance]
10. L. M. Padilla, I. T. Ruginski, S. H. Creem-Regehr, "Effects of ensemble and summary displays on
    interpretations of geospatial uncertainty data", Cognitive Research: Principles and
    Implications 2:40, 2017: https://cognitiveresearchjournal.springeropen.com/articles/10.1186/s41235-017-0076-1
    [controlled experiments]
11. S. S. Kirschenbaum and J. E. Arruda, "Effects of graphic and verbal probability information on
    command decision making", Human Factors 36(3), 406-418, 1994:
    https://sci.utah.edu/~kpotter/Library/Papers/kirschenbaum:1994:EGVP/index.html [controlled
    experiment, 16 submarine officers]
12. P. Baudisch and R. Rosenholtz, "Halo: a technique for visualizing off-screen objects", CHI
    2003, 481-488: https://bibtex.github.io/CHI-2003-BaudischR.html [controlled experiment]
13. S. Gustafson, P. Baudisch, C. Gutwin, P. Irani, "Wedge: clutter-free visualization of
    off-screen locations", CHI 2008, 787-796:
    https://hci.cs.umanitoba.ca/Publications/details/wedge-clutter-free-visualization-of-off-screen-locations
    [controlled experiment]
14. R. Parasuraman and D. H. Manzey, "Complacency and bias in human use of automation: an
    attentional integration", Human Factors 52(3), 381-410, 2010:
    https://pubmed.ncbi.nlm.nih.gov/21077562/ [review]
15. L. Mihaylova, A. Y. Carmi, F. Septier, A. Gning, S. K. Pang, S. Godsill, "Overview of Bayesian
    sequential Monte Carlo methods for group and extended object tracking", Digital Signal
    Processing 25, 1-16, 2014: https://eprints.whiterose.ac.uk/90744/ [review]
16. K. Granström, M. Baum, S. Reuter, "Extended object tracking: introduction, overview and
    applications", Journal of Advances in Information Fusion 12(2), 139-174, 2017:
    https://arxiv.org/abs/1604.00970 [review and tutorial]
17. Y. Chen, Y. Jiao, M. Wu, H. Ma, Z. Lu, "Group target tracking for highly maneuverable unmanned
    aerial vehicles swarms: a perspective", Sensors 23(9), 4465, 2023:
    https://www.mdpi.com/1424-8220/23/9/4465 [perspective; its proposals are untested by its own
    account]
18. D. Birant and A. Kut, "ST-DBSCAN: an algorithm for clustering spatial-temporal data", Data and
    Knowledge Engineering 60(1), 208-221, 2007; implementation notes:
    https://rdrr.io/cran/stdbscan/man/st_dbscan.html [algorithm]


**Standards and practitioner guidance**

19. ANSI/ISA-18.2 performance figures, shelving and suppression, as summarised by Siemens Ltd,
    "Improving alarm management with ISA-18.2: Part 2", Process Online, 2014-03-06:
    https://www.processonline.com.au/content/software-it/article/improving-alarm-management-with-isa-18-2-part-2-1141142642
    [standard, through a vendor's summary]
20. SKYbrary, "Plots, tracks and labels": https://skybrary.aero/articles/plots-tracks-and-labels
    [practitioner guidance]
21. EUROCONTROL Experimental Centre Note 21/00, "Requirements for the implementation of automatic
    and manual label anti-overlap functions":
    https://www.eurocontrol.int/publication/requirements-implementation-automatic-and-manual-label-anti-overlap-functions
    [requirements note]
22. FAA, FIXM US extension 4.4.0, `starsDataBlockType`:
    https://www.fixm.aero/releases/US_Ext-4.4.0/doc/schema_documentation/Nas_starsDataBlockType.html
    [standard documentation]
23. FAA Notice N JO 7110.747, "Formation flights" (cancelled, folded into JO 7110.65):
    https://www.faa.gov/documentLibrary/media/Notice/N_JO_7110.747_Formation_Flights.pdf; LA ARTCC
    training notes on 7110.65 2-1-13: https://laartcc.org/stm/military-and-other-special-operations
    [standard; practitioner summary]
24. MIL-STD-2525D: https://www.jcs.mil/Portals/36/Documents/Doctrine/Other_Pubs/ms_2525d.pdf; text
    modifiers (C quantity, T unique designation) as Esri documents them:
    https://resources.arcgis.com/en/help/main/10.2/000n/000n00000073000000.htm [standard]


**Open source**

25. ATAK-CIV, label manager (priority passes, overlap, resolution threshold):
    https://github.com/deptofdefense/AndroidTacticalAssaultKit-CIV/blob/main/takkernel/engine/src/main/cpp/renderer/core/GLLabelManager.cpp
    and `GLLabel.cpp` beside it [open source, US government]
26. ATAK Civilian Software User Manual 4.5, 2021, "Overlay Manager":
    https://toyon.github.io/LearnATAK/atak_resource/v4.5/ATAK_User_Guide.pdf [documentation of
    the open-source application]
27. vice, an open-source simulator of the FAA's STARS display, `datablockType`:
    https://github.com/mmp/vice/blob/v0.13.0/stars/datablock.go [open source; a reimplementation,
    not the FAA's code]
28. deck.gl, CollisionFilterExtension:
    https://deck.gl/docs/api-reference/extensions/collision-filter-extension [library
    documentation]
29. milsymbol (`quantity`, `echelon` options in its type definitions, version 3.0.4 as in
    `web/package.json`): https://github.com/spatialillusions/milsymbol [open source]


**Closed systems, publicly described**

30. Northrop Grumman's OFFSET team, "From warfighting needs to robot actuation" (the RISE swarm
    system and its field experiment), arXiv 2304.07651, 2023: https://arxiv.org/abs/2304.07651
    [the developer's own field report]
31. Janes, "L3Harris AMORPHOUS software targets drone swarm C2 capability", 2025-03-18:
    https://www.janes.com/defence-intelligence-insights/defence-news/air/l3harris-amorphous-software-targets-drone-swarm-c2-capability
    [vendor claims in trade press]


**Historical record**

32. RAF Museum, "Plotting block, hostile raid", 1939-1945:
    https://collections.rafmuseum.org.uk/collection/object/object-73660 [museum record; the
    reading of its figures is the museum's image description]
