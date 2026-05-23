#!/usr/bin/env python3
"""Validate contracts/ examples + golden vectors.

- Every testvector example validates against its JSON Schema (cross-$ref aware).
- Packet golden vectors re-encode to the committed hex.
Exits non-zero on any failure. Mirrors the checks the Rust test suite also runs,
but at the contract layer (language-independent).
"""
import glob
import json
import struct
import sys
from pathlib import Path

from jsonschema import Draft202012Validator
from referencing import Registry, Resource

ROOT = Path(__file__).resolve().parent.parent
CONTRACTS = ROOT / "contracts"

failures = []


def load_registry():
    schemas = {}
    for f in glob.glob(str(CONTRACTS / "schemas" / "*.json")):
        s = json.load(open(f))
        if "$id" in s:
            schemas[s["$id"]] = s
    reg = Registry().with_resources(
        (sid, Resource.from_contents(s)) for sid, s in schemas.items()
    )
    return schemas, reg


def validate(reg, schemas, schema_id, instance, label):
    v = Draft202012Validator(schemas[schema_id], registry=reg)
    errs = list(v.iter_errors(instance))
    status = "OK" if not errs else "FAIL"
    print(f"  {label:48s} {status}")
    for e in errs:
        failures.append(f"{label}: {e.message}")


def base(name):
    return f"https://measurement-platform/contracts/schemas/{name}"


def check_json_examples(reg, schemas):
    tv = CONTRACTS / "testvectors"
    cases = [
        ("capability_examples.json", "capability", "capability.schema.json"),
        ("logging_config_examples.json", "config", "logging_config.schema.json"),
        ("status/device_status_examples.json", "status", "device_status.schema.json"),
    ]
    for fname, key, schema in cases:
        doc = json.load(open(tv / fname))
        for ex in doc["examples"]:
            validate(reg, schemas, base(schema), ex[key], f"{fname}:{ex['name']}")

    # payload-wrapped
    for fname, key, schema in [
        ("commands/run_test_examples.json", "payload", "run_test_command.schema.json"),
        ("commands/create_test_examples.json", "request", "create_test_request.schema.json"),
    ]:
        doc = json.load(open(tv / fname))
        for ex in doc["examples"]:
            validate(reg, schemas, base(schema), ex[key], f"{fname}:{ex['name']}")

    # log header/summary
    for fname, key, schema in [
        ("log/header_examples.json", "header", "header.schema.json"),
        ("log/summary_examples.json", "summary", "summary.schema.json"),
    ]:
        doc = json.load(open(tv / fname))
        for ex in doc["examples"]:
            validate(reg, schemas, base(schema), ex[key], f"{fname}:{ex['name']}")

    # upload (manifest + init response)
    doc = json.load(open(tv / "upload" / "upload_examples.json"))
    for ex in doc["examples"]:
        validate(reg, schemas, base("upload_manifest.schema.json"), ex["manifest"], f"upload:{ex['name']}:manifest")
        validate(reg, schemas, base("upload_init_response.schema.json"), ex["init_response"], f"upload:{ex['name']}:init")

    # internal control IPC (per-example schema field)
    doc = json.load(open(tv / "internal" / "control_examples.json"))
    for ex in doc["examples"]:
        validate(reg, schemas, base(ex["schema"]), ex["message"], f"internal:{ex['name']}")


def check_packet_vectors():
    pkt = CONTRACTS / "testvectors" / "packets"
    for fname, fmt, pkt_t in [("probe_v1.json", ">BBIQQ", 0x01), ("realtime_v1.json", ">BBIQQQ", 0x02)]:
        doc = json.load(open(pkt / fname))
        for c in doc["cases"]:
            d = c["decoded"]
            if pkt_t == 0x01:
                head = struct.pack(fmt, pkt_t, 1, d["flow_id"], d["sequence_number"], d["send_mono_ns"])
            else:
                head = struct.pack(fmt, pkt_t, 1, d["flow_id"], d["sequence_number"], d["send_mono_ns"], d["send_utc_ns"])
            head += b"\x00" * c.get("trailing_pad_bytes", 0)
            status = "OK" if head.hex() == c["hex"] else "FAIL"
            print(f"  {fname}:{c['name']:38s} {status}")
            if head.hex() != c["hex"]:
                failures.append(f"{fname}:{c['name']}: hex mismatch")


def main():
    schemas, reg = load_registry()
    print("JSON Schema examples:")
    check_json_examples(reg, schemas)
    print("Packet golden vectors:")
    check_packet_vectors()
    if failures:
        print(f"\nFAILED ({len(failures)}):")
        for f in failures:
            print(" -", f)
        sys.exit(1)
    print("\nAll contract examples + vectors valid.")


if __name__ == "__main__":
    main()
