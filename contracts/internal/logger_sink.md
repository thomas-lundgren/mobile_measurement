# Logger sink — the real-time generator → logger boundary

This is the in-process contract that lets the real-time generator record every
packet without ever blocking on disk. It is not a network protocol; it is a
rule set on an in-memory handoff. It holds identically on **both nodes**
(in-process via JNI on mobile; in the data-generator process on the server).

## Structure

```
generator thread (hot path, one per flow)     logger/writer thread (slow path)
  loop at the flow's send/recv tick:             loop:
    ts = sample_dual_clock()                       drain ring in batches
    sendto(peer, packet)        ← measurement      build Arrow RecordBatch
    ring.push(PacketRow{..})    ← wait-free        write segmented Parquet (zstd)
                                                   fsync closed segment
```

- One **bounded, lock-free SPSC ring** per producing thread (single producer =
  generator, single consumer = writer). Slots are preallocated `Row` structs.
- The **writer thread** owns all the slow work: Arrow encoding, segmented
  Parquet, zstd, fsync. None of it touches the generator thread. This is the
  producer side of the segmented writer Stage A already validated
  (`core/log_writer.rs`).

## Hard rules

1. **Order is timestamp → send → push.** The packet leaves the socket before
   it is logged, so logging can never delay the measurement. (On the receive
   side: recv → timestamp → push.)
2. **`push` is wait-free.** No lock, no syscall, no allocation. It writes a
   `Row` into the next slot and bumps an atomic index. If these properties
   can't be met, it doesn't belong on the hot path.
3. **Overflow = drop + count, never block.** If the writer stalls (disk) and
   the ring is full, `push` drops the row and increments `dropped_rows`. It
   does **not** block, spin, or grow the ring. The measurement keeps correct
   timing. Rings are sized so this never fires in normal operation; the drop
   path is a safety valve.
4. **No backpressure to the network.** The send rate is governed only by the
   flow's configured rate and the clock — never by logger or disk state.

## Relationship to invariant 1 (logging-first)

Logging-first means logging is never sacrificed to **upload** — uploads read
only sealed files and never touch the active path. On the hot path, though,
the real-time **send** outranks logging **completeness**: faced with a full
ring, we keep measurement timing and record `dropped_rows` rather than stall.
Correct sizing means the two never actually conflict.

## Rows and sinks

Row types map 1:1 to the Parquet tables (`../parquet/*.yaml`):

| Sink | Row | Producer |
|---|---|---|
| packets | `PacketRow` | each generator thread (its own ring) |
| events  | `EventRow`  | generator + control thread (low rate) |
| clock   | `ClockRow`  | clock-calibration sampler |
| radio   | `RadioRow`  | radio sampler (mobile only) |
| gps     | `GpsRow`    | location sampler (mobile only) |

Only `packets` is on the real-time hot path and needs the wait-free ring.
`radio` / `gps` / `clock` / `events` are low-rate; they use the same sink type
for uniformity but their timing is not critical.

## Reporting drops

`dropped_rows` (per sink) is surfaced two ways at test end:
- as a field in `summary.json` (so a gap is visible without scanning Parquet);
- as an `ERROR` event row (`event_metadata = {"sink":"packets","dropped":N}`)
  when N > 0.

A non-zero `dropped_rows` marks the test `PARTIAL` (recording incomplete) —
consistent with the upload protocol's PARTIAL rule.
