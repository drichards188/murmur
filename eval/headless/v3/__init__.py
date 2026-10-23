"""Scoring v3 (eval/SCORING-V3.md, a draft; docs/WORLD-V3-PLAN.md sections 6 and 7): needs and labels
across every domain, the first situation families on world v3's two maps, the basic v3 oracle, and
the dev check. Built beside scoring v2, which stays frozen: v2's code is imported where v3 reuses
it unchanged, never edited.

- `mission.py`: the per-domain mission (what must be identified, by which sensor, by when) and its
  text for each world;
- `families.py`: F1-F6 and F18 on golden-gate-v3 and san-diego;
- `labels.py`: contact needs per domain, fault needs (v2's), the action labels;
- `metrics.py`: v2's metrics and need served per domain;
- `oracle.py`: the basic v3 oracle (agent `oracle-v3`);
- `run.py`: a v3 batch and its dev check (`python -m eval.headless.v3.run`).
"""
