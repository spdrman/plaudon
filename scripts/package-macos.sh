#!/usr/bin/env bash
# N2.1 — build, sign, notarize and verify the macOS package (spec 16.5, 19 Phase 9).
#
# Honest failure modes, in order of preference:
#   1. A Developer ID identity and notarytool credentials are present: the
#      bundle is signed with hardened runtime, notarized, stapled and verified.
#      This is the only configuration that may be published.
#   2. Only a signing identity is present: the bundle is signed and verified,
#      notarization is recorded as `not_verified` and the RELEASE GATE FAILS.
#   3. Nothing is present: the bundle is ad-hoc signed so it launches locally,
#      both signature and notarization are recorded as `not_verified`, and the
#      RELEASE GATE FAILS.
#
# The script never prints "signed" for an unsigned artifact. The verification
# report it writes is what the evidence bundle archives (spec 24).
#
#   scripts/package-macos.sh [--out <dir>] [--evidence <dir>]
#
# Environment:
#   PLAUDON_MACOS_IDENTITY     Developer ID Application identity, e.g. "Developer ID Application: … (TEAMID)"
#   PLAUDON_NOTARY_PROFILE     notarytool keychain profile name
#   PLAUDON_MACOS_DEPLOYMENT_TARGET  defaults to the Tauri configuration
set -euo pipefail
cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"

OUT="$REPO_ROOT/dist/macos"
EVIDENCE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --evidence) EVIDENCE="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ "$(uname -s)" != "Darwin" ]; then
  echo "reason=unsupported_platform: the macOS package builds on macOS only" >&2
  exit 1
fi

mkdir -p "$OUT"

# ---------------------------------------------------------------- build ----
export SDKROOT="${SDKROOT:-$(xcrun --show-sdk-path)}"
export CXXFLAGS="${CXXFLAGS:--isystem $SDKROOT/usr/include/c++/v1}"

echo "==> building the application bundle"
(cd apps/desktop && npm ci --no-audit --no-fund && npx tauri build --bundles app)

APP_PATH="$(find "$REPO_ROOT/target/release/bundle/macos" -maxdepth 1 -name '*.app' -print -quit)"
if [ -z "$APP_PATH" ]; then
  echo "reason=missing_binary: no .app bundle was produced" >&2
  exit 1
fi
APP_NAME="$(basename "$APP_PATH")"

# The user-local automation CLI ships alongside the app (EPIC M, spec 14.5).
CLI_PATH="$REPO_ROOT/target/release/plaudon"

# ------------------------------------------------------ capability audit ----
# Spec 16.5 and 2.4: dump the effective capabilities and entitlements into the
# build log so a reviewer sees what was actually shipped, not what the
# configuration intended.
echo "==> effective Tauri capabilities"
cat apps/desktop/src-tauri/capabilities/*.json

DEVTOOLS="$(grep -c '"devtools"[[:space:]]*:[[:space:]]*true' apps/desktop/src-tauri/tauri.conf.json || true)"
if [ "$DEVTOOLS" != "0" ]; then
  echo "reason=devtools_enabled: production bundles must have devtools disabled (spec 16.5)" >&2
  exit 1
fi

# ----------------------------------------------------------- signing ------
SIGN_STATUS="not_verified"
SIGN_DETAIL="no signing identity supplied"
NOTARY_STATUS="not_verified"
NOTARY_DETAIL="no notarytool credential supplied"
ENTITLEMENTS="apps/desktop/src-tauri/entitlements.plist"

if [ -n "${PLAUDON_MACOS_IDENTITY:-}" ]; then
  echo "==> signing with $PLAUDON_MACOS_IDENTITY"
  SIGN_ARGS=(--force --timestamp --options runtime --sign "$PLAUDON_MACOS_IDENTITY")
  [ -f "$ENTITLEMENTS" ] && SIGN_ARGS+=(--entitlements "$ENTITLEMENTS")
  [ -f "$CLI_PATH" ] && codesign "${SIGN_ARGS[@]}" "$CLI_PATH"
  codesign "${SIGN_ARGS[@]}" "$APP_PATH"
  SIGN_DETAIL="signed with a Developer ID identity"
else
  # Ad-hoc signature: the bundle launches on this machine and the structure is
  # verifiable, but it carries no identity. It is NOT publishable.
  echo "==> no Developer ID identity; applying an ad-hoc signature"
  [ -f "$CLI_PATH" ] && codesign --force --sign - "$CLI_PATH"
  codesign --force --deep --sign - "$APP_PATH"
  SIGN_DETAIL="ad-hoc signature only; no Developer ID identity was available"
fi

echo "==> verifying the signature"
if codesign --verify --deep --strict --verbose=2 "$APP_PATH" 2>&1; then
  if [ -n "${PLAUDON_MACOS_IDENTITY:-}" ]; then
    SIGN_STATUS="verified"
  else
    SIGN_STATUS="adhoc_verified"
  fi
else
  echo "reason=signature_invalid: codesign verification failed" >&2
  exit 1
fi

# -------------------------------------------------------- notarization ----
if [ -n "${PLAUDON_NOTARY_PROFILE:-}" ] && [ "$SIGN_STATUS" = "verified" ]; then
  echo "==> notarizing"
  ZIP_FOR_NOTARY="$OUT/${APP_NAME%.app}-notarize.zip"
  /usr/bin/ditto -c -k --keepParent "$APP_PATH" "$ZIP_FOR_NOTARY"
  xcrun notarytool submit "$ZIP_FOR_NOTARY" \
    --keychain-profile "$PLAUDON_NOTARY_PROFILE" --wait
  xcrun stapler staple "$APP_PATH"
  if xcrun stapler validate "$APP_PATH" && spctl --assess --type execute --verbose "$APP_PATH"; then
    NOTARY_STATUS="verified"
    NOTARY_DETAIL="notarized and stapled; spctl assessment passed"
  else
    echo "reason=not_notarized: stapling or assessment failed" >&2
    exit 1
  fi
fi

# ------------------------------------------------------------- package ----
ZIP_PATH="$OUT/${APP_NAME}.zip"
rm -f "$ZIP_PATH"
/usr/bin/ditto -c -k --keepParent "$APP_PATH" "$ZIP_PATH"
APP_SHA="$(shasum -a 256 "$ZIP_PATH" | cut -d' ' -f1)"
CLI_SHA=""
[ -f "$CLI_PATH" ] && CLI_SHA="$(shasum -a 256 "$CLI_PATH" | cut -d' ' -f1)"

# -------------------------------------------------------- data dir perms --
# Spec 16.5: restrictive permissions on managed data. The application creates
# its library at first launch; the installer cannot pre-create it, so the
# requirement is asserted by the desktop shell's own test suite and recorded
# here as the source of the claim.
PERMISSIONS_EVIDENCE="asserted by plaudon-security fsguard tests (0700 on the managed library directory)"

REPORT="$OUT/macos-signing-report.json"
cat > "$REPORT" <<JSON
{
  "platform": "macos",
  "app_bundle": "$APP_NAME",
  "artifact": "$(basename "$ZIP_PATH")",
  "artifact_sha256": "$APP_SHA",
  "cli_binary_sha256": "${CLI_SHA:-null}",
  "signature_status": "$SIGN_STATUS",
  "signature_detail": "$SIGN_DETAIL",
  "notarization_status": "$NOTARY_STATUS",
  "notarization_detail": "$NOTARY_DETAIL",
  "devtools_enabled": false,
  "managed_data_permissions": "$PERMISSIONS_EVIDENCE",
  "host_os_build": "$(sw_vers -buildVersion)",
  "commit": "$(git rev-parse HEAD)",
  "publishable": $([ "$SIGN_STATUS" = "verified" ] && [ "$NOTARY_STATUS" = "verified" ] && echo true || echo false)
}
JSON

if [ -n "$EVIDENCE" ]; then
  mkdir -p "$EVIDENCE/signing-verification"
  cp "$REPORT" "$EVIDENCE/signing-verification/macos.json"
fi

echo "artifact: $ZIP_PATH"
echo "sha256:   $APP_SHA"
echo "signature: $SIGN_STATUS ($SIGN_DETAIL)"
echo "notarization: $NOTARY_STATUS ($NOTARY_DETAIL)"

if [ "$SIGN_STATUS" != "verified" ] || [ "$NOTARY_STATUS" != "verified" ]; then
  echo "reason=missing_credential: the artifact is built and verifiable but NOT publishable" >&2
  exit 3
fi
