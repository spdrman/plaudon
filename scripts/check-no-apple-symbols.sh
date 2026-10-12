#!/usr/bin/env bash
# Prove that a non-macOS build links nothing Apple (task H1.2, spec 11.1).
#
# "The Apple bridge will be optional and excluded from Windows builds" is a
# claim about a binary, so it is checked by inspecting a binary rather than by
# reading the cfg attributes and believing them.
#
#   scripts/check-no-apple-symbols.sh <artifact> [<artifact> ...]
#
# On a cross-compiled Windows or Linux artifact this uses whatever symbol
# reader is available: llvm-nm, nm, or strings as a last resort. Any hit on an
# Apple framework name or a bridge entry point fails the check.
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <artifact> [<artifact> ...]" >&2
  exit 2
fi

# Entry points and frameworks that must not appear in a non-Apple artifact.
FORBIDDEN=(
  plaudon_apple_bridge_abi_version
  plaudon_apple_probe_speech
  plaudon_apple_probe_foundation_model
  plaudon_apple_transcribe_file
  plaudon_apple_generate_text
  plaudon_apple_install_speech_assets
  PlaudonAppleBridge
  SpeechAnalyzer
  SpeechTranscriber
  SystemLanguageModel
  FoundationModels
  libswiftCore
)

reader() {
  if command -v llvm-nm >/dev/null 2>&1; then
    llvm-nm --no-sort "$1" 2>/dev/null || true
  elif command -v nm >/dev/null 2>&1; then
    nm "$1" 2>/dev/null || true
  fi
  # `strings` catches a framework name recorded in a load command or an
  # import table even when the symbol table has been stripped.
  strings -a "$1" 2>/dev/null || true
}

status=0
for artifact in "$@"; do
  if [ ! -f "$artifact" ]; then
    echo "missing artifact: $artifact" >&2
    status=1
    continue
  fi
  echo "inspecting $artifact"
  symbols="$(reader "$artifact")"
  for name in "${FORBIDDEN[@]}"; do
    if printf '%s' "$symbols" | grep -qF "$name"; then
      echo "  FAIL: $artifact references $name" >&2
      status=1
    fi
  done
  if [ "$status" -eq 0 ]; then
    echo "  clean: no Apple framework or bridge symbol"
  fi
done

exit "$status"
