# MP Monitor — design specification

> Status: **contracts locked (M0)**. Implementation begins at M1.
> Reference: the detailed phased plan lives in the conversation thread; this
> document captures the locked design decisions.

## What it is

A new Android APK (**MP Monitor**) that turns a handset into an always-on,
passive network-quality sensor. Distinct from MP Logger (which stays focused
on controlled traffic tests). Both apps can coexist on one physical device.

## Relationship to MP Logger

| | MP Logger | MP Monitor |
|---|---|---|
| Driven by | `RUN_TEST` | `SET_LOGGING_CONFIG` (capture + ping sub-objects) |
| Lifecycle | Per-test (finite) | Always-on FGS |
| Traffic | Generates + receives controlled flows | Observes only (VPN); emits only background pings |
| New log tables | — | `traffic.parquet`, `pings.parquet` |
| Upload path | Test-scoped | Session-scoped (`session_<date>/monitor_<device_id>/`) |
| Server | same `server-main` | same `server-main` |
| Protocol | unchanged | additive extension only |

## Enrollment model

Each app enrolls independently. One physical handset has two device rows:

```
devices table:
  dev_01KSHH…  role=logger   hardware_id=a3f8bc12…
  dev_01KS9P…  role=monitor  hardware_id=a3f8bc12…
```

`hardware_id` is the Android SSAID (stable when both apps share the same
signing key — **both APKs MUST be signed with the same key**). The Controller
groups rows by `hardware_id` to form the handset view.

Server enforces: at most one Monitor enrollment per `hardware_id`. A second
Monitor enrollment replaces (de-activates) the first.

## Coexistence rules (mandatory, non-negotiable)

1. **Monitor's VPN excludes Logger** (and itself):
   `VpnService.Builder.addDisallowedApplication(MP_LOGGER_PACKAGE)` + self.
   `MP_LOGGER_PACKAGE` and `MP_MONITOR_PACKAGE` are pinned in
   `contracts/versions/versions.yaml` → Rust `versions.rs` constants.
2. Logger's controlled flows must **never appear** in `traffic.parquet`.
   Verified by the M4 gate (pcap-based CI test).
3. `radio.parquet` and `gps.parquet` are recorded independently by both apps.
   Analysis joins them by `utc_ns` when needed.

## VPN capture architecture

```
VpnService (Kotlin) — owns the tun fd, scope config, quota
  │  ParcelFileDescriptor.getFd()
  ▼
vpn_capture.rs (Rust) — owns the poll/read loop
  │  raw IP packet bytes (batched)
  ▼
flow_aggregator.rs — HashMap<(app_uid?, peer_ip), FlowStats>
  │  1-second bucket flush
  ▼
SegmentedWriter → traffic.parquet
```

UID attribution: Rust batches unknown (proto, sport, dport, peer_ip, peer_port)
5-tuples and calls back to Java `getConnectionOwnerUid()` in bulk (~50 ms
batches). LRU cache keeps call rate low. UID is used internally only; ports
are not logged.

## Address filter (`is_loggable_peer`)

**Dropped** (device-local noise, never useful as a ping target):
- Loopback: `127.0.0.0/8`, `::1/128`
- Link-local: `169.254.0.0/16`, `fe80::/10`
- Multicast: `224.0.0.0/4`, `ff00::/8`
- Broadcast: `255.255.255.255`, subnet-broadcast
- Unspecified: `0.0.0.0`, `::`

**Kept** (includes private/CGNAT — carrier first-hop lives here):
- `10/8`, `172.16/12`, `192.168/16` (RFC 1918)
- `100.64.0.0/10` (CGNAT)
- `fc00::/7` (ULA)
- All public IPv4/IPv6

## Ping target pool

Three categories, evaluated in order:

1. **First-hop gateways** (auto-discovered, always active):
   `ConnectivityManager.getLinkProperties(TRANSPORT_CELLULAR/WIFI)` →
   default-route gateway. Labelled `first_hop_cell` / `first_hop_wlan`.
   Re-resolved on `LinkProperties` change; change emits `FIRST_HOP_CHANGE` event.

2. **Operator-pinned** (`logging_config.ping.targets[]`).

3. **Auto-derived** from `traffic.parquet`:
   top-N peers per `follow_app` where `first_seen_direction = "up"`.
   Only reachable peers (outbound-initiated) qualify.

## Ping probe family

The scheduler tries in order: `icmp` → `udp` (port 33434) → `tcp_syn` (port 443).
If all fail within budget → single row written with `probe_kind = "unreachable"`;
target marked inactive. `probe_kind` is recorded in every `pings.parquet` row.

## Protocol additions (additive — no Logger changes)

| Surface | Addition |
|---|---|
| `EnrollRequest` | `role`, `hardware_id` |
| `Capability` | `monitor` sub-object (`MonitorCapability`) |
| `LoggingConfig` | `capture` (CaptureConfig), `ping` (PingConfig) |
| `DeviceStatus` | `link_state` (LinkState), `monitor_state` (MonitorState) |
| `DeviceSummary` | `role`, `hardware_id` |
| Commands | `LIST_APPS` |
| Event types | `FIRST_HOP_CHANGE`, `MONITOR_CONFIG_APPLIED`, `VPN_SCOPE_CHANGE`, `QUOTA_REACHED` |
| Enums | `device_roles`, `vpn_scopes`, `probe_kinds` |
| Parquet | `traffic.parquet`, `pings.parquet` |
| REST | `GET/POST /devices/{id}/apps[/refresh]` |
| Constants | `MP_LOGGER_PACKAGE`, `MP_MONITOR_PACKAGE` |

## Session upload layout

```
session_<date>/
  logger_<device_id>/          # MP Logger uploads (existing, unchanged)
    test_NNN/ …
  monitor_<device_id>/         # MP Monitor session uploads (new)
    traffic.parquet
    pings.parquet
    events.parquet
    clock.parquet
    radio.parquet
    gps.parquet                # optional (logging_config.gps)
    summary.json
```

`hardware_id` is recorded in `summary.json` to enable cross-role joins by
analysis tools.

## Key analysis views enabled

| View | Inputs | Insight |
|---|---|---|
| Radio vs internet RTT | `pings.parquet` first_hop vs operator targets | Access-stratum latency vs full-path |
| Per-app bytes over time | `traffic.parquet` by app_label | Airtime attribution |
| Top-peer RTT under load | `pings.parquet` × `traffic.parquet` | Bufferbloat per app |
| First-hop change overlay | `events.parquet` FIRST_HOP_CHANGE + HANDOVER | Did the radio event cause the latency excursion? |
| Coverage quality map | `gps.parquet` × `pings.parquet` first_hop | Geographic latency heatmap |
| Logger test context | Monitor `traffic.parquet` × Logger test windows | Was background traffic confounding the test? |

## Privacy

See `contracts/PRIVACY.md`.
