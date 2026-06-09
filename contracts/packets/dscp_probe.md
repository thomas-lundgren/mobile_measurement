# DscpProbePacket (`pkt_t = 0x03`)

A UDP measurement packet used by the `dscp_probe` flow to test DSCP transparency
in both uplink and downlink. Like ProbePacket, the server echoes each packet — but
the echo carries the DSCP values actually observed at the server plus the DSCP the
server marks on the echo, letting the UE compute both directions' transparency from
one round trip.

## Version 1 (`ver = 0x01`)

Core size: **24 bytes**. On-wire size: **26 bytes** (2-byte header + core).

| Offset | Width | Field              | Type   | Notes |
|--------|-------|--------------------|--------|-------|
| 0      | 1     | `pkt_t`            | u8     | Always `0x03`. |
| 1      | 1     | `ver`              | u8     | `0x01` for this layout. |
| 2      | 4     | `flow_id`          | u32 BE | Server-assigned per `RUN_TEST`. |
| 6      | 8     | `sequence_number`  | u64 BE | Monotonic per `(flow_id, sender)`. |
| 14     | 8     | `send_mono_ns`     | u64 BE | Sender's `CLOCK_MONOTONIC` at transmit. |
| 22     | 1     | `dscp_intended_ul` | u8     | DSCP value (0–63) UE set on the uplink socket. |
| 23     | 1     | `dscp_actual_ul`   | u8     | DSCP actually received by the server (from IP TOS via `IP_RECVTOS`). Set to `0xFF` in UE's first send; filled by server in echo. |
| 24     | 1     | `dscp_intended_dl` | u8     | DSCP the server set on the echo socket. Set to `0xFF` in UE's first send; filled by server in echo. |
| 25     | 1     | `_pad`             | u8     | Reserved, always `0x00`. |

## Protocol

**UE sends (uplink):**
- Sets IP TOS byte to `dscp_intended_ul << 2` on the socket before `send()`.
- Cycles `dscp_intended_ul` through `dscp_values[seq % len]` from the flow config.
- Fills `dscp_actual_ul = 0xFF`, `dscp_intended_dl = 0xFF` (unknown at send time).

**Server receives and echoes:**
- Reads IP TOS from `recvmsg` ancillary data (`IP_RECVTOS` / `IPV6_RECVTCLASS`).
- Sets `dscp_actual_ul = (received_tos >> 2) & 0x3F`.
- Chooses `dscp_intended_dl` (typically mirrors `dscp_intended_ul`).
- Sets IP TOS to `dscp_intended_dl << 2` on the echo socket.
- Echoes the packet with the three DSCP fields updated.

**UE receives echo:**
- Reads IP TOS from `recvmsg` ancillary data (`IP_RECVTOS`).
- Computes `dscp_actual_dl = (received_tos >> 2) & 0x3F`.
- Uplink transparency: `dscp_intended_ul` vs `dscp_actual_ul` (from echo payload).
- Downlink transparency: `dscp_intended_dl` (from echo payload) vs `dscp_actual_dl` (IP header).

## packets.parquet columns

| Column      | Logged by | Value |
|-------------|-----------|-------|
| `dscp_sent` | sender    | `dscp_intended_ul` (UE SENT) or `dscp_intended_dl` (server SENT echo) |
| `dscp_recv` | receiver  | `dscp_actual_ul` (server RECEIVED) or `dscp_actual_dl` (UE RECEIVED echo) |

Both columns are `uint8 nullable`: present for `dscp_probe` flow rows, null for all other flow types.

## Analysis

```python
# Uplink transparency
ul = packets[(packets.flow_type == "dscp_probe") & (packets.node == "server")
             & (packets.direction == "RECEIVED")]
ul["transparent"] = ul["dscp_recv"] == ul["dscp_sent"]

# Downlink transparency
dl = packets[(packets.flow_type == "dscp_probe") & (packets.node == "mobile")
             & (packets.direction == "RECEIVED")]
dl["transparent"] = dl["dscp_recv"] == dl["dscp_sent"]
```
