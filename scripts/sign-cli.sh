#!/usr/bin/env bash
# Codesign and notarize the plaudon CLI binary (issue M2.1).
#
# On macOS: uses MACOS_SIGNING_IDENTITY and MACOS_NOTARY_PROFILE
# (or APPLE_ID/APPLE_TEAM_ID/APPLE_APP_PASSWORD).
# On Windows: uses WINDOWS_SIGNING_CERT and WINDOWS_SIGNING_PASSWORD.
#
# Fails loudly when credentials are absent - never silently skips.
# Calls scripts/verify-cli-signature.sh after signing and propagates its exit code.
#
# Usage:
#   scripts/sign-cli.sh <path-to-binary>
#   scripts/sign-cli.sh --help
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ $# -eq 0 ]] || [[ "$1" == "--help" ]]; then
  echo "Usage: $0 <path-to-binary>"
  echo ""
  echo "Codesign and notarize the plaudon CLI binary."
  echo ""
  echo "macOS environment variables (required):"
  echo "  MACOS_SIGNING_IDENTITY     - Developer ID Application identity"
  echo "  MACOS_NOTARY_PROFILE       - Notarization profile name"
  echo "  OR:"
  echo "  APPLE_ID                   - Apple ID email"
  echo "  APPLE_TEAM_ID              - Team ID"
  echo "  APPLE_APP_PASSWORD         - App-specific password"
  echo ""
  echo "Windows environment variables (required):"
  echo "  WINDOWS_SIGNING_CERT       - Path to certificate file"
  echo "  WINDOWS_SIGNING_PASSWORD   - Certificate password"
  exit 0
fi

BINARY="$1"

if [[ ! -f "$BINARY" ]]; then
  echo "Error: Binary not found: $BINARY" >&2
  exit 1
fi

case "$(uname -s)" in
  Darwin)
    # Spec §17.1: macOS release binaries must be codesigned and notarized
    
    if [[ -z "${MACOS_SIGNING_IDENTITY:-}" ]]; then
      echo "Error: MACOS_SIGNING_IDENTITY environment variable is required" >&2
      exit 1
    fi
    
    echo "Codesigning $BINARY with identity: $MACOS_SIGNING_IDENTITY"
    codesign --force --options runtime --sign "$MACOS_SIGNING_IDENTITY" "$BINARY"
    
    # Verify the signature before notarization
    if ! codesign --verify --deep --strict "$BINARY"; then
      echo "Error: Codesigning verification failed" >&2
      exit 1
    fi
    
    # Create a ZIP for notarization (required for single binaries)
    NOTARIZE_ZIP="$(mktemp -d)/plaudon.zip"
    ditto -c -k --keepParent "$BINARY" "$NOTARIZE_ZIP"
    
    echo "Submitting for notarization..."
    
    if [[ -n "${MACOS_NOTARY_PROFILE:-}" ]]; then
      # Use stored credentials via notarytool profile
      xcrun notarytool submit "$NOTARIZE_ZIP" --keychain-profile "$MACOS_NOTARY_PROFILE" --wait
    elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_TEAM_ID:-}" && -n "${APPLE_APP_PASSWORD:-}" ]]; then
      # Use explicit credentials
      xcrun notarytool submit "$NOTARIZE_ZIP" \
        --apple-id "$APPLE_ID" \
        --team-id "$APPLE_TEAM_ID" \
        --password "$APPLE_APP_PASSWORD" \
        --wait
    else
      echo "Error: Either MACOS_NOTARY_PROFILE or (APPLE_ID + APPLE_TEAM_ID + APPLE_APP_PASSWORD) required" >&2
      rm -f "$NOTARIZE_ZIP"
      exit 1
    fi
    
    # Staple the notarization ticket to the binary
    echo "Stapling notarization ticket..."
    xcrun stapler staple "$BINARY" || {
      echo "Warning: Stapling failed, but notarization may have succeeded" >&2
      echo "The binary may still be valid if downloaded from the network." >&2
    }
    
    rm -f "$NOTARIZE_ZIP"
    ;;
    
  MINGW* | MSYS* | CYGWIN*)
    # Windows signing
    
    if [[ -z "${WINDOWS_SIGNING_CERT:-}" ]]; then
      echo "Error: WINDOWS_SIGNING_CERT environment variable is required" >&2
      exit 1
    fi
    
    if [[ -z "${WINDOWS_SIGNING_PASSWORD:-}" ]]; then
      echo "Error: WINDOWS_SIGNING_PASSWORD environment variable is required" >&2
      exit 1
    fi
    
    if ! command -v signtool &>/dev/null; then
      echo "Error: signtool not found in PATH (Windows SDK required)" >&2
      exit 1
    fi
    
    echo "Signing $BINARY with certificate: $WINDOWS_SIGNING_CERT"
    signtool sign /f "$WINDOWS_SIGNING_CERT" /p "$WINDOWS_SIGNING_PASSWORD" \
      /tr http://timestamp.digicert.com /td sha256 /fd sha256 "$BINARY"
    ;;
    
  *)
    echo "Error: Unsupported platform for codesigning: $(uname -s)" >&2
    exit 1
    ;;
esac

echo "Verifying signature..."
exec scripts/verify-cli-signature.sh "$BINARY"
