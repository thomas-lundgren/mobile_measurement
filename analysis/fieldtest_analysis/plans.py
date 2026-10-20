"""What each test ran: its test plan (name, flows with their rates and
sizes). The test's own ``header.json`` (written by the Logger at test start,
spec §12.2) is the source; tests recorded before the Logger wrote it fall
back to the server database (``tests.plan_json`` / ``plan_name``).
"""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path


def from_db(db: str | None) -> dict[str, dict]:
    """Every test's plan from the server database, by test_id (fallback)."""
    if not db:
        return {}
    out = {}
    with sqlite3.connect(f"file:{db}?mode=ro", uri=True) as c:
        for test_id, name, plan_json in c.execute("SELECT test_id, plan_name, plan_json FROM tests"):
            try:
                plan = json.loads(plan_json) if plan_json else {}
            except ValueError:
                plan = {}
            plan.setdefault("name", name)
            out[test_id] = plan
    return out


def for_test(test_dir: Path, db_plans: dict[str, dict] | None = None, header: dict | None = None) -> dict | None:
    """The plan a test ran: from its header (pass it if already loaded),
    else from ``db_plans``."""
    if header is None:
        path = Path(test_dir) / "header.json"
        try:
            header = json.loads(path.read_text()) if path.exists() else {}
        except ValueError:
            header = {}
    plan = header.get("test_plan")
    if isinstance(plan, dict) and plan:
        return plan
    return (db_plans or {}).get(Path(test_dir).name)


def name(plan: dict | None) -> str | None:
    return (plan or {}).get("name")


def flows(plan: dict | None) -> dict[int, dict]:
    """The plan's flows by flow_id."""
    out = {}
    for f in (plan or {}).get("flows", []) or []:
        try:
            out[int(f["flow_id"])] = f
        except (KeyError, TypeError, ValueError):
            continue
    return out
