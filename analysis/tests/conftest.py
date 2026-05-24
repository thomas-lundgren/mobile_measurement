"""Shared fixtures: paths to the real captures + builders for synthetic data."""

from __future__ import annotations

from pathlib import Path

import pandas as pd
import pytest

from fieldtest_analysis import schema
from fieldtest_analysis.loader import TestData, _normalize

FIXTURES = Path(__file__).parent / "fixtures"


@pytest.fixture
def udp_test_dir() -> Path:
    return FIXTURES / "test_udp"


@pytest.fixture
def bidir_test_dir() -> Path:
    return FIXTURES / "test_bidir"


@pytest.fixture
def clocked_test_dir() -> Path:
    """Real capture that includes a device-written mobile/clock.parquet
    (SNTP-over-HTTP offsets) — exercises the disciplined OWD path."""
    return FIXTURES / "test_clocked"


def make_packets(rows: list[dict]) -> pd.DataFrame:
    return _normalize(pd.DataFrame(rows), schema.PACKETS)


def make_clock(rows: list[dict]) -> pd.DataFrame:
    return _normalize(pd.DataFrame(rows), schema.CLOCK)


def make_radio(rows: list[dict]) -> pd.DataFrame:
    return _normalize(pd.DataFrame(rows), schema.RADIO)


def synthetic_test(packets=None, clock=None, radio=None, gps=None) -> TestData:
    td = TestData(path=Path("/synthetic"))
    if packets is not None:
        td.packets = packets
    if clock is not None:
        td.clock = clock
    td.radio = radio
    td.gps = gps
    return td
