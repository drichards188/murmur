# Product

Design context for the Murmur console (`web/`), read by design tooling and by anyone changing the
interface. The owner set the preferences below on 2026-10-27; the rest restates what README.md,
docs/PLAN.md and CLAUDE.md already establish.

## Register

Product. The console is a tool: design serves the decisions made in it. The main menu's backdrop
is the one decorative surface, and even it shows no figure of any run.

## Purpose

Murmur is an open-source command-and-control testbed: one operator supervising about a hundred
autonomous air, ground, surface, subsurface and space nodes, built to measure whether human
judgment over consequential decisions stays meaningful. Orders are proposed by the operator or by
an AI watch officer, checked by a deterministic decision engine, and routed by policy to go out at
once or wait for a person. An engagement needs a hostile declaration, a nomination, and two
signatures from operators with engagement authority, neither of them its proposer.

## Users

Novice-safe, expert-fast. A study participant with no military background must be able to follow
every step cold; an experienced operator must be able to move through the same steps quickly,
from the keyboard. The setting is a desk, a laptop or a large monitor, a watch of a quarter of an
hour or open-ended, alerts and AI proposals competing for attention, and some decisions that kill.

## Personality

A glass cockpit's checklist: calm, clinical, step by step. One decision at a time, the state of
the whole always visible, plain words before codes, the consequence of an action stated where the
action is. Urgency is shown only where something is urgent.

## Anti-references

- Legacy military software: walls of fields, cryptic codes, everything at the same weight.
- Alarm fatigue: red everywhere, constant flashing; when everything is urgent, nothing is.

## Principles

1. The person must understand what they approve. Bottom line first (a headline, a few bullets),
   detail one click away, never a paragraph where a list will do.
2. Never nudge toward yes. Approve and reject carry equal weight; the interface presents, the
   person decides. The AI's proposals are labelled as the AI's.
3. Say what changed. A signature is given on a picture; when the picture moves, show what moved.
4. State the consequence where the action is ("your signature freezes this plan", "nothing moves
   until two approvers sign").
5. Make the two-person rule visible: who has signed, who still must, and why the proposer cannot.

## Accessibility

Not a requirement for now (owner, 2026-10-27). Keep the dark theme's existing contrast.
