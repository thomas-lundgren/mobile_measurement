#!/usr/bin/env bash
# Build the Logger debug APK: cross-compile the shared Rust core for arm64 via
# cargo-ndk into jniLibs, then assemble. Run from anywhere.
#
# Prereqs: Android SDK + NDK (see local.properties), cargo-ndk, rustup target
# aarch64-linux-android. On-device install: adb install -r <apk>.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
export ANDROID_NDK_HOME="${ANDROID_NDK_HOME:-/home/thomas/Android/Sdk/ndk/27.2.12479018}"
export ANDROID_HOME="${ANDROID_HOME:-/home/thomas/Android/Sdk}"
export PATH="$HOME/.cargo/bin:$PATH"

echo "=== cross-compile core (arm64-v8a, release) ==="
( cd "$REPO" && cargo ndk -t arm64-v8a -o "$HERE/logger/src/main/jniLibs" build -p fieldtest-core --release )

echo "=== assembleDebug ==="
GRADLE="$(find "$HOME/.gradle/wrapper/dists" -name gradle -type f | head -1)"
( cd "$HERE" && "$GRADLE" --no-daemon assembleDebug )

APK="$HERE/logger/build/outputs/apk/debug/logger-debug.apk"
echo "=== APK ==="
ls -lh "$APK"
