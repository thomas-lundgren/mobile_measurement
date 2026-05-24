"""Tolerant loader for an aggregated test tree (spec §12.1).

A test directory looks like::

    test_NNN/
      header.json            # optional
      gps.parquet            # mobile only, optional
      radio.parquet          # mobile only, optional
      mobile/  { packets, events, clock }.parquet  (+ summary.json)
      server/  { packets, events, clock }.parquet  (+ summary.json)

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


def _read_one(path: Path, columns: list[str]) -> pd.DataFrame | None:
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
    parts = sorted(node_dir.glob(f"{stem}*.parquet"))
    if not parts:
        return None
    frames = [pq.read_table(p).to_pandas() for p in parts]
    return _normalize(pd.concat(frames, ignore_index=True), columns)


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
        packets=_concat(packet_frames, schema.PACKETS),
        clock=_concat(clock_frames, schema.CLOCK),
        events=_concat(event_frames, schema.EVENTS),
        gps=_read_one(root / "gps.parquet", schema.GPS),
        radio=_read_one(root / "radio.parquet", schema.RADIO),
    )
