#!/usr/bin/env bash
# O2.6 — build, sign, audit and verify the macOS package for the egui shell
# (EPIC O #209; spec 16.5, 19 Phase 9).
#
# The Tauri bundler used to produce the `.app`; the EPIC O cutover (ADR-0024)
# deleted it along with the rest of `apps/desktop`. There is no bundler in the
# egui build, so this script *is* the bundler: it lays out the bundle,
# composes the Info.plist from a committed template plus the generated file
# associations, installs the brand kit icon, signs, audits and reports.
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
# The script never prints "signed" or "notarized" for an artifact that is not.
# Exit 3 means "built and verifiable, NOT publishable".
#
#   scripts/package-macos-egui.sh [--out <dir>] [--evidence <dir>]
#                                 [--features <cargo features>] [--skip-build]
#
# Environment:
#   PLAUDON_MACOS_IDENTITY     Developer ID Application identity, e.g. "Developer ID Application: … (TEAMID)"
#   PLAUDON_NOTARY_PROFILE     notarytool keychain profile name
#   PLAUDON_MACOS_DEPLOYMENT_TARGET  LSMinimumSystemVersion, default 13.0
#   CARGO_BUILD_JOBS           cargo parallelism, default 2 (workstation policy)
#
# Reason tokens, shared with scripts/sign-cli.sh: unsupported_platform,
# missing_binary, missing_tool, signature_invalid, not_notarized,
# missing_credential, entitlement_rejected, identifier_mismatch.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO_ROOT="$PWD"

OUT="$REPO_ROOT/dist/macos-egui"
EVIDENCE=""
FEATURES=""
SKIP_BUILD=0
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --evidence) EVIDENCE="$2"; shift 2 ;;
    --features) FEATURES="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ "$(uname -s)" != "Darwin" ]; then
  echo "reason=unsupported_platform: the macOS package builds on macOS only" >&2
  exit 1
fi
for tool in iconutil plutil codesign /usr/libexec/PlistBuddy; do
  command -v "$tool" >/dev/null 2>&1 || { echo "reason=missing_tool: $tool is required" >&2; exit 1; }
done

mkdir -p "$OUT"

PACKAGING="$REPO_ROOT/apps/plaudon-egui/packaging"
MACOS_DIR="$PACKAGING/macos"
ENTITLEMENTS="$MACOS_DIR/entitlements.plist"
ASSOCIATIONS="$REPO_ROOT/crates/platform/resources/Info.associations.plist"
DEPLOYMENT_TARGET="${PLAUDON_MACOS_DEPLOYMENT_TARGET:-13.0}"
APP_NAME="Plaudon.app"
APP_PATH="$OUT/$APP_NAME"
EXECUTABLE="plaudon-egui"

# ---------------------------------------------------------------- build ----
export SDKROOT="${SDKROOT:-$(xcrun --show-sdk-path)}"
export CXXFLAGS="${CXXFLAGS:--isystem $SDKROOT/usr/include/c++/v1}"
export MACOSX_DEPLOYMENT_TARGET="$DEPLOYMENT_TARGET"
JOBS="${CARGO_BUILD_JOBS:-2}"

BUILD_ARGS=(build --release -j "$JOBS" -p plaudon-egui)
[ -n "$FEATURES" ] && BUILD_ARGS+=(--features "$FEATURES")

if [ "$SKIP_BUILD" = "0" ]; then
  echo "==> building plaudon-egui (release, -j $JOBS, features: ${FEATURES:-none})"
  cargo "${BUILD_ARGS[@]}"
else
  echo "==> --skip-build: reusing target/release/$EXECUTABLE"
fi

BINARY="$REPO_ROOT/target/release/$EXECUTABLE"
if [ ! -x "$BINARY" ]; then
  echo "reason=missing_binary: $BINARY was not produced" >&2
  exit 1
fi

# The user-local automation CLI ships alongside the app, not inside it
# (EPIC M, spec 14.5): it is a separate, separately signed deliverable.
CLI_PATH="$REPO_ROOT/target/release/plaudon"

# --------------------------------------------------------- bundle layout ----
echo "==> composing $APP_NAME"
rm -rf "$APP_PATH"
mkdir -p "$APP_PATH/Contents/MacOS" "$APP_PATH/Contents/Resources"
cp "$BINARY" "$APP_PATH/Contents/MacOS/$EXECUTABLE"
chmod 755 "$APP_PATH/Contents/MacOS/$EXECUTABLE"
printf 'APPL????' > "$APP_PATH/Contents/PkgInfo"

# Icon: the brand kit's `.icns`, copied in verbatim. The kit is the single
# source of truth for the artwork (brand/kit/README.txt), so the bundle icon
# is the brand icon byte-for-byte and nothing re-renders it at package time.
# It is validated before it is installed: `iconutil` round-trips a real
# `.icns` and rejects anything else.
ICNS_SOURCE="$REPO_ROOT/brand/kit/icons/desktop/plaudon.icns"
[ -f "$ICNS_SOURCE" ] || { echo "reason=missing_binary: $ICNS_SOURCE is absent; the brand kit must be checked out" >&2; exit 1; }
ICONSET="$OUT/plaudon.iconset"
rm -rf "$ICONSET"
iconutil -c iconset "$ICNS_SOURCE" -o "$ICONSET" >/dev/null || {
  echo "reason=missing_binary: $ICNS_SOURCE is not a readable .icns" >&2; exit 1; }
rm -rf "$ICONSET"
cp "$ICNS_SOURCE" "$APP_PATH/Contents/Resources/plaudon.icns"

# Info.plist: template + generated associations, merged into one dictionary.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$REPO_ROOT/Cargo.toml" | head -1)"
[ -n "$VERSION" ] || { echo "reason=missing_binary: the workspace version could not be read" >&2; exit 1; }
BUILD_NUMBER="${PLAUDON_BUILD_NUMBER:-$(git rev-list --count HEAD)}"
PLIST="$APP_PATH/Contents/Info.plist"
sed -e "s/@SHORT_VERSION@/$VERSION/" \
    -e "s/@BUNDLE_VERSION@/$VERSION.$BUILD_NUMBER/" \
    -e "s/@MINIMUM_SYSTEM_VERSION@/$DEPLOYMENT_TARGET/" \
    "$MACOS_DIR/Info.plist" > "$PLIST"
/usr/libexec/PlistBuddy -c "Merge $ASSOCIATIONS" "$PLIST" >/dev/null
plutil -lint "$PLIST"

# The bundle identifier decides where the library lives and which Launch
# Services registration survives the cutover (migration plan R7). It must be
# the identifier the application layer resolves its data directory from.
EXPECTED_IDENTIFIER="$(sed -n 's/.*APPLICATION_IDENTIFIER: &str = "\([^"]*\)".*/\1/p' \
  "$REPO_ROOT/crates/app/src/runtime.rs" | head -1)"
BUNDLE_IDENTIFIER="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$PLIST")"
if [ "$BUNDLE_IDENTIFIER" != "$EXPECTED_IDENTIFIER" ]; then
  echo "reason=identifier_mismatch: the bundle says $BUNDLE_IDENTIFIER, the application layer resolves $EXPECTED_IDENTIFIER" >&2
  exit 1
fi
DATA_DIR="$HOME/Library/Application Support/$BUNDLE_IDENTIFIER"

for key in CFBundleURLTypes CFBundleDocumentTypes UTExportedTypeDeclarations \
           NSBluetoothAlwaysUsageDescription NSSpeechRecognitionUsageDescription \
           NSRemovableVolumesUsageDescription; do
  /usr/libexec/PlistBuddy -c "Print :$key" "$PLIST" >/dev/null 2>&1 \
    || { echo "reason=missing_binary: Info.plist lost $key" >&2; exit 1; }
done

# -------------------------------------------------- entitlement audit ----
# The Tauri script dumped the effective capability manifest here. The egui
# shell has no capability manifest — the webview boundary it protected is
# gone — so what is audited instead is the only remaining privilege surface:
# the entitlements, and what the signature actually grants.
echo "==> requested entitlements"
cat "$ENTITLEMENTS"

FORBIDDEN=(
  com.apple.security.cs.allow-jit
  com.apple.security.cs.allow-unsigned-executable-memory
  com.apple.security.cs.disable-library-validation
  com.apple.security.cs.allow-dyld-environment-variables
  com.apple.security.cs.debugger
  com.apple.security.get-task-allow
  com.apple.security.device.camera
  com.apple.security.device.audio-input
  com.apple.security.network.server
)
for key in "${FORBIDDEN[@]}"; do
  if /usr/libexec/PlistBuddy -c "Print :$key" "$ENTITLEMENTS" >/dev/null 2>&1; then
    echo "reason=entitlement_rejected: $key is not a privilege this product may ship (spec 16.5)" >&2
    exit 1
  fi
done

# Nothing but the executable, the icon, the plist and the signature belongs in
# a shipped bundle: no debug symbols, no test binaries, no sources.
if find "$APP_PATH" \( -name '*.dSYM' -o -name '*.rs' -o -name '*.pdb' \) -print -quit | grep -q .; then
  echo "reason=entitlement_rejected: the bundle carries build artifacts that must not ship" >&2
  exit 1
fi

# ----------------------------------------------------------- signing ------
SIGN_STATUS="not_verified"
SIGN_DETAIL="no signing identity supplied"
NOTARY_STATUS="not_verified"
NOTARY_DETAIL="no notarytool credential supplied"

if [ -n "${PLAUDON_MACOS_IDENTITY:-}" ]; then
  echo "==> signing with $PLAUDON_MACOS_IDENTITY"
  SIGN_ARGS=(--force --timestamp --options runtime --entitlements "$ENTITLEMENTS" \
             --sign "$PLAUDON_MACOS_IDENTITY")
  [ -f "$CLI_PATH" ] && codesign "${SIGN_ARGS[@]}" "$CLI_PATH"
  codesign "${SIGN_ARGS[@]}" "$APP_PATH"
  SIGN_DETAIL="signed with a Developer ID identity and the hardened runtime"
else
  # Ad-hoc signature: the bundle launches on this machine and the structure is
  # verifiable, but it carries no identity. It is NOT publishable.
  echo "==> no Developer ID identity; applying an ad-hoc signature"
  [ -f "$CLI_PATH" ] && codesign --force --sign - "$CLI_PATH"
  codesign --force --deep --entitlements "$ENTITLEMENTS" --sign - "$APP_PATH"
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

# What the signature actually granted, not what the file asked for. A
# debugger-attachable production bundle is the native equivalent of shipping
# devtools, and this is where it would show up.
echo "==> effective entitlements"
EFFECTIVE="$(codesign -d --entitlements - --xml "$APP_PATH" 2>/dev/null \
  | plutil -convert xml1 -o - - 2>/dev/null || true)"
if ! printf '%s' "$EFFECTIVE" | grep -q 'com.apple.security.app-sandbox'; then
  # Every configuration above signs with --entitlements, so an unreadable or
  # empty dump means the audit did not happen. An audit that did not happen
  # is a failure, not a pass.
  echo "reason=entitlement_rejected: the signed bundle's entitlements could not be read back" >&2
  exit 1
fi
printf '%s\n' "$EFFECTIVE"
for key in "${FORBIDDEN[@]}"; do
  if printf '%s' "$EFFECTIVE" | grep -q "<key>$key</key>"; then
    echo "reason=entitlement_rejected: the signature granted $key" >&2
    exit 1
  fi
done

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
BIN_SHA="$(shasum -a 256 "$APP_PATH/Contents/MacOS/$EXECUTABLE" | cut -d' ' -f1)"
CLI_SHA=""
[ -f "$CLI_PATH" ] && CLI_SHA="$(shasum -a 256 "$CLI_PATH" | cut -d' ' -f1)"

# Spec 16.5: restrictive permissions on managed data. The application creates
# its library at first launch; the installer cannot pre-create it, so the
# requirement is asserted by the desktop shell's own test suite and recorded
# here as the source of the claim.
PERMISSIONS_EVIDENCE="asserted by plaudon-security fsguard tests (0700 on the managed library directory)"

REPORT="$OUT/macos-signing-report.json"
cat > "$REPORT" <<JSON
{
  "platform": "macos",
  "ui": "egui",
  "app_bundle": "$APP_NAME",
  "artifact": "$(basename "$ZIP_PATH")",
  "artifact_sha256": "$APP_SHA",
  "executable_sha256": "$BIN_SHA",
  "cli_binary_sha256": "${CLI_SHA:-null}",
  "bundle_identifier": "$BUNDLE_IDENTIFIER",
  "data_directory": "$DATA_DIR",
  "minimum_system_version": "$DEPLOYMENT_TARGET",
  "cargo_features": "${FEATURES:-none}",
  "signature_status": "$SIGN_STATUS",
  "signature_detail": "$SIGN_DETAIL",
  "notarization_status": "$NOTARY_STATUS",
  "notarization_detail": "$NOTARY_DETAIL",
  "entitlement_audit": "no forbidden entitlement is requested or granted",
  "managed_data_permissions": "$PERMISSIONS_EVIDENCE",
  "host_os_build": "$(sw_vers -buildVersion)",
  "commit": "$(git rev-parse HEAD)",
  "publishable": $([ "$SIGN_STATUS" = "verified" ] && [ "$NOTARY_STATUS" = "verified" ] && echo true || echo false)
}
JSON

if [ -n "$EVIDENCE" ]; then
  mkdir -p "$EVIDENCE/signing-verification"
  cp "$REPORT" "$EVIDENCE/signing-verification/macos-egui.json"
fi

echo "artifact: $ZIP_PATH"
echo "sha256:   $APP_SHA"
echo "data dir: $DATA_DIR"
echo "signature: $SIGN_STATUS ($SIGN_DETAIL)"
echo "notarization: $NOTARY_STATUS ($NOTARY_DETAIL)"

if [ "$SIGN_STATUS" != "verified" ] || [ "$NOTARY_STATUS" != "verified" ]; then
  echo "reason=missing_credential: the artifact is built and verifiable but NOT publishable" >&2
  exit 3
fi
