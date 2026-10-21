# Upload protocol

Resolves remediation 1.3 (upload protocol) and M4b (test-result reporting).

Uploads are **strictly secondary** to measurement (invariant 1). A test
writes all its files to local durable storage first; upload runs later, from
sealed files only, and may fail, stall, or be deferred indefinitely without
ever touching an active measurement. Everything below is about moving
already-sealed files to the server.

## What gets uploaded

A test directory on the **mobile** node (see `CLAUDE.md` log layout):

```
session_<date>/test_NNN/
  header.json            # 1/test
  gps.parquet            # mobile only, 1/test
  radio.parquet          # mobile only, 1/test
  mobile/
    packets.parquet
    events.parquet
    clock.parquet
    probe.jsonl.gz       # mp-probe records, when it runs
```

The mobile uploads exactly these files. The Parquet tables are written
**segmented** (`<table>_NNNN.parquet`, see `../parquet/README.md`) — e.g.
`mobile/packets_0000.parquet`, `mobile/packets_0001.parquet`, … — so each
segment is its own manifest entry and its own PUT. (The bare `<table>.parquet`
form is also accepted for a single-file table.) The server already holds its
own `server/` recordings for the same `test_id`; on completion it drops the
mobile's files into the same `test_id` directory to assemble the full tree
(spec §13). The server never uploads — it is the destination.

## Three phases

All endpoints require TLS and `Authorization: Bearer <session_token>`. The
server derives `device_id` from the token and **rejects any `test_id` that
does not belong to that device** (no client-claimed identity).

### Phase 1 — initiate (summary + manifest)

```http
POST /api/v1/tests/{test_id}/summary
```

Body: [`upload_manifest.schema.json`](../schemas/upload_manifest.schema.json)
— the test result (`COMPLETED` / `PARTIAL` / `FAILED` / `CANCELLED`, see
below) plus a manifest listing every file: relative path, byte size,
SHA-256 of the **stored bytes**, and transport `encoding`.

Response: [`upload_init_response.schema.json`](../schemas/upload_init_response.schema.json)
— a server-assigned `upload_id` and `needed_paths`: the subset of manifest
files the server does **not** already hold with a matching hash. On a fresh
test that is every file; on a resumed upload it is only the gaps. This is
the resume mechanism (file granularity).

`POST /summary` is **idempotent**: re-posting the same manifest for the same
`test_id` returns the same `upload_id` and a recomputed `needed_paths`.

### Held uploads (away from the home network)

A server started with `MP_UPLOAD_HOLD` holds a device's uploads while the
device is not on the server's local network (and has enough free storage).
A held upload still takes the test's **base files** — `header.json`,
`gps.parquet`, `radio.parquet`, `mobile/clock*.parquet`,
`mobile/events*.parquet` (`testvectors/upload/paths.json` `base`;
server-main `uploads::is_base_path`) — so every test shows its route and
radio on the server at once:

1. `POST /summary` → `200` with `needed_paths` = the base files it still
   lacks, or `503` when it has them all (nothing more is taken while away).
2. The device PUTs those files.
3. `POST /complete` → `409` (the rest is missing): the test stays pending on
   the device.

The device treats `503` on the summary and `409` on complete as **held**:
it moves on to its next pending test, and retries the held ones after a
network change (home Wi-Fi lifts the hold), not on a short backoff.
Posting it is also how the device reports a test result with **no files**
(e.g. a `FAILED` test that produced nothing): manifest `files` is empty.

### Phase 2 — upload each needed file

```http
PUT /api/v1/tests/{test_id}/upload/{upload_id}/files/{path}
Content-Type: application/octet-stream
```

One PUT per path in `needed_paths`. `{path}` is the manifest-relative path
(e.g. `mobile/packets.parquet`), percent-encoded. The body is the file
bytes in the declared `encoding`. The server verifies size + SHA-256 and
stores the file; a mismatching hash is rejected `422` and the file stays in
`needed_paths`.

PUTs are **idempotent and order-independent** — re-PUT overwrites. Files may
be uploaded in any order and across multiple sessions.

**Large-file resume (optional).** A client MAY resume a single interrupted
file with `Content-Range: bytes <start>-<end>/<total>`; the server appends
and re-verifies the whole-file hash at the end. Parquet files are segmented
and individually small, so file-level resume (re-PUT the whole file) is the
expected path; byte-range resume is an optimisation, not required for
conformance.

### Phase 3 — complete

```http
POST /api/v1/tests/{test_id}/upload/{upload_id}/complete
```

The server verifies that every manifest file is present with a matching
hash, assembles the tree under `test_id`, and marks the test's upload state
`COMPLETE`. If any file is missing/mismatched it responds `409` with the
outstanding `needed_paths`; the client returns to Phase 2.

## Retry & resume summary

- **Connection dropped mid-upload** → reconnect, re-`POST /summary`, upload
  only the returned `needed_paths`, `POST /complete`.
- **App killed between files** → on next upload opportunity, same recovery;
  no client-side upload journal required beyond the sealed files themselves.
- **Server already has a file** (hash match) → it is omitted from
  `needed_paths`; never re-sent.
- **Backoff** → client retries with exponential backoff; upload never
  blocks or is blocked by measurement.

## Compression

`encoding` is declared **per file** in the manifest:

- `identity` — **default for `*.parquet`**. Parquet is already zstd-
  compressed internally (`core/log_writer.rs`); re-compressing the file
  wastes CPU for ~no gain.
- `zstd` — allowed for `*.json` (`header.json`), which are
  small and compress well. Optional; `identity` is always acceptable.

The SHA-256 in the manifest is over the **stored (post-encoding) bytes** —
i.e. what travels on the wire — so the server verifies without decoding.

## Test result (M4b)

The `test_result` in the manifest is the terminal state from
`enums.yaml test_results`:

| Result | Meaning |
|---|---|
| `COMPLETED` | Ran to the scheduled `duration_sec`; every flow produced data. |
| `PARTIAL` | Started and logged data, but ended before `duration_sec` (OS kill, crash, connectivity to server lost while logging continued locally) **or** at least one flow produced no data while others did. |
| `FAILED` | Could not start, or aborted before any usable data was logged. |
| `CANCELLED` | Operator `STOP_TEST` before the scheduled end. |

`result_reason` is an optional free-text string for diagnostics
(`"foreground service killed at t+412s"`).

**Note (deliberate deviation from remediation M4b):** packet loss does
**not** make a test `PARTIAL`. Loss is the quantity under measurement — a
run with 40% loss is a *successful* measurement of a bad link, not a partial
test. `PARTIAL` is about completeness of the **recording**, never the
quality of the network.
