"""The current-validation statistics, on synthetic series (no network)."""

import numpy as np
import pytest

from tidewater.validate import score_currents

T = np.arange(0, 3 * 86400, 360.0)
PERIOD = 12.42 * 3600


def tide(shift=0.0, amp=1.0, bias_dir=0.0):
    """A reversing current along 30 deg true: flood toward 30, ebb toward 210."""
    s = amp * np.sin(2 * np.pi * (T + shift) / PERIOD)
    a = np.radians(30 + bias_dir)
    return np.column_stack([s * np.sin(a), s * np.cos(a)])


def test_perfect_layer_scores_zero_error_and_zero_lag():
    ref = tide()
    m = score_currents(T, ref[:, 0], ref[:, 1], lambda sh: tide(sh))
    assert m["speed_rmse"] == pytest.approx(0, abs=1e-9) and m["vector_rmse"] == pytest.approx(0, abs=1e-9)
    assert m["lag_min"] == 0 and m["dir_mae"] == pytest.approx(0, abs=1e-6)
    assert m["axis_deg"] == pytest.approx(30, abs=0.5)


@pytest.mark.parametrize("lag_min", [-48, 30, 90])
def test_phase_lag_sign_positive_means_layer_turns_later(lag_min):
    ref = tide()
    # a layer whose tide turns lag_min later than the reference: layer(t) = ref(t - lag)
    layer = lambda sh: tide(sh - lag_min * 60.0)
    m = score_currents(T, ref[:, 0], ref[:, 1], layer)
    assert m["lag_min"] == lag_min
    assert m["along_rmse_at_best_lag"] == pytest.approx(0, abs=1e-9)


def test_speed_bias_and_direction_error():
    ref = tide()
    m = score_currents(T, ref[:, 0], ref[:, 1], lambda sh: 1.2 * tide(sh, bias_dir=10.0))
    assert m["speed_bias"] > 0 and m["speed_rmse"] == pytest.approx(0.2 * np.sqrt(0.5), rel=0.02)
    # direction is compared where the reference exceeds 0.3 m/s; flood and ebb both read +10 deg off
    assert m["dir_mae"] == pytest.approx(10, abs=0.1) and m["dir_bias"] == pytest.approx(10, abs=0.1)
