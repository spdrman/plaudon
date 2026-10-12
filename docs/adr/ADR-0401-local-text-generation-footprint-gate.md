# ADR-0401: Local text generation footprint gate

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §3.1, §12, §9.4

## Context

Spec §3.1 marks cross-platform local LLM summarization as P1, strictly conditional: "after footprint and model-license gate". Spec §12 requires measured laptop-CPU, Metal and Windows-GPU behaviour before the feature is user-visible, and prohibits silent multi-gigabyte weight downloads. Issue M2.2 records the gate design.

## Decision

The gate is a **checked artifact** at `docs/benchmarks/local-llm-footprint.json`, not a code comment or environment variable. The feature reports itself unavailable until that artifact exists, parses, covers the host platform, and passes the default thresholds. There is no override: the gate is the thing that ships.

### Artifact schema

Schema version 1, containing:

- `schema_version`: u32 constant 1.
- `model_id`: string identifying the measured model.
- `model_bytes`: u64 size of the model file.
- `measurements`: array of `PlatformMeasurement`:
  - `platform`: string `"macos-arm64"`, `"windows-x86_64"`, etc.
  - `backend`: string `"cpu"`, `"metal"`, `"cuda"`, `"vulkan"`.
  - `hardware`: string describing the test host, for attribution.
  - `os_build`: string OS version the measurement ran on.
  - `peak_rss_bytes`: u64 observed peak resident set size.
  - `tokens_per_second`: f64 observed generation throughput.
  - `verified_context_tokens`: u64 context length the runtime actually accepted.
- `licence`: `LicenceReview` object:
  - `model_id`: string.
  - `licence`: SPDX identifier or licence name.
  - `redistribution_permitted`: bool whether we may bundle the weights.
  - `requires_gated_token`: bool whether obtaining the weights needs an account/token.
  - `reviewed_by`: string name of reviewer.
  - `reviewed_at`: ISO-8601 date.

### Default thresholds

- `max_peak_rss_bytes`: 8 GiB.
- `min_tokens_per_second`: 5.
- `min_context_tokens`: 8192.
- `max_model_bytes`: 12 GiB.

These thresholds are deliberately modest: the gate answers "does this run acceptably on a laptop", not "is this competitive with a cloud model".

### Runtime behaviour

The local text model provider reads the gate artifact at feature availability check time. If the artifact is absent, unparseable, missing the host platform, or any threshold fails, the feature is unavailable. The gate verdict is recorded as:

- `Available` with matching measurement details, or
- `GateRefusal` with reason (artifact absent, parse error, platform missing, threshold failed, checksum mismatch).

Model weights are never bundled in the application and never silently downloaded. Inference runs as a child process spawned with `env_clear()` against a user-installed runtime (llama.cpp, GGML, etc.). No override exists.

### Committed artifact absence

The artifact is ABSENT in this repository, so the feature is disabled. A test (`local_llm_never_passes_gate_in_the_committed_tree`) asserts that the committed tree never ships a passing gate, so accidentally committing a populated artifact is caught in CI.

## Rejected alternatives

- **An environment variable override.** Any such switch would be the thing that ships by accident; the gate must be the only path.
- **Bundling weights in the installer.** Spec §12 prohibits this without explicit user action; a 12 GiB model in the installer violates the no-silent-download rule.
- **Documenting the thresholds in a comment.** The gate is a checked artifact so "does the feature work" is a runtime question, not a developer assertion.
- **Allowing any model via a file path.** The gate ensures only measured, licence-reviewed models are offered; an arbitrary path bypasses the review.

## Consequences

- The local text generation feature is structurally disabled in this build and will remain so until a future EPIC records measurements.
- Measurement cannot be faked: the artifact must cite real hardware, real RSS, real throughput, and a dated licence review.
- The feature is unavailable on a platform unless that platform has a passing measurement in the artifact.
- A test enforces that the committed tree never enables the feature, preventing accidental shipment.
- macOS with Apple Foundation Models available does not need this gate; the Apple-native text path is conditional only on OS runtime capability, not on a bundled LLM.
