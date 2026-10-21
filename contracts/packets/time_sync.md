# Time sync — UDP clock exchange (UE ↔ server-main)

The UE's clock sampler (`clock.parquet`, invariant 2) measures its offset from
the server clock with an NTP-style exchange. The server advertises the port in
`GET /api/v1/time` (`ServerTime.time_udp_port`); that HTTP call is also the
fallback when UDP does not get through. No auth: it reveals only the clock.

Deliberately **not** framed like the measurement packets (no `pkt_t`/`ver`):
the request is opaque so any UDP datagram works, and a change of layout gets
a new port rather than a version byte.

## Exchange

Request (UE → server): one datagram, any length. Its first 8 bytes (fewer if
shorter) are a nonce chosen by the UE per request.

Reply (server → UE): one datagram, exactly 24 bytes, big-endian:

| Offset | Bytes | Field            | Type | Meaning |
|-------:|------:|------------------|------|---------|
| 0      | 8     | `nonce`          | raw  | The request's first 8 bytes, zero-padded if it was shorter. |
| 8      | 8     | `t2_recv_utc_ns` | i64  | Server UTC when the request was read. |
| 16     | 8     | `t3_send_utc_ns` | i64  | Server UTC just before the reply was sent. |

The UE keeps a reply only if it is 24 bytes and the nonce matches; with its
own send (t1) and receive (t4) times it computes delay = (t4 − t1) − (t3 − t2)
on its monotonic clock and offset = ((t1 − t2) + (t4 − t3)) / 2 on UTC, and
keeps the lowest-delay sample of a burst.

Golden vectors: `contracts/testvectors/packets/time_sync_v1.json`
(Rust: `server/server-main/src/time_sync.rs`; Java: the Logger's unit tests).
