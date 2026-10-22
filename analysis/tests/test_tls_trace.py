"""TLS record tracking in a headers-only capture: records that span
segments, several records in one segment, and request/reply exchanges."""

from __future__ import annotations

import struct

from fieldtest_analysis import tls_trace

from test_capture import _pcap

C, S = 50000, 443


def _seg(sport, dport, seq, payload: bytes, flags=0x18):
    tcp = struct.pack("!HHIIBBHHH", sport, dport, seq, 0, 5 << 4, flags, 65535, 0, 0) + payload
    ue, server = b"\x0a\0\0\1", b"\x0a\0\0\2"
    src, dst = (ue, server) if dport == S else (server, ue)
    return struct.pack("!BBHHHBBH4s4s", 0x45, 0, 20 + len(tcp), 0, 0, 64, 6, 0, src, dst) + tcp


def _rec(n, ctype=23):
    return struct.pack("!BHH", ctype, 0x0303, n) + b"\0" * n


def test_records_and_exchanges(tmp_path):
    big = _rec(3000)  # request record spanning three segments
    req = [big[:1400], big[1400:2800], big[2800:]]
    recs = [
        (10.000, _seg(C, S, 99, b"", 0x02)),
        (10.010, _seg(S, C, 499, b"", 0x12)),
        (10.020, _seg(C, S, 100, req[0])),
        (10.021, _seg(C, S, 1500, req[1])),
        (10.022, _seg(C, S, 2900, req[2])),
        (10.060, _seg(S, C, 500, b"", 0x10)),  # pure ACK
        (10.900, _seg(S, C, 500, _rec(140))),  # first token event
        (10.950, _seg(S, C, 645, _rec(140) + _rec(118))),  # two in one segment
        (12.000, _seg(C, S, 3105, _rec(800))),  # next request
        (12.500, _seg(S, C, 913, _rec(160))),
    ]
    f = tmp_path / "t.pcap"
    f.write_bytes(_pcap(recs, snap=256))
    s = tls_trace.summarize(f)
    e1, e2 = s["exchanges"]
    assert (e1["up_bytes"], e1["down_records"], e1["down_bytes"]) == (3000, 3, 398)
    assert e1["first_byte_ms"] == 878.0
    assert (e2["up_bytes"], e2["down_bytes"], e2["first_byte_ms"]) == (800, 160, 500.0)
    assert s["pure_acks"]["down"] == 1
    # a snap length that cuts the second header loses that record, then resyncs
    f.write_bytes(_pcap(recs, snap=200))
    e1, e2 = tls_trace.summarize(f)["exchanges"]
    assert (e1["down_records"], e2["down_bytes"]) == (2, 160)
