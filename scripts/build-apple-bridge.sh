#!/usr/bin/env bash
# Build the macOS-only Apple bridge static library (spec 11.1, 11.6).
#
# Not part of the default workspace build. `crates/ai-apple/build.rs` compiles
# the same sources during `cargo build --features apple-bridge`; this script
# exists for CI jobs that want the library once, for the sanitizer suite, and
# for inspecting what was produced.
#
# swiftc is invoked directly rather than through SwiftPM so the build needs no
# package manifest, no network access and no Xcode project.
#
#   scripts/build-apple-bridge.sh [out-dir]
#
# Environment:
#   PLAUDON_MACOS_DEPLOYMENT_TARGET  default 13.0 (the load floor, not the
#                                    Apple AI floor, which is 26.0)
#   PLAUDON_APPLE_ARCH               default: this machine's architecture
set -euo pipefail
cd "$(dirname "$0")/../native/apple-bridge"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "the Apple bridge builds on macOS only" >&2
  exit 1
fi

if ! xcrun -f swiftc >/dev/null 2>&1 && ! command -v swiftc >/dev/null 2>&1; then
  echo "no Swift compiler found; run 'xcode-select --install'" >&2
  exit 1
fi

DEPLOYMENT_TARGET="${PLAUDON_MACOS_DEPLOYMENT_TARGET:-13.0}"
ARCH="${PLAUDON_APPLE_ARCH:-$(uname -m)}"
OUT_DIR="${1:-build}"
TARGET="${ARCH}-apple-macosx${DEPLOYMENT_TARGET}"
SDK="$(xcrun --sdk macosx --show-sdk-path)"

mkdir -p "$OUT_DIR"

# Cargo exports DYLD_FALLBACK_LIBRARY_PATH when it runs a build script, and an
# inherited value makes swift-frontend fail to load its own standard library.
# Unset here too so the script behaves the same inside and outside cargo.
unset DYLD_FALLBACK_LIBRARY_PATH DYLD_LIBRARY_PATH

swiftc \
  -emit-library -static \
  -O \
  -target "$TARGET" \
  -sdk "$SDK" \
  -module-name PlaudonAppleBridge \
  -emit-module -emit-module-path "$OUT_DIR/PlaudonAppleBridge.swiftmodule" \
  -o "$OUT_DIR/libPlaudonAppleBridge.a" \
  Sources/PlaudonAppleBridge/*.swift

echo "static library: $(pwd)/$OUT_DIR/libPlaudonAppleBridge.a"
echo "header:         $(pwd)/include/plaudon_apple_bridge.h"
echo "target:         $TARGET"
echo "sdk:            $SDK"
echo
echo "Exported C entry points:"
nm -gU "$OUT_DIR/libPlaudonAppleBridge.a" 2>/dev/null \
  | awk '/ T _plaudon_apple/ {print "  " substr($3, 2)}' | sort -u
echo
echo "Link with:"
echo "  PLAUDON_APPLE_BRIDGE_LIB_DIR=$(pwd)/$OUT_DIR \\"
echo "    cargo test -p plaudon-ai-apple --features apple-bridge"
