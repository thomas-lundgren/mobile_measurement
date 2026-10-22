"""Evidence for the app-traffic parameters from a headers-only capture of a
real application's TLS traffic (``contracts/app_traffic/parameters.yaml``:
values move from ``unverified`` to ``verified`` with such a trace as source).

Content stays encrypted and is never captured beyond the first bytes of a
segment (snap length 128); what the trace shows is sizes and timing. Per TCP
connection to the server port, the TLS record headers are followed along
the sequence space, so every application-data record has a size even when
it spans segments. An **exchange** is an uplink burst of records (a
request) and the downlink records that follow until the next request:

- ``up_bytes`` (the request, e.g. the context an LLM call sends),
- ``first_byte_ms`` (last request record → first reply record: think time
  plus one RTT),
- ``down_records``, their size (``record_bytes``; TLS 1.3 adds 22 B to each
  plaintext record), the gaps between them and the reply's duration,
- per trace: uplink / downlink bytes and pure ACKs.

    python -m fieldtest_analysis.tls_trace <capture.pcap[.gz]> [--port 443] [--json]
"""

from __future__ import annotations

import argparse
import gzip
import json
import struct
import sys
from pathlib import Path

from .capture import _ETHERNET, _IPV4, _IPV6, _RAW, _RAW_ALT, _SLL, _SLL2
from .stats import pcts

_TLS_TYPES = (20, 21, 22, 23)
_APP_DATA = 23
_MOD = 1 << 32


def _segments(file: Path, port: int):
    """``(t, conn, direction, rel_seq, payload_len, head, flags)`` per TCP
    segment to or from ``port``; ``head`` is the captured payload prefix."""
    opener = gzip.open if file.suffix == ".gz" else open
    isn: dict = {}
    with opener(file, "rb") as f:
        head = f.read(24)
        endian = "<" if head[:4] in (b"\xd4\xc3\xb2\xa1", b"\x4d\x3c\xb2\xa1") else ">"
        frac = 1e-9 if head[:4] in (b"\x4d\x3c\xb2\xa1", b"\xa1\xb2\x3c\x4d") else 1e-6
        link = struct.unpack(endian + "I", head[20:24])[0] & 0x0FFFFFFF
        rec = struct.Struct(endian + "IIII")
        while (h := f.read(16)) and len(h) == 16:
            sec, sub, caplen, _ = rec.unpack(h)
            d = f.read(caplen)
            if len(d) < caplen:
                return
            p = _tcp(d, link)
            if p is None:
                continue
            src, dst, sport, dport, seq, flags, l4_len, hdr, payload = p
            if dport == port:
                conn, direction = (src, sport), "up"
            elif sport == port:
                conn, direction = (dst, dport), "down"
            else:
                continue
            key = (conn, direction)
            if flags & 0x02:
                isn[key] = (seq + 1) % _MOD
                continue
            base = isn.setdefault(key, seq)
            yield (sec + sub * frac, conn, direction, (seq - base) % _MOD, max(0, l4_len - hdr), payload, flags)


def _tcp(d: bytes, link: int):
    off = {_ETHERNET: 14, _SLL: 16, _SLL2: 20, _RAW: 0, _RAW_ALT: 0}.get(link)
    if off is None or len(d) < off + 20:
        return None
    v = d[off] >> 4
    if v == 4:
        ihl, proto = (d[off] & 0x0F) * 4, d[off + 9]
        l4_len = struct.unpack_from("!H", d, off + 2)[0] - ihl
        src, dst, l4 = d[off + 12 : off + 16], d[off + 16 : off + 20], off + ihl
    elif v == 6 and len(d) >= off + 40:
        proto, l4_len = d[off + 6], struct.unpack_from("!H", d, off + 4)[0]
        src, dst, l4 = d[off + 8 : off + 24], d[off + 24 : off + 40], off + 40
    else:
        return None
    if proto != 6 or len(d) < l4 + 14:
        return None
    sport, dport, seq = struct.unpack_from("!HHI", d, l4)
    hdr = (d[l4 + 12] >> 4) * 4
    return src, dst, sport, dport, seq, d[l4 + 13], l4_len, hdr, d[l4 + hdr :]


def records(file: Path, port: int = 443):
    """``[t, conn, direction, type, length, t_end]`` per TLS record whose
    header was captured, in arrival order (``t``: the segment carrying the
    header, ``t_end``: the one completing the record); plus pure-ACK counts
    per direction."""
    nxt: dict = {}  # (conn, direction) -> relative seq where the next record starts
    unfinished: dict = {}  # (conn, direction) -> (index in out, relative end) of a record still arriving
    acks = {"up": 0, "down": 0}
    out = []
    for t, conn, direction, rel, n, head, flags in _segments(file, port):
        if n == 0:
            if flags & 0x10:
                acks[direction] += 1
            continue
        key = (conn, direction)
        if key in unfinished and rel + n >= unfinished[key][1]:
            out[unfinished.pop(key)[0]][5] = t
        start = nxt.get(key)
        if start is None or start < rel:  # no sync yet, or lost it: a segment starting a record resyncs
            start = rel if head[:1] and head[0] in _TLS_TYPES else None
        if start is None:
            nxt.pop(key, None)
            continue
        # walk every record header that begins inside this segment
        while rel <= start < rel + n:
            at = start - rel
            if at + 5 > len(head):
                break  # header beyond the snap length: lose sync until a segment starts one
            ctype, length = head[at], struct.unpack_from("!H", head, at + 3)[0]
            if ctype not in _TLS_TYPES:
                start = None
                break
            start += 5 + length
            out.append([t, conn, direction, ctype, length, t if start <= rel + n else None])
            if start > rel + n:
                unfinished[key] = (len(out) - 1, start)
        if start is None or start < rel:
            nxt.pop(key, None)
        else:
            nxt[key] = start
    return out, acks


def exchanges(recs) -> list[dict]:
    """Request/reply exchanges per connection from application-data records."""
    by_conn: dict = {}
    for t, conn, direction, ctype, length, t_end in recs:
        if ctype == _APP_DATA:
            # a request counts from when it was all sent; a reply from its first byte
            at = (t_end or t) if direction == "up" else t
            by_conn.setdefault(conn, []).append((at, direction, length))
    out = []
    for conn, rs in by_conn.items():
        cur = None
        for t, direction, length in rs:
            if direction == "up":
                if cur is None or cur["down"]:
                    if cur is not None:
                        out.append(cur)
                    cur = {"conn": conn, "start": t, "up": [], "down": []}
                cur["up"].append((t, length))
            elif cur is not None:
                cur["down"].append((t, length))
        if cur is not None:
            out.append(cur)
    return sorted(out, key=lambda e: e["start"])


def summarize(file: Path, port: int = 443) -> dict:
    recs, acks = records(file, port)
    ex = []
    for e in exchanges(recs):
        up_bytes = sum(n for _, n in e["up"])
        down = e["down"]
        if not down:
            continue
        gaps = [(b[0] - a[0]) * 1e3 for a, b in zip(down, down[1:])]
        ex.append({
            "start": round(e["start"], 3),
            "up_bytes": up_bytes,
            "up_records": len(e["up"]),
            "first_byte_ms": round((down[0][0] - e["up"][-1][0]) * 1e3, 1),
            "down_bytes": sum(n for _, n in down),
            "down_records": len(down),
            "record_bytes": pcts([n for _, n in down], ps=(10, 50, 90)),
            "gap_ms": pcts(gaps, ps=(50, 90)) if gaps else None,
            "reply_ms": round((down[-1][0] - down[0][0]) * 1e3, 1),
        })  # fmt: skip
    up = sum(n for _, _, d, c, n, _ in recs if d == "up" and c == _APP_DATA)
    down = sum(n for _, _, d, c, n, _ in recs if d == "down" and c == _APP_DATA)
    return {
        "file": str(file),
        "exchanges": ex,
        "up_bytes": up,
        "down_bytes": down,
        "ul_dl_ratio": round(up / down, 2) if down else None,
        "pure_acks": acks,
        "record_bytes_down": pcts([n for _, _, d, c, n, _ in recs if d == "down" and c == _APP_DATA], ps=(10, 50, 90)),
    }


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("pcap", type=Path)
    ap.add_argument("--port", type=int, default=443)
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args(argv)
    s = summarize(a.pcap, a.port)
    if a.json:
        print(json.dumps(s, indent=1))
        return 0
    print(f"{s['file']}: up {s['up_bytes']} B, down {s['down_bytes']} B, UL:DL {s['ul_dl_ratio']}, "
          f"pure ACKs {s['pure_acks']}, down record B {s['record_bytes_down']}")  # fmt: skip
    for e in s["exchanges"]:
        print(f"  t={e['start']:.1f} up {e['up_bytes']} B → first byte {e['first_byte_ms']} ms, "
              f"{e['down_records']} records / {e['down_bytes']} B over {e['reply_ms']} ms, "
              f"record {e['record_bytes']}, gap {e['gap_ms']}")  # fmt: skip
    return 0


if __name__ == "__main__":
    sys.exit(main())
