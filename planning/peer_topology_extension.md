# Extension — generalised peer topology + NAT-aware deployment

> Status: **proposed**, not implemented. Captures the design discussion for
> letting *any* node (a server, a UE, or another UE) be a peer in a test, and
> the deployment realities behind NAT.

## Motivation

Today the topology is fixed: one **server** node hosts the `data-generator` and
accepts inbound; one **mobile** node (UE) initiates. The Rust core already
implements both roles (generator + receiver + logger), and Parquet aggregation
already names two roles (`mobile/`, `server/`). The remaining limitation is the
**control plane**: `server_host` + `flow_bindings` are hard-coded to "the server
is the reachable peer."

A small generalisation lets the same orchestration cover:

- **server ↔ UE** (today)
- **UE ↔ server** (semantically the same, just role flip)
- **UE ↔ UE** on a shared LAN (works today's mechanics with no relay)
- **UE ↔ UE across the internet** (the hard case — see "Reachability" below)

…and matches a deployment where the server itself sits behind a home router.

## Peer-endpoint registry

Per test, each peer that joins reports a candidate endpoint set; the server
picks, per flow, whichever address the *other* peer can actually reach.

For every peer the server records:

- **Self-reported** local IP + port — works when peers share a LAN.
- **Observed** source IP/port of the peer's control-plane TCP connection — the
  public IP as the server sees it (STUN-style, free to capture; the server
  already terminates the connection).
- A peer **role label** the operator (or scheduler) attaches: `mobile_a`,
  `mobile_b`, `server`, … Used as the per-test subfolder for aggregation
  (`<test>/mobile_a/packets.parquet`, …) so the §12.1 layout generalises.

`RUN_TEST` then carries, per flow, the chosen **peer endpoints** (replacing
the single `server_host` + `server_port`): `{flow_id, initiator, receiver,
receiver_endpoint}`.

## Asymmetry: only the *receiver* of each flow must be reachable

In any flow, one side accepts the inbound connection / first packet and the
other initiates. **Only the accepting side needs a reachable address + open
port.** Schedule flows so the always-reachable peer is the receiver whenever
the other peer is behind a NAT it can't punch through (cellular CGNAT in
particular). The current "UE always initiates" invariant becomes a per-flow
choice driven by which peer is reachable.

## Deployment behind a home router (server-as-receiver)

For a server behind a home/residential router with UEs initiating from anywhere:

1. **Advertise the public address.** Set `MP_SERVER_HOST` to the public IPv4 or
   a DDNS name (residential IPs change). The LAN IP only works for UEs on the
   same network.
2. **Forward these to the server's LAN IP:**

   | Forward (router → server) | Why |
   |---|---|
   | `8080/tcp` | control-plane REST (enroll / poll / status / upload) |
   | `8081/udp` | clock time-sync responder |
   | `41000–41100/tcp` **and** `/udp` | generator flow ports (UE always sends the first packet here) |

3. **NAT hairpinning caveat.** If a UE on the *same* LAN as the server dials the
   public IP, many home routers won't loop the connection back. Either use the
   LAN IP for same-network tests, or enable NAT loopback / hairpin on the
   router.
4. **The data plane stays plaintext** (see Phase-5 hardening notes — TLS would
   distort the latency/jitter/throughput it measures). Use per-flow tokens or
   packet HMAC if integrity/anti-injection becomes a need.

This is the easy, robust deployment: only the *server's* router needs config;
UEs need nothing because outbound NAT state is created by the UE's first
packet.

## Reachability for UE-as-receiver (the hard case)

If a flow asks a phone to accept inbound:

- **On the same Wi-Fi/LAN:** direct, just like the LAN tests we already exercise
  — works with no extra plumbing.
- **On home Wi-Fi behind a normal router:** doable with port forwarding on the
  *phone's* router, or UPnP/PCP if the phone can request a mapping.
- **On cellular (CGNAT):** generally **not reachable** without help. Options,
  roughly in increasing order of effort:
  1. **Hole punching (STUN/ICE-like).** Both peers send to the other's
     observed public endpoint to open NAT mappings; works for many home NATs,
     often fails on symmetric CGNAT.
  2. **Server-relayed data plane (TURN-style).** Always works but you're then
     measuring "phone → server → phone," not "phone → phone" — partially
     defeats the purpose; can be useful for some metrics (e.g., comparing
     direct vs relayed RTT).
  3. **Carrier APN with public addressing / private-relay over a tunnel.**
     Out of scope of the platform itself.

The orchestration changes (endpoint registry, observed IP, hole-punch packets)
are protocol additions; the carrier-side options are deployment choices.

## What already supports this in the codebase

- The shared Rust core (`fieldtest-core`) implements **both** peer roles —
  generator, receiver, echo, and logger — so a UE can host the receive side of
  a flow with no new code.
- `Direction` (`ToServer`/`FromServer`) and the per-test `mobile/`+`server/`
  aggregated tree (§12.1) already encode peer asymmetry; generalising to N
  named peers per test is additive.

## What's new (sketch — not implementation)

1. **Endpoint registry on `server-main`.** Capture observed source IP/port on
   the device control connection; expose a peer-self-report endpoint
   (`POST /api/v1/devices/{id}/endpoint`) for the self-reported local IP/port.
2. **Per-test peer roster.** `CreateTestRequest` accepts ≥2 peers (each a
   `device_id` or `"server"`) and a role label per peer.
3. **Per-flow endpoint binding.** `RUN_TEST` payload carries
   `{initiator, receiver, receiver_endpoint}` instead of a single
   `server_host`+`server_port`.
4. **Receiver selection logic.** `test-control` picks the receiver per flow as
   the most-reachable peer (preferring same-LAN, falling back to the
   always-reachable one); per-flow port allocation moves to whichever peer
   accepts.
5. **Aggregated tree generalisation.** Replace the hard-coded `mobile/` +
   `server/` subfolders with one subfolder per peer role (`peer_a/`, `peer_b/`,
   …) — every per-node analysis path already operates on `td.packets["node"]`,
   so the analysis loader needs only a small change.
6. **Operator surface.** Controller can choose two devices for a test, or
   "device + server."
7. **(Optional)** Hole-punching helper: a brief exchange of "send 1 packet to
   the other peer's observed endpoint" to open NAT state before the real flow
   starts.

None of this requires changing the shared core or the wire packet formats;
it's a control-plane + DTO evolution.

## Bottom line

- The peer-endpoint generalisation is **a control-plane change**, not a core
  change. The data plane and Parquet schemas already carry the necessary
  information (per-node logs, per-flow directions, dual clocks).
- For deployments where the server is the always-reachable peer (the home-router
  case), **port-forwarding the server is sufficient** — UE↔UE topologies "just
  work" on a shared LAN today, and the cellular UE↔UE case is a separate, much
  bigger reachability problem (NAT traversal / relay), independent of TLS.
