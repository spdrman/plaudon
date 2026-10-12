#!/usr/bin/env bash
# Software bill of materials for the release evidence bundle (spec 18.8, 24).
#
# The SBOM is produced from Cargo.lock through `cargo metadata`, so it
# describes the dependency graph that was actually compiled rather than a
# hand-maintained list. No network access beyond the crates.io index Cargo
# already needs.
#
#   scripts/sbom.sh [--out <path>]
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="ci-artifacts/sbom/sbom.json"
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

mkdir -p "$(dirname "$OUT")"

COMMIT="$(git rev-parse HEAD)"
GENERATED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
  # Reproducible runs pin the timestamp (spec 18.8 reproducible build metadata).
  GENERATED_AT="$(date -u -r "${SOURCE_DATE_EPOCH}" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null \
    || date -u -d "@${SOURCE_DATE_EPOCH}" +%Y-%m-%dT%H:%M:%SZ)"
fi

# CycloneDX-shaped document with the fields we can state truthfully: name,
# version, licence as declared by the crate, and the source registry.
cargo metadata --format-version 1 --all-features --locked \
  | COMMIT="$COMMIT" GENERATED_AT="$GENERATED_AT" node -e '
      let raw = "";
      process.stdin.on("data", (chunk) => { raw += chunk; });
      process.stdin.on("end", () => {
        const metadata = JSON.parse(raw);
        const components = metadata.packages
          .map((pkg) => ({
            type: "library",
            name: pkg.name,
            version: pkg.version,
            licenses: pkg.license ? [{ license: { id: pkg.license } }] : [],
            purl: `pkg:cargo/${pkg.name}@${pkg.version}`,
            source: pkg.source || "workspace",
          }))
          .sort((a, b) => (a.purl < b.purl ? -1 : a.purl > b.purl ? 1 : 0));
        const document = {
          bomFormat: "CycloneDX",
          specVersion: "1.5",
          version: 1,
          metadata: {
            timestamp: process.env.GENERATED_AT,
            component: {
              type: "application",
              name: "plaudon",
              version: metadata.workspace_members.length ? undefined : undefined,
            },
            properties: [{ name: "commit", value: process.env.COMMIT }],
          },
          components,
        };
        process.stdout.write(JSON.stringify(document, null, 2) + "\n");
      });
    ' > "$OUT"

echo "sbom written to $OUT ($(grep -c '"purl"' "$OUT") components)"
