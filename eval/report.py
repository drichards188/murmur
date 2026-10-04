"""The batch report: one self-contained HTML file (inline CSS and SVG, no scripts, no
network) from a results directory.

    python -m eval.report eval/results/<batch>        # writes <batch>/report.html

Metrics are recomputed from each run's raw logs (eval/metrics.py `compute_run`), so a fix to
a metric applies to old batches; the replay result recorded at run time is reused.
Rates are pooled over seeds with a Wilson 95% interval; where every seed has cases, the
mean of the per-seed rates is shown with a Student t 95% interval as well (METRICS.md,
"Intervals"). Nothing is published from here; the file stays in the results directory.
"""

from __future__ import annotations

import datetime as dt
import html
import json
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

from . import REPO
from .metrics import compute_run
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


# ---- collection ------------------------------------------------------------------------------


def find_runs(batch: Path) -> list[Path]:
    return sorted(p.parent for p in batch.glob("*/*/seed-*/run.json"))


def collect(batch: Path) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    meta: dict[str, Any] = {}
    if (batch / "batch.json").exists():
        try:
            meta = json.loads((batch / "batch.json").read_text())
        except ValueError:
            meta = {}
    runs = []
    for d in find_runs(batch):
        info = json.loads((d / "run.json").read_text())
        try:
            m = compute_run(d)
        except Exception as e:  # noqa: BLE001 (a broken run is reported, not fatal)
            m = {"error": repr(e)}
        story = []
        if (d / "storyline.jsonl").exists():
            for line in (d / "storyline.jsonl").read_text().splitlines():
                try:
                    story.append(json.loads(line))
                except ValueError:
                    continue
        runs.append({"dir": d, "rel": str(d.relative_to(batch)), "info": info, "m": m, "story": story})
    return meta, runs


def pooled(runs: list[dict[str, Any]], path: list[str]) -> dict[str, Any]:
    """Pool a {k, n} block over runs: Wilson on the sums, t-interval on per-run rates."""
    k = n = 0
    per_run = []
    for r in runs:
        b: Any = r["m"]
        for p in path:
            b = (b or {}).get(p) if isinstance(b, dict) else None
        if not isinstance(b, dict) or b.get("n") is None:
            continue
        k += int(b.get("k", 0))
        n += int(b.get("n", 0))
        if b.get("n"):
            per_run.append(b["k"] / b["n"])
    rate, lo, hi = wilson(k, n)
    mean, tlo, thi = t_interval(per_run) if len(per_run) >= 2 else (None, None, None)
    return {"k": k, "n": n, "rate": rate, "ci": [lo, hi], "seed_mean": mean, "seed_ci": [tlo, thi], "seeds_with_cases": len(per_run)}


def disc(runs: list[dict[str, Any]]) -> dict[str, Any]:
    """Discrimination pooled over runs: sum the four cells, then d' (stats.dprime)."""
    cells = {"hit": 0, "miss": 0, "false_alarm": 0, "correct_reject": 0}
    for r in runs:
        for k in cells:
            cells[k] += int((r["m"].get("discrimination") or {}).get(k, 0))
    return {**cells, "dprime": dprime(cells["hit"], cells["miss"], cells["false_alarm"], cells["correct_reject"])}


def aggregate(runs: list[dict[str, Any]]) -> dict[tuple[str, str], dict[str, Any]]:
    groups: dict[tuple[str, str], list[dict[str, Any]]] = defaultdict(list)
    for r in runs:
        groups[(r["info"].get("scenario", "?"), r["info"].get("arm", "?"))].append(r)
    out = {}
    for key, rs in groups.items():
        good = [r for r in rs if "error" not in r["m"]]
        lat = [d["latency_s"] for r in good for d in r["m"].get("decisions", []) if d.get("latency_s") is not None]
        tte_auto = [r["m"]["time_to_effect"]["auto"]["median"] for r in good if r["m"]["time_to_effect"]["auto"]["median"] is not None]
        tte_h = [r["m"]["time_to_effect"]["human_legitimate"]["median"] for r in good if r["m"]["time_to_effect"]["human_legitimate"]["median"] is not None]
        out[key] = {
            "runs": rs,
            "ok": sum(1 for r in rs if r["info"].get("status") == "ok"),
            "sr": pooled(good, ["should_reject"]),
            "sr_prevented": pooled(good, ["should_reject", "prevented"]),
            "sr_human": pooled(good, ["should_reject", "human"]),
            "sv": pooled(good, ["should_void"]),
            "sv_stopped": pooled(good, ["should_void", "stopped"]),
            "fr": pooled(good, ["legitimate", "false_reject"]),
            "lp": pooled(good, ["legitimate", "passed"]),
            "ov": pooled(good, ["override"]),
            "goodput": pooled(good, ["goodput"]),
            "disc": disc(good),
            "lat": summary(lat),
            "max_queue": max((r["m"]["load"]["max_queue"] for r in good), default=None),
            "expired": sum(r["m"]["load"]["expired"] for r in good),
            "violations": sum(r["m"]["invariants"]["violations"] for r in good),
            "replay_ok": sum(1 for r in good if (r["m"]["invariants"]["replay"] or {}).get("ok")),
            "tte_auto": summary(tte_auto)["median"],
            "tte_human": summary(tte_h)["median"],
            "decisions": [d for r in good for d in r["m"].get("decisions", [])],
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
                f'<title>{esc(scenario)}, {esc(a)}: {b["k"]}/{b["n"]} = {pct(b["rate"])} (95% CI {pct(lo)} to {pct(hi)})</title></circle>'
            )
            out.append(f'<text x="{width - right + 8}" y="{y}" class="tick" dominant-baseline="middle">{b["k"]}/{b["n"]}</text>')
    out.append("</svg>")
    return legend([(a, arm_var(a), "dot") for a in arms]) + "".join(out)


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


def ci_text(b: dict[str, Any], seeds: bool = False) -> str:
    """k/n = rate, the pooled Wilson interval, and (seeds) the mean of the per-seed rates
    with its t interval when at least two seeds had cases."""
    if not b or not b.get("n"):
        return '<span class="muted">no cases</span>'
    out = f'{b["k"]}/{b["n"]} = {pct(b["rate"])}<br><span class="muted">[{pct(b["ci"][0])}, {pct(b["ci"][1])}]</span>'
    if seeds and b.get("seed_mean") is not None:
        lo, hi = b["seed_ci"]
        out += f'<br><span class="muted">seeds {pct(b["seed_mean"])} [{pct(max(0.0, lo))}, {pct(min(1.0, hi))}]</span>'
    return out


def git_sha() -> str:
    try:
        return subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=REPO, capture_output=True, text=True, timeout=5).stdout.strip()
    except (OSError, subprocess.TimeoutExpired):
        return "?"


def what_went_wrong(meta: dict[str, Any], runs: list[dict[str, Any]], agg: dict[tuple[str, str], dict[str, Any]]) -> str:
    items: list[str] = []
    # runs that failed or warned
    for r in runs:
        info = r["info"]
        if info.get("status") != "ok":
            items.append(f'<li><span class="bad">Run failed</span>: <code>{esc(r["rel"])}</code>: {esc(info.get("status"))} {esc((info.get("error") or "")[:300])}</li>')
        for w in info.get("warnings", []):
            items.append(f'<li>Warning in <code>{esc(r["rel"])}</code>: {esc(str(w)[:300])}</li>')
        if "error" in r["m"]:
            items.append(f'<li><span class="bad">Metrics failed</span> for <code>{esc(r["rel"])}</code>: {esc(r["m"]["error"])}</li>')
    # storyline errors and retries
    for r in runs:
        for rec in r["story"]:
            if rec.get("kind") == "error":
                items.append(f'<li>Storyline step <code>{esc(rec.get("step"))}</code> failed in <code>{esc(r["rel"])}</code> at {esc(rec.get("t_rel"))} s: {esc(rec.get("error"))}</li>')
            if rec.get("kind") == "skipped" and rec.get("reason") != "run ended":
                items.append(f'<li>Step <code>{esc(rec.get("step"))}</code> skipped in <code>{esc(r["rel"])}</code>: {esc(rec.get("reason"))}</li>')
        retries = sum(1 for rec in r["story"] if rec.get("kind") == "retry")
        if retries:
            items.append(f'<li><code>{esc(r["rel"])}</code>: {retries} resolution retries (a track was not held yet when the step was due); the step ran later than scripted.</li>')
        ready = (r["info"].get("ready") or {})
        if ready and not ready.get("ready"):
            items.append(f'<li><code>{esc(r["rel"])}</code>: the fleet was not fully ready when the storyline started: {esc(ready.get("vehicles"))}</li>')
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
        for step, s in sorted(steps.items()):
            missing = seeds[key] - s
            if missing:
                items.append(f'<li>Flaky: step <code>{esc(step)}</code> of {esc(key[0])} / {esc(key[1])} produced no intent in seed(s) {esc(sorted(missing))}.</li>')
    # outcome failures: the oversight let something through, or stopped something it should
    # not have; grouped by scenario, arm and step, with the seeds it happened in
    groups: dict[tuple[str, str, str, str], dict[str, Any]] = {}
    kinds = {
        ("should_reject", "passed"): "Should-reject dispatched",
        ("should_void", "passed"): "Should-void dispatched (approved before the bridge noticed the lost link)",
    }
    for (scenario, arm), a in sorted(agg.items(), key=lambda kv: (kv[0][0], arm_key(kv[0][1]))):
        for r in a["runs"]:
            for p in r["m"].get("per_intent", []):
                label, oc = p.get("label"), p.get("outcome")
                what = kinds.get((label, oc))
                if what is None and label == "legitimate" and oc in ("rejected_in_inbox", "expired", "caught_by_rules", "caught_by_system"):
                    what = f"Legitimate intent stopped ({oc})"
                if what is None and label and oc in ("open", "superseded"):
                    what = f"Not scored ({oc})"
                if what is None:
                    continue
                g = groups.setdefault((what, scenario, arm, str(p.get("step"))), {"seeds": [], "why": p.get("label_basis") or "", "reason": p.get("decided_reason") or ""})
                g["seeds"].append(r["info"].get("seed"))
    # the scripted baselines fail by construction (approve-all lets every wrong card through,
    # reject-all stops every right one): one line each, so the decider's failures stand out
    baseline = {"rules-only", "rules-only-reject"}
    by_construction: dict[tuple[str, str], int] = defaultdict(int)
    for (what, scenario, arm, step), g in groups.items():
        if arm in baseline and not what.startswith("Not scored"):
            by_construction[(arm, what)] += len(g["seeds"])
            continue
        detail = esc(g["why"]) if what.startswith("Should-reject") else esc(g["reason"] or "(still pending when the run ended)")
        items.append(
            f'<li>{esc(what)}: {esc(scenario)} / {esc(arm)}, step <code>{esc(step)}</code>, seed(s) {esc(sorted(g["seeds"]))}: {detail}</li>'
        )
    for (arm, what), n in sorted(by_construction.items()):
        items.append(f'<li class="muted">By construction, {esc(arm)}: {esc(what)} x {n} over all scenarios and seeds (see the per-scenario tables).</li>')
    for s in meta.get("skipped", []):
        items.append(f'<li>Scenario <code>{esc(s.get("scenario"))}</code> not run: {esc(s.get("reason"))}</li>')
    if not items:
        items.append("<li>Nothing: every run finished, every step produced its intent, and no labelled proposal slipped through.</li>")
    return "<ul>" + "".join(items) + "</ul>"


def build(batch: Path) -> Path:
    batch = Path(batch)
    meta, runs = collect(batch)
    agg = aggregate(runs)
    scenarios = sorted({k[0] for k in agg})
    arms = sorted({k[1] for k in agg}, key=arm_key)
    spec_by_name = {s["name"]: s for s in meta.get("scenarios", [])}
    n_ok = sum(1 for r in runs if r["info"].get("status") == "ok")
    total_viol = sum(a["violations"] for a in agg.values())
    proxy = next((r["info"].get("proxy") for r in runs if r["info"].get("proxy")), None)

    body: list[str] = []
    body.append(f"<h1>Murmur evaluation: {esc(meta.get('id', batch.name))}</h1>")
    body.append(
        f'<p class="muted">{len(runs)} runs ({n_ok} ok), {len(scenarios)} scenarios, arms {esc(", ".join(arms))}, seeds {esc(meta.get("seeds"))}; '
        f'started {esc(meta.get("started"))}, ended {esc(meta.get("ended"))}; code {esc(git_sha())}; '
        f'{esc(meta.get("parallel"))} stacks in parallel on {esc((meta.get("machine") or {}).get("cpus"))} CPUs; {esc(meta.get("tidewater"))}.</p>'
    )
    for note in meta.get("notes", []):
        body.append(f'<p class="muted">{esc(note)}</p>')
    body.append(
        '<div class="note"><strong>The human-proxy is not a human.</strong> It is a scripted operator model with explicit '
        "parameters (eval/proxy.py, listed below), built to exercise the metrics end to end and to give the rules-only "
        "baseline something to be compared with. Nothing in this report is human data, and nothing here supports a claim "
        "about how people behave: that needs participants (study mode, eval/README.md). Labels come from simulation truth "
        "the operator never sees (METRICS.md).</div>"
    )
    # KPIs
    sr_all = {a: pooled([r for (s, aa), g in agg.items() if aa == a for r in g["runs"] if "error" not in r["m"]], ["should_reject"]) for a in arms}
    kpis = [f'<div class="kpi"><div class="label">Invariant violations</div><div class="value">{total_viol}</div><div class="sub">must be zero, all runs</div></div>']
    for a in arms:
        b = sr_all[a]
        kpis.append(
            f'<div class="kpi"><div class="label">Should-reject caught, {esc(a)}</div><div class="value">{pct(b["rate"])}</div>'
            f'<div class="sub">{b["k"]}/{b["n"]}, 95% CI {pct(b["ci"][0])} to {pct(b["ci"][1])}, all scenarios</div></div>'
        )
    body.append('<div class="kpis">' + "".join(kpis) + "</div>")

    # headline table
    body.append("<h2>Results by scenario and arm</h2>")
    body.append(
        "<p>Rates pooled over seeds, Wilson 95% interval in brackets; for the catch and void rates, also the mean of the "
        "per-seed rates with a Student t 95% interval (clipped to 0-100%) where two or more seeds had cases. Catch: should-reject proposals rejected or voided before effect "
        "(or declined by the proposer). Void: should-void proposals voided by the C2 itself; stopped: not dispatched at all. "
        "False reject: legitimate proposals a human rejected, of those a human decided. d': how well the decider tells "
        "should-reject from legitimate cards (SandTable's trust d', 0 for reject-all or approve-all; cells pooled over seeds). "
        "Latency: seconds from entering the inbox to the decision. Decided before expiry: SandTable's goodput. Time to effect: "
        "median seconds from proposal to dispatch (auto tier / legitimate human tier).</p>"
    )
    rows = []
    for s in scenarios:
        for a in arms:
            g = agg.get((s, a))
            if not g:
                continue
            rows.append(
                f"<tr><td>{esc(s)}</td><td>{esc(a)}</td><td class='n'>{g['ok']}/{len(g['runs'])}</td>"
                f"<td class='n'>{ci_text(g['sr'], True)}</td><td class='n'>{ci_text(g['sv'], True)}</td><td class='n'>{ci_text(g['sv_stopped'])}</td>"
                f"<td class='n'>{ci_text(g['fr'])}</td><td class='n'>{num(g['disc']['dprime'], 2)}<br><span class='muted'>{g['disc']['hit']}/{g['disc']['miss']}/{g['disc']['false_alarm']}/{g['disc']['correct_reject']}</span></td>"
                f"<td class='n'>{num(g['lat']['median'])} / {num(g['lat']['p90'])}<br><span class='muted'>n={g['lat']['n']}</span></td>"
                f"<td class='n'>{ci_text(g['goodput'])}</td>"
                f"<td class='n'>{esc(g['max_queue'])}</td><td class='n'>{g['expired']}</td>"
                f"<td class='n'>{num(g['tte_auto'])} / {num(g['tte_human'])}</td>"
                f"<td class='n'>{g['violations']}</td><td class='n'>{g['replay_ok']}/{len(g['runs'])}</td></tr>"
            )
    body.append(
        '<div class="card"><table><thead><tr><th>scenario</th><th>arm</th><th>runs ok</th><th>should-reject caught</th>'
        "<th>should-void voided</th><th>should-void stopped</th><th>false reject</th><th>d' <span class='muted'>hit/miss/FA/CR</span></th>"
        "<th>latency median / p90 s</th><th>decided before expiry</th><th>max queue</th>"
        "<th>expired</th><th>time to effect auto / human s</th><th>violations</th><th>replay ok</th></tr></thead><tbody>"
        + "".join(rows)
        + "</tbody></table></div>"
    )

    # charts
    sr_rows = [(s, [(a, agg[(s, a)]["sr"]) for a in arms if (s, a) in agg]) for s in scenarios if any(agg.get((s, a), {}).get("sr", {}).get("n") for a in arms)]
    if sr_rows:
        body.append("<h3>Should-reject catch rate</h3>")
        body.append('<div class="card">' + rate_chart(sr_rows, "Should-reject catch rate by scenario and arm", "caught before effect (95% Wilson interval)") + "</div>")
    sv_rows = [(s, [(a, agg[(s, a)]["sv"]) for a in arms if (s, a) in agg]) for s in scenarios if any(agg.get((s, a), {}).get("sv", {}).get("n") for a in arms)]
    if sv_rows:
        body.append("<h3>Correct-void rate</h3>")
        body.append('<div class="card">' + rate_chart(sv_rows, "Should-void voided by the C2 by scenario and arm", "voided by the C2 before dispatch (95% Wilson interval)") + "</div>")
    proxy_decisions = []
    for (s, a), g in agg.items():
        if a in ("human-proxy", "agent+human-proxy", "agent+model", "study"):
            for d in g["decisions"]:
                proxy_decisions.append({**d, "scenario": s, "arm": a})
    if proxy_decisions:
        body.append("<h3>Approval fatigue: time in the inbox against the queue</h3>")
        body.append(
            "<p>Every human (proxy) decision in the batch: how many cards were pending when it was made, how long the card waited, "
            "and whether the decision was right by the label. Hover a mark for the intent.</p>"
        )
        body.append('<div class="card">' + scatter(proxy_decisions, "Decision latency against queue length") + "</div>")
        # the queue table, per scenario where the queue actually varied: pooled over
        # scenarios, the queue length stands in for the scenario (the quiet scenarios hold
        # the uncatchable cases at queue 1), which would read as the opposite of fatigue
        lt = []
        loaded = sorted({d["scenario"] for d in proxy_decisions if d["queue"] >= 3})
        for sc in loaded:
            for a in arms:
                ds = [d for d in proxy_decisions if d["arm"] == a and d["scenario"] == sc]
                if not ds:
                    continue
                for name, (lo, hi) in {"1": (1, 1), "2-3": (2, 3), "4-6": (4, 6), "7+": (7, 10**9)}.items():
                    rs = [d for d in ds if lo <= d["queue"] <= hi]
                    if not rs:
                        continue
                    judged = [d for d in rs if d.get("wrong") is not None and d.get("armed")]
                    k = sum(1 for d in judged if d["wrong"])
                    rate, clo, chi = wilson(k, len(judged))
                    lt.append(
                        f"<tr><td>{esc(sc)}</td><td>{esc(a)}</td><td>{name}</td><td class='n'>{len(rs)}</td><td class='n'>{num(summary([d['latency_s'] for d in rs])['median'])}</td>"
                        f"<td class='n'>{num(summary([d['tracks_viewed'] for d in rs])['mean'], 2)}</td>"
                        f"<td class='n'>{k}/{len(judged)} = {pct(rate)} <span class='muted'>[{pct(clo)}, {pct(chi)}]</span></td></tr>"
                    )
        if lt:
            body.append(
                "<p>By queue length at the decision, within each scenario where the queue grew past two (pooling across "
                "scenarios would confound the queue with the scenario). Wrong: armed cards decided against their label.</p>"
            )
            body.append(
                '<div class="card"><table><thead><tr><th>scenario</th><th>arm</th><th>queue at decision</th><th>decisions</th><th>median latency s</th>'
                "<th>tracks looked up per decision</th><th>armed cards wrong by the label</th></tr></thead><tbody>" + "".join(lt) + "</tbody></table></div>"
            )
        # information shown
        it = []
        for a in arms:
            ds = [d for d in proxy_decisions if d["arm"] == a and d.get("latency_s") is not None and d.get("armed")]
            if not ds:
                continue
            for name, pred in (("looked up no track", lambda d: d["tracks_viewed"] == 0), ("looked up the cited track(s)", lambda d: d["tracks_viewed"] > 0)):
                rs = [d for d in ds if pred(d)]
                if not rs:
                    continue
                judged = [d for d in rs if d.get("wrong") is not None]
                k = sum(1 for d in judged if d["wrong"])
                om = summary([d["open_ms"] / 1000 for d in rs if d.get("open_ms") is not None])
                it.append(
                    f"<tr><td>{esc(a)}</td><td>{name}</td><td class='n'>{len(rs)}</td><td class='n'>{num(summary([d['latency_s'] for d in rs])['median'])}</td>"
                    f"<td class='n'>{num(om['median'])}</td><td class='n'>{num(summary([d['checks_shown'] for d in rs])['mean'])}</td>"
                    f"<td class='n'>{k}/{len(judged)}</td></tr>"
                )
        body.append("<h3>Approval time against the information shown</h3>")
        body.append(
            "<p>From the <code>operator.view</code> events: what the card showed (checks, warnings, rationale) and which tracks "
            "the decider opened, joined with the decision, for cards on armed platforms (the consequential ones). Card open time "
            "is the console's own measure (the proxy's reading time here); latency includes the time before the card was first "
            "looked at.</p>"
        )
        body.append(
            '<div class="card"><table><thead><tr><th>arm</th><th>information looked at</th><th>decisions</th><th>median latency s</th>'
            "<th>median card open s</th><th>checks on the card</th><th>wrong by the label</th></tr></thead><tbody>" + "".join(it) + "</tbody></table></div>"
        )

    # override
    ov_rows = []
    for s in scenarios:
        for a in arms:
            g = agg.get((s, a))
            if not g or not g["ov"]["n"]:
                continue
            withdrawn = sum(r["m"]["override"]["withdrawn"] for r in g["runs"] if "error" not in r["m"])
            agent_n = sum(r["m"]["override"]["agent_intents"] for r in g["runs"] if "error" not in r["m"])
            ov_rows.append(f"<tr><td>{esc(s)}</td><td>{esc(a)}</td><td class='n'>{agent_n}</td><td class='n'>{ci_text(g['ov'])}</td><td class='n'>{withdrawn}</td></tr>")
    if ov_rows:
        body.append("<h3>Override rate (agent proposals)</h3>")
        body.append('<div class="card"><table><thead><tr><th>scenario</th><th>arm</th><th>agent intents</th><th>rejected by a human, of those a human decided</th><th>withdrawn by the agent</th></tr></thead><tbody>' + "".join(ov_rows) + "</tbody></table></div>")

    # per scenario
    body.append("<h2>Scenarios</h2>")
    for s in scenarios:
        spec = spec_by_name.get(s, {})
        body.append(f"<h3>{esc(spec.get('title', s))} <span class='muted'>({esc(s)})</span></h3>")
        if spec.get("path"):
            body.append(f"<p class='muted'>Spec <code>{esc(spec.get('path'))}</code>, fleet <code>{esc(spec.get('fleet'))}</code>, policy <code>{esc(spec.get('policy'))}</code>, events <code>{esc(spec.get('events'))}</code>, {esc(spec.get('duration_s'))} s at rate {esc(spec.get('rate'))}.</p>")
        steps = defaultdict(lambda: defaultdict(lambda: defaultdict(int)))
        labels_of: dict[str, str] = {}
        for a in arms:
            g = agg.get((s, a))
            if not g:
                continue
            for r in g["runs"]:
                for p in r["m"].get("per_intent", []):
                    if p.get("step"):
                        st = p["step"].rsplit("-", 1)[0] if s == "approval-flood" and p["step"].startswith("flood-") else p["step"]
                        key = f"{st} ({p.get('label')})"
                        labels_of[key] = p.get("label")
                        steps[key][a][p.get("outcome") or "?"] += 1
        if steps:
            hdr = "".join(f"<th>{esc(a)}</th>" for a in arms if (s, a) in agg)
            trs = []
            for key in steps:
                cells = []
                for a in arms:
                    if (s, a) not in agg:
                        continue
                    c = steps[key].get(a, {})
                    cells.append("<td>" + (", ".join(f"{esc(o)} {n}" for o, n in sorted(c.items())) or "<span class='muted'>none</span>") + "</td>")
                trs.append(f"<tr><td><code>{esc(key)}</code></td>{''.join(cells)}</tr>")
            body.append(f'<div class="card"><table><thead><tr><th>step (label)</th>{hdr}</tr></thead><tbody>{"".join(trs)}</tbody></table></div>')
        # per run rows
        prs = []
        for a in arms:
            g = agg.get((s, a))
            if not g:
                continue
            for r in sorted(g["runs"], key=lambda r: r["info"].get("seed", 0)):
                m = r["m"]
                if "error" in m:
                    prs.append(f"<tr><td>{esc(a)}</td><td class='n'>{esc(r['info'].get('seed'))}</td><td colspan='6'>metrics failed: {esc(m['error'])}</td></tr>")
                    continue
                prs.append(
                    f"<tr><td>{esc(a)}</td><td class='n'>{esc(r['info'].get('seed'))}</td><td>{esc(r['info'].get('status'))}</td>"
                    f"<td class='n'>{m['counts']['intents']}</td><td class='n'>{m['should_reject']['k']}/{m['should_reject']['n']}</td>"
                    f"<td class='n'>{m['should_void']['k']}/{m['should_void']['n']}</td><td class='n'>{m['counts']['human_decisions']}</td>"
                    f"<td class='n'>{m['invariants']['violations']}</td><td><code>{esc(r['rel'])}</code></td></tr>"
                )
        body.append(
            '<details><summary>Runs</summary><div class="card"><table><thead><tr><th>arm</th><th>seed</th><th>status</th><th>intents</th>'
            "<th>caught</th><th>voided</th><th>human decisions</th><th>violations</th><th>directory</th></tr></thead><tbody>" + "".join(prs) + "</tbody></table></div></details>"
        )

    # invariants
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
    engage = sum((r["m"].get("invariants") or {}).get("engage_intents", 0) for r in runs)
    body.append(f"<p class='muted'>Engagement intents in this batch: {engage} (the two-approver invariant is vacuous until Phase E scenarios run; see eval/engage.py).</p>")

    # what went wrong
    body.append("<h2>What went wrong</h2>")
    body.append("<p>Everything below is listed, not filtered: failed runs, warnings, storyline steps that failed or were skipped, steps that behaved differently across seeds, and every labelled proposal the oversight got wrong.</p>")
    body.append('<div class="card">' + what_went_wrong(meta, runs, agg) + "</div>")
    body.append("<h3>Limits of this measurement</h3>")
    body.append(
        "<ul>"
        "<li>The supervisor in the proxy arms is a script (above). Its catch rate measures the information on the card and in the "
        "track picture against a fixed rule book, not human judgment.</li>"
        "<li>Labels are the scenario author's, applied by truth (contact affiliation, the stand-in navigation offset, the track's "
        "age at proposal). A scenario can only measure what it scripts.</li>"
        "<li>Small samples: a few labelled proposals per run, three seeds. The intervals are wide on purpose; read them.</li>"
        "<li>Stand-ins, not SITL; vehicle physics run at wall-clock speed whatever the scenario rate; time to effect stops at "
        "dispatch (it excludes the vehicle getting there).</li>"
        "<li>Seeds vary the sensing draws, the contacts' motion, the link losses and the proxy's random draws; the storyline's "
        "times are fixed, so seeds do not vary when situations happen.</li>"
        "</ul>"
    )
    # proxy parameters
    if proxy:
        prow = "".join(f"<tr><td><code>{esc(k)}</code></td><td>{esc(v)}</td></tr>" for k, v in proxy.items())
        body.append("<h2>Proxy operator parameters</h2>")
        body.append(f'<div class="card"><table><tbody>{prow}</tbody></table></div>')
    body.append(f"<p class='muted'>Generated {dt.datetime.now(dt.timezone.utc).isoformat(timespec='seconds')} by eval/report.py from {esc(batch)}. Metric definitions: eval/METRICS.md.</p>")

    page = (
        "<!doctype html><html lang='en'><head><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'>"
        f"<title>Murmur evaluation report</title><style>{css()}</style></head>"
        f"<body class='viz-root'><main>{''.join(body)}</main></body></html>"
    )
    out = batch / "report.html"
    out.write_text(page)
    (batch / "summary.json").write_text(
        json.dumps(
            {
                f"{s}|{a}": {k: v for k, v in g.items() if k not in ("runs", "decisions")}
                for (s, a), g in sorted(agg.items())
            },
            indent=1,
            default=str,
        )
    )
    return out


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: python -m eval.report <results-dir>")
    print(build(Path(sys.argv[1])))


if __name__ == "__main__":
    main()
