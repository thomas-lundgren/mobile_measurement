# Internal interfaces (server-side, not public)

These are the boundaries *inside* the server deployment and the boundary
between the traffic engine and its logger on **both** nodes. They are not
reachable by the Controller, the Logger app, or anything outside the host.

| Interface | Between | Kind | Spec |
|---|---|---|---|
| Control API | server-main → test-control → data-generator | local HTTP + JSON | [`control_api.md`](control_api.md) |
| Logger sink | generator thread → logger/writer thread (in-process) | lock-free ring buffer | [`logger_sink.md`](logger_sink.md) |

## Why these two are different in kind

The **control API** is off the hot path — it runs before a flow reaches
`RUNNING` and after it leaves it, plus low-rate status. Latency there does not
affect measurement timing, so it uses the simplest transport (HTTP+JSON,
reusing the axum/serde stack). This **overrides the earlier gRPC decision**
(dev-plan §2 / a `proto/` dir): protobuf/tonic isn't worth the toolchain for
same-host, lockstep-released processes whose control latency is irrelevant.

The **logger sink** *is* the hot path's escape valve. It is an in-process,
lock-free handoff that lets the real-time generator record a packet without
ever blocking on disk. It is not a network protocol — it is a contract on
ring semantics, ordering, and overflow behavior. See `logger_sink.md`.

## Process & thread model (decided)

- **Generator + logger live in one process, as separate threads, on both
  nodes.** On mobile this is forced (the shared Rust core is a JNI `.so` in
  the app process); the server mirrors it for symmetry (invariant 6).
- Separate *processes* give no real-time benefit (the kernel schedules
  threads, not processes). On the server, `test-control` and each
  `data-generator` MAY be separate processes purely for **crash isolation**.
- Real-time timing is protected by (1) never blocking the hot path, (2) CPU
  pinning + raised thread priority, (3) no allocation in the send/recv loop —
  not by process boundaries.

## Transport & trust

Control API endpoints bind to **loopback only** (`127.0.0.1`, or a Unix
domain socket). Same host, same trust domain, started together — **no auth**
on these endpoints. They MUST NOT be exposed on an external interface. (The
public, authenticated surface is the REST API in `openapi/` + the auth
sketches.)
