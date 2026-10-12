# ADR-0400: Diarization runtime and licensing

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §8.7, §3.1, §17.4

## Context

Spec §8.7 requires speaker diarization be treated as independent from transcription, with speaker turns reconciled against transcript time ranges. It mandates that we expose the option only for validated providers and show possible added cost, and requires a feasibility/licensing spike before adding a Python runtime, Hugging Face gated-model tokens, or a second-cloud expense. Spec §3.1 marks advanced diarization as strictly conditional on measured runtime capability. Issue M1.5 records this as the required spike outcome.

## Decision

We ship the versioned diarization plugin contract (`DiarizationProvider`, `DiarizationCapabilities`, `SpeakerTurn`, validated-provider allowlist, turn→segment reconciliation, DER measurement harness) in this EPIC with only the deterministic fake provider in the `VALIDATED_PROVIDERS` allowlist. We do NOT add a Python runtime, any gated-model token, or real provider in this build.

A future provider may be added to the allowlist when ALL of these conditions are met:

1. **Runtime footprint measured.** Peak RSS, startup time, platform coverage (macOS arm64 / Windows x64) recorded on real hardware and within acceptable limits.
2. **Licence review completed.** Code licence, model licence, redistribution permissions, and user disclosure obligations documented. No bundled model that we cannot redistribute unless an explicit user download flow exists.
3. **Gated access resolved.** Either no gated token required, OR an explicit user-supplied token flow that never stores tokens in the application, with disclosure that obtaining the token requires a Hugging Face account and acceptance of model-specific terms.
4. **DER measurement recorded.** The provider passes the DER harness on labelled fixtures including overlapping speech, with sample sizes and hardware noted, and the measurement artifact committed under `docs/benchmarks/diarization-<provider>-der.json`.
5. **Platform coverage verified.** The provider works on every supported platform or is explicitly labelled as unavailable on unsupported platforms.

## Feasibility findings

### pyannote.audio

- **Runtime:** Python 3.8+, PyTorch, torchaudio, pyannote.audio package. Adds a Python interpreter requirement to a Rust application.
- **Models:** Segmentation and diarization models hosted on Hugging Face, gated behind user account + accepted conditions + access token. Token must be supplied by the user; we cannot bundle it.
- **Code licence:** MIT (pyannote.audio repository).
- **Model licence:** Gated on Hugging Face; terms vary by model. User must accept terms to download.
- **Redistribution:** Code may be redistributed; models may not be bundled without user obtaining their own access.
- **Platform support:** Cross-platform where Python/PyTorch work; Metal/MPS on macOS, CUDA/ROCm on supported GPUs.
- **Disclosure required:** User must be told they need a Hugging Face account, model-specific acceptance, and an access token; that the token grants download rights only and does not authenticate against our application; that offline use requires pre-downloaded weights.
- **DER measurement:** NONE TAKEN in this build.

### sherpa-onnx / ONNX Runtime-based offline speaker segmentation

- **Runtime:** ONNX Runtime (C++), no Python. Can be linked directly into Rust via `ort` or similar bindings.
- **Models:** WeSpeaker, 3D-Speaker or similar speaker-embedding models; availability and licensing vary by model source.
- **Code licence:** Apache 2.0 (sherpa-onnx), MIT (ONNX Runtime).
- **Model licence:** Varies by model; must be checked per source.
- **Redistribution:** Code redistributable; models must be evaluated individually.
- **Platform support:** Cross-platform; CPU/CoreML/CUDA/DirectML backends.
- **Disclosure required:** Model source, licence terms, whether weights are bundled or downloaded, and that the feature is local-only with no cloud transmission.
- **DER measurement:** NONE TAKEN in this build.

### Cloud STT models with native speaker labels

- **Runtime:** No additional local runtime; uses existing OpenRouter HTTP client.
- **Licensing:** OpenRouter service terms; downstream provider terms.
- **Cost:** Added per-audio cost; must be disclosed before dispatch.
- **Platform support:** All platforms with network access.
- **Privacy:** Cloud egress; requires consent, refused in Local-only Mode.
- **Disclosure required:** That audio is sent to the cloud provider, that speaker identification is inferred by the model (not legally verified identity), cost estimate, data retention policy for the specific endpoint/model/provider.
- **DER measurement:** NONE TAKEN in this build; must be measured per model before offering.

## Rejected alternatives

- **Bundling pyannote with a hidden token.** We do not have a licence to redistribute gated models, and spec §16.4 prohibits universal tokens.
- **Downloading a Python runtime at install time.** Spec §12 prohibits silent multi-gigabyte downloads; a Python install is a system-level decision, not an application feature.
- **Offering diarization without measurement.** Spec §3.1 requires measured runtime capability; shipping without DER measurement violates the gate.
- **Allowing any provider via a configuration string.** The allowlist exists to enforce the gate; bypassing it would defeat the entire contract.

## Consequences

- The diarization contract ships now; the reconciliation layer and DER harness are testable with the fake provider.
- Real diarization remains unavailable until a provider passes all five conditions.
- NO real-runtime DER measurement has been taken for this build; the harness is validated only against synthetic deterministic fixtures.
- A future EPIC that adds pyannote must include the user token flow, the Hugging Face disclosure, Python runtime detection/setup instructions, and measured DER on labelled audio including overlapping speech.
