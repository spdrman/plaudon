#!/usr/bin/env bash
# Fetch the two bundle-tier speaker models by exact file name and digest
# (ADR-0081 §1, VOICE_PROFILES_SPEC VP-040).
#
# The weights are NOT committed: they are 32 MB together, and the point of the
# model-pack flow is that a user sees the licence and the checksum before the
# bytes are installed. This script is the developer-side half of that flow and
# the only sanctioned way to obtain them for a measurement run.
#
# Two rules are deliberate and must not be "simplified" later:
#
#   1. Each file is fetched by its exact published name. The sherpa-onnx
#      release tag below also carries Rev's non-commercial models; a loop over
#      "the ONNX files in this tag" would download one of those.
#   2. A digest mismatch deletes the file and fails. There is no --force and no
#      "warn and continue": weights whose provenance is not the reviewed one
#      are not the weights that were reviewed.
#
# The digests, sizes and licences here are the same values compiled into
# `crates/ai-speaker/src/checkpoint.rs` and tabulated in
# `docs/licensing/third-party-register.md`;
# `plaudon-integration-tests --test licensing_register` fails when they drift.
#
#   scripts/fetch-speaker-models.sh [--dir <path>]
set -euo pipefail
cd "$(dirname "$0")/.."

DIR="tests/fixtures/speaker/models"
while [ $# -gt 0 ]; do
  case "$1" in
    --dir) DIR="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

mkdir -p "$DIR"

fetch() {
  local url="$1" file="$2" sha="$3" licence_url="$4" licence_file="$5"
  local target="$DIR/$file"
  if [ -f "$target" ]; then
    echo "already present: $target"
  else
    echo "downloading $url"
    curl --fail --location --silent --show-error --output "$target.part" "$url"
    mv "$target.part" "$target"
  fi
  local observed
  observed="$(shasum -a 256 "$target" | cut -d' ' -f1)"
  if [ "$observed" != "$sha" ]; then
    echo "checksum mismatch for $target" >&2
    echo "  expected $sha" >&2
    echo "  observed $observed" >&2
    rm -f "$target"
    exit 1
  fi
  echo "verified $target ($observed)"

  if [ -n "$licence_url" ]; then
    if [ ! -f "$DIR/$licence_file" ]; then
      curl --fail --location --silent --show-error --output "$DIR/$licence_file" "$licence_url"
    fi
    echo "licence text: $DIR/$licence_file"
  fi
}

# WeSpeaker ResNet34-LM, 256-dimensional speaker embedding, CC-BY-4.0.
# The file is the upstream WeSpeaker export plus 241 bytes of ONNX
# metadata_props (sample_rate, output_dim, normalize_samples); the first
# 26 530 309 bytes are byte-identical to
# https://wespeaker-1256283475.cos.ap-shanghai.myqcloud.com/models/voxceleb/voxceleb_resnet34_LM.onnx
# (sha256 7bb2f06e9df17cdf1ef14ee8a15ab08ed28e8d0ef5054ee135741560df2ec068),
# verified 2026-10-12.
fetch \
  "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx" \
  "wespeaker_en_voxceleb_resnet34_LM.onnx" \
  "e9848563da86f263117134dfd7ad63c92355b37de492b55e325400c9d9c39012" \
  "" ""

# pyannote segmentation-3.0 exported to ONNX, MIT, (c) 2022 CNRS. The licence
# text travels with the weights and is fetched beside them.
fetch \
  "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx" \
  "pyannote-segmentation-3.0.onnx" \
  "220ad67ca923bef2fa91f2390c786097bf305bceb5e261d4af67b38e938e1079" \
  "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/LICENSE" \
  "pyannote-segmentation-3.0.LICENSE.txt"

cat <<'ATTRIBUTION'

Attribution these weights oblige (already in NOTICE):
  * pyannote segmentation-3.0, (c) 2022 CNRS, MIT licence.
  * WeSpeaker ResNet34-LM, (c) WeSpeaker authors, CC-BY-4.0.
  * VoxCeleb (Nagrani, Chung, Zisserman; Visual Geometry Group, University of
    Oxford), CC-BY-4.0 — the training corpus whose licence the weights inherit.
ATTRIBUTION
