"""The server's packet capture of a test (``server/capture.pcap[.gz]``,
headers only, written by test-control for the ports of bulk and app flows):
what TCP actually put on the wire next to the app log.

Per flow (matched by its server port in ``header.json`` ``flow_bindings``)
and direction (``down`` = server → UE, ``up`` = UE → server): packets and
bytes, TCP connections, data segment sizes, pure ACKs (count, IP size,
share), retransmitted segments, the DSCP values that arrived or left, UDP
datagrams (app bursts share the app port), and RTT samples from the
server's side — a downlink segment's send to the UE's ACK covering it,
skipped across retransmissions (Karn). Uplink "retransmissions" include
segments the network reordered (the capture sees arrivals).

Sizes are IP sizes (header + payload, no link layer): a bare TCP ACK is 40 B
over IPv4 (52 B with timestamps); on Ethernet that is the often quoted 54 B
(66 B). The reader is plain Python (pcap, Ethernet / Linux cooked / raw IP,
IPv4 / IPv6, TCP / UDP) so analysis needs no capture library.
"""

from __future__ import annotations

import gzip
import struct
from collections import Counter, deque
from pathlib import Path

from .stats import pcts

FILE = "capture.pcap"

_ETHERNET, _RAW, _RAW_ALT, _SLL, _SLL2 = 1, 101, 12, 113, 276
_IPV4, _IPV6 = 0x0800, 0x86DD
_TCP, _UDP = 6, 17
_SYN, _RST, _ACK = 0x02, 0x04, 0x10
_MOD = 1 << 32


def path(test_dir: Path) -> Path | None:
    """The capture file: the raw one while it exists (compression runs after
    the test and replaces it), else the compressed one."""
    server = Path(test_dir) / "server"
    for p in (server / FILE, server / f"{FILE}.gz"):
        if p.exists():
            return p
    return None


def packets(file: Path):
    """``(t_s, dscp, proto, src_port, dst_port, ip_len, tcp)`` per IP packet;
    ``tcp`` is ``(seq, ack, flags, payload_len)`` or None. A truncated last
    record (capture killed mid-write) ends the stream."""
    opener = gzip.open if file.suffix == ".gz" else open
    with opener(file, "rb") as f:
        head = f.read(24)
        if len(head) < 24:
            return
        magic = head[:4]
        if magic in (b"\xd4\xc3\xb2\xa1", b"\x4d\x3c\xb2\xa1"):
            endian = "<"
        elif magic in (b"\xa1\xb2\xc3\xd4", b"\xa1\xb2\x3c\x4d"):
            endian = ">"
        else:
            raise ValueError(f"not a pcap file: {file}")
        frac = 1e-9 if magic in (b"\x4d\x3c\xb2\xa1", b"\xa1\xb2\x3c\x4d") else 1e-6
        linktype = struct.unpack(endian + "I", head[20:24])[0] & 0x0FFFFFFF
        rec = struct.Struct(endian + "IIII")
        while True:
            h = f.read(16)
            if len(h) < 16:
                return
            sec, sub, caplen, _ = rec.unpack(h)
            data = f.read(caplen)
            if len(data) < caplen:
                return
            p = _parse(data, linktype)
            if p is not None:
                yield (sec + sub * frac, *p)


def _parse(d: bytes, linktype: int):
    if linktype == _ETHERNET:
        if len(d) < 14:
            return None
        ethertype, off = struct.unpack_from("!H", d, 12)[0], 14
        while ethertype in (0x8100, 0x88A8) and len(d) >= off + 4:  # VLAN tags
            ethertype, off = struct.unpack_from("!H", d, off + 2)[0], off + 4
    elif linktype == _SLL:
        if len(d) < 16:
            return None
        ethertype, off = struct.unpack_from("!H", d, 14)[0], 16
    elif linktype == _SLL2:
        if len(d) < 20:
            return None
        ethertype, off = struct.unpack_from("!H", d, 0)[0], 20
    elif linktype in (_RAW, _RAW_ALT):
        if not d:
            return None
        ethertype, off = (_IPV4 if d[0] >> 4 == 4 else _IPV6), 0
    else:
        return None

    if ethertype == _IPV4 and len(d) >= off + 20:
        ihl = (d[off] & 0x0F) * 4
        tos, ip_len = d[off + 1], struct.unpack_from("!H", d, off + 2)[0]
        proto, l4, l4_len = d[off + 9], off + ihl, ip_len - ihl
    elif ethertype == _IPV6 and len(d) >= off + 40:
        tos = ((d[off] & 0x0F) << 4) | (d[off + 1] >> 4)
        l4_len = struct.unpack_from("!H", d, off + 4)[0]
        proto, l4, ip_len = d[off + 6], off + 40, l4_len + 40
    else:
        return None
    if proto not in (_TCP, _UDP) or len(d) < l4 + 4:
        return None
    sport, dport = struct.unpack_from("!HH", d, l4)
    tcp = None
    if proto == _TCP:
        if len(d) < l4 + 14:
            return None
        seq, ack = struct.unpack_from("!II", d, l4 + 4)
        hdr = (d[l4 + 12] >> 4) * 4
        tcp = (seq, ack, d[l4 + 13], max(0, l4_len - hdr))
    return tos >> 2, proto, sport, dport, ip_len, tcp


class _Dir:
    def __init__(self):
        self.packets = self.bytes = self.udp = 0
        self.data_sizes: list[int] = []
        self.acks: list[int] = []
        self.retrans = 0
        self.dscp: Counter = Counter()


class _Conn:
    """Sequence state of one TCP connection, per direction."""

    def __init__(self):
        self.isn = {}  # direction -> first sequence number seen
        self.high = {"down": 0, "up": 0}  # highest relative end sent
        self.pending: deque = deque()  # (relative end, send time) of new downlink data


def analyze_file(file: Path, ports: dict[int, int]) -> dict[int, dict] | None:
    """Per flow id: per direction summary, for a capture and ``{server port:
    flow id}``."""
    dirs: dict[tuple[int, str], _Dir] = {}
    conns: dict[tuple, _Conn] = {}
    syns: Counter = Counter()
    rtt: dict[int, list[float]] = {}
    for t, dscp, proto, sport, dport, ip_len, tcp in packets(file):
        if sport in ports:
            port, direction, peer = sport, "down", dport
        elif dport in ports:
            port, direction, peer = dport, "up", sport
        else:
            continue
        flow = ports[port]
        d = dirs.setdefault((flow, direction), _Dir())
        d.packets += 1
        d.bytes += ip_len
        d.dscp[dscp] += 1
        if tcp is None:
            d.udp += 1
            continue
        seq, ack, flags, payload = tcp
        c = conns.setdefault((port, peer), _Conn())
        if flags & _SYN:
            if direction == "up":
                syns[flow] += 1
            c.isn[direction] = (seq + 1) % _MOD
            continue
        if payload == 0 and flags & _ACK and not flags & _RST:
            d.acks.append(ip_len)
        if payload:
            d.data_sizes.append(payload)
            isn = c.isn.setdefault(direction, seq)
            start = (seq - isn) % _MOD
            end = start + payload
            if start < c.high[direction]:
                d.retrans += 1
                if direction == "down":
                    c.pending.clear()  # Karn: no samples across a retransmission
            else:
                c.high[direction] = end
                if direction == "down":
                    c.pending.append((end, t))
        if direction == "up" and flags & _ACK and c.pending and "down" in c.isn:
            acked = (ack - c.isn["down"]) % _MOD
            sent = None
            while c.pending and c.pending[0][0] <= acked:
                sent = c.pending.popleft()[1]
            if sent is not None:
                rtt.setdefault(flow, []).append((t - sent) * 1e3)

    if not dirs:
        return None
    out: dict[int, dict] = {}
    for (flow, direction), d in sorted(dirs.items()):
        f = out.setdefault(flow, {"connections": syns.get(flow, 0), "rtt_ms": None, "rtt_samples": 0})
        segments = len(d.data_sizes)
        tcp_packets = d.packets - d.udp
        f[direction] = {
            "packets": d.packets,
            "bytes": d.bytes,
            "udp_datagrams": d.udp,
            "data_segments": segments,
            "segment_bytes": pcts(d.data_sizes) if d.data_sizes else None,
            "max_segment": max(d.data_sizes) if d.data_sizes else None,
            "pure_acks": len(d.acks),
            "ack_share": round(len(d.acks) / tcp_packets, 3) if tcp_packets else None,
            "ack_ip_bytes": Counter(d.acks).most_common(1)[0][0] if d.acks else None,
            "retransmitted": d.retrans,
            "retrans_rate": round(d.retrans / segments, 4) if segments else None,
            "dscp": {int(k): v for k, v in sorted(d.dscp.items())},
        }
    for flow, samples in rtt.items():
        out[flow]["rtt_ms"] = pcts(samples)
        out[flow]["rtt_samples"] = len(samples)
    return out


def analyze(td) -> dict | None:
    """Per flow id (as a string, for JSON): the capture summary, or None
    when the test has no capture."""
    file = path(td.path)
    if file is None:
        return None
    ports = {b["server_port"]: b["flow_id"] for b in td.header.get("flow_bindings") or [] if b.get("server_port")}
    if not ports:
        return None
    try:
        flows = analyze_file(file, ports)
    except (OSError, EOFError, ValueError):
        return None
    return None if flows is None else {str(k): v for k, v in flows.items()}
