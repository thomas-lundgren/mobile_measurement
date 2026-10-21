#!/usr/bin/env python3
"""Validate contracts/ examples + golden vectors.

- Every testvector example validates against its JSON Schema (cross-$ref aware).
- Packet golden vectors re-encode to the committed hex.
- Every JSON Schema the server also has as a Rust type (same title in the
  generated OpenAPI) has the same shape there: properties, required fields,
  enum values and flow variants. The schema is hand-written and the OpenAPI
  generated from the Rust DTOs, so this keeps the two from drifting.
Exits non-zero on any failure. Mirrors the checks the Rust test suite also runs,
but at the contract layer (language-independent).
"""
import glob
import json
import struct
import sys
from pathlib import Path

import yaml
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
        ("core/stop_summary_examples.json", "summary", "core_stop_summary.schema.json"),
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


def _resolve(node, doc, files):
    while isinstance(node, dict) and "$ref" in node:
        file, _, frag = node["$ref"].partition("#")
        if file:
            doc = files[file.rsplit("/", 1)[-1]]
        node = doc
        for p in [p for p in frag.strip("/").split("/") if p]:
            node = node[p]
    return node, doc


def _shape(node, doc, files):
    """What both sides must agree on: property names, required fields, enum
    values and the variants of a `type`-discriminated union. Types, bounds
    and docs are left out (OpenAPI spells them differently)."""
    node, doc = _resolve(node, doc, files)
    alts = node.get("oneOf") or node.get("anyOf")
    if alts:
        alts = [v for v in alts if _resolve(v, doc, files)[0] != {"type": "null"}]
        if len(alts) == 1:  # Option<T> is "null or T" in OpenAPI
            return _shape(alts[0], doc, files)
        out = {}
        for v in alts:
            v, vdoc = _resolve(v, doc, files)
            t, _ = _resolve(v.get("properties", {}).get("type", {}), vdoc, files)
            out[t.get("const") or (t.get("enum") or [None])[0]] = _shape(v, vdoc, files)
        return {"oneOf": out}
    if "allOf" in node and len(node["allOf"]) == 1:
        return _shape(node["allOf"][0], doc, files)
    if "properties" in node:
        return {
            "props": {k: _shape(v, doc, files) for k, v in node["properties"].items()},
            "required": sorted(node.get("required", [])),
        }
    if "const" in node:
        return {"enum": [node["const"]]}
    if "enum" in node:
        return {"enum": sorted(x for x in node["enum"] if x is not None)}
    t = node.get("type")
    types = t if isinstance(t, list) else [t]
    if "array" in types:
        return {"items": _shape(node.get("items", {}), doc, files)}
    if "object" in types and isinstance(node.get("additionalProperties"), dict):
        return {"map": _shape(node["additionalProperties"], doc, files)}
    return "scalar"


def _diff(a, b, path, out):
    if isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b), key=str):
            if k not in a:
                out.append(f"{path}.{k}: only in the Rust type")
            elif k not in b:
                out.append(f"{path}.{k}: only in the JSON Schema")
            else:
                _diff(a[k], b[k], f"{path}.{k}", out)
    elif a != b:
        out.append(f"{path}: JSON Schema {a} vs Rust type {b}")


def check_schema_parity():
    files = {Path(f).name: json.load(open(f)) for f in glob.glob(str(CONTRACTS / "schemas" / "*.json"))}
    openapi = yaml.safe_load(open(CONTRACTS / "openapi" / "openapi.yaml"))
    components = openapi["components"]["schemas"]
    for name, schema in sorted(files.items()):
        title = schema.get("title")
        if title not in components:
            print(f"  {name:48s} (no Rust type in the OpenAPI)")
            continue
        out = []
        _diff(_shape(schema, schema, files), _shape(components[title], openapi, files), title, out)
        print(f"  {name:48s} {'OK' if not out else 'FAIL'}")
        failures.extend(f"{name}: {d}" for d in out)


def check_upload_paths():
    """testvectors/upload/paths.json against the manifest schema's path pattern
    (the server's valid_upload_path is tested against the same vectors)."""
    import re

    schema = json.load(open(CONTRACTS / "schemas" / "upload_manifest.schema.json"))
    source = schema["$defs"]["ManifestFile"]["properties"]["path"]["pattern"]
    # JSON Schema patterns are ECMA-262: "$" never matches before a final "\n"
    pattern = re.compile(source[:-1] + r"\Z" if source.endswith("$") else source)
    doc = json.load(open(CONTRACTS / "testvectors" / "upload" / "paths.json"))
    for want, paths in ((True, doc["valid"]), (False, doc["invalid"])):
        for p in paths:
            ok = (pattern.search(p) is not None) == want
            print(f"  paths.json:{p!r:44s} {'OK' if ok else 'FAIL'}")
            if not ok:
                failures.append(f"paths.json: {p!r} should be {'valid' if want else 'invalid'}")


def check_time_sync_vectors():
    """contracts/packets/time_sync.md: reply = nonce (8, zero-padded) ++ t2 ++ t3 (i64 BE)."""
    doc = json.load(open(CONTRACTS / "testvectors" / "packets" / "time_sync_v1.json"))
    for c in doc["cases"]:
        nonce = (bytes.fromhex(c["request_hex"])[:8] + b"\x00" * 8)[:8]
        reply = nonce + struct.pack(">qq", c["t2_recv_utc_ns"], c["t3_send_utc_ns"])
        ok = reply.hex() == c["reply_hex"]
        print(f"  time_sync_v1.json:{c['name']:38s} {'OK' if ok else 'FAIL'}")
        if not ok:
            failures.append(f"time_sync_v1.json:{c['name']}: hex mismatch")


def main():
    schemas, reg = load_registry()
    print("JSON Schema examples:")
    check_json_examples(reg, schemas)
    print("Packet golden vectors:")
    check_packet_vectors()
    check_time_sync_vectors()
    print("Upload paths:")
    check_upload_paths()
    print("JSON Schema vs Rust types (generated OpenAPI):")
    check_schema_parity()
    if failures:
        print(f"\nFAILED ({len(failures)}):")
        for f in failures:
            print(" -", f)
        sys.exit(1)
    print("\nAll contract examples + vectors valid.")


if __name__ == "__main__":
    main()
