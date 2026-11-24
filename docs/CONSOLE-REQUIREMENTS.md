# Console requirements: what the operator must see, and when

Goal-directed task analysis (Endsley, Bolte and Jones 2003), written out once so every element on
screen can be checked against it. Derived from docs/PRODUCT.md and two sweeps of the console
(2026-11-24, bd epic murmur-iobd), which judged each element by the decision it serves, its
salience against its value (Wickens' SEEV), whether an alarm has a response (ISA-18.2), whether the
normal state is quiet (dark cockpit, ISA-101) and whether each fact has one home. The cutdown of
the console was built from it, element by element.

A change to the console names the requirement each new element serves and its tier; an element
that serves none does not go in.

**The rule (owner, 2026-11-24):** an element stays only if it serves one of the requirements below,
at the tier given. Everything else is removed. It is easier to add a missing thing back after
testing than to find what should go in a wall of data. The ledger records every removal and where
it lived, so adding back is a lookup.

Tiers (at most two levels from where a decision is made):

- **Glance**: on screen without a click, quiet when normal.
- **One click**: the bottom line of one thing, where its decision is made (a card, the details).
- **Fold**: a labelled fold inside a one-click view, for the evidence behind it.

## Goals

- **G1 Ours:** know where my vehicles are, what each is doing, and whether any is in trouble.
- **G2 Contacts:** know what is out there, what each is, how sure, where it heads, which matter.
- **G3 What waits on me:** decide approvals, reviews and signatures, understanding what I approve.
- **G4 Act:** task vehicles; nominate an engagement.
- **G5 Follow through:** what I set going is happening, and how it ended.
- **G6 The run:** what this is, my job, the time (situations and the tutorial).

## Requirements

Level: L1 perceive, L2 comprehend, L3 project.

| Id | The person must | Level | Tier | Serves |
|---|---|---|---|---|
| R1 | See where each of ours is, and which it is (symbol, callsign) | L1 | Glance (map; list row) | G1 |
| R2 | See at once that one of ours is in trouble and what it can no longer do (damaged, silent, presumed or confirmed lost, link down, out of rounds, low battery) | L2 | Glance (exception slot on row and symbol) | G1 |
| R3 | Know what each of ours is doing, in words of its order (patrolling, on station, en route, holding, returning) | L2 | Glance on the row; one click for its order | G1, G5 |
| R4 | Choose a vehicle for a task: kind, speed, endurance or battery, armed and rounds left, sensors and their reach now | L2, L3 | One click (vehicle details "Now" and "Platform"); the order form | G4 |
| R5 | Understand a link problem: what failed, since when, what the vehicle will do (its failsafe) | L2, L3 | One click (vehicle details "Link"), only when abnormal on the glance | G1 |
| R6 | See each contact: where, affiliation, what it is in words, fresh or old | L1, L2 | Glance (map symbol, one label; Tracks row) | G2 |
| R7 | Decide about a contact: what it is and how sure, why (AIS, sensor identification), what it did (acts against ours), where it heads | L2, L3 | One click (track card); evidence in a fold | G2, G3 |
| R8 | Know whether anything needs me, how much, and what is most urgent with its time left | L1 | Glance (one number; a strip only when something alone blocks a step) | G3 |
| R9 | Decide one item: the ask in one sentence; who asks (the AI labelled as the AI); the consequence of each answer; the evidence; what changed since it was asked; time left; who has signed and who still must; two buttons of equal weight naming their outcomes | L2, L3 | One click (the card); evidence in a fold, never removed | G3 |
| R10 | Be alarmed once per event that needs a response: who did what to whom, the consequence, the response expected | L1, L2 | Glance (one banner per event) then the card | G1, G3 |
| R11 | Give an order and nominate: target or area, vehicles, what the dry run says will happen and what blocks it | L2, L3 | The order form and nomination form (on demand) | G4 |
| R12 | Follow an order: its state in one word; on failure, why and what to do | L2 | One click (Command log card per order) | G5 |
| R13 | Follow an engagement: platform en route, time to effect, the picture holding, Abort | L2, L3 | One click (the running card); route on the map | G5 |
| R14 | Read an outcome: the assessment's verdict first, then what can follow | L2, L3 | One click (the assessed card) | G5 |
| R15 | Know the world's time; in a situation T+ and time left | L1 | Glance (top bar, once) | G6 |
| R16 | Know what this run is, my job, and the next thing to watch | L2 | One line at glance in a situation; the panel on one click | G6 |
| R17 | At group scale, see each group's strength against its minimum, its losses, and its order | L2 | Glance (group chip/strip); one click (Groups drawer) | G1, G5 |
| R18 | Know when conditions change what a sensor or link can do (fog, night, sea state, coverage) | L2, L3 | One click (Weather, Links views) and where it changes a decision (a check, a reach) | G2, G4 |
| R19 | Know when the console itself is not live (bridge down, reconnecting, a stale feed) | L1 | Glance, only when abnormal | all |
| R20 | Move the map and the view (zoom, pan, top view, fit) | | Controls, quiet | all |
| R21 | Know the run's standing that changes what my acts mean: a dev run is not evidence; who I am acting as | L2 | Quiet mark; one click to change | G3 |
| R22 | Ask the AI watch officer for work and read its replies (runs whose AI takes requests) | L2 | One click (Officer drawer) | G4 |
| R23 | Read back what happened in the run, in plain words (a situation's narrated timeline) | L2 | Fold ("What happened") in the Situation panel | G6 |
| R24 | End or leave the run (the menu) | | Quiet control | G6 |

## Settled for the build (2026-11-24, from ledger 1's open items)

- The Menu button stays (R24); "MURMUR" stays, small; the subtitle goes.
- The Officer stays as stage A left it (R22).
- The Situation panel keeps one "What happened" fold (R23); the AI session line, raw scripted
  decisions, run id and log path go.
- Weather animation stays as it is: the owner chose animated weather on 2026-10-30. Not reopened.
- Weapon situations open in top view, 3D one key away (D6).
- The Sitrep's headline becomes the top bar's glance line; the Sitrep button shows only at group
  scale (D2).
- No engineering-details setting (D4): what serves no requirement is removed.

## Rules that follow

1. **No requirement, no element.** Developer and simulator internals (sim tags, protocol states,
   ms and loss numbers when normal, file paths, make commands, run ids, raw ids where a name
   exists, authenticity notes) serve none of the above: removed, not hidden behind a setting.
2. **One home per fact.** A second copy stays only if it serves a different requirement there.
3. **Quiet when normal.** Colour, motion and ticking only on an exception (R2, R8, R10, R19).
4. **Alarm only with a response** (ISA-18.2): no response expected, no alarm styling.
5. **Decision cards (R9) fold, never delete.** What a person approves must stay one click from
   the buttons; the card leads with the ask and folds the rest.
6. **Drawers do not hide the scene they are about**: the map keeps the subject in view.

## Settled for the build, ledger 2 (2026-11-24)

- Snooze goes; Acknowledge stays.
- A running or assessed engagement is in "Needs you" only when its picture changed, a gun is
  firing, or the verdict is persists or unknown; otherwise "For information".
- Globe merges into one Space view, offered only where the scenario has a space segment.
- The main menu (Menu.tsx, the launcher) is out of this pass: the owner has edits there.
- The lost-contact claim card keeps Confirm as its single act.
- The Sitrep's node table stays only for its trouble filters at group scale; "Copy as text" goes.
