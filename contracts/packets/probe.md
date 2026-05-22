# ProbePacket (`pkt_t = 0x01`)

The probe packet is the smallest measurement primitive — a tiny UDP datagram
echoed by the loopback peer so the originator can compute RTT and detect
loss. Framing rules: see [README.md](README.md).

## Version 1 (`ver = 0x01`)

Core size: **20 bytes**. On-wire size: **22 bytes** (header + core, no
padding expected).

| Offset | Width | Field             | Type   | Notes |
|--------|-------|-------------------|--------|-------|
| 0      | 1     | `pkt_t`           | u8     | Always `0x01`. |
| 1      | 1     | `ver`             | u8     | `0x01` for this layout. |
| 2      | 4     | `flow_id`         | u32 BE | Server-assigned per `RUN_TEST`. |
| 6      | 8     | `sequence_number` | u64 BE | Monotonic per `(flow_id, sender)`; starts at 0. |
| 14     | 8     | `send_mono_ns`    | u64 BE | Sender's `CLOCK_MONOTONIC` at transmit. |

The echoer **does not modify** the packet — it returns the bytes verbatim.
The originator computes RTT from `now_mono_ns - send_mono_ns` on receipt.

## Growth path

Any change to the field set — adding `send_utc_ns`, an echoer-side dwell
timestamp, a payload-size hint, etc. — is a new version (`ver = 0x02`) with
its own `core_size` entry. No optional-field encoding lives inside v1.

## Reference: legacy in-repo layout

The current `core/src/packet.rs` implements an unframed 20-byte layout
(`flow_id`, `sequence_number`, `send_mono_ns` only — no `pkt_t` / `ver`
prefix). This is **spike code from Stage A** and predates this contract;
Phase 1 will update it to the v1 framing above.
