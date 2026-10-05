"""The batch report: one self-contained HTML file (inline CSS and SVG, no scripts, no
network) from a results directory, and a `summary.json` beside it.

    python -m eval.report eval/results/<batch>        # writes <batch>/report.html

Metrics are recomputed from each run's raw logs (eval/metrics.py `compute_run`), so a fix to
a metric applies to old batches; the replay result recorded at run time is reused.

What the report will and will not say (eval/METRICS.md):

- Only runs that finished (`ok`) are scored. A run that failed, timed out or left no
  `run.json` (checked against the batch's job list) is listed under "What went wrong" and
  kept out of every number.
- Wrong proposals are split by what the C2 showed when they were made (the frozen rule book,
  eval/evidence.py): "the C2 showed it" (catchable from the picture) and "picture wrong"
  (the C2 showed nothing against it). No number pools the two, and no number pools
  scenarios.
- The primary interval is clustered by seed: one rate per run (a run is one seed of one
  scenario and arm), their mean, and a Student t 95% interval. The pooled Wilson interval
  is shown second; it treats proposals as independent, which they are not.
- The proxy arms are a model, not people: their rates are outputs of eval/proxy.py at the
  parameters listed, and their intervals are Monte Carlo variation of that model across
  seeds, never an estimate about humans.
- Held-out scenarios (written after the rule book was frozen) are reported apart.
"""

from __future__ import annotations

import datetime as dt
import html
import json
import re
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

from .engage import load_suite
from .evidence import RULEBOOK_VERSION
from .metrics import NOT_HUMAN, compute_run
from .provenance import code_id, is_doc
from .provenance import collect as current_code
from .stats import dprime, summary, t_interval, wilson

ARM_ORDER = ["rules-only", "rules-only-reject", "human-proxy", "agent+human-proxy", "agent+model", "study"]
# categorical slots (dataviz reference palette), fixed per arm, never cycled
ARM_COLOR = {
    "rules-only": ("#2a78d6", "#3987e5"),
    "human-proxy": ("#eb6834", "#d95926"),
    "agent+human-proxy": ("#1baf7a", "#199e70"),
    "rules-only-reject": ("#eda100", "#c98500"),
    "agent+model": ("#e87ba4", "#d55181"),
    "study": ("#008300", "#008300"),
}
RIGHT = ("#2a78d6", "#3987e5")
WRONG = ("#eb6834", "#d95926")
# who decides the inbox in each arm, in the words the report uses (never bare "human")
DECIDER = {
    "rules-only": "a script that approves everything",
    "rules-only-reject": "a script that rejects everything",
    "human-proxy": "the proxy operator model",
    "agent+human-proxy": "the proxy operator model",
    "agent+model": "the proxy operator model",
    "study": "a participant",
}
PROXY_ARMS = ("human-proxy", "agent+human-proxy", "agent+model")


def esc(x: Any) -> str:
    return html.escape("" if x is None else str(x))


def pct(r: float | None, digits: int = 0) -> str:
    return "n/a" if r is None else f"{100 * r:.{digits}f}%"


def num(x: float | None, digits: int = 1) -> str:
    return "n/a" if x is None else f"{x:.{digits}f}"


def arm_var(a: str) -> str:
    """The CSS custom property for an arm's color ("+" is not allowed in the name)."""
    return "--arm-" + re.sub(r"[^a-z0-9]+", "-", a.lower())


def arm_key(a: str) -> int:
    return ARM_ORDER.index(a) if a in ARM_ORDER else len(ARM_ORDER)


def arm_label(a: str) -> str:
    """The arm's name as tiles and headings show it: the proxy is always called a model."""
    return f"{a} (model)" if a in PROXY_ARMS else a


# ---- collection ------------------------------------------------------------------------------


def find_runs(batch: Path) -> list[Path]:
    return sorted(p.parent for p in batch.glob("*/*/seed-*/run.json"))


def collect(batch: Path) -> tuple[dict[str, Any], list[dict[str, Any]], list[dict[str, Any]]]:
    """(batch meta, scored runs, excluded runs). A run is scored only if its status is ok
    (in its run.json and in the batch's record of it); every other run, and every job of
    the batch that left no run.json, is excluded and listed with why."""
    meta: dict[str, Any] = {}
    if (batch / "batch.json").exists():
        try:
            meta = json.loads((batch / "batch.json").read_text())
        except ValueError:
            meta = {}
    batch_status = {r.get("dir"): r for r in meta.get("runs", [])}
    runs, excluded = [], []
    seen = set()
    for d in find_runs(batch):
        rel = str(d.relative_to(batch))
        seen.add(rel)
        try:
            info = json.loads((d / "run.json").read_text())
        except ValueError as e:
            excluded.append({"rel": rel, "why": f"run.json unreadable: {e}"})
            continue
        b = batch_status.get(rel, {})
        status = info.get("status")
        if b.get("status") == "timed out":
            status = "timed out"
        if status != "ok":
            excluded.append({"rel": rel, "why": f"{status}: {(b.get('error') or info.get('error') or '')[:400]}", "info": info})
            continue
        try:
            m = compute_run(d)
        except Exception as e:  # noqa: BLE001 (a broken run is reported, not fatal)
            excluded.append({"rel": rel, "why": f"metrics failed: {e!r}", "info": info})
            continue
        auth = m.get("auth") or {}
        if info.get("arm") == "study" and not auth.get("human_control_evidence"):
            # a participant's decisions count only when they signed in with their own
            # credential, the log replayed and its anchors held (rules 8)
            why = "; ".join(auth.get("why_not") or [f"operators not authenticated (auth {auth.get('mode')})"])
            excluded.append(
                {
                    "rel": rel,
                    "why": f"a study run that is not evidence about people: {why}",
                    "info": info,
                }
            )
            continue
        story = []
        if (d / "storyline.jsonl").exists():
            for line in (d / "storyline.jsonl").read_text().splitlines():
                try:
                    story.append(json.loads(line))
                except ValueError:
                    continue
        runs.append({"dir": d, "rel": rel, "info": info, "m": m, "story": story})
    for job in meta.get("jobs", []):
        if job.get("dir") not in seen:
            b = batch_status.get(job.get("dir"), {})
            excluded.append({"rel": job.get("dir"), "why": f"no run.json ({b.get('status') or 'never ran'}; {b.get('error') or ''})"})
    return meta, runs, excluded


def get(b: Any, path: list[str]) -> Any:
    for p in path:
        b = b.get(p) if isinstance(b, dict) else None
    return b


def seed_block(runs: list[dict[str, Any]], path: list[str]) -> dict[str, Any]:
    """A {k, n} block over runs. Primary: one rate per run (one seed), their mean and a
    Student t 95% interval (clustered by seed). Secondary: the pooled k/n with a Wilson
    interval, which treats the proposals as independent."""
    k = n = 0
    per_seed = []
    for r in runs:
        b = get(r["m"], path)
        if not isinstance(b, dict) or not b.get("n"):
            continue
        k += int(b.get("k", 0))
        n += int(b.get("n", 0))
        per_seed.append(b["k"] / b["n"])
    rate, lo, hi = wilson(k, n)
    mean, tlo, thi = t_interval(per_seed) if len(per_seed) >= 2 else (None, None, None)
    return {
        "k": k,
        "n": n,
        "rate": rate,
        "ci": [lo, hi],
        "seed_rates": [round(x, 4) for x in per_seed],
        "seed_mean": mean,
        "seed_ci": [None if tlo is None else max(0.0, tlo), None if thi is None else min(1.0, thi)],
        "seeds_with_cases": len(per_seed),
    }


def chart_block(b: dict[str, Any]) -> dict[str, Any]:
    """The block a dot plot draws: the seed-clustered mean and interval where two seeds had
    cases, else the pooled rate and its Wilson interval."""
    if b.get("seed_mean") is not None:
        return {**b, "rate": b["seed_mean"], "ci": b["seed_ci"]}
    return b


def disc(runs: list[dict[str, Any]], key: str = "discrimination") -> dict[str, Any]:
    """Discrimination over runs: the four cells summed, then d' (stats.dprime); and the
    per-seed d' values."""
    cells = {"hit": 0, "miss": 0, "false_alarm": 0, "correct_reject": 0}
    per_seed = []
    for r in runs:
        d = r["m"].get(key) or {}
        for k in cells:
            cells[k] += int(d.get(k, 0))
        if d.get("dprime") is not None:
            per_seed.append(d["dprime"])
    return {**cells, "dprime": dprime(cells["hit"], cells["miss"], cells["false_alarm"], cells["correct_reject"]), "seed_dprime": per_seed}


def read_stats(runs: list[dict[str, Any]], c2_shows_it: bool) -> dict[str, Any] | None:
    """For a proxy arm: the should-reject cards (by whether the C2 showed it) the model
    decided in the inbox, how many it read, and how many of those its rule book fired on
    (from the run's decision records: `diligent`, `if_read`)."""
    n = read = fired = caught = 0
    p_read = []
    for r in runs:
        dec = {d.get("intent_id"): d for d in r["info"].get("decisions", []) if "diligent" in d}
        if not dec:
            continue
        for p in r["m"].get("per_intent", []):
            if p.get("label") != "should_reject" or p.get("c2_shows_it") is not c2_shows_it:
                continue
            d = dec.get(p.get("id"))
            if d is None:
                continue
            n += 1
            p_read.append(float(d.get("p_read", 0.0)))
            if d.get("diligent"):
                read += 1
                if d.get("if_read") == "reject":
                    fired += 1
            if d.get("action") == "reject":
                caught += 1
    if not n:
        return None
    return {"n": n, "read": read, "fired": fired, "caught": caught, "p_read_mean": sum(p_read) / len(p_read)}


def tte(runs: list[dict[str, Any]], key: str) -> dict[str, Any]:
    """Time to effect: the median of the per-run medians (each run one seed), and the
    median of every value pooled."""
    meds = [get(r["m"], ["time_to_effect", key, "median"]) for r in runs]
    meds = [m for m in meds if m is not None]
    vals = [v for r in runs for v in (get(r["m"], ["time_to_effect", key, "values"]) or [])]
    return {"median_of_run_medians": summary(meds)["median"], "pooled_median": summary(vals)["median"], "n": len(vals), "runs": len(meds)}


def aggregate(runs: list[dict[str, Any]]) -> dict[tuple[str, str], dict[str, Any]]:
    groups: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    for r in runs:
        groups[(r["info"].get("scenario", "?"), r["info"].get("arm", "?"))].append(r)
    out = {}
    for key, rs in groups.items():
        lat = [d["latency_s"] for r in rs for d in r["m"].get("decisions", []) if d.get("latency_s") is not None]
        legit_shown = sum(1 for r in rs for p in r["m"].get("per_intent", []) if p.get("label") == "legitimate" and p.get("c2_shows_it"))
        out[key] = {
            "runs": rs,
            "seeds": sorted(r["info"].get("seed") for r in rs),
            "sr_shown": seed_block(rs, ["should_reject", "c2_shows_it"]),
            "sr_wrong_picture": seed_block(rs, ["should_reject", "picture_wrong"]),
            "sr_shown_prevented": seed_block(rs, ["should_reject", "c2_shows_it", "prevented"]),
            "sr_wrong_picture_prevented": seed_block(rs, ["should_reject", "picture_wrong", "prevented"]),
            "read_shown": read_stats(rs, True),
            "read_wrong_picture": read_stats(rs, False),
            "sv": seed_block(rs, ["should_void"]),
            "sv_stopped": seed_block(rs, ["should_void", "stopped"]),
            "fr": seed_block(rs, ["legitimate", "false_reject"]),
            "fr_kept": seed_block(rs, ["legitimate", "false_reject_without_changed"]),
            "lp": seed_block(rs, ["legitimate", "passed"]),
            "legit_c2_against": legit_shown,
            "ov": seed_block(rs, ["override"]),
            "goodput": seed_block(rs, ["goodput"]),
            "disc": disc(rs),
            "disc_armed": disc(rs, "discrimination_armed"),
            "disc_kept": disc(rs, "discrimination_without_changed"),
            "disc_armed_kept": disc(rs, "discrimination_armed_without_changed"),
            "lat": summary(lat),
            "max_queue": max((r["m"]["load"]["max_queue"] for r in rs), default=None),
            "expired": sum(r["m"]["load"]["expired"] for r in rs),
            "violations": sum(r["m"]["invariants"]["violations"] for r in rs),
            "replay_ok": sum(1 for r in rs if (r["m"]["invariants"]["replay"] or {}).get("ok")),
            "tte_auto": tte(rs, "auto"),
            "tte_human": tte(rs, "human_legitimate"),
            "premise_failed": sum(len(r["m"].get("premises", {}).get("failed", [])) for r in rs),
            "changed": sum(len(r["m"].get("changed_before_decision", [])) for r in rs),
            "labelled": sum(r["m"]["counts"]["labelled"] for r in rs),
            "premise_checked": sum(r["m"].get("premises", {}).get("checked", 0) for r in rs),
            "decisions": [d for r in rs for d in r["m"].get("decisions", [])],
        }
    return out


# ---- charts (inline SVG) ---------------------------------------------------------------------


def legend(items: list[tuple[str, str, str]]) -> str:
    """items: (label, css var, shape)"""
    parts = []
    for label, var, shape in items:
        mark = (
            f'<svg width="14" height="14" aria-hidden="true"><circle cx="7" cy="7" r="5" fill="var({var})"/></svg>'
            if shape == "dot"
            else f'<svg width="14" height="14" aria-hidden="true"><path d="M3 3 L11 11 M11 3 L3 11" stroke="var({var})" stroke-width="2.5" stroke-linecap="round"/></svg>'
        )
        parts.append(f'<span class="key">{mark}{esc(label)}</span>')
    return f'<div class="legend">{"".join(parts)}</div>'


def rate_chart(rows: list[tuple[str, list[tuple[str, dict[str, Any]]]]], title: str, xlabel: str) -> str:
    """A dot plot: one row per scenario, one dot per arm with its Wilson interval."""
    if not rows:
        return ""
    arms = sorted({a for _, items in rows for a, _ in items}, key=arm_key)
    left, right, top, row_h = 190, 70, 18, 16 + 16 * len(arms)
    width = 760
    plot_w = width - left - right
    height = top + row_h * len(rows) + 40
    x = lambda v: left + v * plot_w  # noqa: E731
    out = [f'<svg class="chart" viewBox="0 0 {width} {height}" role="img" aria-label="{esc(title)}">']
    for v in (0, 0.25, 0.5, 0.75, 1.0):
        out.append(f'<line x1="{x(v):.1f}" y1="{top - 6}" x2="{x(v):.1f}" y2="{height - 34}" class="grid"/>')
        out.append(f'<text x="{x(v):.1f}" y="{height - 18}" class="tick" text-anchor="middle">{int(v * 100)}%</text>')
    out.append(f'<text x="{left + plot_w / 2:.1f}" y="{height - 2}" class="axis-label" text-anchor="middle">{esc(xlabel)}</text>')
    for i, (scenario, items) in enumerate(rows):
        y0 = top + i * row_h
        out.append(f'<text x="{left - 10}" y="{y0 + row_h / 2:.1f}" class="row-label" text-anchor="end" dominant-baseline="middle">{esc(scenario)}</text>')
        for j, a in enumerate(arms):
            b = dict(items).get(a)
            if not b or not b.get("n"):
                continue
            y = y0 + 10 + j * 16
            lo, hi = b["ci"]
            out.append(f'<line x1="{x(lo):.1f}" y1="{y}" x2="{x(hi):.1f}" y2="{y}" stroke="var({arm_var(a)})" stroke-width="2" stroke-linecap="round"/>')
            out.append(
                f'<circle cx="{x(b["rate"]):.1f}" cy="{y}" r="5" fill="var({arm_var(a)})" stroke="var(--surface-1)" stroke-width="2">'
                f'<title>{esc(scenario)}, {esc(arm_label(a))}: {b["k"]}/{b["n"]} pooled; shown {pct(b["rate"])} (95% interval {pct(lo)} to {pct(hi)})</title></circle>'
            )
            out.append(f'<text x="{width - right + 8}" y="{y}" class="tick" dominant-baseline="middle">{b["k"]}/{b["n"]}</text>')
    out.append("</svg>")
    return legend([(arm_label(a), arm_var(a), "dot") for a in arms]) + "".join(out)


def scatter(decisions: list[dict[str, Any]], title: str) -> str:
    """Latency against queue length at decision; right and wrong decisions apart."""
    pts = [d for d in decisions if d.get("latency_s") is not None]
    if not pts:
        return ""
    width, height, left, bottom, top, right = 760, 300, 60, 46, 14, 20
    qmax = max(max(d["queue"] for d in pts), 4)
    lmax = max(d["latency_s"] for d in pts)
    lmax = max(10.0, (int(lmax / 20) + 1) * 20)
    pw, ph = width - left - right, height - top - bottom
    x = lambda q: left + (q - 0.5) / (qmax) * pw  # noqa: E731
    y = lambda v: top + ph - v / lmax * ph  # noqa: E731
    out = [f'<svg class="chart" viewBox="0 0 {width} {height}" role="img" aria-label="{esc(title)}">']
    step = 20 if lmax > 60 else 10
    for v in range(0, int(lmax) + 1, step):
        out.append(f'<line x1="{left}" y1="{y(v):.1f}" x2="{width - right}" y2="{y(v):.1f}" class="grid"/>')
        out.append(f'<text x="{left - 8}" y="{y(v):.1f}" class="tick" text-anchor="end" dominant-baseline="middle">{v}</text>')
    for q in range(1, qmax + 1):
        out.append(f'<text x="{x(q):.1f}" y="{height - bottom + 16}" class="tick" text-anchor="middle">{q}</text>')
    out.append(f'<text x="{left + pw / 2:.1f}" y="{height - 6}" class="axis-label" text-anchor="middle">cards pending when the decision was made</text>')
    out.append(f'<text x="14" y="{top + ph / 2:.1f}" class="axis-label" text-anchor="middle" transform="rotate(-90 14 {top + ph / 2:.1f})">seconds in the inbox</text>')
    for i, d in enumerate(pts):
        jitter = ((i * 37) % 11 - 5) / 22.0
        cx, cy = x(d["queue"] + jitter), y(d["latency_s"])
        tip = f'intent {d["intent_id"]} ({d.get("arm", "")}, {d.get("scenario", "")}): {d["action"]}, label {d.get("label") or "none"}, {d["latency_s"]:.0f} s, queue {d["queue"]}, tracks looked up {d.get("tracks_viewed", 0)}'
        if d.get("wrong"):
            out.append(
                f'<g><path d="M{cx - 4:.1f} {cy - 4:.1f} L{cx + 4:.1f} {cy + 4:.1f} M{cx + 4:.1f} {cy - 4:.1f} L{cx - 4:.1f} {cy + 4:.1f}" stroke="var(--wrong)" stroke-width="2.5" stroke-linecap="round"/>'
                f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="7" fill="transparent"><title>{esc(tip)}</title></circle></g>'
            )
        else:
            fill = "var(--right)" if d.get("wrong") is False else "var(--text-muted)"
            out.append(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="4" fill="{fill}" stroke="var(--surface-1)" stroke-width="2"><title>{esc(tip)}</title></circle>')
    out.append("</svg>")
    return legend([("right by the label", "--right", "dot"), ("wrong by the label", "--wrong", "cross"), ("not labelled", "--text-muted", "dot")]) + "".join(out)


# ---- the page --------------------------------------------------------------------------------

CSS = """
.viz-root{color-scheme:light;--surface-1:#fcfcfb;--page:#f9f9f7;--text-primary:#0b0b0b;--text-secondary:#52514e;
--text-muted:#898781;--grid:#e1e0d9;--axis:#c3c2b7;--border:rgba(11,11,11,.10);--right:%(right_l)s;--wrong:%(wrong_l)s;%(arms_l)s}
@media (prefers-color-scheme: dark){:root:where(:not([data-theme="light"])) .viz-root{color-scheme:dark;--surface-1:#1a1a19;
--page:#0d0d0d;--text-primary:#fff;--text-secondary:#c3c2b7;--text-muted:#898781;--grid:#2c2c2a;--axis:#383835;
--border:rgba(255,255,255,.10);--right:%(right_d)s;--wrong:%(wrong_d)s;%(arms_d)s}}
:root[data-theme="dark"] .viz-root{color-scheme:dark;--surface-1:#1a1a19;--page:#0d0d0d;--text-primary:#fff;
--text-secondary:#c3c2b7;--text-muted:#898781;--grid:#2c2c2a;--axis:#383835;--border:rgba(255,255,255,.10);
--right:%(right_d)s;--wrong:%(wrong_d)s;%(arms_d)s}
html,body{margin:0}
body.viz-root{background:var(--page);color:var(--text-primary);font:15px/1.5 system-ui,-apple-system,"Segoe UI",sans-serif}
main{max-width:1200px;margin:0 auto;padding:24px 16px 64px}
h1{font-size:26px;margin:0 0 4px}h2{font-size:20px;margin:40px 0 8px;padding-top:8px;border-top:1px solid var(--border)}
h3{font-size:16px;margin:24px 0 6px}p,li{color:var(--text-secondary);max-width:80ch}
.note{background:var(--surface-1);border:1px solid var(--border);border-radius:8px;padding:12px 16px;margin:12px 0}
.note strong{color:var(--text-primary)}
.card{background:var(--surface-1);border:1px solid var(--border);border-radius:10px;padding:16px;margin:12px 0;overflow-x:auto}
table{border-collapse:collapse;width:100%%;font-size:13px}
th,td{text-align:left;padding:5px 8px;border-bottom:1px solid var(--grid);vertical-align:top}
th{color:var(--text-secondary);font-weight:600}td.n{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}
.muted{color:var(--text-muted)}.bad{font-weight:600}
code{font-size:12.5px}
svg.chart{width:100%%;height:auto;display:block}
svg .grid{stroke:var(--grid);stroke-width:1}svg .tick{fill:var(--text-muted);font-size:11px}
svg .row-label{fill:var(--text-secondary);font-size:12px}svg .axis-label{fill:var(--text-secondary);font-size:12px}
.legend{display:flex;flex-wrap:wrap;gap:14px;margin:4px 0 8px;font-size:13px;color:var(--text-secondary)}
.key{display:inline-flex;align-items:center;gap:6px}
.kpis{display:grid;grid-template-columns:repeat(auto-fit,minmax(170px,1fr));gap:12px;margin:16px 0}
.kpi{background:var(--surface-1);border:1px solid var(--border);border-radius:10px;padding:12px 14px}
.kpi .label{font-size:13px;color:var(--text-secondary)}.kpi .value{font-size:26px;font-weight:600}
.kpi .sub{font-size:12px;color:var(--text-muted)}
details summary{cursor:pointer;color:var(--text-secondary)}
"""


def css() -> str:
    def arms(i: int) -> str:
        return "".join(f"{arm_var(a)}:{c[i]};" for a, c in ARM_COLOR.items())

    return CSS % {"right_l": RIGHT[0], "wrong_l": WRONG[0], "right_d": RIGHT[1], "wrong_d": WRONG[1], "arms_l": arms(0), "arms_d": arms(1)}


def engage_section(suite: dict[str, Any]) -> str:
    """Phase E's scenario suite: designed ends, replays, forged logs, invariants, per
    scenario and track-noise level over seeds."""
    t = suite["summary"]["totals"]
    noises = sorted({row["noise"] for row in suite["summary"]["table"]})
    seeds = suite.get("seeds", [])
    out = ["<h2>Engagement scenarios (Phase E)</h2>"]
    out.append(
        f"<p>Every invariant of the engagement decision chain has a scenario in <code>{esc(suite.get('scenarios'))}</code> that tries "
        f"to break it. <code>murmur-scenarios</code> plays each against the real decision engine (deterministic for a file, a seed and "
        f"a noise level), replays its audit log and checks that a forged copy is caught; this harness then runs its own invariants on "
        f"every log, including the two-person rule read from the log alone: an engagement dispatched, or its effect released, without "
        f"two distinct signers with engagement authority (neither the proposer) on one frozen plan counts as a violation. Seeds "
        f"{esc(seeds)}; track noise {esc(', '.join(f'{n:g}' for n in noises))} m (1-sigma Gaussian on every track estimate, every second).</p>"
    )
    out.append(
        f"<p><strong>{t['violations']} invariant violations</strong> in {t['runs']} scenario runs ({t['released']} effects released, each "
        f"checked for two signers on one plan); {t['ok']}/{t['runs']} ended as designed; {t['replay_ok']}/{t['runs']} replayed; "
        f"{t['tamper_detected']}/{t['tamper_runs']} forged logs caught.</p>"
    )
    by_scenario: dict[str, dict[float, dict[str, Any]]] = defaultdict(dict)
    meta: dict[str, dict[str, Any]] = {}
    for row in suite["summary"]["table"]:
        by_scenario[row["scenario"]][row["noise"]] = row
        meta.setdefault(row["scenario"], row)
    head = "".join(f"<th>as designed, {n:g} m</th>" for n in noises)
    trs = []
    for sc in sorted(by_scenario):
        m = meta[sc]
        cells = []
        for n in noises:
            row = by_scenario[sc].get(n)
            if not row:
                cells.append("<td class='n muted'>-</td>")
                continue
            cls = "n" if row["ok"] == row["runs"] else "n bad"
            cells.append(f"<td class='{cls}'>{row['ok']}/{row['runs']}</td>")
        rows = list(by_scenario[sc].values())
        tamper_runs = sum(r["tamper_runs"] for r in rows)
        tamper = f"{sum(r['tamper_detected'] for r in rows)}/{tamper_runs}" if tamper_runs else "<span class='muted'>none</span>"
        viol = sum(r["violations"] for r in rows)
        released = sum(r["released"] for r in rows)
        trs.append(
            f"<tr><td><code>{esc(sc)}</code><br><span class='muted'>{esc(m.get('attack'))}</span></td><td>{esc(m.get('invariant'))}</td>"
            f"<td>{esc(m.get('expect'))}</td>{''.join(cells)}<td class='n'>{tamper}</td><td class='n'>{released}</td>"
            f"<td class='n{' bad' if viol else ''}'>{viol}</td></tr>"
        )
    out.append(
        '<div class="card"><table><thead><tr><th>scenario (attack)</th><th>invariant</th><th>designed end</th>'
        f"{head}<th>forged logs caught</th><th>effects released</th><th>invariant violations</th></tr></thead><tbody>"
        + "".join(trs)
        + "</tbody></table></div>"
    )
    return "".join(out)


def ci_text(b: dict[str, Any] | None) -> str:
    """Seed-clustered mean [t interval] (seeds with cases), then the pooled k/n [Wilson]."""
    if not b or not b.get("n"):
        return '<span class="muted">no cases</span>'
    out = ""
    if b.get("seed_mean") is not None:
        lo, hi = b["seed_ci"]
        out += f'{pct(b["seed_mean"])} <span class="muted">[{pct(lo)}, {pct(hi)}], {b["seeds_with_cases"]} seeds</span><br>'
    else:
        out += f'<span class="muted">{b["seeds_with_cases"]} seed with cases: no seed interval</span><br>'
    out += f'<span class="muted">pooled {b["k"]}/{b["n"]} = {pct(b["rate"])} [{pct(b["ci"][0])}, {pct(b["ci"][1])}]</span>'
    return out


def read_text(rs: dict[str, Any] | None) -> str:
    """A proxy arm's should-reject cards in the inbox as the model handled them."""
    if not rs:
        return ""
    return (
        f'<br><span class="muted">model: read {rs["read"]}/{rs["n"]} (mean P(read) {rs["p_read_mean"]:.2f}); '
        f'rule book fired on {rs["fired"]}/{rs["read"]} read</span>'
    )


def provenance_text(runs: list[dict[str, Any]], meta: dict[str, Any]) -> tuple[str, list[str]]:
    """The code the runs ran (from each run.json), and warnings when it differs between runs
    or the tree was dirty."""
    ids: dict[str, int] = defaultdict(int)
    bins: dict[str, set[str]] = defaultdict(set)
    for r in runs:
        p = r["info"].get("provenance")
        ids[code_id(p)] += 1
        for name, h in ((p or {}).get("binaries") or {}).items():
            bins[name].add((h or "missing")[:12])
    warnings = []
    if len(ids) > 1:
        warnings.append(f"the runs ran different code: {dict(ids)}")
    if any("dirty" in k for k in ids):
        changed = sorted({c for r in runs for c in ((r["info"].get("provenance") or {}).get("changed") or []) if not is_doc(c)})
        warnings.append(f"runs ran from a tree with uncommitted code changes: {', '.join(changed[:20])}")
    # (records written before the porcelain fix lost the first letter of the first path)
    docs_only = sorted({("d" + c if c.startswith("ocs/") else c) for r in runs for c in ((r["info"].get("provenance") or {}).get("changed") or []) if is_doc(c)})
    if docs_only:
        warnings.append(f"documentation was being edited during the batch (no code change): {', '.join(docs_only[:10])}")
    for name, hs in bins.items():
        if len(hs) > 1:
            warnings.append(f"{name} differed between runs: {sorted(hs)}")
    text = ", ".join(f"{k} ({v} runs)" for k, v in sorted(ids.items())) or code_id(meta.get("provenance"))
    hashes = "; ".join(f"{n} {sorted(h)[0]}" for n, h in sorted(bins.items()) if len(h) == 1)
    return f"code {esc(text)}{'; binaries sha256 ' + esc(hashes) if hashes else ''}", warnings


def per_seed(texts: dict[Any, str]) -> str:
    """One text when every seed says the same, else each seed's own (a grouped line must
    never show one seed's figure for all of them)."""
    vals = set(texts.values())
    if len(vals) == 1:
        return esc(next(iter(vals)))
    return "; ".join(f"seed {esc(k)}: {esc(v)}" for k, v in sorted(texts.items(), key=lambda kv: str(kv[0])))


def what_went_wrong(
    meta: dict[str, Any],
    runs: list[dict[str, Any]],
    excluded: list[dict[str, Any]],
    agg: dict[tuple[str, str], dict[str, Any]],
    provenance_warnings: list[str],
    suite: dict[str, Any] | None = None,
) -> str:
    items: list[str] = []
    for w in provenance_warnings:
        items.append(f'<li><span class="bad">Provenance</span>: {esc(w)}</li>')
    for x in excluded:
        items.append(f'<li><span class="bad">Run excluded from every number</span>: <code>{esc(x["rel"])}</code>: {esc(x["why"])}</li>')
    if suite:
        for e in suite["summary"]["totals"]["errors"]:
            items.append(f'<li><span class="bad">Engagement suite failed to run</span> (seed {esc(e["seed"])}, noise {esc(e["noise"])} m): {esc(e["error"])}</li>')
        for row in suite["summary"]["table"]:
            if row["violations"]:
                items.append(f'<li><span class="bad">Engagement invariant violated</span>: {esc(row["scenario"])} at {esc(row["noise"])} m noise: {row["violations"]} (see suite.json)</li>')
            if row["tamper_runs"] and row["tamper_detected"] < row["tamper_runs"]:
                items.append(f'<li><span class="bad">Forged log not caught</span>: {esc(row["scenario"])} ({esc(row["tamper"])}) at {esc(row["noise"])} m noise in {row["tamper_runs"] - row["tamper_detected"]} run(s)</li>')
            if row["ok"] < row["runs"]:
                seeds = sorted(f["seed"] for f in row["failures"])
                first = row["failures"][0]
                ends = ", ".join(f"{k} {v}" for k, v in sorted(row["final_states"].items(), key=lambda kv: str(kv[0])))
                items.append(
                    f'<li>Engagement scenario not as designed: <code>{esc(row["scenario"])}</code> at {esc(row["noise"])} m track noise, '
                    f'{row["runs"] - row["ok"]}/{row["runs"]} seeds ({esc(seeds)}); designed end {esc(row["expect"])}, ends: {esc(ends)}; '
                    f'first: {esc(first["failure"])}</li>'
                )
    # premises: the run did not show what the scenario says it shows, grouped by step
    prem: dict[tuple[str, str, str], dict[str, Any]] = {}
    for r in runs:
        for f in r["m"].get("premises", {}).get("failed", []):
            key = (r["info"].get("scenario"), r["info"].get("arm"), f["step"])
            g = prem.setdefault(key, {"seeds": [], "failed": set()})
            g["seeds"].append(r["info"].get("seed"))
            for x in f["failed"]:
                g["failed"].add(x.split(":")[0])
            g.setdefault("example", f["failed"])
    for (sc, arm, step), g in sorted(prem.items()):
        items.append(
            f'<li><span class="bad">Premise failed</span>: {esc(sc)} / {esc(arm)}, step <code>{esc(step)}</code>, seed(s) {esc(sorted(g["seeds"]))} '
            f'({esc(", ".join(sorted(g["failed"])))}); e.g. {esc("; ".join(g["example"]))}. The case is scored by what the run showed, not by the premise.</li>'
        )
    # the situation changed between the proposal and the decision (truth samples)
    changed: dict[tuple[str, str, str, str, str], list[Any]] = defaultdict(list)
    for r in runs:
        for c in r["m"].get("changed_before_decision", []):
            key = (r["info"].get("scenario"), r["info"].get("arm"), c["step"], f"{c['at_proposal']} to {c['at_decision']}", c["outcome"])
            changed[key].append((r["info"].get("seed"), c["why"]))
    for (sc, arm, step, change, oc), seeds in sorted(changed.items()):
        items.append(
            f'<li>Situation changed before the decision: {esc(sc)} / {esc(arm)}, step <code>{esc(step)}</code>, label {esc(change)}, '
            f'outcome {esc(oc)}, seed(s) {esc(sorted(x[0] for x in seeds))}: {per_seed({x[0]: x[1] for x in seeds})}. Scored by the label at proposal.</li>'
        )
    # clocks, warnings, storyline errors and retries
    for r in runs:
        for c in r["info"].get("clock_checks", []) or []:
            if not c.get("ok"):
                items.append(f'<li><span class="bad">Clock check failed</span> in <code>{esc(r["rel"])}</code>: {esc(c)}</li>')
        for w in r["info"].get("warnings", []):
            if not str(w).startswith("clock check failed"):
                items.append(f'<li>Warning in <code>{esc(r["rel"])}</code>: {esc(str(w)[:300])}</li>')
        for rec in r["story"]:
            if rec.get("kind") == "error":
                items.append(f'<li>Storyline step <code>{esc(rec.get("step"))}</code> failed in <code>{esc(r["rel"])}</code> at {esc(rec.get("t_rel"))} s: {esc(rec.get("error"))}</li>')
            if rec.get("kind") == "skipped" and rec.get("reason") not in ("run ended", "in this arm the agent makes its own proposals from the world model"):
                items.append(f'<li>Step <code>{esc(rec.get("step"))}</code> skipped in <code>{esc(r["rel"])}</code>: {esc(rec.get("reason"))}</li>')
        retries = sum(1 for rec in r["story"] if rec.get("kind") == "retry")
        if retries:
            items.append(f'<li><code>{esc(r["rel"])}</code>: {retries} resolution retries (a track was not held yet when the step was due); the step ran later than scripted.</li>')
    # flakiness: a step that produced an intent in some seeds and not in others
    by_group: dict[tuple[str, str], dict[str, set[int]]] = defaultdict(lambda: defaultdict(set))
    seeds: dict[tuple[str, str], set[int]] = defaultdict(set)
    for r in runs:
        key = (r["info"].get("scenario"), r["info"].get("arm"))
        seeds[key].add(r["info"].get("seed"))
        for rec in r["story"]:
            if rec.get("kind") == "propose" and (rec.get("intent_id") is not None or rec.get("declined")):
                by_group[key][rec.get("step")].add(r["info"].get("seed"))
    for key, steps in sorted(by_group.items()):
        if key[1] == "agent+human-proxy":
            continue  # the agent chooses its own proposals: a different set per seed is the design
        for step, s in sorted(steps.items()):
            missing = seeds[key] - s
            if missing:
                items.append(f'<li>Flaky: step <code>{esc(step)}</code> of {esc(key[0])} / {esc(key[1])} produced no intent in seed(s) {esc(sorted(missing))}.</li>')
    # outcome failures, grouped by scenario, arm and step, with the seeds
    groups: dict[tuple[str, str, str, str], dict[str, Any]] = {}
    for (scenario, arm), a in sorted(agg.items(), key=lambda kv: (kv[0][0], arm_key(kv[0][1]))):
        for r in a["runs"]:
            for p in r["m"].get("per_intent", []):
                label, oc = p.get("label"), p.get("outcome")
                what = None
                if label == "should_reject" and oc == "passed":
                    what = "Should-reject dispatched, " + ("the C2 showed it" if p.get("c2_shows_it") else "picture wrong")
                elif label == "should_void" and oc == "passed":
                    what = "Should-void dispatched: approved while the vehicle's link was cut (truth), and sent"
                elif label == "legitimate" and oc in ("rejected_in_inbox", "expired", "caught_by_rules", "caught_by_system"):
                    what = f"Legitimate intent stopped ({oc})" + (", the C2 showed something against it" if p.get("c2_shows_it") else "")
                elif label and oc in ("open", "superseded"):
                    what = f"Not scored ({oc})"
                if what is None:
                    continue
                g = groups.setdefault((what, scenario, arm, str(p.get("step"))), {"seeds": [], "why": {}, "reason": {}, "evidence": {}})
                seed = r["info"].get("seed")
                g["seeds"].append(seed)
                g["why"][seed] = p.get("label_basis") or ""
                g["reason"][seed] = p.get("decided_reason") or ""
                g["evidence"][seed] = "; ".join(p.get("evidence") or [])
    # the scripted baselines fail by construction (approve-all lets every wrong card through,
    # reject-all stops every right one): one line each, so the decider's failures stand out
    baseline = {"rules-only", "rules-only-reject"}
    by_construction: dict[tuple[str, str], int] = defaultdict(int)
    for (what, scenario, arm, step), g in groups.items():
        if arm in baseline and not what.startswith("Not scored"):
            by_construction[(arm, what)] += len(g["seeds"])
            continue
        if what.startswith("Should-reject"):
            detail = per_seed(g["why"]) + (f"; the C2 showed: {per_seed(g['evidence'])}" if any(g["evidence"].values()) else "")
        else:
            detail = per_seed({k: v or "(still pending when the run ended)" for k, v in g["reason"].items()})
        items.append(f'<li>{esc(what)}: {esc(scenario)} / {esc(arm)}, step <code>{esc(step)}</code>, seed(s) {esc(sorted(g["seeds"]))}: {detail}</li>')
    for (arm, what), n in sorted(by_construction.items()):
        items.append(f'<li class="muted">By construction, {esc(arm)}: {esc(what)} x {n} over all scenarios and seeds (see the per-scenario tables).</li>')
    for s in meta.get("skipped", []):
        items.append(f'<li>Scenario <code>{esc(s.get("scenario"))}</code> not run: {esc(s.get("reason"))}</li>')
    if not items:
        items.append("<li>Nothing: every run finished, every premise held, every step produced its intent, and no labelled proposal slipped through.</li>")
    return "<ul>" + "".join(items) + "</ul>"


def results_table(agg: dict[tuple[str, str], dict[str, Any]], scenarios: list[str], arms: list[str]) -> str:
    rows = []
    for s in scenarios:
        for a in arms:
            g = agg.get((s, a))
            if not g:
                continue
            proxy = a in PROXY_ARMS
            rows.append(
                f"<tr><td>{esc(s)}</td><td>{esc(arm_label(a))}</td><td class='n'>{len(g['runs'])}<br><span class='muted'>seeds {esc(g['seeds'])}</span></td>"
                f"<td class='n'>{ci_text(g['sr_shown'])}{read_text(g['read_shown']) if proxy else ''}</td>"
                f"<td class='n'>{ci_text(g['sr_wrong_picture'])}{read_text(g['read_wrong_picture']) if proxy else ''}</td>"
                f"<td class='n'>{ci_text(g['sv'])}<br><span class='muted'>stopped {g['sv_stopped']['k']}/{g['sv_stopped']['n']}</span></td>"
                f"<td class='n'>{ci_text(g['fr'])}<br><span class='muted'>without label-changed: {g['fr_kept']['k']}/{g['fr_kept']['n']}; "
                f"legitimate with the C2 against: {g['legit_c2_against']}</span></td>"
                f"<td class='n'>{num(g['disc_armed']['dprime'], 2)} / {num(g['disc_armed_kept']['dprime'], 2)}<br><span class='muted'>all cards {num(g['disc']['dprime'], 2)} / {num(g['disc_kept']['dprime'], 2)}; "
                f"armed {g['disc_armed']['hit']}/{g['disc_armed']['miss']}/{g['disc_armed']['false_alarm']}/{g['disc_armed']['correct_reject']}</span></td>"
                f"<td class='n'>{num(g['lat']['median'])} / {num(g['lat']['p90'])}<br><span class='muted'>n={g['lat']['n']}</span></td>"
                f"<td class='n'>{ci_text(g['goodput'])}<br><span class='muted'>expired {g['expired']}, max queue {esc(g['max_queue'])}</span></td>"
                f"<td class='n'>{num(g['tte_auto']['median_of_run_medians'])} / {num(g['tte_human']['median_of_run_medians'])}</td>"
                f"<td class='n'>{g['premise_checked'] - g['premise_failed']}/{g['premise_checked']}</td>"
                f"<td class='n'>{g['changed']}/{g['labelled']}</td>"
                f"<td class='n{' bad' if g['violations'] else ''}'>{g['violations']}</td><td class='n'>{g['replay_ok']}/{len(g['runs'])}</td></tr>"
            )
    if not rows:
        return ""
    return (
        '<div class="card"><table><thead><tr><th>scenario</th><th>arm</th><th>runs</th>'
        "<th>should-reject caught: the C2 showed it</th><th>should-reject caught: picture wrong</th>"
        "<th>should-void voided by the C2</th><th>legitimate rejected in the inbox</th>"
        "<th>d' armed: all / without label-changed <span class='muted'>hit/miss/FA/CR</span></th><th>latency median / p90 s</th>"
        "<th>decided in time, of all inbox cards</th><th>time to effect auto / human s</th><th>premises held</th>"
        "<th>label changed by the decision</th>"
        "<th>violations</th><th>replay ok</th></tr></thead><tbody>" + "".join(rows) + "</tbody></table></div>"
    )


def scenario_sections(agg: dict[tuple[str, str], dict[str, Any]], scenarios: list[str], arms: list[str], spec_by_name: dict[str, Any]) -> str:
    body = []
    for s in scenarios:
        spec = spec_by_name.get(s, {})
        body.append(f"<h3>{esc(spec.get('title', s))} <span class='muted'>({esc(s)}{', held out' if spec.get('heldout') else ''})</span></h3>")
        if spec.get("path"):
            body.append(
                f"<p class='muted'>Spec <code>{esc(spec.get('path'))}</code>, fleet <code>{esc(spec.get('fleet'))}</code>, contacts "
                f"<code>{esc(spec.get('contacts'))}</code>, policy <code>{esc(spec.get('policy'))}</code>, events <code>{esc(spec.get('events'))}</code>, "
                f"{esc(spec.get('duration_s'))} s after the origin.</p>"
            )
        steps: dict[str, dict[str, dict[str, int]]] = defaultdict(lambda: defaultdict(lambda: defaultdict(int)))
        for a in arms:
            g = agg.get((s, a))
            if not g:
                continue
            for r in g["runs"]:
                for p in r["m"].get("per_intent", []):
                    if not p.get("step"):
                        continue
                    st = p["step"]
                    if a == "agent+human-proxy" and st.startswith("agent-"):
                        st = "agent's own"
                    shown = "" if p.get("c2_shows_it") is None else (", C2 showed it" if p.get("c2_shows_it") else ", C2 showed nothing")
                    key = f"{st} ({p.get('label')}{shown if p.get('label') != 'should_void' else ''})"
                    steps[key][a][p.get("outcome") or "?"] += 1
        if steps:
            hdr = "".join(f"<th>{esc(arm_label(a))}</th>" for a in arms if (s, a) in agg)
            trs = []
            for key in sorted(steps):
                cells = []
                for a in arms:
                    if (s, a) not in agg:
                        continue
                    c = steps[key].get(a, {})
                    cells.append("<td>" + (", ".join(f"{esc(o)} {n}" for o, n in sorted(c.items())) or "<span class='muted'>none</span>") + "</td>")
                trs.append(f"<tr><td><code>{esc(key)}</code></td>{''.join(cells)}</tr>")
            body.append(f'<div class="card"><table><thead><tr><th>step (label from truth, what the C2 showed)</th>{hdr}</tr></thead><tbody>{"".join(trs)}</tbody></table></div>')
        prs = []
        for a in arms:
            g = agg.get((s, a))
            if not g:
                continue
            for r in sorted(g["runs"], key=lambda r: r["info"].get("seed", 0)):
                m = r["m"]
                sr = m["should_reject"]
                prs.append(
                    f"<tr><td>{esc(arm_label(a))}</td><td class='n'>{esc(r['info'].get('seed'))}</td><td class='n'>{esc(r['info'].get('ready_at_s'))}</td>"
                    f"<td class='n'>{m['counts']['intents']}</td><td class='n'>{sr['c2_shows_it']['k']}/{sr['c2_shows_it']['n']}</td>"
                    f"<td class='n'>{sr['picture_wrong']['k']}/{sr['picture_wrong']['n']}</td>"
                    f"<td class='n'>{m['should_void']['k']}/{m['should_void']['n']}</td><td class='n'>{m['counts']['human_decisions']}</td>"
                    f"<td class='n'>{len(m.get('premises', {}).get('failed', []))}</td>"
                    f"<td class='n'>{m['invariants']['violations']}</td><td><code>{esc(r['rel'])}</code></td></tr>"
                )
        body.append(
            '<details><summary>Runs</summary><div class="card"><table><thead><tr><th>arm</th><th>seed</th><th>fleet ready s</th><th>intents</th>'
            "<th>caught, C2 showed it</th><th>caught, picture wrong</th><th>voided</th><th>inbox decisions</th><th>premises failed</th>"
            "<th>violations</th><th>directory</th></tr></thead><tbody>" + "".join(prs) + "</tbody></table></div></details>"
        )
    return "".join(body)


AGENT_ARM = "agent+human-proxy"


def _range(xs: list[float | None], digits: int = 2) -> str:
    v = [x for x in xs if x is not None]
    if not v:
        return "n/a"
    lo, hi = min(v), max(v)
    return f"{lo:.{digits}f}" if lo == hi else f"{lo:.{digits}f} to {hi:.{digits}f}"


def _counts(xs: list[Any]) -> str:
    c: dict[str, int] = defaultdict(int)
    for x in xs:
        c[str(x)] += 1
    return ", ".join(f"{esc(k)} {v}" for k, v in sorted(c.items()))


def agent_cases(agg: dict[tuple[str, str], dict[str, Any]]) -> list[dict[str, Any]]:
    """Every proposal the agent arm made, with its run's scenario and seed and the model's
    handling of it (from the run's decision records)."""
    out = []
    for (s, a), g in agg.items():
        if a != AGENT_ARM:
            continue
        for r in g["runs"]:
            dec = {d.get("intent_id"): d for d in r["info"].get("decisions", []) if "diligent" in d}
            for p in r["m"].get("per_intent", []):
                if not str(p.get("step") or "").startswith("agent-"):
                    continue
                d = dec.get(p.get("id")) or {}
                out.append({**p, "scenario": s, "seed": r["info"].get("seed"), "read": d.get("diligent"), "if_read": d.get("if_read")})
    return out


def agent_section(agg: dict[tuple[str, str], dict[str, Any]], spec_by_name: dict[str, Any]) -> str:
    """The agent arm, presented as what it is: one fixed policy that ignores the storyline,
    so the scenarios sharing a picture replicate one situation."""
    cases = agent_cases(agg)
    if not cases:
        return ""
    by_scenario: dict[str, set[str]] = defaultdict(set)
    for c in cases:
        by_scenario[c["scenario"]].add(str(c.get("target_contact")))
    # a situation is the set of contacts the agent proposed on: scenarios with the same set
    # are replicates of it
    situations: dict[frozenset, list[str]] = defaultdict(list)
    for s, contacts in by_scenario.items():
        situations[frozenset(contacts)].append(s)
    out = ["<h2>The agent arm: two situations, replicated</h2>"]
    out.append(
        "<p>The agent arm's proposer (eval/agent.py <code>AgentPolicy</code>) ignores the storyline: every 15 s from 20 s it "
        "proposes an armed orbit over the most suspicious live track near the bridge it has not proposed on yet. Scenarios that "
        "share a picture therefore give it the same tracks at the same times, and its rows are replicates of the situations "
        "below, not separate scenarios. Its cases are not comparable with the storyline arms' cases either: they are early "
        "proposals on tracks whose evidence changes before the decision, where the storyline's catchable cases keep theirs.</p>"
    )
    shown = [c for c in cases if c.get("label") == "should_reject" and c.get("c2_shows_it")]
    if shown:
        read = [c for c in shown if c.get("read")]
        fired = [c for c in read if c.get("if_read") == "reject"]
        below = [c for c in shown if c.get("target_conf_at_decision") is not None and c["target_conf_at_decision"] < 0.5]
        per = []
        for contacts, scs in sorted(situations.items(), key=lambda kv: sorted(kv[0])):
            xs = [c for c in shown if c["scenario"] in scs]
            if xs:
                per.append(f"{len(xs)} on {esc(', '.join(sorted({str(c.get('target_contact')) for c in xs})))}, decided "
                           f"{_range([c.get('decided_after_s') for c in xs], 1)} s after the proposal")
        out.append(
            f"<p>Every wrong agent proposal the C2 showed something against ({len(shown)} cases: {'; '.join(per)}) was shown with "
            f"an affiliation confidence of {_range([c.get('target_conf_at_proposal') for c in shown])} at proposal, under the rule "
            f"book's 0.5, and the low confidence was the only evidence. By the decision the sensors had firmed it up: "
            f"{_range([c.get('target_conf_at_decision') for c in shown])} in the last truth sample before the decision (taken "
            f"every 5 s, so up to 5 s before it; {len(below)} of {len(shown)} still under 0.5 there). The model's rule book fired "
            f"on {len(fired)} of the {len(read)} it read, so when it read the tracks the confidence was 0.5 or more: the "
            "evidence the category rests on was gone by the time the decider looked.</p>"
        )
    for contacts, scs in sorted(situations.items(), key=lambda kv: sorted(kv[0])):
        scs = sorted(scs)
        cs = [c for c in cases if c["scenario"] in scs]
        runs = sum(len(agg[(s, AGENT_ARM)]["runs"]) for s in scs)
        out.append(f"<h3>Proposals on {esc(', '.join(sorted(contacts)))}: {len(scs)} scenarios, {runs} runs</h3>")
        diffs = []
        for s in scs:
            spec = spec_by_name.get(s) or {}
            links = any(st.get("kind") == "link" for st in spec.get("steps", []))
            diffs.append(
                f"<code>{esc(s)}</code> (fleet {esc(Path(str(spec.get('fleet') or '?')).stem)}, policy {esc(Path(str(spec.get('policy') or '?')).stem)}"
                f"{', events ' + esc(Path(str(spec.get('events'))).stem) if spec.get('events') else ''}{', link cuts' if links else ''})"
            )
        out.append("<p class='muted'>Replicates: " + "; ".join(diffs) + ".</p>")
        steps: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for c in cs:
            steps[c["step"]].append(c)
        trs = []
        for step, xs in sorted(steps.items()):
            read = [x for x in xs if x.get("read")]
            trs.append(
                f"<tr><td><code>{esc(step)}</code></td><td>{_counts([x.get('target_contact') for x in xs])}</td>"
                f"<td class='n'>{_range([x.get('proposed_at_s') for x in xs], 0)}</td><td>{_counts([x.get('label') for x in xs])}</td>"
                f"<td>{_counts(['shown' if x.get('c2_shows_it') else 'nothing' for x in xs])}</td>"
                f"<td class='n'>{_range([x.get('target_conf_at_proposal') for x in xs])}</td><td class='n'>{_range([x.get('target_conf_at_decision') for x in xs])}</td>"
                f"<td class='n'>{_range([x.get('decided_after_s') for x in xs], 1)}</td>"
                f"<td class='n'>{len(read)}/{len(xs)} read, fired {sum(1 for x in read if x.get('if_read') == 'reject')}</td>"
                f"<td>{_counts([x.get('outcome') for x in xs])}</td>"
                f"<td class='n'>{sum(1 for x in xs if x.get('changed_before_decision'))}</td></tr>"
            )
        out.append(
            '<div class="card"><table><thead><tr><th>step</th><th>target contact</th><th>proposed at s</th><th>label (truth at proposal)</th>'
            "<th>what the C2 showed at proposal</th><th>target's affiliation confidence at proposal</th><th>at the decision</th>"
            "<th>decided after s</th><th>model</th><th>outcome</th><th>label changed by the decision</th></tr></thead><tbody>"
            + "".join(trs)
            + "</tbody></table></div>"
        )
        if any(s == "heldout-friend-alongside" for s in scs):
            times = [c.get("proposed_at_s") for c in cs if c["scenario"] == "heldout-friend-alongside"]
            out.append(
                f"<p class='muted'>In <code>heldout-friend-alongside</code> the agent proposes at {_range(times, 0)} s after the origin; the "
                "police boat is alongside the raider only from about 150 s, so the agent arm never meets the situation that scenario "
                "is about.</p>"
            )
    return "".join(out)


def build(batch: Path) -> Path:
    batch = Path(batch)
    meta, runs, excluded = collect(batch)
    agg = aggregate(runs)
    spec_by_name = {s["name"]: s for s in meta.get("scenarios", [])}
    for r in runs:
        spec_by_name.setdefault(r["info"].get("scenario"), r["info"].get("spec") or {})
    scenarios_all = sorted({k[0] for k in agg})
    heldout = [s for s in scenarios_all if (spec_by_name.get(s) or {}).get("heldout")]
    dev = [s for s in scenarios_all if s not in heldout]
    arms = sorted({k[1] for k in agg}, key=arm_key)
    total_viol = sum(a["violations"] for a in agg.values())
    proxy = next((r["info"].get("proxy") for r in runs if r["info"].get("proxy")), None)
    suite = load_suite(batch)
    prov_text, prov_warn = provenance_text(runs, meta)
    # the code computing this page's numbers from the runs' raw logs (may be newer than the
    # code the runs ran, which is what prov_text says)
    metrics_code = code_id(current_code())
    planned = len(meta.get("jobs", [])) or (len(runs) + len(excluded))

    body: list[str] = []
    body.append(f"<h1>Murmur evaluation: {esc(meta.get('id', batch.name))}</h1>")
    if not runs and not excluded:
        body.append(f'<p class="muted">The engagement scenario suite only; started {esc(meta.get("started"))}; {esc(code_id(meta.get("provenance")))}.</p>')
    else:
        ready = [r["info"].get("ready_at_s") for r in runs if r["info"].get("ready_at_s") is not None]
        body.append(
            f'<p class="muted">{planned} runs planned, {len(runs)} scored, {len(excluded)} excluded (listed under What went wrong); '
            f'{len(scenarios_all)} scenarios ({len(heldout)} held out), arms {esc(", ".join(arms))}, seeds {esc(meta.get("seeds"))}; '
            f'started {esc(meta.get("started"))}, ended {esc(meta.get("ended"))}; runs: {prov_text}; metrics computed by {esc(metrics_code)}; '
            f'{esc(meta.get("parallel"))} stacks in parallel on {esc((meta.get("machine") or {}).get("cpus"))} CPUs; {esc(meta.get("tidewater"))}; '
            f'fleet ready {num(min(ready), 1) if ready else "n/a"} to {num(max(ready), 1) if ready else "n/a"} s after the stand-ins started, before every origin.</p>'
        )
    for note in meta.get("notes", []):
        body.append(f'<p class="muted">{esc(note)}</p>')
    auth_modes: dict[str, int] = defaultdict(int)
    for r in runs:
        auth_modes[str((r["m"].get("auth") or {}).get("mode", "none recorded"))] += 1
    if auth_modes:
        body.append(
            '<p class="muted">Operator identities: '
            + esc(", ".join(f"{n} run(s) {mode}" for mode, n in sorted(auth_modes.items())))
            + ". A dev run names its operators by request header (MURMUR_AUTH=dev): its deciders are scripts, and it is not "
            "evidence about people. A study run counts only if its operators signed in with their own credentials (rules 8) "
            "and its log replayed.</p>"
        )
    body.append(
        '<div class="note"><strong>Not human data.</strong> ' + esc(NOT_HUMAN) + " The proxy operator is a model with explicit "
        f"parameters (eval/proxy.py, listed at the end). It reads a card with probability P(read) = diligence x fatigue "
        "(fatigue halves it every <code>fatigue_queue</code> cards in the queue), and when it reads, it applies the generic rule "
        f"book of eval/evidence.py ({esc(RULEBOOK_VERSION)}), the same one the metrics use to sort wrong proposals; when it does "
        "not read, it approves. So, by construction, on a wrong card the C2 shows, it catches about P(read) of them (when the rule "
        "fires on what it sees at decision time), and on a wrong card the C2 shows nothing against, it catches none. Its rates "
        "here are outputs of that model at those parameters; their intervals are Monte Carlo variation of the model across "
        "seeds, not estimates about people. A claim about human judgment needs participants (study mode, eval/README.md). "
        "Labels come from simulation truth that no decider sees (eval/METRICS.md).</div>"
    )
    kpis = [f'<div class="kpi"><div class="label">Invariant violations</div><div class="value">{total_viol}</div><div class="sub">must be zero; {len(runs)} scored runs</div></div>']
    if suite:
        t = suite["summary"]["totals"]
        kpis.append(
            f'<div class="kpi"><div class="label">Engagement scenarios: invariant violations</div><div class="value">{t["violations"]}</div>'
            f'<div class="sub">{t["runs"]} scenario runs, {t["released"]} effects released; must be zero</div></div>'
        )
        kpis.append(
            f'<div class="kpi"><div class="label">Engagement scenarios ended as designed</div><div class="value">{t["ok"]}/{t["runs"]}</div>'
            f'<div class="sub">{t["tamper_detected"]}/{t["tamper_runs"]} forged logs caught, {t["replay_ok"]} replayed</div></div>'
        )
    if runs:
        prem_checked = sum(g["premise_checked"] for g in agg.values())
        prem_failed = sum(g["premise_failed"] for g in agg.values())
        kpis.append(
            f'<div class="kpi"><div class="label">Scenario premises held</div><div class="value">{prem_checked - prem_failed}/{prem_checked}</div>'
            f'<div class="sub">what each step says the run shows, checked against what it recorded</div></div>'
        )
        kpis.append(f'<div class="kpi"><div class="label">Runs excluded</div><div class="value">{len(excluded)}</div><div class="sub">failed, timed out or missing; never pooled</div></div>')
    body.append('<div class="kpis">' + "".join(kpis) + "</div>")

    if runs:
        body.append("<h2>Results by scenario and arm</h2>")
        body.append(
            "<p>Each cell: the mean of the per-seed rates with a Student t 95% interval over the seeds that had cases (primary: "
            "proposals within one run share a stack, a picture and a decider, so they are not independent), then the pooled count "
            "with its Wilson 95% interval (secondary; when every seed gives the same rate the t interval has zero width, and this is the one to read). Wrong proposals are split by what the C2 showed when they were made "
            "(the rule book on the recorded card and tracks): <em>the C2 showed it</em> (catchable from the picture) or "
            "<em>picture wrong</em> (nothing the C2 showed spoke against it); the two are never pooled. Caught: rejected or voided "
            "before dispatch, or declined by the proposer. For the proxy arms, the line under it says how the model handled those "
            "cards: how many it read, and on how many its rule book fired. Should-void: voided by the C2 itself, of the proposals "
            "decided while the tasked vehicle's link was cut (truth). Legitimate rejected: of the legitimate proposals decided in "
            "the inbox, by the arm's decider (see the arms below), with the count of legitimate proposals the C2 showed something "
            "against. d': SandTable's trust d', on armed cards and on all cards decided in the inbox (0 for reject-all or "
            "approve-all). Decided in time: SandTable's goodput, cards approved or rejected of every card that reached the inbox. "
            "Time to effect: the median over runs of each run's median seconds from proposal to dispatch (auto tier / "
            "legitimate inbox cards). Premises: the run showed what each step's premise says. Label changed by the decision: "
            "labelled proposals whose label, recomputed with truth when they were decided, differs from the one at proposal "
            "(the scored one); each is listed under What went wrong.</p>"
        )
        body.append("<p>Deciders: " + "; ".join(f"<strong>{esc(a)}</strong>: {esc(DECIDER.get(a, '?'))}" for a in arms) + ". "
                    "The agent arm is not in these tables: it ignores the storyline, so its rows would be replicates of two "
                    "situations; it has its own section below. Legitimate rejected and d' are shown with and without the "
                    "proposals whose label changed before the decision (a rejection that was right by truth at the decision is "
                    "scored a false alarm against the label at proposal).</p>")
        body.append("<h3>Development scenarios</h3>")
        main_arms = [a for a in arms if a != AGENT_ARM]
        body.append(results_table(agg, dev, main_arms))
        if heldout:
            body.append("<h3>Held-out scenarios</h3>")
            body.append(
                "<p>The rule book (eval/evidence.py) was committed before these scenarios were written and was not changed for "
                "them. But they were designed knowing the rule book, each around one rule it has (a protected track inside the "
                "radius, a low affiliation confidence) or around a picture-wrong premise, and they were adjusted after the freeze "
                "(the dark ferry went from a 9 m/s transit, which a fixed aim point would lose and the no-track-at-aim rule would "
                "catch, to a drift; the police boat moved to its own contacts file; the early orbits of friend-alongside moved "
                "earlier). Their results restate their premises times the model's P(read). They are not a test of how the rule "
                "book generalises.</p>"
            )
            body.append(results_table(agg, heldout, main_arms))

        for title, key, label in (
            ("Should-reject caught, the C2 showed it", "sr_shown", "caught before dispatch: seed-mean and t interval where two seeds had cases, else pooled with Wilson"),
            ("Should-reject caught, picture wrong", "sr_wrong_picture", "caught before dispatch: seed-mean and t interval where two seeds had cases, else pooled with Wilson"),
            ("Should-void voided by the C2", "sv", "voided by the C2 before dispatch"),
        ):
            rows = [(s, [(a, chart_block(agg[(s, a)][key])) for a in main_arms if (s, a) in agg]) for s in dev + heldout if any(agg.get((s, a), {}).get(key, {}).get("n") for a in main_arms)]
            if rows:
                body.append(f"<h3>{esc(title)}</h3>")
                body.append('<div class="card">' + rate_chart(rows, title, label) + "</div>")

        proxy_decisions = []
        for (s, a), g in agg.items():
            if a in PROXY_ARMS or a == "study":
                for d in g["decisions"]:
                    proxy_decisions.append({**d, "scenario": s, "arm": a})
        if proxy_decisions:
            body.append("<h3>Time in the inbox against the queue (an echo of the model's parameters)</h3>")
            body.append(
                "<p>Every inbox decision of the proxy arms: the cards pending when it was made, how long the card waited, and "
                "whether it was right by the label. For the proxy this is its own parameters coming back (looks every "
                "<code>attend_s</code>, reads faster and less often as the queue grows: <code>fatigue_queue</code>, "
                "<code>hurry</code>), shown to check that the harness measures them, not as a finding about fatigue.</p>"
            )
            body.append('<div class="card">' + scatter(proxy_decisions, "Decision latency against queue length") + "</div>")

        body.append(agent_section(agg, spec_by_name))

        body.append("<h2>Scenarios</h2>")
        body.append(scenario_sections(agg, dev + heldout, arms, spec_by_name))

        body.append("<h2>Invariants</h2>")
        body.append(
            "<p>Each must be zero in every run (METRICS.md, \"Invariants\"). Replay: <code>murmur-replay</code> recomputed every check and "
            "policy decision from the logged inputs and verified the hash chain, <code>operator.view</code> events included.</p>"
        )
        inv_names: list[str] = []
        for r in runs:
            for k in (r["m"].get("invariants") or {}).get("counts", {}):
                if k not in inv_names:
                    inv_names.append(k)
        irows = []
        for name in inv_names:
            cells = []
            for a in arms:
                v = sum(int((r["m"].get("invariants") or {}).get("counts", {}).get(name, 0)) for (s, aa), g in agg.items() if aa == a for r in g["runs"])
                cells.append(f"<td class='n{' bad' if v else ''}'>{v}</td>")
            irows.append(f"<tr><td><code>{esc(name)}</code></td>{''.join(cells)}</tr>")
        body.append(f'<div class="card"><table><thead><tr><th>invariant</th>{"".join(f"<th>{esc(a)}</th>" for a in arms)}</tr></thead><tbody>{"".join(irows)}</tbody></table></div>')

    if suite:
        body.append(engage_section(suite))

    body.append("<h2>What went wrong</h2>")
    body.append(
        "<p>Everything below is listed, not filtered: excluded runs, provenance mismatches, failed premises and clock checks, "
        "warnings, storyline steps that failed or were skipped, steps that behaved differently across seeds, and every labelled "
        "proposal the oversight got wrong.</p>"
    )
    body.append('<div class="card">' + what_went_wrong(meta, runs, excluded, agg, prov_warn, suite) + "</div>")
    body.append("<h3>Limits of this measurement</h3>")
    body.append(
        "<ul>"
        "<li>The decider in the proxy arms is a model (above): what it catches is P(read) times what its rule book sees, by "
        "construction. These numbers test the harness and the scenarios; they say nothing about people.</li>"
        "<li>Labels come from truth, at the moment each one is about: what a proposal targets (the contact's affiliation and "
        "presence, where it really is against the aim point and radius, protected contacts inside the radius, the tasked "
        "vehicle's real position) when it is proposed; whether the tasked vehicle's link was cut when it is decided. A scenario "
        "can only measure what it scripts, and its premises say what it scripts; a failed premise is listed above.</li>"
        "<li>\"The C2 showed it\" is the rule book applied to the card and the tracks the C2 held when the proposal was made. The "
        "proxy applies the same rule book when it decides, to the tracks then; the two can differ (a target that drifted, a "
        "track that went stale in between), and the model lines under the proxy rates show how often its rule fired.</li>"
        "<li>Small samples: a few labelled proposals per run and a few seeds. Read the intervals.</li>"
        "<li>One clock: every scenario's storyline, events and contacts start at one origin, a fixed time after the stand-ins "
        "start; a run whose fleet was not ready by then fails (listed). Seeds vary the sensing draws, the link losses, the "
        "contacts' motion jitter and the proxy's draws, not when situations happen.</li>"
        "<li>Stand-ins, not SITL; time to effect stops at dispatch (it excludes the vehicle getting there).</li>"
        "</ul>"
    )
    if proxy:
        prow = "".join(f"<tr><td><code>{esc(k)}</code></td><td>{esc(v)}</td></tr>" for k, v in proxy.items())
        body.append("<h2>Proxy operator model parameters</h2>")
        body.append(f'<div class="card"><table><tbody>{prow}</tbody></table></div>')
    body.append(f"<p class='muted'>Generated {dt.datetime.now(dt.timezone.utc).isoformat(timespec='seconds')} by eval/report.py from {esc(batch)}. Metric definitions: eval/METRICS.md.</p>")

    page = (
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'>"
        f"<title>Murmur evaluation report</title><style>{css()}</style></head>"
        f"<body class='viz-root'><main>{''.join(body)}</main></body></html>"
    )
    out = batch / "report.html"
    out.write_text(page)
    summary_doc = {
        "not_human_data": True,
        "note": NOT_HUMAN + " Proxy-arm rates are outputs of the proxy operator model (eval/proxy.py) at the parameters below; their intervals are Monte Carlo variation of that model across seeds.",
        "batch": meta.get("id", batch.name),
        "started": meta.get("started"),
        "ended": meta.get("ended"),
        "code": prov_text,
        "metrics_code": metrics_code,
        "provenance_warnings": prov_warn,
        "rulebook": RULEBOOK_VERSION,
        "proxy_params": proxy,
        "runs_planned": planned,
        "runs_scored": len(runs),
        "runs_excluded": [{"run": x["rel"], "why": x["why"]} for x in excluded],
        "heldout_scenarios": heldout,
        "intervals": "seed_mean and seed_ci: mean of per-seed rates with a Student t 95% interval (primary); rate and ci: pooled with a Wilson 95% interval (secondary)",
        "groups": {f"{s}|{a}": {k: v for k, v in g.items() if k not in ("runs", "decisions")} for (s, a), g in sorted(agg.items())},
    }
    (batch / "summary.json").write_text(json.dumps(summary_doc, indent=1, default=str))
    return out


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: python -m eval.report <results-dir>")
    print(build(Path(sys.argv[1])))


if __name__ == "__main__":
    main()
