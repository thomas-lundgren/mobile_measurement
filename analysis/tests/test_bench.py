"""mp-probe records and radio-log lines (bench.py)."""

from __future__ import annotations

import gzip
import json

from fieldtest_analysis import bench

PCC = ("         1792472351.068  2830  3415 D RILJ    : [UNSL]< UNSOL_PHYSICAL_CHANNEL_CONFIG [{mConnectionStatus=PrimaryServing,"
       "mCellBandwidthDownlinkKhz=10000,mCellBandwidthUplinkKhz=10000,mNetworkType=LTE,mFrequencyRange=MID,"
       "mDownlinkChannelNumber=1750,mUplinkChannelNumber=19750,mContextIds=[2, 3],mPhysicalCellId=414,mBand=3,"
       "mDownlinkFrequency=1860000,mUplinkFrequency=1765000}, {mConnectionStatus=SecondaryServing,"
       "mCellBandwidthDownlinkKhz=100000,mCellBandwidthUplinkKhz=100000,mNetworkType=NR,mFrequencyRange=MID,"
       "mDownlinkChannelNumber=644640,mUplinkChannelNumber=644640,mContextIds=[2],mPhysicalCellId=81,mBand=78,"
       "mDownlinkFrequency=3669600,mUplinkFrequency=3669600}] [PHONE0]")  # fmt: skip
LCE = ("1792471707.480  2830  3411 D RILJ    : [UNSL]< UNSOL_LCE_INFO_RECV [{mType=0, mDownlinkCapacityKbps=7507, "
       "mUplinkCapacityKbps=2400}, {mType=1, mDownlinkCapacityKbps=0, mUplinkCapacityKbps=0}] [PHONE0]")  # fmt: skip


def test_channel_config_lists_every_carrier():
    r = bench.parse_logcat_line(PCC)
    assert r["kind"] == "channel_config" and r["utc_ns"] == 1792472351_068000000
    assert r["summary"] == "LTE B3 PCI 414 10/10 MHz + NR B78 PCI 81 100/100 MHz (secondary)"
    assert json.loads(r["payload"])[1]["mDownlinkFrequency"] == "3669600"


def test_lce_and_ignored_lines():
    r = bench.parse_logcat_line(LCE)
    assert r["kind"] == "lce" and r["summary"] == "DL 7507 / UL 2400 kbit/s"
    assert bench.parse_logcat_line("1792471707.480  1 2 D Foo: bar") is None


def test_probe_log_gives_qdisc_and_radio_rows_and_test_file_wins(tmp_path):
    mobile = tmp_path / "mobile"
    mobile.mkdir()
    lines = [
        {"t": 100, "k": "start", "version": "0.2.0"},
        {"t": 200, "k": "q", "dev": "rmnet1", "qdisc": "mq", "b": 10, "p": 2, "d": 0, "o": 0, "r": 5, "bb": 300, "bp": 2},
        {"t": 300, "k": "u", "SndbufErrors": 1},
        {"t": 1792471707480000000, "k": "l", "line": LCE},
    ]
    with gzip.open(mobile / bench.PROBE_LOG, "wt") as f:
        f.writelines(json.dumps(x) + "\n" for x in lines)
        f.write('{"t": 400, "k": "q", "dev": "rmn')  # cut off mid-line by an interrupted fetch
    q, r = bench.read_probe_log(mobile / bench.PROBE_LOG)
    assert q.to_dict("records") == [{"utc_ns": 200, "dev": "rmnet1", "qdisc": "mq", "sent_bytes": 10, "sent_pkts": 2,
                                     "dropped": 0, "overlimits": 0, "requeues": 5, "backlog_bytes": 300,
                                     "backlog_pkts": 2}]  # fmt: skip
    assert r["kind"].tolist() == ["lce"]
    # the test's own file is used (no bench dir needed); the window filters
    q, r = bench.load_for_test(tmp_path, None, 150, 250)
    assert len(q) == 1 and r.empty
    # no file: falls back to the (absent) bench dir -> empty frames
    q, r = bench.load_for_test(tmp_path / "other", None, 0, 1)
    assert q.empty and r.empty
