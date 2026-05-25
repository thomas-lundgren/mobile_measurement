#!/usr/bin/env bash
# Regenerate the Controller's REST client from the single-source OpenAPI contract
# (contracts/openapi/openapi.yaml, itself code-generated from server-main via
# utoipa). Output: Kotlin sources under src/main/kotlin/.../client/ (committed).
#
#   ./generate_client.sh
#
# Override the generator jar with OPENAPI_GENERATOR_JAR; it's fetched if absent.
set -euo pipefail

VERSION="${OPENAPI_GENERATOR_VERSION:-7.11.0}"
JAR="${OPENAPI_GENERATOR_JAR:-$HOME/.cache/openapi-gen/openapi-generator-cli-$VERSION.jar}"
HERE="$(cd "$(dirname "$0")" && pwd)"
SPEC="$HERE/../../contracts/openapi/openapi.yaml"
PKG="com.measurement.controller.client"
DEST="$HERE/src/main/kotlin/com/measurement/controller/client"

if [ ! -f "$JAR" ]; then
  echo "fetching openapi-generator-cli $VERSION ..."
  mkdir -p "$(dirname "$JAR")"
  curl -sSL -o "$JAR" \
    "https://repo1.maven.org/maven2/org/openapitools/openapi-generator-cli/$VERSION/openapi-generator-cli-$VERSION.jar"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

java -jar "$JAR" generate \
  -i "$SPEC" -g kotlin -o "$TMP" --skip-validate-spec \
  --additional-properties=library=jvm-okhttp4,serializationLibrary=gson,packageName=$PKG,apiPackage=$PKG.api,modelPackage=$PKG.model

# Keep only the Kotlin sources (the app module owns build config + deps).
rm -rf "$DEST"
mkdir -p "$DEST"
cp -r "$TMP/src/main/kotlin/com/measurement/controller/client/." "$DEST/"
echo "client regenerated into $DEST"
