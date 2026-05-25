#!/usr/bin/env bash
# Generated-code-in-sync gate (development_plan.md §6): the committed Controller
# REST client must equal a fresh regeneration from contracts/openapi/openapi.yaml
# (which is itself code-generated from server-main via utoipa). Needs Java.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CLIENT="android/controller/src/main/kotlin/com/measurement/controller/client"

"$ROOT/android/controller/generate_client.sh" >/dev/null

if ! git -C "$ROOT" diff --quiet -- "$CLIENT"; then
  echo "ERROR: the generated Controller client is out of sync with"
  echo "       contracts/openapi/openapi.yaml. Run:"
  echo "         android/controller/generate_client.sh"
  echo "       and commit the result."
  git -C "$ROOT" --no-pager diff --stat -- "$CLIENT"
  exit 1
fi
echo "OK: generated Controller client matches the contract."
