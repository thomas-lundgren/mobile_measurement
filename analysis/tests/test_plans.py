from __future__ import annotations

import json
import sqlite3

from fieldtest_analysis import plans


def _db(tmp_path):
    db = tmp_path / "mp.db"
    with sqlite3.connect(db) as c:
        c.execute("CREATE TABLE tests (test_id TEXT, plan_name TEXT, plan_json TEXT)")
        c.execute("INSERT INTO tests VALUES ('tst_a', 'from-db', ?)",
                  (json.dumps({"flows": [{"flow_id": 1, "type": "probe", "rate_pps": 50}]}),))
    return str(db)


def test_header_plan_wins_and_db_is_the_fallback(tmp_path):
    db_plans = plans.from_db(_db(tmp_path))
    old = tmp_path / "ses_x" / "tst_a"
    old.mkdir(parents=True)
    p = plans.for_test(old, db_plans)
    assert plans.name(p) == "from-db"
    assert plans.flows(p)[1]["rate_pps"] == 50

    (old / "header.json").write_text(json.dumps(
        {"test_plan": {"name": "from-header", "flows": [{"flow_id": 3, "type": "realtime_udp"}]}}))
    p = plans.for_test(old, db_plans)
    assert plans.name(p) == "from-header"
    assert list(plans.flows(p)) == [3]


def test_unknown_test_has_no_plan(tmp_path):
    assert plans.for_test(tmp_path / "tst_none", {}) is None
    assert plans.flows(None) == {} and plans.name(None) is None
