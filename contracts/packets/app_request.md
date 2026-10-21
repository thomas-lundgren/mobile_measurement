# AppRequest (`pkt_t = 0x04`)

The object request of an `app_traffic` flow. Unlike the other packets it travels
on a **TCP** connection the UE opened to the flow's server port. A connection
carries one request at a time; the UE keeps up to 6 connections per agent, reuses
them and closes them after 20 s idle, as browsers and apps do.

The server reads `pkt_t` and `ver` first and serves both versions. UEs send v2
from Logger builds with the `agent` profile on.

## Version 2 (`ver = 0x02`) — AppExchange

One exchange: bytes up, a server think time, bytes down. Covers v1's download
and upload, and adds what an LLM call (context up, time to first token, a paced
token stream) and a tool call (request body, latency, result) need.

Core size: **42 bytes**. On-wire size: **44 bytes**.

| Offset | Width | Field         | Type   | Notes |
|--------|-------|---------------|--------|-------|
| 0      | 1     | `pkt_t`       | u8     | Always `0x04`. |
| 1      | 1     | `ver`         | u8     | `0x02` for this layout. |
| 2      | 1     | `activity`    | u8     | Activity code (table below). |
| 3      | 1     | reserved      | u8     | `0`. |
| 4      | 4     | `flow_id`     | u32 BE | Server-assigned per `RUN_TEST`. |
| 8      | 8     | `object_id`   | u64 BE | As v1. |
| 16     | 8     | `up_bytes`    | u64 BE | Bytes that follow the header (request body, LLM context, upload). |
| 24     | 8     | `down_bytes`  | u64 BE | Response size; `0` = no response, only the acknowledgement. |
| 32     | 4     | `delay_ms`    | u32 BE | Server think time after the upload, before the first response byte. |
| 36     | 4     | `rate_bps`    | u32 BE | Pace the response at this rate (bit/s); `0` = as fast as TCP takes it. |
| 40     | 4     | `chunk_bytes` | u32 BE | Paced: bytes per write (an SSE event); `0` = 5 ms worth, at most 64 KiB. |

Protocol: the server reads `up_bytes`, waits `delay_ms`, then sends
`down_bytes`. Paced, chunk *k* is due *k* · `chunk_bytes` · 8 / `rate_bps`
after the think time: the reply is generated at that rate whatever the network
does, so what a stall holds back goes out in one write when it clears (as an
LLM server's would). With `down_bytes = 0` the server echoes the 44-byte header
once the upload is in.

Logs: both nodes tag every row with `app_activity`, `app_object_bytes`
(`down_bytes`) and `app_up_bytes`.

Golden vectors: `contracts/testvectors/packets/app_exchange_v2.json`.

## Version 1 (`ver = 0x01`)

Core size: **22 bytes**. On-wire size: **24 bytes** (2-byte header + core).

| Offset | Width | Field       | Type   | Notes |
|--------|-------|-------------|--------|-------|
| 0      | 1     | `pkt_t`     | u8     | Always `0x04`. |
| 1      | 1     | `ver`       | u8     | `0x01` for this layout. |
| 2      | 1     | `kind`      | u8     | `0` = download, `1` = upload. |
| 3      | 1     | `activity`  | u8     | What the object is for (table below). Logged by both nodes. |
| 4      | 4     | `flow_id`   | u32 BE | Server-assigned per `RUN_TEST`. |
| 8      | 8     | `object_id` | u64 BE | `agent << 48 \| episode << 20 \| object`; the `sequence_number` of every row of this object. |
| 16     | 8     | `size`      | u64 BE | Object size in bytes. |

## Protocol

- **Download** (`kind = 0`): the server answers with `size` bytes (zeros).
- **Upload** (`kind = 0x01`): `size` bytes follow the header; the server echoes
  the 24-byte header back once it has received them all, so the UE sees when
  the upload completed.

## Activities

The codes are the list `app_activities` in `contracts/enums/enums.yaml`
(code = position; entries are only appended).

`cloud_tool` and `llm_resume` (agent with tools in the cloud): the provider's
reply to one LLM call, continued after a tool ran on its side — the tool
result text, then the next turn's tokens. Each is its own exchange with
`up_bytes = 0`; its 44-byte request is a protocol artifact (a real provider
keeps streaming on the open response).

Golden vectors: `contracts/testvectors/packets/app_request_v1.json`.
