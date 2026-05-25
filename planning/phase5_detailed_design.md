# Phase 5 — Hardening & field readiness (detailed design)

Make the MVP survive real field conditions: flaky networks, long sessions,
multiple devices, low battery/storage, and crashes. Reference: ROADMAP Phase 5,
spec §failure-handling.

## Slices

- **5a — Logger resilience (reconnect/backoff).** *(this slice)* The worker's
  poll loop dies on any transient error (server restart, coverage gap): one
  outer try/catch around the whole session. Restructure into a **reconnecting
  outer loop** (connect → onboard → poll) with **exponential backoff**, so a
  dropped connection just reconnects and resumes. Re-authenticate on session
  loss; enrollment is already idempotent (cached `device_id`). Logging-first is
  unaffected — a test in flight keeps logging locally.
- **5b — Battery & storage telemetry + guards.** Report `battery` + `storage`
  in the status heartbeat (dashboard shows `batt=—` today). Refuse to start a
  test when free storage is below a floor (logging-first means a full disk is
  the one thing that breaks measurement).
- **5c — Logging-first chaos.** A test that kills the writer mid-test and
  asserts the sealed segments are still readable (segmented Parquet already
  targets this; lock it in). Plus a dependency check that the measurement path
  can't reach the upload client.
- **5d — Dynamic `SET_LOGGING_CONFIG`.** Server pushes a config update command;
  the device applies it and bumps `logging_config_version` without restarting.
- **5e — Clock-uncertainty validation.** Property test that OWD uncertainty
  propagates (never negative, grows with each clock's bound).

## 5a detail

```
run(server, token):
  client = ServerClient(server)
  backoff = 1s
  while running:
    try:
      onboard: ensureKey → enrollIfNeeded (cached) → authenticate → register
      setState IDLE; backoff = 1s            # connected: reset
      pollLoop(client, session, deviceId)    # returns only when !running
    except Interrupted: return
    except Exception e:
      if !running: return
      setState "RECONNECTING: …"; sleep(backoff); backoff = min(backoff*2, 30s)
```

`pollLoop` runs the poll/status cadence; any request exception propagates to the
outer loop, which backs off and re-authenticates (a fresh session). A transient
failure inside a single poll is thus recoverable rather than fatal. Verified by
restarting server-main mid-session and confirming the Logger returns to IDLE.
