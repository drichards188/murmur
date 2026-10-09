"""The playbook engine: Murmur's deterministic (no model) AI agent (murmur-t8m, step 1).

A library of named, versioned plays (a trigger read from facts, a few checks, one proposal,
its withdraw condition and its rationale template), run on a fixed tick against one read of
the world, with an arbiter that guards the operator's inbox and a ledger that remembers every
proposal and how the operator answered it. The design is the research report's Part C
(docs/DECISIONS.md, "The playbook engine"); what "useful" means, frozen before its first live
run, is CRITERIA.md.

One engine, three roles: the free option for a situation (`AI=playbook`, no model, no account:
`agents/playbook_agent.py`), the baseline the model is measured against, and the shadow beside
the model watch officer (`agents/watch/shadow.py`: reads only, would-be proposals logged).

    config.py     the shipped defaults (defaults.toml) under a situation's [playbook] table, hashed
    reads.py      one tick's reads and its time (the oldest `t` among them)
    beliefs.py    pure functions of the reads: contacts of interest, CPA and TCPA, coverage,
                  vehicle health, free vehicles, inbox pressure, engagements in play
    plays/        each play: candidates from the beliefs, its withdraw condition, its material change
    arbiter.py    priority classes, the total order, claims, the inbox budget
    ledger.py     own cards and their states, cooldowns, back-offs (JSON per run)
    rationale.py  the evidence record and the fixed templates; the delimiter and its parser
    rulebook.py   the evaluation's frozen rule book (eval/evidence.py) as a veto
    engine.py     the tick: read, believe, play, arbitrate, preview, veto, propose, follow up
"""

ENGINE_VERSION = "1"
