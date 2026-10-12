#!/usr/bin/env bash
# Verify that the plaudon CLI binary is genuinely codesigned and notarized
# (issue M2.1, contract with Team-N).
#
# Exits 0 ONLY when the binary is genuinely codesigned AND notarized/stapled.
# Every other outcome exits non-zero and prints exactly one `reason=<token>`
# line to stderr, token from:
#   missing_binary, unsigned, signature_invalid, not_notarized,
#   missing_credential, unsupported_platform
#
# Usage:
#   scripts/verify-cli-signature.sh <path>
#   scripts/verify-cli-signature.sh --help
set -euo pipefail

if [[ $# -eq 0 ]] || [[ "$1" == "--help" ]]; then
  echo "Usage: $0 <path-to-binary>"
  echo ""
  echo "Verify that a binary is codesigned and notarized."
  echo "Exits 0 only when genuinely signed and notarized."
  exit 0
fi

BINARY="$1"

# Missing binary check
if [[ ! -f "$BINARY" ]]; then
  echo "reason=missing_binary" >&2
  exit 1
fi

case "$(uname -s)" in
  Darwin)
    # Spec §17.1: macOS binaries must be codesigned and notarized.
    
    # Check if binary is signed at all (codesign -v exits 0 for ad-hoc too)
    if ! codesign --verify --deep --strict --verbose=2 "$BINARY" 2>&1 | grep -q "satisfies"; then
      echo "reason=signature_invalid" >&2
      exit 1
    fi
    
    # Check if ad-hoc signed (Signature=adhoc) - this is unsigned from release perspective
    if codesign -dv "$BINARY" 2>&1 | grep -q "Signature=adhoc"; then
      echo "reason=unsigned" >&2
      exit 1
    fi
    
    # Check if there's any signature at all beyond ad-hoc
    if ! codesign -dv "$BINARY" 2>&1 | grep -q "Authority="; then
      echo "reason=unsigned" >&2
      exit 1
    fi
    
    # Verify the signature is valid
    if ! codesign --verify --deep --strict "$BINARY" 2>/dev/null; then
      echo "reason=signature_invalid" >&2
      exit 1
    fi
    
    # Check notarization via stapler
    if ! xcrun stapler validate "$BINARY" &>/dev/null; then
      # Try spctl as fallback
      if ! spctl --assess --type execute "$BINARY" &>/dev/null; then
        echo "reason=not_notarized" >&2
        exit 1
      fi
    fi
    
    # All checks passed
    exit 0
    ;;
    
  MINGW* | MSYS* | CYGWIN*)
    # Windows via git-bash
    if command -v signtool &>/dev/null; then
      # Check if signed and verify
      if ! signtool verify /pa /v "$BINARY" &>/dev/null; then
        # Could be unsigned or invalid signature - signtool doesn't distinguish clearly
        # so we check if it has ANY signature
        if signtool verify "$BINARY" 2>&1 | grep -qi "not signed"; then
          echo "reason=unsigned" >&2
        else
          echo "reason=signature_invalid" >&2
        fi
        exit 1
      fi
      exit 0
    else
      echo "reason=missing_credential" >&2
      exit 1
    fi
    ;;
    
  *)
    # Linux and other platforms: not supported for codesigning workflow
    echo "reason=unsupported_platform" >&2
    exit 1
    ;;
esac
