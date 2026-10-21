"""Tolerant loader for an aggregated test tree (spec §12.1).

A test directory looks like::

    test_NNN/
      header.json            # optional
      gps.parquet            # mobile only, optional
      radio.parquet          # mobile only, optional
      mobile/  { packets, events, clock }.parquet  (+ probe.jsonl.gz)
      server/  { packets, events, clock }.parquet

Everything is optional: a mobile-only upload (no ``server/``, no ``clock``,
no ``header``) loads fine; per-node ``packets.parquet`` may be **segmented**
(``packets_0000.parquet`` …). Unknown columns are preserved; missing canonical
columns are added as NA so downstream code can rely on them existing.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path

import pandas as pd
import pyarrow.parquet as pq

from . import schema


#: "PAR1" + footer length + "PAR1": the smallest file that can be valid parquet.
MIN_PARQUET_BYTES = 12


def _normalize(df: pd.DataFrame, columns: list[str]) -> pd.DataFrame:
    """Guarantee `columns` exist (NA if absent), cast time columns to int64,
    and keep any extra columns the file happened to carry."""
    for col in columns:
        if col not in df.columns:
            df[col] = pd.NA
    for col in df.columns:
        if col in schema.TIME_COLUMNS:
            # uint64 ns -> nullable Int64 so diffs are signed and NA-safe.
            df[col] = pd.to_numeric(df[col], errors="coerce").astype("Int64")
    return df


def read_one(path: Path, columns: list[str]) -> pd.DataFrame | None:
    if not path.exists():
        return None
    df = pq.read_table(path).to_pandas()
    return _normalize(df, columns)


def _read_segmented(node_dir: Path, stem: str, columns: list[str]) -> pd.DataFrame | None:
    """Read all ``<stem>*.parquet`` segments, concatenated. Covers a single
    ``<stem>.parquet``, node-wide ``<stem>_NNNN.parquet`` (mobile), and the
    server's flow-scoped ``<stem>_fN_NNNN.parquet`` segments."""
    if not node_dir.is_dir():
        return None
    # A segment shorter than a parquet header + footer was never closed by its
    # writer (e.g. a cancelled test); skip it rather than lose the whole test.
    parts = [p for p in sorted(node_dir.glob(f"{stem}*.parquet")) if p.stat().st_size >= MIN_PARQUET_BYTES]
    if not parts:
        return None
    frames = [pq.read_table(p).to_pandas() for p in parts]
    return _normalize(pd.concat(frames, ignore_index=True), columns)


#: Kernel receive times further than this before the app's read are not trusted.
MAX_READ_LAG_NS = 10_000_000_000


def _arrival_times(pk: pd.DataFrame) -> pd.DataFrame:
    """Make a RECEIVED row's time its arrival, not the app's read.

    Where the row carries a kernel receive time (``kernel_rx_utc_ns``,
    ``SO_TIMESTAMPNS``), ``utc_ns`` becomes that time and ``mono_ns`` moves
    back by the same read lag, so RTT, one-way delay and arrival gaps leave
    out how late the receiving thread got to the packet. The app's read time
    stays in ``read_utc_ns`` (every row; equal to ``utc_ns`` where unchanged).
    """
    pk["read_utc_ns"] = pk["utc_ns"]
    if pk.empty:
        return pk
    kernel = pd.to_numeric(pk["kernel_rx_utc_ns"], errors="coerce").astype("Int64")
    lag = pk["utc_ns"] - kernel
    ok = ((pk["direction"] == schema.DIR_RECEIVED) & lag.notna() & (lag >= 0) & (lag < MAX_READ_LAG_NS)).fillna(False)
    if ok.any():
        pk.loc[ok, "utc_ns"] = kernel[ok]
        pk.loc[ok, "mono_ns"] = pk.loc[ok, "mono_ns"] - lag[ok]
    return pk


def receive_times(recv: pd.DataFrame) -> tuple[pd.Series, str]:
    """Receive times (UTC ns, float) of a stream's RECEIVED rows and which
    clock they are on: the kernel's when at least 90 % of the rows carry it
    (``"kernel"``; a row without falls back to its read time), else the app's
    read time throughout (``"app"``) — never a mix within one stream. Works on
    loaded frames and on raw rows alike."""
    read = pd.to_numeric(recv["read_utc_ns"] if "read_utc_ns" in recv else recv["utc_ns"], errors="coerce")
    if "kernel_rx_utc_ns" in recv:
        kernel = pd.to_numeric(recv["kernel_rx_utc_ns"], errors="coerce")
        if len(kernel) and kernel.notna().mean() > 0.9:
            return kernel.fillna(read).astype("float64"), "kernel"
    return read.astype("float64"), "app"


@dataclass
class TestData:
    """All recordings for one test, normalized. Frames may be empty but the
    canonical columns always exist."""

    path: Path
    header: dict = field(default_factory=dict)
    packets: pd.DataFrame = field(default_factory=lambda: pd.DataFrame(columns=schema.PACKETS))
    clock: pd.DataFrame = field(default_factory=lambda: pd.DataFrame(columns=schema.CLOCK))
    events: pd.DataFrame = field(default_factory=lambda: pd.DataFrame(columns=schema.EVENTS))
    gps: pd.DataFrame | None = None
    radio: pd.DataFrame | None = None

    @property
    def nodes(self) -> list[str]:
        if self.packets.empty:
            return []
        return sorted(self.packets["node"].dropna().unique().tolist())

    @property
    def test_id(self) -> str | None:
        for src in (self.packets, self.radio, self.gps):
            if src is not None and not src.empty and src["test_id"].notna().any():
                return str(src["test_id"].dropna().iloc[0])
        return self.header.get("test_id")


def load_test(test_dir: str | Path) -> TestData:
    """Load one aggregated test tree, tolerating any missing file or column."""
    root = Path(test_dir)
    if not root.is_dir():
        raise FileNotFoundError(f"test dir not found: {root}")

    header: dict = {}
    header_path = root / "header.json"
    if header_path.exists():
        header = json.loads(header_path.read_text())

    packet_frames: list[pd.DataFrame] = []
    clock_frames: list[pd.DataFrame] = []
    event_frames: list[pd.DataFrame] = []
    for node in ("mobile", "server"):
        node_dir = root / node
        pk = _read_segmented(node_dir, "packets", schema.PACKETS)
        if pk is not None:
            pk["node"] = pk["node"].fillna(node)
            packet_frames.append(pk)
        ck = _read_segmented(node_dir, "clock", schema.CLOCK)
        if ck is not None:
            ck["node"] = ck["node"].fillna(node)
            clock_frames.append(ck)
        ev = _read_segmented(node_dir, "events", schema.EVENTS)
        if ev is not None:
            ev["node"] = ev["node"].fillna(node)
            event_frames.append(ev)

    def _concat(frames, cols):
        if not frames:
            return pd.DataFrame(columns=cols)
        return pd.concat(frames, ignore_index=True)

    return TestData(
        path=root,
        header=header,
        packets=_arrival_times(_concat(packet_frames, schema.PACKETS)),
        clock=_concat(clock_frames, schema.CLOCK),
        events=_concat(event_frames, schema.EVENTS),
        gps=read_one(root / "gps.parquet", schema.GPS),
        radio=read_one(root / "radio.parquet", schema.RADIO),
    )
