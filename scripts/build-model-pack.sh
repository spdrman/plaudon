#!/usr/bin/env bash
# Build a redistributable offline model pack (spec §1.2(2), §24 — task F2.3).
#
# A pack is an uncompressed directory tree so that an air-gapped installation
# can use it directly from removable media, and so that there is no
# decompression ratio for a hostile artifact to lie about:
#
#   <out>/pack.json
#   <out>/models/<engine>/<version>/<file>
#   <out>/licenses/<SPDX>.txt
#
# Reproducible: the emitted `artifact_digest` is a function of the sorted
# (path, size, sha256) triples and the pack version only, so two builds from
# identical inputs agree byte for byte. That digest is what goes into the
# release evidence bundle.
#
# Usage:
#   scripts/build-model-pack.sh --out dist/offline-pack --version 2026.10.1 \
#       --model id=tiny-en,engine=whisper.cpp,version=v1,file=/path/ggml-tiny.en.bin,\
#license=MIT,license_file=licenses/MIT.txt,display=Whisper Tiny (English),\
#languages=en,min_ram=536870912
#
# Only models whose licence is marked redistributable are accepted; everything
# else is refused rather than quietly shipped.
set -euo pipefail
cd "$(dirname "$0")/.."

OUT=""
PACK_VERSION=""
declare -a MODEL_SPECS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --version) PACK_VERSION="$2"; shift 2 ;;
    --model) MODEL_SPECS+=("$2"); shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ -n "$OUT" ]] || { echo "--out is required" >&2; exit 2; }
[[ -n "$PACK_VERSION" ]] || { echo "--version is required" >&2; exit 2; }
[[ ${#MODEL_SPECS[@]} -gt 0 ]] || { echo "at least one --model is required" >&2; exit 2; }

PACK_VERSION="$PACK_VERSION" OUT="$OUT" python3 - "${MODEL_SPECS[@]}" <<'PY'
import hashlib, json, os, shutil, sys

out = os.environ["OUT"]
pack_version = os.environ["PACK_VERSION"]
specs = sys.argv[1:]

SPDX_REDISTRIBUTABLE = {
    "MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "CC0-1.0",
    "CC-BY-4.0", "CC-BY-SA-4.0", "Unlicense", "ISC",
}

def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()

def parse(spec):
    fields = {}
    for part in spec.split(","):
        if not part:
            continue
        key, _, value = part.partition("=")
        fields[key.strip()] = value.strip()
    return fields

os.makedirs(out, exist_ok=True)
models, files = [], []

for spec in specs:
    f = parse(spec)
    for required in ("id", "engine", "version", "file", "license", "license_file"):
        if required not in f:
            sys.exit(f"model spec is missing `{required}`: {spec}")
    spdx = f["license"]
    if spdx not in SPDX_REDISTRIBUTABLE:
        sys.exit(
            f"refusing to pack `{f['id']}`: licence `{spdx}` is not on the "
            f"verified-redistributable list"
        )
    if not os.path.isfile(f["license_file"]):
        sys.exit(f"licence text not found: {f['license_file']}")

    leaf = os.path.basename(f["file"])
    rel_model = f"models/{f['engine']}/{f['version']}/{leaf}"
    rel_license = f"licenses/{os.path.basename(f['license_file'])}"
    for rel, src in ((rel_model, f["file"]), (rel_license, f["license_file"])):
        dst = os.path.join(out, rel)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copyfile(src, dst)
        files.append({
            "path": rel,
            "size_bytes": os.path.getsize(dst),
            "sha256": digest(dst),
        })

    models.append({
        "id": f["id"],
        "engine": f["engine"],
        "version": f["version"],
        "display_name": f.get("display", f["id"]),
        "file_name": leaf,
        "size_bytes": os.path.getsize(os.path.join(out, rel_model)),
        "sha256": digest(os.path.join(out, rel_model)),
        "languages": [tag for tag in f.get("languages", "").split("|") if tag],
        "license": {
            "spdx_id": spdx,
            "name": f.get("license_name", spdx),
            "redistributable": True,
            "text_file": os.path.basename(f["license_file"]),
        },
        "source": {"kind": "pack", "entry_path": rel_model},
        "hardware": {
            "min_ram_bytes": int(f.get("min_ram", 536870912)),
            "recommended_ram_bytes": int(f.get("recommended_ram", 2147483648)),
            "notes": f.get("notes", ""),
        },
    })

# Deduplicate licence entries that several models share.
unique = {}
for entry in files:
    unique[entry["path"]] = entry
files = sorted(unique.values(), key=lambda entry: entry["path"])

manifest = {
    "schema_version": 1,
    "pack_version": pack_version,
    "models": models,
    "files": files,
}
with open(os.path.join(out, "pack.json"), "w", encoding="utf-8") as handle:
    json.dump(manifest, handle, indent=2, sort_keys=True)
    handle.write("\n")

# Mirror `PackManifest::artifact_digest` exactly.
h = hashlib.sha256()
h.update(pack_version.encode())
h.update(b"\x00")
for line in sorted(f"{e['path']} {e['size_bytes']} {e['sha256']}" for e in files):
    h.update(line.encode())
    h.update(b"\n")
artifact_digest = h.hexdigest()

evidence = {
    "pack_version": pack_version,
    "artifact_digest_sha256": artifact_digest,
    "models": [
        {"id": m["id"], "sha256": m["sha256"], "license": m["license"]["spdx_id"]}
        for m in models
    ],
}
with open(os.path.join(out, "evidence.json"), "w", encoding="utf-8") as handle:
    json.dump(evidence, handle, indent=2, sort_keys=True)
    handle.write("\n")

print(json.dumps(evidence, indent=2, sort_keys=True))
PY
