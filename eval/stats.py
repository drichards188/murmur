"""Small statistics helpers: intervals across seeds, with no SciPy dependency."""

from __future__ import annotations

import math
import random
from statistics import NormalDist, mean, median, stdev

def _betacf(a: float, b: float, x: float) -> float:
    """Continued fraction for the regularized incomplete beta (Lentz's method)."""
    tiny = 1e-300
    qab, qap, qam = a + b, a + 1.0, a - 1.0
    c, d = 1.0, 1.0 - qab * x / qap
    d = 1.0 / (d if abs(d) > tiny else tiny)
    h = d
    for m in range(1, 300):
        m2 = 2 * m
        aa = m * (b - m) * x / ((qam + m2) * (a + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + aa / c if abs(1.0 + aa / c) > tiny else tiny
        h *= d * c
        aa = -(a + m) * (qab + m) * x / ((a + m2) * (qap + m2))
        d = 1.0 + aa * d
        d = 1.0 / (d if abs(d) > tiny else tiny)
        c = 1.0 + aa / c if abs(1.0 + aa / c) > tiny else tiny
        delta = d * c
        h *= delta
        if abs(delta - 1.0) < 1e-14:
            break
    return h


def betainc(a: float, b: float, x: float) -> float:
    """The regularized incomplete beta function I_x(a, b)."""
    if x <= 0.0:
        return 0.0
    if x >= 1.0:
        return 1.0
    lbeta = math.lgamma(a + b) - math.lgamma(a) - math.lgamma(b) + a * math.log(x) + b * math.log(1.0 - x)
    if x < (a + 1.0) / (a + b + 2.0):
        return math.exp(lbeta) * _betacf(a, b, x) / a
    return 1.0 - math.exp(lbeta) * _betacf(b, a, 1.0 - x) / b


def t_cdf(t: float, df: float) -> float:
    """Student t cumulative distribution."""
    x = df / (df + t * t)
    tail = 0.5 * betainc(df / 2.0, 0.5, x)
    return 1.0 - tail if t >= 0 else tail


def t_quantile(p: float, df: float) -> float:
    """Student t quantile (inverse CDF) by bisection on `t_cdf`; exact to 1e-10."""
    lo, hi = -1e6, 1e6
    for _ in range(200):
        mid = (lo + hi) / 2.0
        if t_cdf(mid, df) < p:
            lo = mid
        else:
            hi = mid
        if hi - lo < 1e-10:
            break
    return (lo + hi) / 2.0


def t95(df: int) -> float:
    """The two-sided 95% Student t critical value for `df` degrees of freedom."""
    return t_quantile(0.975, float(df))


def wilson(k: int, n: int, z: float = 1.96) -> tuple[float | None, float | None, float | None]:
    """Wilson score interval for a proportion: (rate, lo, hi); (None, None, None) if n = 0."""
    if n <= 0:
        return None, None, None
    p = k / n
    denom = 1 + z * z / n
    centre = (p + z * z / (2 * n)) / denom
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / denom
    return p, max(0.0, centre - half), min(1.0, centre + half)


def t_interval(values: list[float]) -> tuple[float | None, float | None, float | None]:
    """Mean and a 95% Student t interval across runs (seeds: the unit of replication, so the
    interval treats a run's cases as a cluster). One value: no interval."""
    xs = [float(v) for v in values if v is not None]
    if not xs:
        return None, None, None
    m = mean(xs)
    if len(xs) < 2:
        return m, None, None
    s = stdev(xs)
    half = t95(len(xs) - 1) * s / math.sqrt(len(xs))
    return m, m - half, m + half


def bootstrap_mean(values: list[float], seed: int = 1, draws: int = 2000) -> tuple[float | None, float | None, float | None]:
    xs = [float(v) for v in values if v is not None]
    if not xs:
        return None, None, None
    if len(xs) < 2:
        return xs[0], None, None
    rng = random.Random(seed)
    means = sorted(mean(rng.choices(xs, k=len(xs))) for _ in range(draws))
    return mean(xs), means[int(0.025 * draws)], means[int(0.975 * draws) - 1]


def quantile(values: list[float], q: float) -> float | None:
    xs = sorted(float(v) for v in values if v is not None)
    if not xs:
        return None
    if len(xs) == 1:
        return xs[0]
    pos = q * (len(xs) - 1)
    lo = int(math.floor(pos))
    hi = min(lo + 1, len(xs) - 1)
    return xs[lo] + (xs[hi] - xs[lo]) * (pos - lo)


def summary(values: list[float]) -> dict:
    xs = [float(v) for v in values if v is not None]
    if not xs:
        return {"n": 0, "mean": None, "median": None, "p90": None, "min": None, "max": None}
    return {
        "n": len(xs),
        "mean": round(mean(xs), 3),
        "median": round(median(xs), 3),
        "p90": round(quantile(xs, 0.9) or 0.0, 3),
        "min": round(min(xs), 3),
        "max": round(max(xs), 3),
    }


def spearman(xs: list[float], ys: list[float]) -> float | None:
    """Spearman rank correlation; None with fewer than 3 pairs or no variance."""
    if len(xs) != len(ys) or len(xs) < 3:
        return None

    def ranks(v: list[float]) -> list[float]:
        order = sorted(range(len(v)), key=lambda i: v[i])
        r = [0.0] * len(v)
        i = 0
        while i < len(order):
            j = i
            while j + 1 < len(order) and v[order[j + 1]] == v[order[i]]:
                j += 1
            avg = (i + j) / 2 + 1
            for k in range(i, j + 1):
                r[order[k]] = avg
            i = j + 1
        return r

    rx, ry = ranks(xs), ranks(ys)
    mx, my = mean(rx), mean(ry)
    sxy = sum((a - mx) * (b - my) for a, b in zip(rx, ry, strict=True))
    sxx = sum((a - mx) ** 2 for a in rx)
    syy = sum((b - my) ** 2 for b in ry)
    if sxx == 0 or syy == 0:
        return None
    return sxy / math.sqrt(sxx * syy)


def dprime(hit: int, miss: int, fa: int, cr: int) -> float | None:
    """Signal-detection discrimination d' = z(hit rate) - z(false-alarm rate), with the
    log-linear correction for empty cells ((k + 0.5) / (n + 1)), as SandTable's `trust_dprime`
    (src/sandtable/metrics.py). None when either class is empty; 0.0 at the two corners
    (rejected everything, or approved everything), where no decision was discriminated."""
    npos, nneg = hit + miss, fa + cr
    if npos == 0 or nneg == 0:
        return None
    if hit + fa == 0 or miss + cr == 0:
        return 0.0
    z = NormalDist().inv_cdf
    hr = min(max((hit + 0.5) / (npos + 1.0), 1e-6), 1 - 1e-6)
    far = min(max((fa + 0.5) / (nneg + 1.0), 1e-6), 1 - 1e-6)
    return z(hr) - z(far)
