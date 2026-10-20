"""Bench collector parsers (tc qdisc dump, radio log lines)."""

from __future__ import annotations

import json

from fieldtest_analysis import bench

TC = """qdisc mq 0: dev rmnet1 root
 Sent 10756435752 bytes 10021970 pkt (dropped 3, overlimits 0 requeues 90122)
 backlog 64216b 44p requeues 90122
qdisc pfifo_fast 0: dev rmnet1 parent :2 bands 3 priomap  1 2 2 2 1 2 0 0 1 1 1 1 1 1 1 1
 Sent 54812032 bytes 1022554 pkt (dropped 0, overlimits 0 requeues 0)
 backlog 0b 0p requeues 0
qdisc noqueue 0: dev lo root refcnt 2
 Sent 0 bytes 0 pkt (dropped 0, overlimits 0 requeues 0)
 backlog 0b 0p requeues 0
"""

PCC = ("         1792472351.068  2830  3415 D RILJ    : [UNSL]< UNSOL_PHYSICAL_CHANNEL_CONFIG [{mConnectionStatus=PrimaryServing,"
       "mCellBandwidthDownlinkKhz=10000,mCellBandwidthUplinkKhz=10000,mNetworkType=LTE,mFrequencyRange=MID,"
       "mDownlinkChannelNumber=1750,mUplinkChannelNumber=19750,mContextIds=[2, 3],mPhysicalCellId=414,mBand=3,"
       "mDownlinkFrequency=1860000,mUplinkFrequency=1765000}, {mConnectionStatus=SecondaryServing,"
       "mCellBandwidthDownlinkKhz=100000,mCellBandwidthUplinkKhz=100000,mNetworkType=NR,mFrequencyRange=MID,"
       "mDownlinkChannelNumber=644640,mUplinkChannelNumber=644640,mContextIds=[2],mPhysicalCellId=81,mBand=78,"
       "mDownlinkFrequency=3669600,mUplinkFrequency=3669600}] [PHONE0]")  # fmt: skip
LCE = ("1792471707.480  2830  3411 D RILJ    : [UNSL]< UNSOL_LCE_INFO_RECV [{mType=0, mDownlinkCapacityKbps=7507, "
       "mUplinkCapacityKbps=2400}, {mType=1, mDownlinkCapacityKbps=0, mUplinkCapacityKbps=0}] [PHONE0]")  # fmt: skip


def test_tc_dump_gives_root_qdisc_per_device():
    rows = bench.parse_tc(TC, 123)
    assert [r["dev"] for r in rows] == ["rmnet1", "lo"]
    r = rows[0]
    assert r["requeues"] == 90122 and r["dropped"] == 3
    assert r["backlog_bytes"] == 64216 and r["backlog_pkts"] == 44 and r["utc_ns"] == 123


def test_channel_config_lists_every_carrier():
    r = bench.parse_logcat_line(PCC)
    assert r["kind"] == "channel_config" and r["utc_ns"] == 1792472351_068000000
    assert r["summary"] == "LTE B3 PCI 414 10/10 MHz + NR B78 PCI 81 100/100 MHz (secondary)"
    assert json.loads(r["payload"])[1]["mDownlinkFrequency"] == "3669600"


def test_lce_and_ignored_lines():
    r = bench.parse_logcat_line(LCE)
    assert r["kind"] == "lce" and r["summary"] == "DL 7507 / UL 2400 kbit/s"
    assert bench.parse_logcat_line("1792471707.480  1 2 D Foo: bar") is None
