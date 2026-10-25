"""Scoring v3's picture record (eval/headless/v3/record.py): each track's looks under the physical
model (which sensor reached which level, SCORING-V3.md choice 5) round-trip exactly, and a picture
without looks (the legacy model's, every v1 and v2 record) encodes exactly as v2's encoder does."""

from __future__ import annotations

import json
import random
from typing import Any

from eval.headless.v2.record import Picture, PictureEncoder
from eval.headless.v3.record import PictureEncoderV3, looks, reached

EPOCH = 1_781_431_200.0
LEVELS = ("detect", "recognise", "identify")


def sample(k: int, tracks: list[dict[str, Any]]) -> dict[str, Any]:
    return {"t": float(k), "tracks": tracks, "association": [[tr["id"], "c"] for tr in tracks], "alerts": [], "ours": []}


def track(tid: str, k: int, looks_: list[dict[str, Any]] | None) -> dict[str, Any]:
    idn: dict[str, Any] = {"kind": "small_craft", "confidence": 0.5, "conflict": None, "supported_by": ["eo_ir"]}
    if looks_ is not None:
        idn["looks"] = looks_
    return {"id": tid, "domain": "surface", "position": [-122.5, 37.8], "velocity_mps": None, "affiliation": "unknown",
            "affiliation_confidence": 0.5, "identity": idn, "sources": [], "last_seen_t": EPOCH + k}


def test_looks_round_trip_at_every_sample():
    rng = random.Random(3)
    enc = PictureEncoderV3(EPOCH)
    marks: dict[str, tuple[str, float]] = {}
    truth = []
    for k in range(150):
        # the tracker keeps every look it was given: a mark, once made, stays; its confidence
        # rises or its label changes
        if rng.random() < 0.3:
            key = f"{rng.choice([3, 5])}:{rng.choice(['eo_ir', 'sonar'])}:{rng.choice(LEVELS)}"
            label, conf = marks.get(key, ("small_craft", 0.0))
            marks[key] = (rng.choice([label, "small_fast_boat"]), round(min(1.0, conf + rng.random() / 5), 3))
        rows = [{"uid": int(key.split(":")[0]), "sensor": key.split(":")[1], "level": key.split(":")[2], "label": lab,
                 "confidence": c} for key, (lab, c) in sorted(marks.items())]
        enc.add(sample(k, [track("T-0001", k, rows or None)]))
        truth.append({key: (lab, round(c, 2)) for key, (lab, c) in marks.items()})
    pic = Picture(json.loads(json.dumps(enc.finish())))
    for k, want in enumerate(truth):
        got = {f"{x['uid']}:{x['sensor']}:{x['level']}": (x["label"], x["confidence"]) for x in looks(pic, "T-0001", k)}
        assert got == want, k
    last = len(truth) - 1
    ids = reached(pic, "T-0001", last, "identify")
    assert ids == sorted({key.rsplit(":", 1)[0] for key in truth[last] if key.endswith(":identify")})


def test_a_picture_without_looks_encodes_as_v2s():
    rng = random.Random(9)
    a, b = PictureEncoder(EPOCH), PictureEncoderV3(EPOCH)
    for k in range(60):
        tracks = [track(f"T-000{i}", k, None) for i in range(1, 3) if rng.random() < 0.9]
        a.add(sample(k, tracks))
        b.add(sample(k, tracks))
    assert json.dumps(a.finish(), sort_keys=True) == json.dumps(b.finish(), sort_keys=True)
    assert looks(Picture(b.finish()), "T-0001", 10) == []
