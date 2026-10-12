#!/usr/bin/env bash
# Build the macOS-only Apple bridge static library (spec 11.1, 11.6).
#
# Not part of the default workspace build. `crates/ai-apple` links the result
# only with `--features apple-bridge`, and a Windows build never runs this
# script or links an Apple framework.
#
# swiftc is invoked directly rather than through SwiftPM so the build needs no
# package manifest, no network access and no Xcode project.
set -euo pipefail
cd "$(dirname "$0")/../native/apple-bridge"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "the Apple bridge builds on macOS only" >&2
  exit 1
fi

DEPLOYMENT_TARGET="${PLAUDON_MACOS_DEPLOYMENT_TARGET:-26.0}"
ARCH="${PLAUDON_APPLE_ARCH:-$(uname -m)}"
OUT_DIR="${1:-build}"
TARGET="${ARCH}-apple-macosx${DEPLOYMENT_TARGET}"

mkdir -p "$OUT_DIR"
swiftc \
  -emit-library -static \
  -O \
  -target "$TARGET" \
  -module-name PlaudonAppleBridge \
  -emit-module -emit-module-path "$OUT_DIR/PlaudonAppleBridge.swiftmodule" \
  -o "$OUT_DIR/libPlaudonAppleBridge.a" \
  Sources/PlaudonAppleBridge/Bridge.swift

echo "static library: $(pwd)/$OUT_DIR/libPlaudonAppleBridge.a"
echo "header:         $(pwd)/include/plaudon_apple_bridge.h"
echo "target:         $TARGET"
echo
echo "Exported C entry points:"
nm -gU "$OUT_DIR/libPlaudonAppleBridge.a" 2>/dev/null \
  | awk '/ T _plaudon_apple/ {print "  " substr($3, 2)}' | sort -u
echo
echo "Link with:"
echo "  RUSTFLAGS=\"-L $(pwd)/$OUT_DIR\" cargo test -p plaudon-ai-apple --features apple-bridge"
