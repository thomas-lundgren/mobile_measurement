# Golden vectors — wire packets

Every implementation of the packet codec (Rust core, Java/Kotlin codec,
Python analysis loader) MUST round-trip every vector here byte-for-byte.
This is the cross-language conformance gate.

## Format

Each vector file is a JSON document:

```json
{
  "packet_type": "Probe",
  "version": 1,
  "cases": [
    {
      "name": "short-human-readable label",
      "decoded": { /* per-type structured fields */ },
      "trailing_pad_bytes": 0,
      "hex": "0101...",
      "notes": "optional"
    }
  ]
}
```

- **`decoded`** is the canonical structured representation. Fields match
  the spec in `../../packets/<type>.md`.
- **`trailing_pad_bytes`** (default `0`) is the number of zero bytes
  appended after the core (used for realtime padding).
- **`hex`** is the exact bytes on the wire — lowercase, no spaces. This
  is what every codec is tested against.

## Test obligations

1. **Encode test:** given `decoded` (+ `trailing_pad_bytes`), produce `hex`.
2. **Decode test:** given `hex`, produce `decoded`. Trailing bytes beyond
   `core_size` are ignored.

## Files

- [`probe_v1.json`](probe_v1.json) — `ProbePacket` v1.
- [`realtime_v1.json`](realtime_v1.json) — `RealtimePacket` v1.

New vectors are added with new features; existing vectors are **never
deleted or modified** (forward-compat invariant).
