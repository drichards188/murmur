"""The space segment in the headless simulator (docs/HEADLESS.md, section 15): satellite element
sets pinned per world as a committed snapshot, so a space episode is reproducible from the
commit alone.

A snapshot is `sim/headless/space/<world>/`: the constellation catalog and the named areas as
they were when it was made (`constellations.toml`, `areas.toml`), every element-set file of the
catalog's queries gzipped (`cache/<query>.json.gz`) beside its own `.meta.json` (where and when
murmur-space got it), and `provenance.json` (each file's SHA-256 and size uncompressed, the
metas, when and how the snapshot was made, and its `id`: one SHA-256 over every file's name
and digest). `unpack` writes it once to `logs/headless/space-snapshots/<world>-<id>/` as the
directory tree the bridge's space loader and the link emulator read (`sim/space/...`, with
`sim/platforms.toml` linked to this checkout's), checking every digest; an episode with
`Options.space_snapshot` points both at it (`MURMUR_ROOT`, the spec's `space_root`). Without
one, nothing changes: the bridge's segment stays off and the emulator's SATCOM reads
`sim/space/cache`, as before.

To make one from the element sets in a cache directory (`murmur-space fetch` or `import` fills
it):

    .venv/bin/python -m eval.headless.space snapshot --world golden-gate --cache sim/space/cache \\
        --note "CelesTrak GP JSON fetched 2026-10-02"
"""

from __future__ import annotations

import argparse
import datetime as dt
import gzip
import hashlib
import json
import os
import shutil
import tempfile
from pathlib import Path
from typing import Any

from . import REPO

SNAPSHOTS = REPO / "sim" / "headless" / "space"
UNPACKED = REPO / "logs" / "headless" / "space-snapshots"
PROVENANCE = "provenance.json"


def snapshot_dir(world: str) -> Path:
    d = SNAPSHOTS / world
    if not (d / PROVENANCE).is_file():
        known = sorted(p.name for p in SNAPSHOTS.iterdir() if (p / PROVENANCE).is_file()) if SNAPSHOTS.is_dir() else []
        raise ValueError(f"no space snapshot for world {world!r} under {SNAPSHOTS} (known: {', '.join(known) or 'none'})")
    return d


def provenance(world: str) -> dict[str, Any]:
    return json.loads((snapshot_dir(world) / PROVENANCE).read_text())


def _sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def snapshot_id(files: dict[str, str]) -> str:
    """One SHA-256 over every file's name and its content's SHA-256 (uncompressed), in name
    order: the snapshot's identity."""
    h = hashlib.sha256()
    for name in sorted(files):
        h.update(name.encode() + b"\0" + files[name].encode() + b"\n")
    return h.hexdigest()


def _contents(world: str) -> dict[str, bytes]:
    """Every file of the snapshot as the loader reads it (uncompressed), by its path under
    `sim/space/`."""
    d = snapshot_dir(world)
    out: dict[str, bytes] = {}
    for name in ("constellations.toml", "areas.toml"):
        out[name] = (d / name).read_bytes()
    for f in sorted((d / "cache").iterdir()):
        if f.name.endswith(".json.gz"):
            out[f"cache/{f.name[:-3]}"] = gzip.decompress(f.read_bytes())
        elif f.name.endswith(".meta.json"):
            out[f"cache/{f.name}"] = f.read_bytes()
    return out


def verify(world: str) -> list[str]:
    """What does not match the snapshot's provenance (empty: it holds)."""
    prov = provenance(world)
    files = _contents(world)
    problems = []
    want = {f["name"]: f["sha256"] for f in prov["files"]}
    for name in sorted(set(want) | set(files)):
        if name not in files:
            problems.append(f"{name}: in the provenance, not in the snapshot")
        elif name not in want:
            problems.append(f"{name}: in the snapshot, not in the provenance")
        elif _sha(files[name]) != want[name]:
            problems.append(f"{name}: SHA-256 {_sha(files[name])[:12]}, the provenance says {want[name][:12]}")
    got = snapshot_id({n: _sha(b) for n, b in files.items()})
    if got != prov["id"]:
        problems.append(f"id {got[:16]}, the provenance says {prov['id'][:16]}")
    return problems


def unpack(world: str, into: Path | None = None) -> Path:
    """The snapshot as a root the bridge's space loader and the emulator read
    (`<root>/sim/space/{constellations.toml, areas.toml, cache/}`, `<root>/sim/platforms.toml`
    linked to this checkout's): written once per snapshot id, atomically (concurrent episodes
    may ask at once), after every digest is checked."""
    prov = provenance(world)
    base = into or UNPACKED
    root = base / f"{world}-{prov['id'][:16]}"
    if (root / "sim" / "space" / PROVENANCE).is_file():
        return root
    problems = verify(world)
    if problems:
        raise ValueError(f"space snapshot {world}: " + "; ".join(problems))
    base.mkdir(parents=True, exist_ok=True)
    tmp = Path(tempfile.mkdtemp(prefix=f".{world}-", dir=base))
    try:
        space = tmp / "sim" / "space"
        (space / "cache").mkdir(parents=True)
        for name, data in _contents(world).items():
            (space / name).write_bytes(data)
        (tmp / "sim" / "platforms.toml").symlink_to(REPO / "sim" / "platforms.toml")
        # last: its presence says the tree is whole
        shutil.copyfile(snapshot_dir(world) / PROVENANCE, space / PROVENANCE)
        try:
            os.rename(tmp, root)
        except OSError:
            if not (root / "sim" / "space" / PROVENANCE).is_file():
                raise
    finally:
        if tmp.exists():
            shutil.rmtree(tmp, ignore_errors=True)
    return root


def look_cache_file(world: str, links: Path) -> Path:
    """Where the emulator keeps SATCOM looks between episodes of this snapshot
    (crates/murmur-headless space_cache.rs): one file per (link profiles, snapshot), apart from
    the files of episodes on `sim/space/cache` (situations.py `space_cache_file`)."""
    h = hashlib.sha256(links.read_bytes())
    h.update(provenance(world)["id"].encode())
    return REPO / "logs" / "headless" / "space-cache" / f"snap-{h.hexdigest()[:16]}.jsonl"


def describe(world: str) -> dict[str, Any]:
    """What an episode record names of the snapshot it ran on."""
    prov = provenance(world)
    return {"world": world, "id": prov["id"], "path": str(snapshot_dir(world).relative_to(REPO)),
            "made": prov.get("made"), "source": prov.get("source")}


def make_snapshot(world: str, cache: Path, catalog: Path, areas: Path, note: str) -> Path:
    """Write `sim/headless/space/<world>/` from a cache directory, the catalog and the areas:
    only the files the catalog's queries read (its `celestrak` queries, as murmur-space's cache
    names them)."""
    import tomllib

    cat = tomllib.loads(catalog.read_text())
    queries = sorted({q for c in cat.get("class", []) for q in c.get("celestrak", [])})

    def key(q: str) -> str:
        return "".join(ch.lower() if ch.isascii() and ch.isalnum() else "-" for ch in q)

    out = SNAPSHOTS / world
    if out.exists():
        shutil.rmtree(out)
    (out / "cache").mkdir(parents=True)
    files: list[dict[str, Any]] = []
    metas: list[dict[str, Any]] = []
    for name, src in (("constellations.toml", catalog), ("areas.toml", areas)):
        b = src.read_bytes()
        (out / name).write_bytes(b)
        files.append({"name": name, "sha256": _sha(b), "bytes": len(b), "from": str(src)})
    for q in queries:
        data = (cache / f"{key(q)}.json").read_bytes()
        meta = (cache / f"{key(q)}.meta.json").read_bytes()
        # mtime 0: the same bytes for the same data, so a re-made snapshot changes nothing
        (out / "cache" / f"{key(q)}.json.gz").write_bytes(gzip.compress(data, 9, mtime=0))
        (out / "cache" / f"{key(q)}.meta.json").write_bytes(meta)
        files.append({"name": f"cache/{key(q)}.json", "sha256": _sha(data), "bytes": len(data), "query": q})
        files.append({"name": f"cache/{key(q)}.meta.json", "sha256": _sha(meta), "bytes": len(meta)})
        metas.append(json.loads(meta))
    prov = {
        "what": "satellite element sets pinned for the headless simulator's space segment (docs/HEADLESS.md)",
        "world": world,
        "id": snapshot_id({f["name"]: f["sha256"] for f in files}),
        "made": dt.date.today().isoformat(),
        "command": f".venv/bin/python -m eval.headless.space snapshot --world {world} --cache {cache} "
                   f"--catalog {catalog} --areas {areas} --note {json.dumps(note)}",
        "source": note,
        "element_sets": [{k: m.get(k) for k in ("query", "url", "fetched_at", "objects", "bytes")} for m in metas],
        "files": files,
    }
    (out / PROVENANCE).write_text(json.dumps(prov, indent=1, sort_keys=True) + "\n")
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("snapshot", help="pin the element sets of a cache directory for a world")
    s.add_argument("--world", required=True)
    s.add_argument("--cache", type=Path, default=REPO / "sim" / "space" / "cache")
    s.add_argument("--catalog", type=Path, default=REPO / "sim" / "space" / "constellations.toml")
    s.add_argument("--areas", type=Path, default=REPO / "sim" / "space" / "areas.toml")
    s.add_argument("--note", required=True, help="where the element sets came from")
    v = sub.add_parser("verify", help="check a snapshot against its provenance")
    v.add_argument("--world", required=True)
    a = ap.parse_args()
    if a.cmd == "snapshot":
        out = make_snapshot(a.world, a.cache, a.catalog, a.areas, a.note)
        print(f"{out}: id {provenance(a.world)['id'][:16]}")
    else:
        problems = verify(a.world)
        print("\n".join(problems) if problems else f"{a.world}: holds")
        raise SystemExit(1 if problems else 0)


if __name__ == "__main__":
    main()
