#!/usr/bin/env bash
# Desktop-JVM JNI conformance harness. Builds the core cdylib, compiles the
# Java bridge + harness, and runs the probe test through the real RustCore JNI
# surface (Stage-A style, x86_64). Run from the repo root: core/jni_harness/run.sh
set -euo pipefail

cargo build -p fieldtest-core
HARNESS="core/jni_harness"
OUT="$(mktemp -d)"
javac -d "$OUT" \
    "$HARNESS/com/measurement/logger/RustCore.java" \
    "$HARNESS/Harness.java"
java -Djava.library.path=target/debug -cp "$OUT" Harness
