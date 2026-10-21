# App-traffic UDP burst (`pkt_t = 0x05` – `0x08`)

The `udp_burst` engine of an `app_traffic` flow (test plan `engines`): one
object moved as `count` paced UDP datagrams instead of a TCP exchange — a
video segment, a frame upload — so the same workload can be compared over TCP
and over paced UDP. It uses the flow's server port over UDP (the TCP side
serves `exchange` and `bulk`). The phone opens every burst (invariant 5); the
server does what each request says and knows nothing about the workload.

Layouts are generated from [`layouts.yaml`](layouts.yaml); this page adds the
protocol.

## Downlink (`direction = 0`)

1. The phone sends `AppBurstRequest` from the socket it will receive on,
   again every 300 ms until the first datagram arrives (at most 3 times).
2. The server, once per `(flow_id, object_id)`, waits `delay_ms`, then sends
   `count` `AppBurstData` datagrams of `size` bytes, `interval_us` apart, to
   the request's source address, marked with `dscp` unless `0xFF`.
3. The phone reads until it has `count` datagrams or none came for 1 s past
   the burst's expected end.

## Uplink (`direction = 1`)

1. The phone sends `AppBurstRequest` (so the server knows the burst), then
   `count` `AppBurstData` datagrams, `interval_us` apart.
2. Then `AppBurstEnd` with how many it sent, again every 300 ms (at most 3
   times) until the server's `AppBurstAck` arrives with how many it received.

A burst whose datagrams went missing still ended: the outcome is `ok` and
the loss shows as received against planned datagrams
(`app_events.parquet` `packets` / `plan_packets`). `timeout`: no datagram
(downlink) or no acknowledgement (uplink) at all.

## Logs

Both nodes log every data datagram they send or receive as a `packets`
row: protocol UDP, `sequence_number` = `object_id`, `app_packet_index` =
`index`, `app_activity`, the sender's timestamps on received rows (one-way
delay), the kernel receive time and the received DSCP where available.

## AppBurstRequest (`0x05`)

<!-- BEGIN GENERATED: AppBurstRequest v1 (contracts/packets/layouts.yaml; contracts/codegen/packet_codec.py) -->
| Offset | Width | Field | Type | Notes |
|--------|-------|-------|------|-------|
| 0 | 1 | `pkt_t` | u8 | Always `0x05`. |
| 1 | 1 | `ver` | u8 | `0x01` for this layout. |
| 2 | 4 | `flow_id` | u32 BE | Server-assigned per `RUN_TEST`. |
| 6 | 8 | `object_id` | u64 BE | As `AppExchange`: the `sequence_number` of every row of this object. |
| 14 | 1 | `activity` | u8 | Activity code (`enums.yaml` app_activities). |
| 15 | 1 | `direction` | u8 | `0` = downlink (the server sends), `1` = uplink (the phone sends). |
| 16 | 1 | `dscp` | u8 | DSCP (0–63) to mark the server's datagrams with; `0xFF` = unmarked. |
| 17 | 1 | `_reserved` | u8 | Reserved, `0`. |
| 18 | 4 | `count` | u32 BE | Datagrams in the burst. |
| 22 | 4 | `size` | u32 BE | UDP payload bytes per datagram (an `AppBurstData` header, zero-padded). |
| 26 | 4 | `interval_us` | u32 BE | Spacing between datagrams (µs): the pacing. |
| 30 | 4 | `delay_ms` | u32 BE | Downlink: server think time before the first datagram. |

Core: 32 bytes; 34 on the wire.
<!-- END GENERATED: AppBurstRequest v1 -->

## AppBurstData (`0x06`)

<!-- BEGIN GENERATED: AppBurstData v1 (contracts/packets/layouts.yaml; contracts/codegen/packet_codec.py) -->
| Offset | Width | Field | Type | Notes |
|--------|-------|-------|------|-------|
| 0 | 1 | `pkt_t` | u8 | Always `0x06`. |
| 1 | 1 | `ver` | u8 | `0x01` for this layout. |
| 2 | 4 | `flow_id` | u32 BE | Server-assigned per `RUN_TEST`. |
| 6 | 8 | `object_id` | u64 BE | The burst's object. |
| 14 | 4 | `index` | u32 BE | Position in the burst, from 0. |
| 18 | 4 | `count` | u32 BE | Datagrams in the burst. |
| 22 | 8 | `send_mono_ns` | u64 BE | Sender's `CLOCK_MONOTONIC` at transmit. |
| 30 | 8 | `send_utc_ns` | u64 BE | Sender's `CLOCK_REALTIME` at transmit. |

Core: 36 bytes; 38 on the wire.
<!-- END GENERATED: AppBurstData v1 -->

## AppBurstEnd (`0x07`)

<!-- BEGIN GENERATED: AppBurstEnd v1 (contracts/packets/layouts.yaml; contracts/codegen/packet_codec.py) -->
| Offset | Width | Field | Type | Notes |
|--------|-------|-------|------|-------|
| 0 | 1 | `pkt_t` | u8 | Always `0x07`. |
| 1 | 1 | `ver` | u8 | `0x01` for this layout. |
| 2 | 4 | `flow_id` | u32 BE | Server-assigned per `RUN_TEST`. |
| 6 | 8 | `object_id` | u64 BE | The burst's object. |
| 14 | 4 | `sent` | u32 BE | Datagrams the phone sent. |

Core: 16 bytes; 18 on the wire.
<!-- END GENERATED: AppBurstEnd v1 -->

## AppBurstAck (`0x08`)

<!-- BEGIN GENERATED: AppBurstAck v1 (contracts/packets/layouts.yaml; contracts/codegen/packet_codec.py) -->
| Offset | Width | Field | Type | Notes |
|--------|-------|-------|------|-------|
| 0 | 1 | `pkt_t` | u8 | Always `0x08`. |
| 1 | 1 | `ver` | u8 | `0x01` for this layout. |
| 2 | 4 | `flow_id` | u32 BE | Server-assigned per `RUN_TEST`. |
| 6 | 8 | `object_id` | u64 BE | The burst's object. |
| 14 | 4 | `received` | u32 BE | Datagrams the server received. |

Core: 16 bytes; 18 on the wire.
<!-- END GENERATED: AppBurstAck v1 -->
