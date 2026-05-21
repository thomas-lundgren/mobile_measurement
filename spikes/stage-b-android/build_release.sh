#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

# Build Rust core for Android arm64
echo "=== Build Rust core (release) ==="
cd ../..
cargo ndk -t arm64-v8a build --release
cd -

# Copy .so into jniLibs
cp ../../target/aarch64-linux-android/release/libfieldtest_core.so \
   logger/src/main/jniLibs/arm64-v8a/

# Build signed release APK
GRADLE_BIN=$(find ~/.gradle -name "gradle" -type f | head -1)
echo "=== Build signed release APK ==="
"$GRADLE_BIN" clean assembleRelease

APK="logger/build/outputs/apk/release/logger-release.apk"
echo "=== APK ready ==="
ls -lh "$APK"

# Verify signature
echo "=== Verify signature ==="
~/Android/Sdk/build-tools/36.1.0/apksigner verify -v "$APK" 2>&1 | grep -E "Verified|Number of signers"

echo ""
echo "Install on device:"
echo "  adb install -r $APK"
