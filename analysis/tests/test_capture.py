"""The server capture reader: a synthetic pcap (Linux cooked, IPv4) with a
handshake, downlink data, a retransmission, ACKs and an app UDP datagram."""

from __future__ import annotations

import gzip
import struct

from fieldtest_analysis import capture

SERVER, UE = 41003, 50000


def _ip(proto, tos, sport, dport, l4):
    total = 20 + len(l4)
    hdr = struct.pack("!BBHHHBBH4s4s", 0x45, tos, total, 0, 0, 64, proto, 0, b"\x0a\0\0\1", b"\x0a\0\0\2")
    return hdr + l4


def _tcp(sport, dport, seq, ack, flags, payload=0):
    hdr = struct.pack("!HHIIBBHHH", sport, dport, seq, ack, 5 << 4, flags, 65535, 0, 0)
    return _ip(6, 0, sport, dport, hdr + b"\0" * payload)


def _udp(sport, dport, tos, payload):
    return _ip(17, tos, sport, dport, struct.pack("!HHHH", sport, dport, 8 + payload, 0) + b"\0" * payload)


def _pcap(records, snap=128):
    out = struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, snap, 113)
    for t, ip in records:
        frame = struct.pack("!HHH8sH", 0, 1, 6, b"", 0x0800) + ip  # SLL: 16 B
        cap = frame[:snap]
        out += struct.pack("<IIII", int(t), round((t % 1) * 1e6), len(cap), len(frame)) + cap
    return out


def _records():
    S, A = 0x02, 0x10
    return [
        (100.000, _tcp(UE, SERVER, 999, 0, S)),
        (100.020, _tcp(SERVER, UE, 4999, 1000, S | A)),
        (100.040, _tcp(UE, SERVER, 1000, 5000, A, 200)),  # request
        (100.050, _tcp(SERVER, UE, 5000, 1200, A, 1400)),
        (100.051, _tcp(SERVER, UE, 6400, 1200, A, 1400)),
        (100.080, _tcp(UE, SERVER, 1200, 7800, A)),  # ACKs both: one sample, 29 ms
        (100.090, _tcp(SERVER, UE, 7800, 1200, A, 1400)),
        (100.300, _tcp(SERVER, UE, 7800, 1200, A, 1400)),  # retransmission
        (100.340, _tcp(UE, SERVER, 1200, 9200, A)),  # no sample (Karn)
        (100.400, _udp(UE, SERVER, 46 << 2, 1200)),  # app burst datagram, EF
        (100.500, _udp(1234, 9999, 0, 10)),  # another port: ignored
    ]


def test_flow_summary(tmp_path):
    (tmp_path / "server").mkdir()
    (tmp_path / "server" / "capture.pcap.gz").write_bytes(gzip.compress(_pcap(_records())))
    f = capture.analyze_file(capture.path(tmp_path), {SERVER: 4})[4]
    assert f["connections"] == 1
    assert f["rtt_samples"] == 1 and f["rtt_ms"]["p50"] == 29.0
    down, up = f["down"], f["up"]
    assert (down["packets"], down["data_segments"], down["retransmitted"]) == (5, 4, 1)
    assert down["max_segment"] == 1400 and down["retrans_rate"] == 0.25
    assert (up["pure_acks"], up["ack_ip_bytes"], up["udp_datagrams"]) == (2, 40, 1)
    assert up["dscp"] == {0: 4, 46: 1}
    assert up["data_segments"] == 1 and up["retransmitted"] == 0


def test_truncated_capture_and_raw_file_preferred(tmp_path):
    (tmp_path / "server").mkdir()
    data = _pcap(_records())
    (tmp_path / "server" / "capture.pcap").write_bytes(data[:-30])  # killed mid-record
    (tmp_path / "server" / "capture.pcap.gz").write_bytes(b"partial")
    assert capture.path(tmp_path).name == "capture.pcap"
    f = capture.analyze_file(capture.path(tmp_path), {SERVER: 4})[4]
    assert f["up"]["udp_datagrams"] == 1  # the cut record was the other port's


def test_no_capture(tmp_path):
    assert capture.path(tmp_path) is None
