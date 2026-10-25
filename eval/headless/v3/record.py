"""Scoring v3's part of the picture record (eval/SCORING-V3.md 2.2 and 2.6, choice 5): v2's encoding
(`murmur-picture/1`, eval/headless/v2/record.py) and, under the physical sensing model, each track's
looks: which observer's sensor reached which level (detection, recognition, identification), what
it called the contact and the surest it has been (crates/murmur-bridge tracks/model.rs
`Track::json`, `identity.looks`). A legacy track has no looks, so a v1 or v2 record is unchanged;
v2's encoder and reader are used as they are.

    tracks  {track id: {..., v2's ...,
              looks  {"<uid>:<sensor>:<level>": [[k, label, confidence], ...]}}}

as change points, as v2's rows: the state at sample `k` is the last row at or before it. The
tracker keeps every look it was given (a level reached stays), so a key once written stays.
Confidence is rounded to 0.01 (a camera's look follows its range each second).
"""

from __future__ import annotations

from typing import Any

from ..v2.record import Picture, PictureEncoder, _r

LEVELS = ("detect", "recognise", "identify")


class PictureEncoderV3(PictureEncoder):
    """v2's encoder, and each track's looks when the picture carries them."""

    def add(self, p: dict[str, Any]) -> None:
        super().add(p)
        k = len(self.times) - 1
        for tr in p.get("tracks") or []:
            looks = (tr.get("identity") or {}).get("looks")
            if not looks:
                continue
            tid = str(tr["id"])
            rows_by = self.tracks[tid].setdefault("looks", {})
            for lk in looks:
                key = f"{lk['uid']}:{lk['sensor']}:{lk['level']}"
                self._change(rows_by.setdefault(key, []), (tid, "look", key), k, (lk.get("label"), _r(lk.get("confidence"), 2)))


def looks(pic: Picture, tid: str, k: int | None) -> list[dict[str, Any]]:
    """The looks on track `tid` at sample `k`: `{uid, sensor, level, label, confidence}`, by
    observer, sensor and level ([] for a track the physical model gave no looks, or absent)."""
    T = pic.tracks.get(tid)
    if T is None or not pic.present(tid, k):
        return []
    out = []
    for key, rows in (T.get("looks") or {}).items():
        r = pic._row(rows, k)
        if r is None:
            continue
        uid, sensor, level = key.split(":")
        out.append({"uid": int(uid), "sensor": sensor, "level": level, "label": r[1], "confidence": r[2]})
    out.sort(key=lambda x: (x["uid"], x["sensor"], LEVELS.index(x["level"]) if x["level"] in LEVELS else 9))
    return out


def reached(pic: Picture, tid: str, k: int | None, level: str, label: str | None = None) -> list[str]:
    """The sensors (`<uid>:<sensor>`) whose look on track `tid` at sample `k` reached `level` (with
    `label`, when given): which sensor reached identification, say, that `supported_by` cannot
    tell (choice 5)."""
    return sorted({f"{x['uid']}:{x['sensor']}" for x in looks(pic, tid, k)
                   if x["level"] == level and (label is None or x["label"] == label)})
