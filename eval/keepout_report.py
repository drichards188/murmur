"""The report's section for the keep-out measurement (eval/keepout.py): the primary variant
(the bridge's tracker) in full, the other variants beside it, and what the numbers can and
cannot carry. Model results from simulated geometry, unreviewed."""

from __future__ import annotations

import html
from typing import Any

from .keepout import BEHAVIOURS, COMPONENTS, SENS_TTE


def esc(x: Any) -> str:
    return html.escape("" if x is None else str(x))


def pct(r: dict[str, Any] | None, ci: bool = True) -> str:
    if not r or r.get("rate") is None:
        return "n/a"
    s = f"{100 * r['rate']:.0f}%"
    if ci and r.get("lo") is not None:
        s += f" <span class='muted'>[{100 * r['lo']:.0f}-{100 * r['hi']:.0f}]</span>"
    return s


def heat(r: dict[str, Any] | None, extra: str = "") -> str:
    """A table cell shaded by its block rate."""
    if not r or r.get("rate") is None:
        return "<td class='n'>n/a</td>"
    a = round(45 * r["rate"])
    return f"<td class='n' style='background:color-mix(in srgb, var(--wrong) {a}%, transparent)'>{pct(r)}{extra}</td>"


def miss(r: dict[str, Any] | None) -> str:
    """Passes that missed someone, with the 95% upper bound when none did."""
    if not r or not r.get("n"):
        return "n/a"
    if r["k"] == 0:
        ub = r.get("upper95_tries")
        return f"0 of {r['n']}" + (f" <span class='muted'>(&lt;{100 * ub:.2f}%)</span>" if ub is not None else "")
    return f"{r['k']} of {r['n']} <span class='muted'>({100 * r['rate']:.2f}%)</span>"


def bkey(b: str) -> int:
    return BEHAVIOURS.index(b) if b in BEHAVIOURS else len(BEHAVIOURS)


def table(head: str, rows: list[str]) -> str:
    return f"<div class='card'><table><thead>{head}</thead><tbody>{''.join(rows)}</tbody></table></div>"


def rate_tables(s: dict[str, Any]) -> str:
    """Block rate on tracks per rules version, and what perfect knowledge would block."""
    rows = s.get("tte") or []
    if not rows:
        return ""
    grid = sorted({r["tte_s"] for r in rows})
    behaviours = sorted({r["behaviour"] for r in rows}, key=bkey)
    densities = sorted({r["density"] for r in rows})
    cells = {(r["behaviour"], r["density"], r["tte_s"], r["rules"]): r for r in rows}
    out = []
    head = "<tr><th>behaviour</th><th>per km<sup>2</sup></th>" + "".join(f"<th class='n'>{t} s</th>" for t in grid) + "</tr>"
    for v in sorted({r["rules"] for r in rows}):
        trs = [
            f"<tr><td>{esc(b)}</td><td class='n'>{d:g}</td>" + "".join(heat((cells.get((b, d, t, v)) or {}).get("tracks")) for t in grid) + "</tr>"
            for b in behaviours
            for d in densities
        ]
        out.append(f"<p><strong>Rules {v}: blocked by the keep-out on tracks</strong></p>" + table(head, trs))
    v = max(r["rules"] for r in rows)
    trs = []
    for b in behaviours:
        for d in densities:
            tds = []
            for t in grid:
                c = cells.get((b, d, t, v)) or {}
                tds.append(f"<td class='n'>{pct(c.get('perfect'), False)} <span class='muted'>/ {pct(c.get('perfect_late'), False)} / {pct(c.get('perfect_circle'), False)}</span></td>")
            trs.append(f"<tr><td>{esc(b)}</td><td class='n'>{d:g}</td>{''.join(tds)}</tr>")
    out.append(
        "<p><strong>Perfect knowledge</strong>: the share of the same nominations with a protected contact truly inside the keep-out at the effect "
        "time (what a keep-out that knew where everyone would be would block), then over the 60 s release window, then inside the effect circle "
        "at the effect time.</p>" + table(head, trs)
    )
    return "".join(out)


def blocks_table(s: dict[str, Any]) -> str:
    head = (
        "<tr><th>behaviour</th><th>rules</th><th class='n'>blocks</th><th class='n'>needed</th><th class='n'>severe</th>"
        "<th class='n'>needed over the release window</th><th class='n'>conservative</th><th class='n'>passes</th><th class='n'>missed (keep-out)</th>"
        "<th class='n'>missed (circle)</th><th class='n'>missed over the release window (keep-out / circle)</th></tr>"
    )
    trs = []
    for r in s.get("blocks") or []:
        trs.append(
            f"<tr><td>{esc(r['behaviour'])}</td><td class='n'>{r['rules']}</td><td class='n'>{r['blocks']}</td><td class='n'>{pct(r['needed'])}</td>"
            f"<td class='n'>{pct(r['severe'])}</td><td class='n'>{pct(r['needed_late'])}</td><td class='n'>{pct(r['conservative'])}</td>"
            f"<td class='n'>{r['passes']}</td><td class='n'>{miss(r['missed'])}</td><td class='n'>{miss(r['missed_severe'])}</td>"
            f"<td class='n'>{r['missed_late']['k']} / {r['missed_severe_late']['k']}</td></tr>"
        )
    return table(head, trs)


def components_table(s: dict[str, Any]) -> str:
    head = "<tr><th>behaviour</th><th>rules</th><th class='n'>conservative</th>" + "".join(f"<th class='n'>{esc(c)}: deepest / alone</th>" for c in COMPONENTS) + "<th class='n'>without rules 9 and 12 regions</th></tr>"
    trs = []
    for r in s.get("blocks") or []:
        cbc = r["conservative_by_component"]
        tds = "".join(f"<td class='n'>{pct(cbc[c]['dominant'], False)} / {pct(cbc[c]['pass_without'], False)}</td>" for c in COMPONENTS)
        trs.append(f"<tr><td>{esc(r['behaviour'])}</td><td class='n'>{r['rules']}</td><td class='n'>{r['conservative']['k']}</td>{tds}<td class='n'>{pct(r['conservative_pass_without_rules_9_12'], False)}</td></tr>")
    return table(head, trs)


def thresholds_table(s: dict[str, Any]) -> str:
    p = s.get("params") or {}
    cruise = p.get("cruise_mps") or {}
    dispatch = float(p.get("dispatch_s") or 20.0)
    plats = sorted(cruise)
    head = (
        "<tr><th>behaviour</th><th>per km<sup>2</sup></th><th>rules</th><th class='n'>situations</th><th class='n'>50%</th><th class='n'>90%</th>"
        + "".join(f"<th class='n'>{esc(pl)} within, 50% / 90%</th>" for pl in plats)
        + "</tr>"
    )

    def tt(x: Any) -> str:
        return "n/a" if x is None else (esc(x) if isinstance(x, str) else f"{x:.0f} s")

    def within(x: Any, v: float) -> str:
        if x is None:
            return "n/a"
        if isinstance(x, str):
            return esc(x[0]) + f"{max(0.0, float(x[1:]) - dispatch) * v / 1000:.1f} km"
        return f"{max(0.0, x - dispatch) * v / 1000:.1f} km"

    trs = []
    for r in s.get("thresholds") or []:
        t50, t90 = r["t50"], r["t90"]
        trs.append(
            f"<tr><td>{esc(r['behaviour'])}</td><td class='n'>{r['density']:g}</td><td class='n'>{r['rules']}</td><td class='n'>{r['situations']}</td>"
            f"<td class='n'>{tt(t50[0])} <span class='muted'>[{tt(t50[1])}, {tt(t50[2])}]</span></td>"
            f"<td class='n'>{tt(t90[0])} <span class='muted'>[{tt(t90[1])}, {tt(t90[2])}]</span></td>"
            + "".join(f"<td class='n'>{within(t50[0], cruise[pl])} / {within(t90[0], cruise[pl])}</td>" for pl in plats)
            + "</tr>"
        )
    return table(head, trs)


def dist_table(s: dict[str, Any]) -> str:
    drows = s.get("dist") or []
    if not drows:
        return ""
    dists = sorted({r["dist_m"] for r in drows})
    plats = sorted({r["platform"] for r in drows})
    dv = sorted({r["rules"] for r in drows})
    cells = {(r["behaviour"], r["density"], r["platform"], r["dist_m"], r["rules"]): r for r in drows}
    head = "<tr><th>behaviour</th><th>per km<sup>2</sup></th>" + "".join(f"<th class='n'>{esc(pl)} {d / 1000:g} km</th>" for pl in plats for d in dists) + "</tr>"
    trs = []
    for b in sorted({r["behaviour"] for r in drows}, key=bkey):
        for d in sorted({r["density"] for r in drows}):
            tds = []
            for pl in plats:
                for dist in dists:
                    vals = [cells.get((b, d, pl, dist, v)) for v in dv]
                    pf = (vals[-1] or {}).get("perfect")
                    tds.append("<td class='n'>" + " / ".join(pct(x["tracks"], False) if x else "n/a" for x in vals) + f" <span class='muted'>({pct(pf, False)})</span></td>")
            trs.append(f"<tr><td>{esc(b)}</td><td class='n'>{d:g}</td>{''.join(tds)}</tr>")
    return (
        "<p>Blocked on tracks under rules " + esc(" / ".join(str(v) for v in dv)) + ", perfect knowledge in brackets, by platform and distance (bearing drawn). "
        "A holding target's effect point is where it is, so the platform's speed matters only through the time to effect it gives (dispatch delay plus "
        "distance over cruise speed).</p>" + table(head, trs)
    )


def quality_table(s: dict[str, Any]) -> str:
    q = s.get("tracker_quality") or []
    if not q:
        return ""
    ut = s.get("untracked_contacts") or [0, 0]
    head = (
        "<tr><th>contact behaviour</th><th class='n'>contacts sampled</th><th class='n'>tracked</th><th class='n'>with a velocity</th><th class='n'>with an average velocity</th>"
        "<th class='n'>manoeuvre seen</th><th class='n'>young</th><th class='n'>velocity error, median / p90</th><th class='n'>position error, median</th>"
        "<th class='n'>reported 1-sigma, median</th></tr>"
    )

    def sh(k: int, n: int) -> str:
        return f"{100 * k / n:.0f}%" if n else "n/a"

    trs = [
        f"<tr><td>{esc(r['behaviour'])}</td><td class='n'>{r['contacts']}</td><td class='n'>{sh(r['tracked'], r['contacts'])}</td>"
        f"<td class='n'>{sh(r['with_velocity'], r['tracked'])}</td><td class='n'>{sh(r['with_mean'], r['tracked'])}</td><td class='n'>{sh(r['manoeuvre'], r['tracked'])}</td>"
        f"<td class='n'>{sh(r['young'], r['tracked'])}</td><td class='n'>{esc(r['vel_err_median'])} / {esc(r['vel_err_p90'])} m/s</td>"
        f"<td class='n'>{esc(r['pos_err_median'])} m</td><td class='n'>{esc(r['sigma_median'])} m</td></tr>"
        for r in q
    ]
    return f"<p class='muted'>Contacts no sensor detected (no track, so unprotected): {ut[1]} of {ut[0]}.</p>" + table(head, trs)


def friendly_table(s: dict[str, Any]) -> str:
    fr = s.get("friendly") or []
    if not fr:
        return ""
    head = "<tr><th class='n'>time to effect</th><th>rules</th><th class='n'>fields</th><th class='n'>blocked</th><th class='n'>by the lost one</th><th class='n'>by one predicted</th><th class='n'>needed</th></tr>"
    trs = [
        f"<tr><td class='n'>{r['tte_s']} s</td><td class='n'>{r['rules']}</td><td class='n'>{r['fields']}</td><td class='n'>{pct(r['blocked'])}</td><td class='n'>{r['lost']}</td><td class='n'>{r['predicted']}</td><td class='n'>{r['needed']}</td></tr>"
        for r in fr
        if r["rules"] == max(x["rules"] for x in fr)
    ]
    return table(head, trs)


def regions_tables(s: dict[str, Any]) -> str:
    out = []
    for q in s.get("regions") or []:
        rv = q["rules"].get(str(max(int(k) for k in q["rules"])), [])
        comps = [c for c in COMPONENTS if any(c in x["regions"] for x in rv)]
        head = "<tr><th>region</th>" + "".join(f"<th class='n'>{x['span_s']:g} s</th>" for x in rv) + "</tr>"
        trs = [f"<tr><td>{esc(c)}</td>" + "".join(f"<td class='n'>{x['regions'][c]['radius_m']:.0f}</td>" if c in x["regions"] else "<td class='n'>-</td>" for x in rv) + "</tr>" for c in comps]
        out.append(f"<p><strong>{esc(q['regions'])}</strong></p>" + table(head, trs))
    return "".join(out)


def sensitivity_table(c: dict[str, Any]) -> str:
    rows = c.get("sensitivity") or []
    if not rows:
        return ""
    times = SENS_TTE
    tl = " / ".join(f"{t}" for t in times)
    head = (
        f"<tr><th>variant</th><th>per km<sup>2</sup></th><th class='n'>blocked at {tl} s: rules 6</th><th class='n'>rules 9</th><th class='n'>rules 12</th><th class='n'>rules 14</th>"
        "<th class='n'>perfect knowledge</th><th class='n'>50% / 90% pass, newest rules</th><th class='n'>needed share of blocks, rules 6 / newest</th>"
        "<th class='n'>missed at the keep-out line, rules 6 / newest</th><th class='n'>missed in the effect circle, rules 6 / newest</th>"
        "<th class='n'>missed over the release window, rules 6 / newest</th></tr>"
    )

    def tt(x: Any) -> str:
        return "n/a" if x is None else (esc(x) if isinstance(x, str) else f"{x:.0f} s")

    trs = []
    for row in rows:
        dens = list(row["by_density"].items())
        lv = str(row.get("latest", 12))
        for i, (d, x) in enumerate(dens):
            b = x["blocked"]
            cells = "".join("<td class='n'>" + " / ".join(pct(b.get(f"{t}|{v}"), False) for t in times) + "</td>" for v in (6, 9, 12, 14))
            perf = " / ".join(pct(x["perfect"].get(f"{t}"), False) for t in times)
            t50 = (x["t50"].get(lv) or [None])[0]
            t90 = ((x.get("t90") or {}).get(lv) or [None])[0]
            tail = ""
            if i == 0:
                n = len(dens)
                ms = row.get("missed_severe") or {}
                tail = (
                    f"<td class='n' rowspan='{n}'>{pct(row['needed'].get('6'), False)} / {pct(row['needed'].get(lv), False)}</td>"
                    f"<td class='n' rowspan='{n}'>{miss(row['missed'].get('6'))} / {miss(row['missed'].get(lv))}</td>"
                    f"<td class='n' rowspan='{n}'>{miss(ms.get('6'))} / {miss(ms.get(lv))}</td>"
                    f"<td class='n' rowspan='{n}'>{miss(row['missed_late'].get('6'))} / {miss(row['missed_late'].get(lv))}</td>"
                )
            name = f"<td rowspan='{len(dens)}'><strong>{esc(row['variant'])}</strong><br><span class='muted'>{esc(row.get('about'))}</span></td>" if i == 0 else ""
            trs.append(f"<tr>{name}<td class='n'>{esc(d)}</td>{cells}<td class='n'>{perf}</td><td class='n'>{tt(t50)} / {tt(t90)}</td>{tail}</tr>")
    return table(head, trs)


def live_table(variants: dict[str, Any]) -> str:
    """The live run's question across trackers: blocked on tracks at each distance, rules 6 and
    9 / 12, perfect knowledge."""
    live = [(n, s) for n, s in variants.items() if not s.get("tte") and s.get("dist")]
    if not live:
        return ""
    dists = sorted({r["dist_m"] for _, s in live for r in s["dist"]})
    head = "<tr><th>variant</th><th>tracker</th><th class='n'>circling boat within</th><th class='n'>per km<sup>2</sup></th>" + "".join(
        f"<th class='n'>magura {d / 1000:g} km: rules 6 / 9 / 12 / 14 (perfect)</th>" for d in dists
    ) + "</tr>"
    trs = []
    for n, s in live:
        p = s.get("params") or {}
        cells = {(r["density"], r["dist_m"], r["rules"]): r for r in s["dist"]}
        for d in sorted({r["density"] for r in s["dist"]}):
            tds = []
            for dist in dists:
                vals = [cells.get((d, dist, v)) for v in (6, 9, 12, 14)]
                pf = (vals[-1] or {}).get("perfect")
                tds.append("<td class='n'>" + " / ".join(pct(x["tracks"], False) if x else "n/a" for x in vals) + f" <span class='muted'>({pct(pf, False)})</span></td>")
            trs.append(f"<tr><td>{esc(n)}</td><td>{esc(p.get('tracker'))}</td><td class='n'>{esc(p.get('near_circler_km'))} km</td><td class='n'>{d:g}</td>{''.join(tds)}</tr>")
    return table(head, trs)


def air_table(variants: dict[str, Any]) -> str:
    """The air variants: the aircraft's track, and what it does to the block rate."""
    rows = [(n, s) for n, s in variants.items() if (s.get("params") or {}).get("air_contacts")]
    if not rows:
        return ""
    head = (
        "<tr><th>variant</th><th>tracker</th><th>aircraft</th><th class='n'>aircraft with a velocity (sampled)</th><th class='n'>its reported 1-sigma</th>"
        "<th class='n'>blocked, rules 12 / 14, at 30 / 60 / 120 s, 0.03 per km<sup>2</sup></th><th class='n'>0.1 per km<sup>2</sup></th></tr>"
    )
    trs = []
    for n, s in rows:
        p = s.get("params") or {}
        q = next((x for x in s.get("tracker_quality") or [] if x["behaviour"] == "air"), None)
        cells = {(r["density"], r["tte_s"], r["rules"]): r for r in s.get("tte", []) if r["behaviour"] == "mixed"}
        bl = [
            "<td class='n'>" + "; ".join(" / ".join(pct((cells.get((d, t, v)) or {}).get("tracks"), False) for t in (30, 60, 120)) for v in (12, 14)) + "</td>"
            for d in (0.03, 0.1)
        ]
        vel = f"{q['with_velocity']} of {q['tracked']}" if q else "n/a"
        trs.append(
            f"<tr><td>{esc(n)}</td><td>{esc(p.get('tracker'))}</td><td>{esc(p.get('air_motion', 'loiter'))}</td><td class='n'>{vel}</td>"
            f"<td class='n'>{esc(q['sigma_median'] if q else None)} m</td>{''.join(bl)}</tr>"
        )
    return table(head, trs)


def variant_block(s: dict[str, Any], title: str) -> str:
    """Every table of one variant."""
    out = [f"<h3>{esc(title)}</h3>"]
    p = s.get("params") or {}
    c = s.get("counts") or {}
    ut = s.get("untracked_contacts") or [0, 0]
    out.append(
        f"<p class='muted'>{esc(p.get('about'))}. {s.get('situations')} situations ({esc(s.get('situations_per_cell_per_seed'))} per seed, density and behaviour; "
        f"seeds {esc(s.get('seeds'))}), {esc(s.get('contacts'))} protected contacts"
        + (f" ({ut[1]} never tracked)" if ut[0] else "")
        + f", {esc(c.get('evaluations'))} evaluations each under rules {esc(', '.join(str(v) for v in p.get('rules') or []))}"
        + (f"; keep_out_m {esc(p.get('keep_out_m'))} (a copy of the policy)" if p.get("keep_out_m") is not None else "")
        + f"; evaluations where the regions disagree with the engine's verdict {esc(c.get('mismatches'))} (must be 0), errors {esc(c.get('errors'))}, "
        f"no preview {esc(c.get('no_preview'))}; the actual time to effect within 5 s of the nominal in all but {esc(c.get('tte_off'))} of {esc(c.get('tte_evaluations'))}.</p>"
    )
    out.append(rate_tables(s))
    if s.get("blocks"):
        out.append(
            "<p><strong>Needed or conservative</strong>, pooled over the grid's densities and times to effect, each weighted as the grid has them (uniform; "
            "the per-time counts are in summary.json, <code>blocks_by_tte</code>), with intervals clustered by situation. <strong>Needed</strong>: a protected "
            "contact truly inside the shipped policy's keep-out line (the effect radius plus 200 m, in every variant) at the effect time; <strong>severe</strong>: "
            "inside the effect circle. <strong>Missed</strong>: a pass with someone inside (the bound in brackets: the 95% upper bound when none was seen, "
            "treating passes as independent; per situation it is wider). The release window counts anyone inside from the effect time to 60 s after (a holding "
            "target may be released that late): the nomination check is a screen, and the release's own check (not run here) is meant to catch what moves in "
            "later.</p>"
        )
        out.append(blocks_table(s))
        out.append("<p><strong>Which component blocks</strong> the conservative blocks (nobody inside the keep-out): the component reaching in deepest, and whether the nomination would have passed with that component's regions left out and every other region as it was (alone).</p>")
        out.append(components_table(s))
    if s.get("thresholds"):
        out.append("<p><strong>Time to effect for a 50% and a 90% chance of passing</strong> on tracks (bootstrap over situations), and how close each platform must start for a holding target.</p>")
        out.append(thresholds_table(s))
    out.append(dist_table(s))
    q = quality_table(s)
    if q:
        out.append("<p><strong>What the tracker reported</strong> against the truth, for the contacts of the first situations of each chunk.</p>" + q)
    f = friendly_table(s)
    if f:
        out.append("<p><strong>The keep-out on our vehicles</strong>, each field once, newest rules.</p>" + f)
    return "".join(out)


CAN = (
    "<p><strong>What these numbers can support</strong> (model results, unreviewed):</p><ul>"
    "<li>Blocks climb steeply with the time to effect and with traffic density, for every tracker: launching closer is the strongest lever.</li>"
    "<li>Rules 9 and 12 never let a nomination through with someone in the effect circle, in any variant; rules 6 did, a few times per 15,000 passes. At "
    "the keep-out line rules 9 and 12 cut the misses several-fold (the sensitivity table has the counts).</li>"
    "<li>A boat circling within 1 km of the target blocks nearly every nomination under rules 9 and 12, whatever the tracker, matching the live run; "
    "within 2 km it depends on the tracker.</li>"
    "<li>Most blocks come from uncertainty about where contacts will be, not from contacts being close: compare each rate with perfect knowledge.</li>"
    "<li>The 200 m buffer is a lever about as large as better sensors, mainly at short times to effect; whether it may shrink is a decision about the "
    "effect model's margin, which this does not measure.</li>"
    "<li>An aircraft seen only by our 2D radar (no ADS-B, no height-finding radar) blocks every nomination within its reach.</li>"
    "</ul><p><strong>What they cannot support:</strong></p><ul>"
    "<li>That missing velocity estimates drive surface blocks in general: that holds for one USV's radar alone and depends on the sensors.</li>"
    "<li>That the buffer is not a lever.</li>"
    "<li>Any absolute rate for a real place, or any loosening (judge a loosening by the misses at the keep-out line it brings, under the real tracker).</li>"
    "<li>Anything about the plan's re-checks while signatures wait, the release's own check, or what operators do when blocked.</li></ul>"
)


def keepout_section(c: dict[str, Any]) -> str:
    variants = c.get("variants") or {}
    prim_name = c.get("primary")
    prim = variants.get(prim_name) or next(iter(variants.values()), {})
    p = prim.get("params") or {}
    out = ["<h2>Keep-outs against engagements (rules 6, 9, 12)</h2>"]
    out.append('<div class="note"><strong>Model results from simulated geometry, unreviewed.</strong> ' + esc(c.get("label")) + "</div>")
    out.append(
        "<p>Generated situations (eval/keepout.py: stated distributions, seeded) decided by the engine's own nomination checks under rules 6, 9 and 12 "
        "(eval/keepout_probe: <code>rules::evaluate</code> on the engine's view of the world, read-only). A hostile target holds in ez-gate-west, declared by "
        "an operator; protected contacts (neutral or unknown, none declared) around it at a density per km<sup>2</sup>, moving as one behaviour or a mix; our "
        "observing USV 1 to 3 km out; sometimes another of ours lost; the engaging platform placed for each time to effect. The protected contacts' tracks "
        "come from the bridge's own sensing model and IMM tracker (compiled from crates/murmur-bridge/src/tracks unchanged) on simulated fixes of their "
        "motion: <strong>radar</strong>, the observing USV's radar alone (the pessimistic sensor case, with the full tables), and <strong>all</strong>, every "
        "sensor it carries; or from a stated model; the variants are listed below.</p>"
    )
    items = "".join(f"<li><strong>{esc(k)}</strong>: {esc(v)}</li>" for k, v in (p.get("motion") or {}).items())
    mix = p.get("mix") or {}
    out.append(f"<ul>{items}<li><strong>mixed</strong>: {esc(', '.join(f'{k} {v:g}' for k, v in mix.items()))}</li></ul>")
    out.append(CAN)
    out.append(sensitivity_table(c) and ("<h3>Sensitivity, mixed traffic</h3><p>Each variant changes one thing from its tracker's base (stated beside it). "
               "Blocked on tracks at each time, what perfect knowledge would block then (rules-independent), the time to effect for a 50% and a 90% chance of "
               "passing under rules 12, the needed share of blocks and the passes that missed someone: inside the shipped keep-out line, inside the effect "
               "circle, and over the release window.</p>" + sensitivity_table(c)))
    lt = live_table(variants)
    if lt:
        out.append(
            "<h3>The live run's question</h3><p>The 10-06 live run blocked 77 of 77 nominations while a magura was 1 to 2 km out near a circling tour "
            "boat. Here: one boat circling within 1 or 2 km of the target (followed for a while, as the live one was), mixed traffic around it at the live "
            "run's density and above, a magura at 1, 1.5 and 2 km, under each tracker.</p>" + lt
        )
    at = air_table(variants)
    if at:
        out.append(
            "<h3>Air traffic: the worst case</h3><p>One light aircraft added to the surface traffic, seen only by our sensors (no ADS-B, no height-finding "
            "radar). On 2D radar fixes the bridge's air tracker keeps the aircraft's velocity too uncertain to report, flying straight or turning, so the "
            "keep-out counts it anywhere within 100 m/s of where it was; EO/IR helps a straight one, not a loitering one. The air tracker's constants were "
            "scaled from the surface ones and never checked against truth.</p>" + at
        )
    out.append(variant_block(prim, f"{prim_name}: the pessimistic sensor case"))
    for name, s in variants.items():
        if name == prim_name:
            continue
        out.append(f"<details><summary>Variant {esc(name)}: {esc((s.get('params') or {}).get('about'))}</summary>{variant_block(s, name)}</details>")
    reg = regions_tables(prim)
    if reg:
        out.append(
            "<h3>How large the engine makes each region</h3><p>Radius in metres (<code>keep_out_sigma</code> included) of each region "
            "<code>engage::track_reach</code> gives reference surface tracks (15 m 1-sigma), newest rules, by the span from the track's estimate to the effect; "
            "the keep-out (the effect radius plus 200 m) is beyond the effect point on top.</p>" + reg
        )
    out.append("<h3>What this does not cover</h3>")
    out.append(
        "<ul>"
        "<li>How often such situations arise: the densities, behaviours, their mix, the share of new tracks, the lanes and the single observing vehicle are "
        "assumptions; the live stack's several observers gave better tracks than one USV's radar.</li>"
        "<li>Truth labels use the generated motion; real traffic turns, stops and gets retasked. An aircraft counts in 3D for the labels (above the "
        "keep-out's height it is outside it), while the engine, with no altitude from our 2D radar, counts it in plan.</li>"
        "<li>Out-of-view contacts: every contact stays within our sensors, so the bridge's drop of a track unseen for 180 s never fires here (a dropped "
        "track stops counting in keep-outs: murmur-533); a contact never detected (a new one in its first second) has no track and is not protected.</li>"
        "<li>Only the nomination's checks run: the plan's re-checks while signatures wait (every second) would block more, and the release's own check is "
        "what stops an effect with someone inside it then.</li>"
        "<li>No person decides anything here: what operators do when blocked (wait, reposition, accept a lost vehicle, give up) is not modelled.</li>"
        "</ul>"
    )
    return "".join(out)
