# Internal control API (resolves remediation 3.4)

Local HTTP/1.1 + JSON over loopback (or a Unix domain socket), same-host
only, no auth (see [README](README.md)). Two unidirectional hops — each
caller only calls "downstream," so there are no callbacks:

```
Controller ──REST──▶ server-main ──/internal/tests──▶ test-control ──/internal/generators──▶ data-generator(s)
```

Message bodies are JSON Schemas under `../schemas/internal_*.json`. Flow shape
and bindings reuse `test_plan.schema.json` ($defs Flow, FlowBinding) — the
single source.

## Hop 1 — server-main → test-control

server-main owns the device registry, the device command queue, and the
operator REST API. It assigns `test_id`, then delegates preparation and
generator coordination to test-control.

```http
POST /internal/tests
```
Body: [`internal_start_test.schema.json`](../schemas/internal_start_test.schema.json)
— `{ test_id, device_id, session_id, server_host, plan (port-less TestPlan), logging_config_override? }`.
test-control allocates one port per flow, spins up a generator per flow, and
drives the per-device state machine (`enums.yaml test_control_states`).
Returns `202 Accepted`.

```http
GET /internal/tests/{test_id}
```
Returns [`internal_test_status.schema.json`](../schemas/internal_test_status.schema.json)
— current `state`, the allocated `flow_bindings` (once `state` ≥ WAITING_FOR_CLIENT),
per-flow generator states, and the terminal `result` once known.

```http
POST /internal/tests/{test_id}/stop
```
Cancels: stops the generators, drives the test to a terminal state. Returns `202`.

**Orchestration (server-main side):** POST the test → poll GET until
`state = WAITING_FOR_CLIENT` with `flow_bindings` populated → assemble the
`RunTestPayload` (test_plan + flow_bindings + server_host) → enqueue `RUN_TEST`
for the device. When the test ends (device reports done via status, or duration
elapses), POST `/stop`.

## Hop 2 — test-control → data-generator

One generator per flow (a dedicated real-time thread set; see
[`logger_sink.md`](logger_sink.md)). test-control owns port allocation, so it
tells the generator which port to bind.

```http
POST /internal/generators
```
Body: [`internal_generator_prepare.schema.json`](../schemas/internal_generator_prepare.schema.json)
— `{ test_id, device_id, session_id, flow (port-less Flow), server_port (allocated), node_log_dir }`
(device_id/session_id are stamped as join columns in the server's packets.parquet).
The generator binds `server_port`, opens its logger sink, and proceeds
INITIALIZING → READY (listening). Returns `{ generator_id }`.

The generator derives its role from the flow: probe → echo; `realtime_udp`/
`bulk_tcp` FromServer → send; ToServer → receive. No explicit role field.

```http
GET /internal/generators/{generator_id}
```
Returns [`internal_generator_status.schema.json`](../schemas/internal_generator_status.schema.json)
— `state` (`enums.yaml generator_states`), `bound_port`, live counters
(`packets_sent`, `packets_received`, `dropped_rows`), and `error`.

```http
POST /internal/generators/{generator_id}/stop
```
STOPPING → flush the logger ring, finalize Parquet segments → STOPPED. Returns
final counters.

**No explicit `start`.** A generator goes READY (listening) and then
WAITING_FOR_CLIENT → RUNNING automatically when the UE connects / sends its
first packet (invariant 5: the UE always initiates). test-control gates the
test on all generators reaching READY before server-main tells the device.

## Lifecycle alignment

| test-control state | generators | server-main action |
|---|---|---|
| PREPARE | POST /generators for each flow | — |
| WAITING_FOR_GENERATOR | generators → READY | poll GET /internal/tests |
| WAITING_FOR_CLIENT | all READY; bindings known | enqueue RUN_TEST |
| RUNNING | generators RUNNING | relay device status |
| STOPPING / DONE / FAILED | POST /generators/{id}/stop | dequeue / mark result |
