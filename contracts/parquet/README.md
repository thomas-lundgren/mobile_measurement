# Parquet log schemas

One YAML file per log table defines its Arrow schema. These are the single
source for the Rust writer (`core/log_writer.rs`) and the Python analysis
loaders. Files are written **segmented** (`<table>_NNNN.parquet`) so a crash
loses at most one open segment (invariant 1); the schema is identical across
a table's segments.

## Conventions

### Dual clock (invariant 2)

Every row that records a moment carries both `mono_ns` (monotonic,
authoritative for ordering / intervals / jitter) and `utc_ns` (wall clock,
for cross-node correlation and OWD). They are sampled atomically by the core
(`core/src/timestamp.rs`). UTC is **never** used for ordering.

> Naming note: these are `mono_ns` / `utc_ns`, matching the implemented core.
> Spec §12 still says `t_mono_ns` / `t_utc_ns`; the spec is to be reconciled
> to the implemented names.

### Nullability (invariant 4)

Identity and dual-clock columns are non-nullable (always written). Every
capability-dependent or flow-type-dependent column is **nullable** and reads
back `null` when absent. New columns may be added without bumping
`schema_version`; readers MUST tolerate unknown columns and missing columns.
Removing or retyping a column DOES bump `schema_version`
(`versions/versions.yaml`). A CI additive-only diff gate enforces this.

### Join columns (remediation 2.1)

Metadata lives in `header.json`, not repeated per row. But so that rows stay
joinable once concatenated across files (the file path is lost in a
dataframe), each table carries a small set of constant-per-file key columns,
dictionary-encoded so they cost almost nothing:

- Per-node tables (`packets`, `events`, `clock`): `device_id`, `session_id`,
  `test_id`, `node`.
- Mobile-only test-root tables (`radio`, `gps`): `device_id`, `session_id`,
  `test_id` (node is always the mobile; omitted).

### Compression

ZSTD at the Parquet level (already configured in `core/log_writer.rs`). This
is why the upload protocol uses `identity` transport encoding for `.parquet`
(see `../upload/README.md`).

## Tables

| File | Scope | Spec |
|---|---|---|
| [`packets.parquet.yaml`](packets.parquet.yaml) | per node, all flows | §12.3 |
| [`radio.parquet.yaml`](radio.parquet.yaml) | mobile, 1/test | §12.4 |
| [`gps.parquet.yaml`](gps.parquet.yaml) | mobile, 1/test | §12.5 |
| [`clock.parquet.yaml`](clock.parquet.yaml) | per node | §12.6 |
| [`events.parquet.yaml`](events.parquet.yaml) | per node | §12.7 |

## Type vocabulary

`uint32 uint64 int32 int64 float32 float64 utf8 bool` plus `list` (with an
`item:` type) and `struct` (with `fields:`). These map directly to Arrow
`DataType`s and to pyarrow.
