# RealtimePacket (`pkt_t = 0x02`)

The realtime packet carries a fixed-rate unidirectional UDP stream used to
measure one-way delay and jitter. Unlike `Probe`, it is **not echoed**; the
receiver logs per-packet arrival timestamps directly. Both sender's
`mono_ns` and `utc_ns` are embedded so the receiver can compute OWD with
known clock-uncertainty bounds (post-processing only — see spec §11).

Framing rules: see [README.md](README.md).

## Version 1 (`ver = 0x01`)

Core size: **28 bytes**. On-wire minimum: **30 bytes** (header + core).

| Offset | Width | Field             | Type   | Notes |
|--------|-------|-------------------|--------|-------|
| 0      | 1     | `pkt_t`           | u8     | Always `0x02`. |
| 1      | 1     | `ver`             | u8     | `0x01` for this layout. |
| 2      | 4     | `flow_id`         | u32 BE | Server-assigned per `RUN_TEST`. |
| 6      | 8     | `sequence_number` | u64 BE | Monotonic per `(flow_id, sender)`; starts at 0. |
| 14     | 8     | `send_mono_ns`    | u64 BE | Sender's `CLOCK_MONOTONIC` at transmit. |
| 22     | 8     | `send_utc_ns`     | u64 BE | Sender's `CLOCK_REALTIME` at transmit. |

`send_mono_ns` and `send_utc_ns` are sampled **atomically** at the sender —
see the dual-clock invariant (invariant 2). UTC is never used for ordering;
it exists for cross-node OWD estimation.

## Padding to target `packet_size`

Realtime flows declare a target `packet_size` (per the test plan). The
sender appends `packet_size - 30` zero bytes after the core to hit the
target. For `packet_size = 64` the trailer is 34 zero bytes; for
`packet_size = 30` no padding is emitted.

Receivers MUST ignore trailing bytes beyond the core. The padding has no
on-wire structure — it exists only to control the size of the UDP datagram
for the throughput / inter-arrival measurement.

## Growth path

Any change to the field set is a new version (`ver = 0x02`) with its own
`core_size` entry.
