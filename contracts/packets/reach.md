# Reach probe — reachability check wire protocol

Used by the device's reachability check (`contracts/schemas/reachability_report.schema.json`)
to find out which ports/protocols toward the server get through the current
network. Deliberately **not** framed like the measurement packets in this
directory: it is a one-line ASCII exchange, so it also passes middleboxes
that only forward printable payloads, and a human can test it with `nc`.

## Exchange

Request (device → server), one UDP datagram or one TCP line:

```
MPREACH1 <nonce>\n
```

Reply (server → device):

```
MPREACH1 OK <nonce> <ip>:<port>\n
```

- `<nonce>`: 1–32 characters `[A-Za-z0-9]`, chosen by the device per check;
  the reply must echo it, so a cached or injected answer is rejected.
- `<ip>:<port>`: the device's address as the server saw it (after NAT or a
  proxy). IPv6 is written `[addr]:port`.
- TCP: the server reads one line (max 64 bytes, 2 s timeout), replies, closes.
- UDP: one reply datagram per request datagram; the device retries up to 3
  times, 500 ms apart.
- Anything else gets no reply (UDP) or a closed connection (TCP).

## Ports

The server's reach responders listen on internal ports set by
`MP_REACH_LISTEN` (e.g. `tcp:8443,udp:8443,tcp:8088,tcp:3478,udp:3478,tcp:41100,udp:41100`);
the router maps the public ports the device dials onto them
(e.g. 443 → 8443, 80 → 8088). The device's default probe list is in the
Logger (`ReachabilityChecker.TARGETS`); the control port (8080) is checked
with a plain `GET /api/v1/time` instead.
