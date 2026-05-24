# Phase 3 — Android Controller MVP (detailed design)

The Controller is the **operator** app: it remote-controls and monitors the
fleet via server-main's REST API. It performs **no measurement** (no core, no
JNI, no traffic). Reference: ROADMAP Phase 3, spec §controller surface.

## Surface the Controller needs (vs. what exists)

server-main today exposes only `POST /auth/login`, `POST /tests`,
`GET /tests/{id}` for operators. The Controller additionally needs:

| Endpoint | Purpose | Status |
|---|---|---|
| `POST /api/v1/auth/login` | operator JWT | exists |
| `GET  /api/v1/devices` | fleet list + live status (dashboard) | **3a (new)** |
| `GET  /api/v1/tests` | recent tests list | **3a (new)** |
| `POST /api/v1/tests` | start a test (port-less plan) | exists |
| `GET  /api/v1/tests/{id}` | test detail/state | exists |
| `POST /api/v1/tests/{id}/stop` | stop a running test | **3a (new)** |

All operator endpoints are JWT-authed (the existing `authed_operator` guard).

## Build slices

- **3a — operator API surface (server).** Add the three endpoints + DTOs
  (`DeviceSummary`, `TestSummary`), wire them into the utoipa doc, regenerate
  `contracts/openapi/openapi.yaml`, integration-test them. Fully host-verifiable;
  defines the contract the client generates from. *(this slice)*
- **3b — generated client + app skeleton.** Kotlin app `android/controller`
  (no core/JNI). API client generated from `openapi.yaml` (openapi-generator if
  available, else a thin hand-written client pinned to the contract). Login
  screen → token stored; device-list screen.
- **3c — control + dashboard.** Start a test (pick device + plan), live test
  state, stop; device detail (logger_state, battery, last-seen, current test).

## Decisions

- **Kotlin** (ROADMAP), single-module Gradle app under `android/controller`,
  reusing the existing SDK/AGP/Gradle setup. Compose or Views — Views (AppCompat)
  to match the Logger and keep it dependency-light.
- **No shared core.** The Controller is pure REST; it never links the `.so`.
- **Auth:** username/password → JWT (HS256), `Authorization: Bearer`. Token
  held in memory + `EncryptedSharedPreferences`; no device keypair.
- **DeviceSummary** = `devices` LEFT JOIN latest `device_status` so the
  dashboard shows liveness (logger_state, last-seen, battery) for devices that
  have reported, and bare enrollment rows for those that haven't.
- **Stop** sets the test terminal (`state=DONE`, `result=CANCELLED`, `ended_at`)
  and calls test-control stop (frees generators/ports), idempotent.
