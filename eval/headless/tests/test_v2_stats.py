"""Scoring v2's statistics (eval/SCORING-V2.md, section 8) against known values and against
scipy's own implementations where it has them."""

from __future__ import annotations

import math

import numpy as np
import pytest
from scipy import stats as st

from eval.headless.batch import replicate_seed
from eval.headless.v2 import stats as S


def test_bca_matches_scipy_and_handles_degenerate_data():
    rng = np.random.default_rng(3)
    x = rng.exponential(2.0, size=40)
    ours = S.bca(x, n_boot=20000, seed=1)
    ref = st.bootstrap((x,), np.mean, n_resamples=20000, method="BCa", random_state=np.random.default_rng(1))
    assert ours["estimate"] == pytest.approx(float(x.mean()))
    assert ours["lo"] == pytest.approx(ref.confidence_interval.low, abs=0.05)
    assert ours["hi"] == pytest.approx(ref.confidence_interval.high, abs=0.05)
    # skewed data: the BCa interval leans right of the percentile interval's symmetric guess
    assert ours["hi"] - ours["estimate"] > ours["estimate"] - ours["lo"]
    d = S.bca([0.5, 0.5, 0.5])
    assert d["lo"] == d["hi"] == 0.5 and d["degenerate"]
    assert S.bca([])["estimate"] is None


def test_paired_cluster_bootstrap_averages_replicates_first():
    a = {"s1": [1.0, 0.0], "s2": [1.0], "s3": [0.5, 0.5, 0.5]}
    b = {"s1": [0.0], "s2": [0.0], "s3": [0.5], "s4": [1.0]}
    d = S.paired_differences(a, b)
    assert d == {"s1": 0.5, "s2": 1.0, "s3": 0.0}
    r = S.paired_cluster_bootstrap(a, b, n_boot=2000)
    assert r["n_situations"] == 3 and r["estimate"] == pytest.approx(0.5)
    assert r["lo"] <= 0.5 <= r["hi"]


def test_exact_sign_test_known_values():
    r = S.sign_test([1.0] * 10)
    assert r["p"] == pytest.approx(2 / 1024)
    r = S.sign_test([1, 1, 1, -1, 0, 2, 3])
    assert (r["positive"], r["negative"], r["zero"]) == (5, 1, 1)
    assert r["p"] == pytest.approx(st.binomtest(5, 6, 0.5).pvalue)
    # the median's interval from order statistics: n = 10 -> [x(2), x(9)], coverage 1 - 2 P(B <= 1)
    xs = list(range(1, 11))
    r = S.sign_test(xs)
    assert (r["lo"], r["hi"]) == (2.0, 9.0)
    assert r["coverage"] == pytest.approx(1 - 2 * st.binom.cdf(1, 10, 0.5))


def test_student_t_over_situations():
    r = S.t_interval([1.0, 2.0, 3.0, 4.0])
    half = st.t.ppf(0.975, 3) * np.std([1, 2, 3, 4], ddof=1) / 2
    assert r["estimate"] == 2.5 and r["lo"] == pytest.approx(2.5 - half) and r["hi"] == pytest.approx(2.5 + half)
    assert S.t_interval([1.0])["lo"] is None


def test_sign_flip_permutation():
    # every one of 6 differences positive: only the all-positive and all-negative patterns reach it
    r = S.sign_flip([1.0, 2.0, 3.0, 1.0, 2.0, 3.0])
    assert r["p"] == pytest.approx(2 / 64) and "exact" in r["method"]
    assert S.sign_flip([1.0, -1.0])["p"] == 1.0
    big = S.sign_flip(list(np.linspace(0.1, 2.0, 30)), n_perm=5000)
    assert "Monte Carlo" in big["method"] and big["p"] < 0.01


def test_rmst_and_kaplan_meier():
    # served at 0 (before the need), 30 s, and a miss censored at 60: the mean of min(T, 60)
    r = S.rmst([0.0, 30.0, 60.0], [True, True, False], tau=60.0)
    assert r["rmst"] == pytest.approx(30.0)
    assert r["curve"][0] == (0.0, pytest.approx(2 / 3)) and r["curve"][1] == (30.0, pytest.approx(1 / 3))
    # censoring before tau: the Kaplan-Meier area, not the naive mean
    km = S.rmst([10.0, 20.0, 40.0], [True, False, True], tau=60.0)
    # S = 1 to 10, 2/3 to 40, then 0: area = 10 + 30 * 2/3
    assert km["rmst"] == pytest.approx(10 + 30 * 2 / 3)
    assert S.rmst([], [])["rmst"] is None


def test_clopper_pearson_known_values():
    r = S.clopper_pearson(0, 10)
    assert r["lo"] == 0.0 and r["hi"] == pytest.approx(0.30850, abs=1e-4)
    r = S.clopper_pearson(10, 10)
    assert r["hi"] == 1.0 and r["lo"] == pytest.approx(0.69150, abs=1e-4)
    r = S.clopper_pearson(3, 10)
    assert r["lo"] == pytest.approx(0.06674, abs=1e-4) and r["hi"] == pytest.approx(0.65245, abs=1e-4)


def test_holm():
    h = S.holm({"a": 0.01, "b": 0.04, "c": 0.03, "d": None})
    assert h["a"]["p_holm"] == pytest.approx(0.03) and h["a"]["reject"]
    assert h["c"]["p_holm"] == pytest.approx(0.06) and not h["c"]["reject"]
    assert h["b"]["p_holm"] == pytest.approx(0.06) and not h["b"]["reject"]
    assert h["d"]["p"] is None


def test_icc_and_n_eff():
    # identical within clusters, different between: ICC 1, n_eff = the number of clusters
    groups = [[1.0, 1.0, 1.0], [0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.0, 0.0, 0.0]]
    assert S.icc(groups) == pytest.approx(1.0)
    assert S.n_eff(groups)["n_eff"] == pytest.approx(4.0)
    # no clustering: ICC clipped at 0, n_eff = N
    flat = [[1.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 0.0]]
    assert S.icc(flat) == 0.0 and S.n_eff(flat)["n_eff"] == 8.0
    assert S.icc([[1.0]]) is None


def test_power_sizes():
    # 8.6: 15 points between 0.6 and 0.75 at power 0.8 needs about 150
    assert S.n_two_proportions(0.6, 0.75) == 150
    assert S.POWER_SIZES == {"rate": 150, "burden": 30}
    assert S.n_paired_mean(1.0, 2.0) == math.ceil(((st.norm.ppf(0.975) + st.norm.ppf(0.8)) * 2.0) ** 2)


def test_multiverse_robust_and_fragile():
    cells = {"a": {"lo": 0.1, "hi": 0.3}, "b": {"lo": -0.1, "hi": 0.2}}
    assert S.multiverse(cells, +1)["verdict"] == "robust"
    cells["c"] = {"lo": -0.4, "hi": -0.05}
    v = S.multiverse(cells, +1)
    assert v["verdict"] == "fragile" and v["against"] == ["c"]


def test_scotts_pi():
    # Scott (1955)'s form: Po 0.8, pooled marginals 0.5/0.5 -> Pe 0.5, pi 0.6
    a = ["y"] * 5 + ["n"] * 5
    b = ["y"] * 4 + ["n"] + ["y"] + ["n"] * 4
    assert S.scotts_pi(a, b) == pytest.approx(0.6)
    assert S.scotts_pi(["y", "y"], ["y", "y"]) == 1.0
    r = S.scotts_pi_interval(a, b, n_boot=500)
    assert r["pi"] == pytest.approx(0.6) and r["lo"] <= 0.6 <= r["hi"]


def test_replicate_seeds():
    """SCORING-V2.md section 1: sha256("{family}:{situation_seed}:{r}") mod 2^31."""
    import hashlib

    want = int(hashlib.sha256(b"fast-unknown:1000:0").hexdigest(), 16) % 2 ** 31
    assert replicate_seed("fast-unknown", 1000, 0) == want
    assert len({replicate_seed("fast-unknown", 1000, r) for r in range(4)}) == 4
    assert replicate_seed("fast-unknown", 1000, 0) != replicate_seed("decoy", 1000, 0)
