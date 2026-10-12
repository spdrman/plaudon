#!/usr/bin/env bash
# Measure full-library encryption overhead for spec §17.5 evaluation
# (issue M2.5).
#
# Measures read/write throughput against a reference library at the §17.5
# scale (10,000 recordings / 100,000+ segments) for the plaintext baseline,
# and documents the comparison rows for candidate approaches.
#
# Writes a reproducible Markdown report to docs/benchmarks/encryption-overhead.md
# including hardware, OS build, toolchain and commit.
#
# NEVER fabricates measurements: candidate rows not actually measured are
# recorded as "not measured" rather than invented numbers.
#
# Usage:
#   scripts/measure-encryption-overhead.sh
#   scripts/measure-encryption-overhead.sh --help
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ $# -gt 0 ]] && [[ "$1" == "--help" ]]; then
  echo "Usage: $0"
  echo ""
  echo "Measure encryption overhead for full-library operations."
  echo "Writes report to docs/benchmarks/encryption-overhead.md"
  exit 0
fi

REPORT_DIR="docs/benchmarks"
REPORT="$REPORT_DIR/encryption-overhead.md"

mkdir -p "$REPORT_DIR"

# Gather provenance
HARDWARE="$(uname -m)"
if [[ -f /proc/cpuinfo ]]; then
  CPU_MODEL="$(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2 | xargs)"
elif [[ "$(uname -s)" == "Darwin" ]]; then
  CPU_MODEL="$(sysctl -n machdep.cpu.brand_string)"
else
  CPU_MODEL="unknown"
fi

OS_BUILD="$(uname -sr)"
TOOLCHAIN="$(rustc --version)"
COMMIT="$(git rev-parse HEAD 2>/dev/null || echo "unknown")"
TIMESTAMP="$(date -u +"%Y-%m-%d %H:%M:%S UTC")"

echo "Measuring encryption overhead..." >&2
echo "Hardware: $HARDWARE / $CPU_MODEL" >&2
echo "OS: $OS_BUILD" >&2
echo "Toolchain: $TOOLCHAIN" >&2
echo "Commit: $COMMIT" >&2

# Run the plaintext baseline using the existing scale test
echo "" >&2
echo "Running plaintext baseline (this may take several minutes)..." >&2

BASELINE_OUTPUT="$(mktemp)"
if cargo test -p plaudon-app-core --test search_scale -- --ignored --nocapture 2>&1 | tee "$BASELINE_OUTPUT"; then
  # Extract throughput numbers from the test output if available
  # The test should output something like "inserted N records in X seconds"
  BASELINE_INSERT_TIME="not measured"
  BASELINE_SEARCH_TIME="not measured"
  
  if grep -q "inserted.*in.*seconds" "$BASELINE_OUTPUT"; then
    BASELINE_INSERT_TIME="$(grep "inserted" "$BASELINE_OUTPUT" | head -1)"
  fi
  
  if grep -q "search.*in.*seconds" "$BASELINE_OUTPUT"; then
    BASELINE_SEARCH_TIME="$(grep "search" "$BASELINE_OUTPUT" | head -1)"
  fi
else
  echo "Warning: Baseline benchmark did not complete successfully" >&2
  BASELINE_INSERT_TIME="failed"
  BASELINE_SEARCH_TIME="failed"
fi

rm -f "$BASELINE_OUTPUT"

# Write the report
cat > "$REPORT" <<MARKDOWN
# Encryption Overhead Benchmark

**Generated:** $TIMESTAMP

## Provenance

- **Hardware:** $HARDWARE
- **CPU:** $CPU_MODEL
- **OS:** $OS_BUILD
- **Toolchain:** $TOOLCHAIN
- **Commit:** $COMMIT

## Methodology

This benchmark evaluates the full-library encryption overhead as specified in
§17.5. The reference scale is 10,000 recordings with 100,000+ segments.

### Baseline: Plaintext SQLite

The current plaintext implementation serves as the baseline. All measurements
are relative to this.

**Insert throughput:** $BASELINE_INSERT_TIME

**Search throughput:** $BASELINE_SEARCH_TIME

## Candidate Approaches

### SQLCipher (AES-256-CBC)

**Status:** not measured

SQLCipher provides transparent database encryption via SQLite's VFS layer.
Performance depends on whether hardware AES acceleration is available.

**Expected overhead:** 10-30% (based on published SQLCipher benchmarks)

**Integration complexity:** Medium (requires SQLCipher-enabled SQLite build)

**Key management:** Application-level passphrase or key derivation required

### SQLite SEE (AES-128/256)

**Status:** not measured

SQLite Encryption Extension is a proprietary solution from the SQLite authors.

**Expected overhead:** 5-15% (vendor claims)

**Integration complexity:** High (proprietary license, no pre-built binaries)

**Key management:** Application-level

### Application-Layer Field Encryption

**Status:** not measured

Encrypt sensitive fields (transcript text, speaker names) before storage using
a library like \`ring\` or \`sodiumoxide\`.

**Expected overhead:** Highly variable depending on field selection

**Integration complexity:** High (requires careful schema redesign, key rotation strategy)

**Key management:** Application-level with per-field or per-recording keys

### File-System Level Encryption

**Status:** not measured

Rely on OS-level encryption (FileVault, BitLocker, LUKS) rather than
application-level.

**Expected overhead:** Negligible to application (handled by OS)

**Integration complexity:** None (deployment/documentation only)

**Key management:** OS-level, outside application scope

## Recommendations

Recommendation deferred pending actual measurements of candidate approaches.
The baseline measurement is now available for comparison.

## Notes

- Spec §17.2, §17.6: measurements must not be fabricated. Approaches marked
  "not measured" have not been implemented or benchmarked on this hardware.
- This is a prototype harness. Production measurements will require multiple
  runs and statistical analysis of variance.
MARKDOWN

echo "" >&2
echo "Report written to $REPORT" >&2
