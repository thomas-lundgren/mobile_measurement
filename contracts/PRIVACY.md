# Privacy notes — MP Monitor

## What is collected

MP Monitor captures **traffic metadata** — not packet content.

| Table | What is stored | What is NOT stored |
|---|---|---|
| `traffic.parquet` | peer IP, app label, bytes/packet counts per 1-second bucket | ports, protocol, payload, URL paths, hostnames |
| `pings.parquet` | operator-configured target IPs, RTT, loss | payload, DNS queries |
| `events.parquet` (monitor events) | gateway IP changes, config version applied, VPN scope/quota | N/A |

The Logger APK and Monitor APK are **hard-excluded** from capture by VpnService configuration. Their traffic never appears in `traffic.parquet`.

## IP addresses and GDPR

Peer IP addresses are PII-adjacent (they reveal which services an app communicates with). This platform is designed for **operator-controlled test devices**, not consumer deployments. Operators are responsible for:

- Configuring capture filters narrowly (prefer `scope = allowlist` with specific apps).
- Setting appropriate daily quotas.
- Securing uploaded session data at rest.
- Complying with applicable data protection law (GDPR, etc.) in their deployment context.

## App inventory

The installed-app inventory (`AppList`) lists package names and labels on the device. This reveals what software is installed. On operator-managed test handsets this is expected and intended; on shared or personal devices the operator must obtain appropriate consent.

The server stores the latest inventory per device ID. It is not shared with third parties.

**Server-side opt-out.** A per-tenant configuration flag (`app_inventory_upload_enabled`, default: `true`) can suppress storage of `AppList` uploads. Capture still works; the Controller app-picker will show UIDs without labels.

## GPS

GPS recording is controlled by `logging_config.gps` (default: `true` for Monitor sessions). If location privacy is a concern, set `gps: false` — no route data is collected.

## Minimisation principles

1. Capture filters are **server-controlled** — the device only logs what the operator enables.
2. Daily byte quotas prevent unbounded collection.
3. Payload is **never** inspected or stored.
4. Private/CGNAT peer addresses are kept (needed for first-hop latency measurement); loopback, link-local, multicast, and broadcast addresses are discarded at capture time.
