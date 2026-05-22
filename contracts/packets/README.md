# Wire packets — framing & versioning

This directory pins the bytes that travel between the data-generator on one
node and the traffic engine on the other. Layouts are hand-authored;
cross-language conformance is enforced by the golden vectors in
`../testvectors/packets/`.

Packets are expected to grow. Growth happens by **bumping the per-type
format version**, not by runtime-extensible field encoding.

## Framing (all packet types)

```
+--------+--------+----------------------+
| pkt_t  |  ver   |    core fields       |
| (u8)   |  (u8)  |    fixed by (pkt_t, ver)
+--------+--------+----------------------+
   0        1         2 .. 2+core_size
```

- **`pkt_t` (offset 0, u8)** — packet type. Registry below.
- **`ver` (offset 1, u8)** — per-type format version. Initial = `0x01`.
- **Core fields** — fixed width determined by `(pkt_t, ver)`. Big-endian
  (network byte order) for every multi-byte integer.

Trailing bytes beyond `2 + core_size` are **ignored on receive** (used for
padding realtime UDP to a target `packet_size`; see [`realtime.md`](realtime.md)).

Over TCP the framing is the same, but a length prefix is added by the
transport layer (bulk flows do not use this packet format — they stream
opaque bytes with a transport-level sequence).

## Packet-type registry

| `pkt_t` | Name | Core size (v1) | Spec |
|---|---|---|---|
| `0x01` | `Probe` | 20 bytes | [`probe.md`](probe.md) |
| `0x02` | `Realtime` | 28 bytes | [`realtime.md`](realtime.md) |
| `0x03` – `0x7F` | reserved | — | — |
| `0x80` – `0xFF` | private | — | — |

## Growth: bump `ver`

Adding, removing, renaming, widening, or reordering a core field — all
require allocating a new `(pkt_t, ver)` entry. Senders and receivers ship
together (monorepo, lockstep release), so a coordinated bump is cheap.

The per-language size table maps `(pkt_t, ver) → core_size`. Receivers
reject unknown `(pkt_t, ver)` pairs with a typed error.

Old `ver` values stay supported in the size table for the compatibility
window declared in `versions/versions.yaml` (`protocol_version` bump policy).

## Conformance

Every implementation (Rust core, Java/Kotlin codec, Python analysis loader)
must:

1. Round-trip every vector in `../testvectors/packets/` byte-for-byte.
2. Decode a packet with trailing padding bytes by reading exactly
   `core_size` bytes and ignoring the rest.
3. Reject a `(pkt_t, ver)` it does not support with a typed error, not a
   panic.

CI runs the conformance suite against every language's codec on each PR.

## Not in scope

- **Bulk TCP flows** — these stream payload bytes without this header.
  Their per-flow framing (length-prefixed chunks, optional in-band
  timestamps) is handled in `bulk_tcp.md` (to be authored alongside the
  bulk-TCP generator in Phase 1).
- **Authenticity / integrity** — wire packets are unauthenticated; the
  measurement is the value, and tampering only corrupts your own data.
  Control-plane auth (challenge/response) protects the API, not these
  UDP payloads.
