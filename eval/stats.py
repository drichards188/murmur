"""Small statistics helpers: intervals across seeds, with no SciPy dependency."""

from __future__ import annotations

import math
import random
from statistics import NormalDist, mean, median, stdev

# two-sided 95% Student t quantiles for n - 1 degrees of freedom (n = 2..10), then normal
T95 = {1: 12.706, 2: 4.303, 3: 3.182, 4: 2.776, 5: 2.571, 6: 2.447, 7: 2.365, 8: 2.306, 9: 2.262}


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
    """Mean and a 95% t-interval across runs (seeds). One value: no interval."""
    xs = [float(v) for v in values if v is not None]
    if not xs:
        return None, None, None
    m = mean(xs)
    if len(xs) < 2:
        return m, None, None
    s = stdev(xs)
    t = T95.get(len(xs) - 1, 1.96)
    half = t * s / math.sqrt(len(xs))
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
