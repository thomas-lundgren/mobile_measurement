"""Spectral analysis on synthetic 1 ms streams."""

from __future__ import annotations

import numpy as np
from conftest import make_packets, synthetic_test

from fieldtest_analysis import spectral

MS = 1_000_000


def _stream(regrid_ms: float, n: int = 6000, seed: int = 1):
    """1 ms sender with ±200 µs jitter; the path holds each packet until the
    next multiple of `regrid_ms`, then delivers it 10 ms later."""
    rng = np.random.default_rng(seed)
    send = np.arange(n) * MS + rng.integers(-200_000, 200_000, n)
    grid = regrid_ms * MS
    rx = (np.ceil(send / grid) * grid).astype(np.int64) + 10 * MS + rng.integers(0, 20_000, n)
    rows = []
    for i, (s, r) in enumerate(zip(send, rx)):
        rows.append({"node": "mobile", "flow_id": 2, "sequence_number": i, "direction": "SENT", "protocol": "UDP",
                     "mono_ns": int(s), "utc_ns": int(s), "payload_size": 200, "send_status": "OK"})
        rows.append({"node": "server", "flow_id": 2, "sequence_number": i, "direction": "RECEIVED", "protocol": "UDP",
                     "mono_ns": int(r), "utc_ns": int(r), "kernel_rx_utc_ns": int(r),
                     "sender_mono_ns": int(s), "sender_utc_ns": int(s), "payload_size": 200})
    return synthetic_test(packets=make_packets(rows))


def test_point_spectrum_of_periodic_and_random_times():
    f = np.arange(2.0, 1500.5, 0.5)
    at = lambda p, hz: p[np.argmin(abs(f - hz))]  # noqa: E731
    periodic = spectral.point_spectrum(np.arange(4000) * 0.0025, f)
    assert at(periodic, 400) > 3000 and at(periodic, 1000) < 50  # 2.5 ms grid: 400 Hz, nothing at 1 kHz
    rnd = spectral.point_spectrum(np.sort(np.random.default_rng(0).uniform(0, 10, 4000)), f)
    assert np.median(rnd) < 3 and rnd.max() < 40


def test_regridding_on_the_path_shows_as_a_new_line_with_harmonics():
    [e] = spectral.analyze(_stream(regrid_ms=2.5))
    peaks = {p["hz"]: p for p in e["arrival_peaks"]}
    assert 400.0 in peaks and peaks[400.0]["added_by_path"]
    assert peaks[400.0]["fundamental_hz"] == 400.0
    assert any(p["fundamental_hz"] == 400.0 and p["hz"] == 800.0 for p in e["arrival_peaks"])


def test_fundamentals_group_harmonics():
    assert spectral.fundamentals([400.0, 800.0, 1200.0, 1000.0]) == {400.0: [400.0, 800.0, 1200.0], 1000.0: [1000.0]}
