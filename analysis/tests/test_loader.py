"""Loader tolerance: real mobile-only trees + synthetic missing/unknown columns."""

from __future__ import annotations

import pandas as pd
import pyarrow as pa
import pyarrow.parquet as pq

from fieldtest_analysis import schema
from fieldtest_analysis.loader import load_test


def test_loads_real_mobile_only_tree(udp_test_dir):
    td = load_test(udp_test_dir)
    # segmented packets concatenated
    assert not td.packets.empty
    assert td.packets["node"].unique().tolist() == ["mobile"]
    # canonical columns guaranteed present even though the file lacks some
    for col in schema.PACKETS:
        assert col in td.packets.columns
    # mobile-only: no server, no clock, no header
    assert td.nodes == ["mobile"]
    assert td.clock.empty
    assert td.header == {}
    # aux traces present in this capture
    assert td.gps is not None and not td.gps.empty
    assert td.radio is not None and not td.radio.empty
    assert td.test_id is not None


def test_segments_concatenated(bidir_test_dir):
    td = load_test(bidir_test_dir)
    # four packet segments → all rows present, all four flows
    assert sorted(td.packets["flow_id"].dropna().unique().tolist()) == [1, 2, 3, 4]
    assert td.gps is None  # this capture had no GPS fix


def test_missing_dir_raises(tmp_path):
    import pytest

    with pytest.raises(FileNotFoundError):
        load_test(tmp_path / "nope")


def test_unknown_columns_preserved_and_missing_filled(tmp_path):
    # a packets file missing `protocol` but carrying an unknown `future_field`
    node = tmp_path / "mobile"
    node.mkdir()
    tbl = pa.table(
        {
            "flow_id": pa.array([1, 1], pa.uint32()),
            "sequence_number": pa.array([0, 1], pa.uint64()),
            "direction": ["SENT", "RECEIVED"],
            "mono_ns": pa.array([10, 20], pa.uint64()),
            "utc_ns": pa.array([100, 200], pa.uint64()),
            "future_field": ["x", "y"],  # unknown — must be tolerated
        }
    )
    pq.write_table(tbl, node / "packets.parquet")

    td = load_test(tmp_path)
    assert "protocol" in td.packets.columns  # missing canonical filled with NA
    assert td.packets["protocol"].isna().all()
    assert "future_field" in td.packets.columns  # unknown preserved
    # time columns coerced to signed nullable Int64
    assert str(td.packets["mono_ns"].dtype) == "Int64"
    assert td.packets["node"].tolist() == ["mobile", "mobile"]


def test_unclosed_segment_is_skipped(twonode_test_dir, tmp_path):
    import shutil

    d = tmp_path / "t"
    shutil.copytree(twonode_test_dir, d)
    (d / "server" / "packets_f9_0000.parquet").write_bytes(b"PAR1")
    td = load_test(d)
    assert set(td.nodes) == {"mobile", "server"}


def test_received_rows_take_the_kernel_arrival_time(tmp_path):
    # RECEIVED with a kernel stamp 300 ns before the app read it; one without;
    # a SENT row (never moved); a stamp after the read (bogus, ignored)
    node = tmp_path / "mobile"
    node.mkdir()
    tbl = pa.table(
        {
            "flow_id": pa.array([1, 1, 1, 1], pa.uint32()),
            "sequence_number": pa.array([0, 1, 2, 3], pa.uint64()),
            "direction": ["RECEIVED", "RECEIVED", "SENT", "RECEIVED"],
            "mono_ns": pa.array([1_000, 2_000, 3_000, 4_000], pa.uint64()),
            "utc_ns": pa.array([10_000, 20_000, 30_000, 40_000], pa.uint64()),
            "kernel_rx_utc_ns": pa.array([9_700, None, 29_000, 40_500], pa.uint64()),
        }
    )
    pq.write_table(tbl, node / "packets.parquet")

    pk = load_test(tmp_path).packets.sort_values("sequence_number")
    assert pk["utc_ns"].tolist() == [9_700, 20_000, 30_000, 40_000]
    assert pk["mono_ns"].tolist() == [700, 2_000, 3_000, 4_000]
    assert pk["read_utc_ns"].tolist() == [10_000, 20_000, 30_000, 40_000]
