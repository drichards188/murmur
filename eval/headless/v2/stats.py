"""Scoring v2's statistics (eval/SCORING-V2.md, section 8). numpy and scipy only.

The unit is the situation: replicates are averaged within a situation first (8.1), and every
interval resamples or tests situations, never episodes or needs as if independent.

- `bca` / `paired_cluster_bootstrap`: the bias-corrected and accelerated bootstrap (Efron 1987;
  10,000 resamples) of the mean over situations; paired: of the per-situation differences.
  `stratified_bca`: the same of a mean pooled over strata (families), resampling within each.
- `sign_test`: the exact two-sided sign test (ties dropped) with the distribution-free interval
  for the median difference (order statistics).
- `t_interval`: Student t over situations (rates and burden per hour), with `bca` beside it as
  the check 8.4 asks for.
- `sign_flip`: the paired sign-flip permutation test of the mean difference (exact up to 16
  situations, else Monte Carlo).
- `rmst`, `kaplan_meier`: the restricted mean time to an event up to `tau` from the Kaplan-Meier
  curve (misses censored at `tau`, so the RMST is the mean of min(T, tau)).
- `clopper_pearson`: the exact binomial interval (constraints, 3.7).
- `holm`: Holm's step-down adjustment of a family of p values.
- `icc`, `n_eff`: the one-way ANOVA intraclass correlation of a per-need outcome within
  situations, and the effective number of needs `N / (1 + (m - 1) ICC)` with `m` Kish's mean
  cluster size `sum(m_i^2) / N`.
- `n_two_proportions`, `n_paired_mean`: sample sizes at a given power (8.6).
- `multiverse`: robust or fragile over the cells of the stated dimensions (8.5).
- `scotts_pi`, `scotts_pi_interval`: agreement for the blind checks (2.7).
"""

from __future__ import annotations

import itertools
from collections.abc import Callable, Sequence
from typing import Any

import numpy as np
from scipy import stats as st

N_BOOT = 10_000
ALPHA = 0.05


def _arr(x: Sequence[float]) -> np.ndarray:
    a = np.asarray([float(v) for v in x if v is not None], dtype=float)
    return a[np.isfinite(a)]


def _r(x: float | None, nd: int = 6) -> float | None:
    return None if x is None or not np.isfinite(x) else round(float(x), nd)


def _p(x: float | None) -> float | None:
    """A p value, unrounded (Holm compares it with small thresholds)."""
    return None if x is None or not np.isfinite(x) else float(x)


def bca(values: Sequence[float], n_boot: int = N_BOOT, seed: int = 0, alpha: float = ALPHA,
        stat: Callable[[np.ndarray], float] | None = None) -> dict[str, Any]:
    """The BCa bootstrap interval of `stat` (default the mean) over the values (one per
    situation). Degenerate data (one value, or all equal) give the point as its interval."""
    x = _arr(values)
    n = len(x)
    out: dict[str, Any] = {"method": "bca", "n": n, "n_boot": n_boot, "estimate": None, "lo": None, "hi": None}
    if n == 0:
        return out
    f = stat or (lambda a: float(np.mean(a)))
    theta = f(x)
    out["estimate"] = _r(theta)
    if n < 2 or np.allclose(x, x[0]):
        out["lo"] = out["hi"] = _r(theta)
        out["degenerate"] = True
        return out
    rng = np.random.default_rng(seed)
    idx = rng.integers(0, n, size=(n_boot, n))
    if stat is None:
        boot = x[idx].mean(axis=1)
    else:
        boot = np.array([f(x[i]) for i in idx])
    prop = (np.sum(boot < theta) + 0.5 * np.sum(boot == theta)) / n_boot
    prop = min(max(prop, 1.0 / (2 * n_boot)), 1.0 - 1.0 / (2 * n_boot))
    z0 = st.norm.ppf(prop)
    jack = np.array([f(np.delete(x, i)) for i in range(n)])
    d = jack.mean() - jack
    den = 6.0 * (np.sum(d ** 2) ** 1.5)
    a = float(np.sum(d ** 3) / den) if den > 0 else 0.0
    zs = st.norm.ppf([alpha / 2.0, 1.0 - alpha / 2.0])
    adj = st.norm.cdf(z0 + (z0 + zs) / (1.0 - a * (z0 + zs)))
    lo, hi = np.quantile(boot, adj, method="linear")
    out.update(lo=_r(lo), hi=_r(hi), z0=_r(z0), acceleration=_r(a))
    return out


def stratified_bca(strata: dict[Any, Sequence[float]], n_boot: int = N_BOOT, seed: int = 0,
                   alpha: float = ALPHA) -> dict[str, Any]:
    """The BCa bootstrap of the mean over every value pooled (one per situation, each weighted
    alike), resampling within each stratum with its size kept (a family's situations stay that
    family's): the bias correction from the stratified resamples, the acceleration from the
    jackknife leaving one situation out. Degenerate data give the point as its interval."""
    xs = {k: _arr(v) for k, v in strata.items()}
    xs = {k: v for k, v in xs.items() if len(v)}
    out: dict[str, Any] = {"method": "stratified bca", "n": int(sum(len(v) for v in xs.values())), "n_boot": n_boot,
                           "strata": {str(k): len(v) for k, v in sorted(xs.items(), key=lambda kv: str(kv[0]))},
                           "estimate": None, "lo": None, "hi": None}
    if not xs:
        return out
    x = np.concatenate([xs[k] for k in sorted(xs, key=str)])
    n = len(x)
    theta = float(x.mean())
    out["estimate"] = _r(theta)
    if n < 2 or np.allclose(x, x[0]):
        out["lo"] = out["hi"] = _r(theta)
        out["degenerate"] = True
        return out
    rng = np.random.default_rng(seed)
    sums = np.zeros(n_boot)
    for k in sorted(xs, key=str):
        v = xs[k]
        sums += v[rng.integers(0, len(v), size=(n_boot, len(v)))].sum(axis=1)
    boot = sums / n
    prop = (np.sum(boot < theta) + 0.5 * np.sum(boot == theta)) / n_boot
    prop = min(max(prop, 1.0 / (2 * n_boot)), 1.0 - 1.0 / (2 * n_boot))
    z0 = st.norm.ppf(prop)
    jack = (x.sum() - x) / (n - 1)
    d = jack.mean() - jack
    den = 6.0 * (np.sum(d ** 2) ** 1.5)
    a = float(np.sum(d ** 3) / den) if den > 0 else 0.0
    zs = st.norm.ppf([alpha / 2.0, 1.0 - alpha / 2.0])
    adj = st.norm.cdf(z0 + (z0 + zs) / (1.0 - a * (z0 + zs)))
    lo, hi = np.quantile(boot, adj, method="linear")
    out.update(lo=_r(lo), hi=_r(hi), z0=_r(z0), acceleration=_r(a))
    return out


def situation_means(by_situation: dict[Any, Sequence[float]]) -> dict[Any, float]:
    """Each situation's replicates averaged (8.1); a situation with no value is left out."""
    out = {}
    for s, vs in by_situation.items():
        a = _arr(vs if isinstance(vs, (list, tuple, np.ndarray)) else [vs])
        if len(a):
            out[s] = float(a.mean())
    return out


def paired_differences(a: dict[Any, Sequence[float]], b: dict[Any, Sequence[float]]) -> dict[Any, float]:
    """Per situation present in both, mean(a's replicates) - mean(b's)."""
    ma, mb = situation_means(a), situation_means(b)
    return {s: ma[s] - mb[s] for s in sorted(set(ma) & set(mb), key=str)}


def paired_cluster_bootstrap(a: dict[Any, Sequence[float]], b: dict[Any, Sequence[float]], n_boot: int = N_BOOT,
                             seed: int = 0, alpha: float = ALPHA) -> dict[str, Any]:
    """The paired cluster bootstrap of mean(a - b) over situations: replicates averaged within
    each situation, the situations resampled (BCa)."""
    d = paired_differences(a, b)
    out = bca(list(d.values()), n_boot=n_boot, seed=seed, alpha=alpha)
    out["method"] = "paired cluster bootstrap (BCa)"
    out["n_situations"] = len(d)
    return out


def sign_test(diffs: Sequence[float], alpha: float = ALPHA) -> dict[str, Any]:
    """The exact two-sided sign test of median zero (ties dropped), and the distribution-free
    interval for the median from order statistics (coverage at least 1 - alpha)."""
    x = _arr(diffs)
    pos, neg = int(np.sum(x > 0)), int(np.sum(x < 0))
    n = pos + neg
    p = float(st.binomtest(pos, n, 0.5).pvalue) if n else 1.0
    out: dict[str, Any] = {"method": "exact sign test", "n": len(x), "positive": pos, "negative": neg,
                           "zero": len(x) - n, "p": _p(p), "median": _r(float(np.median(x))) if len(x) else None,
                           "lo": None, "hi": None}
    m = len(x)
    if m:
        xs = np.sort(x)
        # the largest k with P(B <= k - 1) <= alpha / 2 for B ~ Bin(m, 1/2): [x_(k), x_(m-k+1)]
        k = int(st.binom.ppf(alpha / 2.0, m, 0.5))
        while k > 0 and st.binom.cdf(k - 1, m, 0.5) > alpha / 2.0:
            k -= 1
        if k >= 1:
            out["lo"], out["hi"] = _r(xs[k - 1]), _r(xs[m - k])
            out["coverage"] = _r(1.0 - 2.0 * st.binom.cdf(k - 1, m, 0.5))
    return out


def t_interval(values: Sequence[float], alpha: float = ALPHA) -> dict[str, Any]:
    x = _arr(values)
    n = len(x)
    out: dict[str, Any] = {"method": "student t", "n": n, "estimate": _r(float(x.mean())) if n else None,
                           "lo": None, "hi": None}
    if n >= 2:
        sd = float(x.std(ddof=1))
        half = float(st.t.ppf(1.0 - alpha / 2.0, n - 1)) * sd / np.sqrt(n)
        out.update(lo=_r(x.mean() - half), hi=_r(x.mean() + half), sd=_r(sd), df=n - 1)
    return out


def sign_flip(diffs: Sequence[float], n_perm: int = N_BOOT, seed: int = 0, exact_max: int = 16) -> dict[str, Any]:
    """The paired sign-flip permutation test of mean(diffs) = 0, two-sided: exact (every sign
    pattern) up to `exact_max` situations, else `n_perm` random patterns (the observed one
    counted, Phipson and Smyth 2010)."""
    x = _arr(diffs)
    n = len(x)
    if n == 0:
        return {"method": "sign-flip permutation", "n": 0, "p": None}
    obs = abs(float(x.mean()))
    tol = 1e-12 * max(1.0, obs)
    if n <= exact_max:
        signs = np.array(list(itertools.product((1.0, -1.0), repeat=n)))
        means = np.abs((signs * x).mean(axis=1))
        p = float(np.mean(means >= obs - tol))
        return {"method": "sign-flip permutation (exact)", "n": n, "p": _p(p), "patterns": len(signs)}
    rng = np.random.default_rng(seed)
    signs = rng.choice((-1.0, 1.0), size=(n_perm, n))
    means = np.abs((signs * x).mean(axis=1))
    p = (1.0 + float(np.sum(means >= obs - tol))) / (n_perm + 1.0)
    return {"method": "sign-flip permutation (Monte Carlo)", "n": n, "p": _p(p), "patterns": n_perm}


def kaplan_meier(times: Sequence[float], events: Sequence[bool]) -> list[tuple[float, float]]:
    """The Kaplan-Meier survival curve as (time, S just after it) steps, starting (0, 1). An
    event at time 0 (a need served before it arose) drops the curve at 0."""
    t = np.asarray(times, dtype=float)
    e = np.asarray(events, dtype=bool)
    curve: list[tuple[float, float]] = [(0.0, 1.0)]
    s = 1.0
    for u in np.unique(t[e]):
        at_risk = int(np.sum(t >= u))
        d = int(np.sum((t == u) & e))
        if at_risk:
            s *= 1.0 - d / at_risk
        if u == 0.0:
            curve[0] = (0.0, s)
        else:
            curve.append((float(u), s))
    return [(round(a, 6), round(b, 6)) for a, b in curve]


def rmst(times: Sequence[float], events: Sequence[bool], tau: float = 60.0) -> dict[str, Any]:
    """The restricted mean time to the event up to `tau`: the area under the Kaplan-Meier curve
    on [0, tau]. With every censoring at `tau` it is the mean of min(T, tau)."""
    t = np.minimum(np.asarray(times, dtype=float), tau)
    e = np.asarray(events, dtype=bool) & (np.asarray(times, dtype=float) <= tau)
    if len(t) == 0:
        return {"rmst": None, "n": 0, "tau": tau, "events": 0, "curve": []}
    curve = kaplan_meier(t, e)
    area, prev_t, prev_s = 0.0, 0.0, curve[0][1]
    for u, s in curve[1:]:
        if u >= tau:
            break
        area += prev_s * (u - prev_t)
        prev_t, prev_s = u, s
    area += prev_s * (tau - prev_t)
    return {"rmst": _r(area), "n": int(len(t)), "tau": tau, "events": int(e.sum()), "curve": curve}


def clopper_pearson(k: int, n: int, alpha: float = ALPHA) -> dict[str, Any]:
    if n <= 0:
        return {"method": "clopper-pearson", "k": k, "n": n, "rate": None, "lo": None, "hi": None}
    lo = 0.0 if k == 0 else float(st.beta.ppf(alpha / 2.0, k, n - k + 1))
    hi = 1.0 if k == n else float(st.beta.ppf(1.0 - alpha / 2.0, k + 1, n - k))
    return {"method": "clopper-pearson", "k": int(k), "n": int(n), "rate": _r(k / n), "lo": _r(lo), "hi": _r(hi)}


def holm(pvalues: dict[str, float | None], alpha: float = ALPHA) -> dict[str, dict[str, Any]]:
    """Holm's step-down adjustment: each p's adjusted value and whether it is rejected at alpha
    (None p values are left out of the family and come back as such)."""
    items = sorted(((k, float(p)) for k, p in pvalues.items() if p is not None), key=lambda kv: kv[1])
    m = len(items)
    out: dict[str, dict[str, Any]] = {k: {"p": None, "p_holm": None, "reject": None} for k, p in pvalues.items() if p is None}
    running = 0.0
    still = True
    for i, (k, p) in enumerate(items):
        adj = min(1.0, (m - i) * p)
        running = max(running, adj)
        rej = still and p <= alpha / (m - i)
        still = still and rej
        out[k] = {"p": _p(p), "p_holm": _p(running), "reject": bool(rej)}
    return out


def icc(groups: Sequence[Sequence[float]]) -> float | None:
    """ICC(1), one-way ANOVA, unequal group sizes (n0 = (N - sum n_i^2 / N) / (k - 1));
    clipped at 0. None with fewer than two groups or no within-group replication."""
    gs = [_arr(g) for g in groups]
    gs = [g for g in gs if len(g)]
    k = len(gs)
    N = sum(len(g) for g in gs)
    if k < 2 or N <= k:
        return None
    grand = np.concatenate(gs).mean()
    ssb = sum(len(g) * (g.mean() - grand) ** 2 for g in gs)
    ssw = sum(float(((g - g.mean()) ** 2).sum()) for g in gs)
    msb = ssb / (k - 1)
    msw = ssw / (N - k)
    n0 = (N - sum(len(g) ** 2 for g in gs) / N) / (k - 1)
    den = msb + (n0 - 1) * msw
    if den <= 0:
        return 0.0 if msb <= 0 else None
    return float(max(0.0, (msb - msw) / den))


def n_eff(groups: Sequence[Sequence[float]]) -> dict[str, Any]:
    """The effective number of observations of outcomes clustered in situations: N divided by
    the design effect 1 + (m - 1) ICC, m Kish's mean cluster size."""
    gs = [_arr(g) for g in groups]
    gs = [g for g in gs if len(g)]
    N = sum(len(g) for g in gs)
    if N == 0:
        return {"n": 0, "n_eff": None, "icc": None, "clusters": 0}
    r = icc(gs)
    m = sum(len(g) ** 2 for g in gs) / N
    deff = 1.0 + (m - 1.0) * (r if r is not None else 0.0)
    return {"n": int(N), "clusters": len(gs), "icc": _r(r), "design_effect": _r(deff), "n_eff": _r(N / deff, 2)}


def n_two_proportions(p1: float, p2: float, alpha: float = ALPHA, power: float = 0.8) -> int:
    """Situations per arm to detect p1 against p2 (two-sided, normal approximation, no pairing
    correlation assumed: 8.6's rule)."""
    za, zb = st.norm.ppf(1.0 - alpha / 2.0), st.norm.ppf(power)
    return int(np.ceil((za + zb) ** 2 * (p1 * (1 - p1) + p2 * (1 - p2)) / (p1 - p2) ** 2))


def n_paired_mean(delta: float, sd_diff: float, alpha: float = ALPHA, power: float = 0.8) -> int:
    """Situations to detect a mean paired difference `delta` with SD `sd_diff` (normal
    approximation)."""
    za, zb = st.norm.ppf(1.0 - alpha / 2.0), st.norm.ppf(power)
    return int(np.ceil(((za + zb) * sd_diff / delta) ** 2))


#: 8.6: the sizes the power analysis gives now (phase 5 recomputes them on v2 dev data):
#: 15 points between 0.6 and 0.75 at power 0.8, and one card per 10 min of burden
POWER_SIZES = {"rate": n_two_proportions(0.6, 0.75), "burden": 30}


def multiverse(cells: dict[str, dict[str, Any]], direction: int) -> dict[str, Any]:
    """8.5: a conclusion in `direction` (+1: the difference is positive, -1: negative) is robust
    if no cell's interval excludes zero the other way; else fragile, with those cells."""
    against = []
    for name, c in cells.items():
        lo, hi = c.get("lo"), c.get("hi")
        if lo is None or hi is None:
            continue
        if direction > 0 and hi < 0:
            against.append(name)
        elif direction < 0 and lo > 0:
            against.append(name)
    return {"direction": direction, "n_cells": len(cells), "verdict": "fragile" if against else "robust",
            "against": sorted(against)}


def scotts_pi(a: Sequence[Any], b: Sequence[Any]) -> float | None:
    """Scott's pi of two raters' labels on the same items: (Po - Pe) / (1 - Pe), Pe from the
    pooled marginals. 1.0 when the pooled labels are all one category and both agree."""
    if len(a) != len(b) or not a:
        return None
    n = len(a)
    po = sum(1 for x, y in zip(a, b) if x == y) / n
    cats: dict[Any, int] = {}
    for x in list(a) + list(b):
        cats[x] = cats.get(x, 0) + 1
    pe = sum((c / (2 * n)) ** 2 for c in cats.values())
    if pe >= 1.0:
        return 1.0 if po == 1.0 else 0.0
    return (po - pe) / (1.0 - pe)


def scotts_pi_interval(a: Sequence[Any], b: Sequence[Any], clusters: Sequence[Any] | None = None,
                       n_boot: int = 2000, seed: int = 0, alpha: float = ALPHA) -> dict[str, Any]:
    """Scott's pi with a percentile bootstrap interval, resampling items, or whole clusters
    (episodes) when given."""
    pi = scotts_pi(a, b)
    out: dict[str, Any] = {"pi": _r(pi), "n": len(a), "lo": None, "hi": None}
    if pi is None or len(a) < 2:
        return out
    rng = np.random.default_rng(seed)
    A, B = list(a), list(b)
    groups: list[list[int]]
    if clusters is not None:
        by: dict[Any, list[int]] = {}
        for i, c in enumerate(clusters):
            by.setdefault(c, []).append(i)
        groups = list(by.values())
    else:
        groups = [[i] for i in range(len(A))]
    vals = []
    for _ in range(n_boot):
        pick = rng.integers(0, len(groups), size=len(groups))
        idx = [i for g in pick for i in groups[g]]
        v = scotts_pi([A[i] for i in idx], [B[i] for i in idx])
        if v is not None:
            vals.append(v)
    if vals:
        out["lo"], out["hi"] = _r(float(np.quantile(vals, alpha / 2.0))), _r(float(np.quantile(vals, 1 - alpha / 2.0)))
    return out
