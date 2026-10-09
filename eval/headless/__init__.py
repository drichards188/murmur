"""The headless simulator's Python side (docs/HEADLESS.md; evaluation only).

    sim.py        the `murmur-headless` process client and the session adapter for agents
    situations.py a situation as an episode spec (Harbor watch, and generated ones)
    operator.py   the proxy operator on simulated time (eval/proxy.py's model)
    episode.py    one episode: the situation runner's steps, the playbook's loop, the record
    families.py   the generator of situation families (split: split.toml)
    labels.py     truth labels and the association of targets to contacts
    metrics.py    the metrics of an episode record
    fullstack.py  the same record from a full-stack situation run's files
    batch.py      N episodes across cores; the aggregate
    gate.py       G1 and G1b (CRITERIA.md)

MODEL RESULTS ONLY: the operator is a scripted model with stated parameters, the situations
are generated from stated distributions, the labels are the generated truth. Nothing here is
human data or evidence about people.
"""

from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
