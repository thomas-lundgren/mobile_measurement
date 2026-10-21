# ProbePacket (`pkt_t = 0x01`)

The probe packet is the smallest measurement primitive — a tiny UDP datagram
echoed by the loopback peer so the originator can compute RTT and detect
loss. Framing rules: see [README.md](README.md).

## Version 1 (`ver = 0x01`)

Core size: **20 bytes**. On-wire size: **22 bytes** (header + core, no
padding expected).

<!-- BEGIN GENERATED: ProbePacket v1 (contracts/packets/layouts.yaml; contracts/codegen/packet_codec.py) -->
| Offset | Width | Field | Type | Notes |
|--------|-------|-------|------|-------|
| 0 | 1 | `pkt_t` | u8 | Always `0x01`. |
| 1 | 1 | `ver` | u8 | `0x01` for this layout. |
| 2 | 4 | `flow_id` | u32 BE | Server-assigned per `RUN_TEST`. |
| 6 | 8 | `sequence_number` | u64 BE | Monotonic per `(flow_id, sender)`; starts at 0. |
| 14 | 8 | `send_mono_ns` | u64 BE | Sender's `CLOCK_MONOTONIC` at transmit. |

Core: 20 bytes; 22 on the wire.
<!-- END GENERATED: ProbePacket v1 -->

The echoer **does not modify** the packet — it returns the bytes verbatim.
The originator computes RTT from `now_mono_ns - send_mono_ns` on receipt.

## Growth path

Any change to the field set — adding `send_utc_ns`, an echoer-side dwell
timestamp, a payload-size hint, etc. — is a new version (`ver = 0x02`) with
its own `core_size` entry. No optional-field encoding lives inside v1.
