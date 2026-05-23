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

## Many flows: rings fan in to one writer

A test runs several flows at once (e.g. realtime UDP + bulk TCP + probe). SPSC
is a property of **each ring**, not of the logger — so flows scale by adding
rings, never by widening one:

```
realtime_udp thread ──push──▶ ring R1 ─┐
bulk_tcp thread ─────push──▶ ring R2 ──┼──▶ writer thread ──▶ packets.parquet
probe thread ────────push──▶ ring R3 ─┘     (drains all rings)  (flow_id column)
```

- **One ring per flow.** Each ring has exactly one producer (its flow thread)
  and one consumer (the writer), so every ring stays strictly SPSC.
- **One writer multiplexes all rings into the one per-node `packets.parquet`.**
  Consuming several SPSC rings from a single thread is fine — the "one consumer
  per ring" rule holds. Rows from all flows interleave in the file, tagged by
  `flow_id`. Per-flow order is preserved (each flow's rows came through its own
  FIFO ring); cross-flow order is irrelevant — analysis sorts by `mono_ns`.
- **One writer per output file.** Parquet writing is single-threaded per file,
  so there is exactly one writer for `packets.parquet`. Aggregate row volume (a
  few flows × their rates) is modest and easily drained off the hot path; if the
  disk stalls, each ring's drop+count fires independently.

### What each flow type logs

- **realtime_udp / probe** — one `PacketRow` per packet (high rate, hot path).
- **bulk_tcp** — TCP hides segmentation, so it does **not** log per segment.
  The generator logs one `PacketRow` per **application-level chunk** it
  writes/reads (e.g. per 64 KB block): `payload_size` = chunk size, so
  throughput = Σ `payload_size` / time. Much lower row rate than realtime, same
  ring/writer path.

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

Only `packets` is on the real-time hot path and needs the wait-free SPSC ring
(one per flow). `radio` / `gps` / `clock` / `events` are low-rate and their
timing is not critical:

- `clock` / `radio` / `gps` each have a single sampler thread → still SPSC.
- **`events` has multiple producers** (each generator thread *and* the control
  thread can emit events), so it is **not** SPSC. Because it is off the hot
  path, it uses a multi-producer channel (MPSC) or a small mutex-guarded buffer
  drained by the writer — the strict wait-free SPSC discipline is reserved for
  the packet path where it actually matters.

## Reporting drops

`dropped_rows` (per sink) is surfaced two ways at test end:
- as a field in `summary.json` (so a gap is visible without scanning Parquet);
- as an `ERROR` event row (`event_metadata = {"sink":"packets","dropped":N}`)
  when N > 0.

A non-zero `dropped_rows` marks the test `PARTIAL` (recording incomplete) —
consistent with the upload protocol's PARTIAL rule.
