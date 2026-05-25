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
#   - contract examples + packet vectors validate
#   - the Python analysis test suite
#   - generated Controller client in sync with the OpenAPI contract (needs Java)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "==> cargo fmt --all --check"
cargo fmt --all --check

echo "==> cargo clippy --workspace --all-targets -- -D warnings"
cargo clippy --workspace --all-targets -- -D warnings

echo "==> cargo test --workspace"
cargo test --workspace

echo "==> contract examples + packet vectors"
[ -d venv ] || python3 -m venv venv
./venv/bin/pip install -q jsonschema referencing pyyaml
./venv/bin/python ci/validate_contracts.py

echo "==> analysis: pytest"
(
  cd analysis
  [ -d .venv ] || python3 -m venv .venv
  ./.venv/bin/pip install -q -e ".[dev,plots]"
  ./.venv/bin/python -m pytest -q
)

echo "==> generated Controller client in sync with the contract"
if command -v java >/dev/null 2>&1; then
  ci/check_client_drift.sh
else
  echo "   (skip: java not found — CI enforces this)"
fi

echo "OK: format, lint, tests, contracts, analysis, and client all pass."
