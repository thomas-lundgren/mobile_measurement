# AppRequest (`pkt_t = 0x04`)

The object request of an `app_traffic` flow. Unlike the other packets it travels
on a **TCP** connection the UE opened to the flow's server port. A connection
carries one request at a time; the UE keeps up to 6 connections per agent, reuses
them and closes them after 20 s idle, as browsers and apps do.

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

| Code | Name | Code | Name |
|------|------|------|------|
| 0 | `other` | 5 | `mail_send` |
| 1 | `web_page` | 6 | `video_chunk` |
| 2 | `web_object` | 7 | `feed_image` |
| 3 | `mail_sync` | 8 | `photo_upload` |
| 4 | `mail_attachment` | | |

Golden vectors: `contracts/testvectors/packets/app_request_v1.json`.
