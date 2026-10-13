#!/usr/bin/env bash
# Vendor the pinned ONNX Runtime build the `onnx` feature links (ADR-0081 §3,
# issue #283).
#
# `ort`'s `download-binaries` feature fetches a prebuilt ONNX Runtime over TLS
# *from the build script*. This project does not enable it: a release build must
# not reach a CDN while it compiles, and that feature also drags `ureq`,
# `socks`, `lzma-rust2` and `hmac-sha256` into the build graph. Instead the
# archive is fetched once, here, by exact URL and digest, and the build links it
# through `ORT_LIB_LOCATION` — after which `cargo build --offline` works.
#
# Three rules are deliberate and must not be "simplified" later:
#
#   1. The URL and the digest are the ones `ort-sys` itself publishes for the
#      pinned `ort` version (`build/download/dist.tsv` in the vendored crate
#      source). So these are the same bytes the build script would have
#      downloaded — verified against upstream's own table rather than against a
#      value this project invented.
#   2. A digest mismatch deletes the file and fails. There is no --force and no
#      "warn and continue".
#   3. Nothing fetched here is committed. The extracted static library is 83 MB
#      on macOS and 343 MB on Windows; `vendor/` is ignored by git and the
#      digest is what is under review.
#
# The values below are compiled into `crates/ai-speaker/src/runtime.rs` and
# tabulated in `docs/licensing/third-party-register.md`;
# `plaudon-integration-tests --test licensing_register` fails when they drift.
#
#   scripts/fetch-onnxruntime.sh [--target <triple>] [--all] [--dir <path>]
#
# Windows has its own script, `scripts/fetch-onnxruntime.ps1`, because the
# PowerShell host there has neither `curl --fail` nor `tar` semantics we rely on
# here.
set -euo pipefail
cd "$(dirname "$0")/.."

DIR="vendor/onnxruntime"
TARGETS=""

host_target() {
  if command -v rustc >/dev/null 2>&1; then
    rustc -vV | awk '/^host: /{print $2}'
  else
    echo "unknown"
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGETS="$2"; shift 2 ;;
    --all) TARGETS="aarch64-apple-darwin x86_64-pc-windows-msvc"; shift ;;
    --dir) DIR="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$TARGETS" ] || TARGETS="$(host_target)"

# ONNX Runtime 1.31.0, MIT, (c) Microsoft Corporation. The builds are pyke's
# single-file static libraries, which is what `ort` 2.0.0-rc.14 expects.
url_for() {
  case "$1" in
    aarch64-apple-darwin)
      echo "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.31.0/aarch64-apple-darwin+coreml.tar.lzma2" ;;
    x86_64-pc-windows-msvc)
      echo "https://cdn.pyke.io/0/pyke:ort-rs/ms@1.31.0/x86_64-pc-windows-msvc+directml.tar.lzma2" ;;
    *) echo "" ;;
  esac
}

sha_for() {
  case "$1" in
    aarch64-apple-darwin)
      echo "4033efca0e6c1606bd2ec3870d4cb19d0dba0ad0160dbfb616e66cae969ed68c" ;;
    x86_64-pc-windows-msvc)
      echo "f99aaef23f12986abb05924c944d8433521263ad756b462df762a5a982c98405" ;;
    *) echo "" ;;
  esac
}

library_for() {
  case "$1" in
    aarch64-apple-darwin) echo "libonnxruntime.a" ;;
    x86_64-pc-windows-msvc) echo "onnxruntime.lib" ;;
    *) echo "" ;;
  esac
}

sha256_of() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    sha256sum "$1" | cut -d' ' -f1
  fi
}

# The dist is a raw LZMA2 stream (no .xz container) wrapping a tar, with the
# 64 MiB dictionary `ort-sys` decodes it with. `tar` cannot read that and macOS
# ships no `xz`, so Python's `lzma` does the one job it is needed for.
extract() {
  local archive="$1" into="$2"
  python3 - "$archive" <<'PY' | tar -x -C "$into" -f -
import lzma, sys

decompressor = lzma.LZMADecompressor(
    format=lzma.FORMAT_RAW,
    filters=[{"id": lzma.FILTER_LZMA2, "dict_size": 1 << 26}],
)
with open(sys.argv[1], "rb") as source:
    while chunk := source.read(1 << 20):
        sys.stdout.buffer.write(decompressor.decompress(chunk))
PY
}

command -v python3 >/dev/null 2>&1 || {
  echo "python3 is required to decompress the raw LZMA2 dist" >&2
  exit 1
}

for target in $TARGETS; do
  url="$(url_for "$target")"
  sha="$(sha_for "$target")"
  library="$(library_for "$target")"
  if [ -z "$url" ]; then
    echo "no ONNX Runtime archive is vendored for '$target'." >&2
    echo "The two reference platforms are aarch64-apple-darwin and x86_64-pc-windows-msvc" >&2
    echo "(ADR-0081 §4); adding a third means adding its digest to" >&2
    echo "crates/ai-speaker/src/runtime.rs and the licence register first." >&2
    exit 2
  fi

  into="$DIR/$target"
  mkdir -p "$into"
  archive="$into/$(basename "$url")"

  if [ -f "$archive" ]; then
    echo "already present: $archive"
  else
    echo "downloading $url"
    curl --fail --location --silent --show-error --output "$archive.part" "$url"
    mv "$archive.part" "$archive"
  fi

  observed="$(sha256_of "$archive")"
  if [ "$observed" != "$sha" ]; then
    echo "checksum mismatch for $archive" >&2
    echo "  expected $sha" >&2
    echo "  observed $observed" >&2
    rm -f "$archive"
    exit 1
  fi
  echo "verified $archive ($observed)"

  if [ ! -f "$into/$library" ]; then
    extract "$archive" "$into"
  fi
  if [ ! -f "$into/$library" ]; then
    echo "the archive did not contain $library" >&2
    exit 1
  fi
  echo "extracted $into/$library"

  cat <<EOF

ONNX Runtime 1.31.0 for $target is vendored. To build with it:

  export ORT_LIB_LOCATION="\$PWD/$into"
  cargo build --offline -p plaudon-ai-speaker --features onnx

EOF
done

cat <<'ATTRIBUTION'
Attribution this binary obliges (already in NOTICE):
  * ONNX Runtime, (c) Microsoft Corporation, MIT licence.
  * The Windows build additionally carries DirectML, (c) Microsoft
    Corporation, which ships with Windows and is linked, not redistributed.
ATTRIBUTION
