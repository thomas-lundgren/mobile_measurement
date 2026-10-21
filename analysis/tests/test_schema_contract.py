"""The loader's canonical columns match ``contracts/parquet/*.parquet.yaml``.

The contract is the single source; ``schema.py`` keeps a copy so the package
runs without the repo, and this test keeps the copy honest (the Rust writers
have the same check in ``core/tests/parquet_contract.rs``).
"""

from __future__ import annotations

from pathlib import Path

import pytest
import yaml

from fieldtest_analysis import schema

CONTRACTS = Path(__file__).resolve().parents[2] / "contracts" / "parquet"

#: Contract tables the analysis does not read (yet).
NOT_READ = {"pings", "traffic"}


def _contract(table: str) -> set[str]:
    doc = yaml.safe_load((CONTRACTS / f"{table}.parquet.yaml").read_text())
    return {c["name"] for c in (doc.get("join_columns") or []) + (doc.get("columns") or [])}


def _tables() -> list[str]:
    return sorted(p.name.removesuffix(".parquet.yaml") for p in CONTRACTS.glob("*.parquet.yaml"))


@pytest.mark.parametrize("table", _tables())
def test_columns_match_contract(table):
    cols = getattr(schema, table.upper(), None)
    if table in NOT_READ:
        assert cols is None, f"{table} is read now: drop it from NOT_READ"
        return
    assert cols is not None, f"schema.py has no column list for {table}"
    want = _contract(table)
    assert set(cols) == want, f"missing {sorted(want - set(cols))}, extra {sorted(set(cols) - want)}"
    assert len(cols) == len(set(cols)), "duplicate column"


# --- logged values (contracts/enums/enums.yaml) -------------------------------

ENUMS = yaml.safe_load((CONTRACTS.parent / "enums" / "enums.yaml").read_text())


def test_logged_value_constants_match_the_contract():
    assert [schema.NODE_MOBILE, schema.NODE_SERVER] == ENUMS["log_nodes"]
    assert [schema.DIR_SENT, schema.DIR_RECEIVED] == ENUMS["packet_directions"]
    assert [schema.PROTO_UDP, schema.PROTO_TCP] == ENUMS["packet_protocols"]
    openapi = yaml.safe_load((CONTRACTS.parent / "openapi" / "openapi.yaml").read_text())
    assert schema.TRANSPORT_WIFI in openapi["components"]["schemas"]["NetworkTransport"]["enum"]


def test_every_radio_mode_has_a_label():
    from fieldtest_analysis.findings import RADIO_MODE_LABELS

    assert set(RADIO_MODE_LABELS) == set(ENUMS["radio_network_types"])
