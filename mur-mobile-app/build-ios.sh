#!/usr/bin/env bash
#
# Build the Rust mobile SDK as an iOS XCFramework and (re)generate the Swift
# bindings, then you can `xcodegen generate && open MurVoice.xcodeproj`.
#
# Requires **full Xcode** (not just Command Line Tools) for `xcodebuild`, plus
# the iOS Rust targets (added automatically below).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
CRATE="mur-mobile-sdk"
LIB="mur_mobile_sdk"

echo "==> Ensuring iOS Rust targets"
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios

echo "==> Building static libs (release) for device + simulator"
cd "$ROOT"
cargo build -p "$CRATE" --release --target aarch64-apple-ios
cargo build -p "$CRATE" --release --target aarch64-apple-ios-sim
cargo build -p "$CRATE" --release --target x86_64-apple-ios

echo "==> Fusing the simulator slices (arm64 + x86_64)"
SIM_DIR="$ROOT/target/ios-sim/release"
mkdir -p "$SIM_DIR"
lipo -create \
  "$ROOT/target/aarch64-apple-ios-sim/release/lib${LIB}.a" \
  "$ROOT/target/x86_64-apple-ios/release/lib${LIB}.a" \
  -output "$SIM_DIR/lib${LIB}.a"

echo "==> Regenerating Swift bindings"
cargo build -p "$CRATE" --features cli
"$ROOT/target/debug/uniffi-bindgen" generate \
  --library "$ROOT/target/debug/lib${LIB}.dylib" \
  --language swift --out-dir "$HERE/Generated"

echo "==> Assembling headers for the xcframework"
HDR="$HERE/Generated/headers"
rm -rf "$HDR"; mkdir -p "$HDR"
cp "$HERE/Generated/${LIB}FFI.h" "$HDR/"
# xcframework expects the modulemap to be named module.modulemap.
cp "$HERE/Generated/${LIB}FFI.modulemap" "$HDR/module.modulemap"

echo "==> Creating MurMobileSDK.xcframework"
rm -rf "$HERE/Frameworks/MurMobileSDK.xcframework"
mkdir -p "$HERE/Frameworks"
xcodebuild -create-xcframework \
  -library "$ROOT/target/aarch64-apple-ios/release/lib${LIB}.a" -headers "$HDR" \
  -library "$SIM_DIR/lib${LIB}.a" -headers "$HDR" \
  -output "$HERE/Frameworks/MurMobileSDK.xcframework"

echo "==> Done. Next:"
echo "    cd $HERE && xcodegen generate && open MurVoice.xcodeproj"
