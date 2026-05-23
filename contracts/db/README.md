# server-main database (resolves remediation 3.3)

`schema.sql` is the canonical DDL for server-main's control-plane database.
In Phase 1 it becomes the first sqlx migration under `server/migrations/`
(this file stays the single source; the migration is generated/copied from it,
and CI diffs the two).

## Engine & location

- **SQLite, WAL mode**, a single file on the server host (e.g.
  `/var/lib/mp/mp.db`). Chosen for field-experiment scale (tens of devices,
  low control-plane write rate): zero-ops, survives restarts, no separate
  server to run. sqlx also supports Postgres; the DDL is written SQLite-first
  with portability notes at the top of `schema.sql` for the day a fleet
  outgrows one host.
- **Server-side only.** The mobile has no copy of this DB; its persistence is
  the local Parquet store + a small app-local upload queue.

## What is NOT here

**No measurement data.** packets / radio / gps / clock / events are Parquet
files on the filesystem (the logging-first store, `parquet/*.yaml`); the
assembled `session_<date>/test_<id>/...` trees are referenced by path. The DB
holds only control-plane metadata.

## Tables

| Group | Tables | Backed by |
|---|---|---|
| Auth | `operators`, `enrollment_tokens`, `auth_challenges`, `device_sessions` | development_plan §7; auth sketch |
| Registry | `devices`, `device_status` | capability + device_status schemas |
| Orchestration | `sessions`, `tests`, `flows`, `commands` | test_plan, run_test, enums (commands/states) |
| Upload | `uploads`, `upload_files` | upload protocol (`upload/`) |

Notes:
- `devices.pubkey_spki` is the credential; deleting a device row revokes it.
- `flows` normalizes the port-less plan + the server-allocated `server_port`
  (the flow_bindings) for queryability; `tests.plan_json` keeps the verbatim
  TestPlan.
- `commands` is the queue the device polls; `idx_commands_device_status` makes
  "oldest pending for device D" cheap.
- `upload_files` (keyed by test/node/path + sha256) is what computes
  `needed_paths` for resumable upload.
- Enum columns carry `CHECK` constraints mirroring `enums.yaml`, so the DB
  rejects invalid states/commands/types independently of the app.

## Validation

`schema.sql` executes cleanly in SQLite (12 tables, 5 indexes, no FK
problems); constraint/FK rejection is smoke-tested. Phase 1 adds real
migration tests against the live sqlx pool.
