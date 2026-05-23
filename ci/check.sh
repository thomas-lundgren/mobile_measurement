#!/usr/bin/env bash
# Local CI gate — the same checks CI runs. Run from the repo root: ci/check.sh
#
# Covers development_plan.md §6 gates that apply today:
#   - formatting (rustfmt)
#   - lints as errors (clippy -D warnings)
#   - the full test suite, which includes the contract gates:
#       * enum/version parity vs contracts/*.yaml         (mp-contracts)
#       * packet + auth-signing golden vectors            (fieldtest-core)
#       * OpenAPI drift vs contracts/openapi/openapi.yaml (mp-server-main)
#       * DB migration == contracts/db/schema.sql         (mp-server-main)
#       * full probe end-to-end                           (mp-server-main)
set -euo pipefail

echo "==> cargo fmt --all --check"
cargo fmt --all --check

echo "==> cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test --workspace"
cargo test --workspace

echo "OK: format, lint, and tests all pass."
