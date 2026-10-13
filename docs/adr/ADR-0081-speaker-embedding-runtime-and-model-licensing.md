# ADR-0081: Speaker embedding runtime and model licensing

- **Status:** Accepted (2026-10-12; §3 amended the same day on measured evidence, below)
- **Date:** 2026-10-12
- **Spec:** VOICE_PROFILES_SPEC §5 (VP-040–045), §14; base spec §8.7, §12, §3.1; extends ADR-0400

## Context

ADR-0400 shipped the diarization contract with only the deterministic fake provider and set five
conditions for any real provider: measured runtime footprint, completed licence review, resolved
gated access, recorded DER, and platform coverage. Voice profiles need two local model artifacts —
a segmentation model and a speaker-embedding model — plus an inference runtime, on macOS Apple
Silicon and Windows x64, CPU-only, with no Python. The 2026-10-12 research pass
(sources in [docs/specs/VOICE_PROFILES_SPEC.md](../specs/VOICE_PROFILES_SPEC.md) §5/§22)
established: pyannote `segmentation-3.0` weights are MIT (© 2022 CNRS) and are redistributed
un-gated with the licence text by sherpa-onnx's GitHub releases; WeSpeaker publishes official ONNX
embedding models whose weights policy is CC-BY-4.0 (VoxCeleb-trained); pyannote 3.1 publishes a
calibrated clustering threshold for exactly the WeSpeaker ResNet34-LM checkpoint; SpeechBrain's
cleaner-labelled ECAPA has no official ONNX export; NVIDIA TitaNet trains on non-redistributable
LDC corpora; Rev's Reverb diarization models are non-commercial; and no maintained pure-Rust
speaker-embedding implementation exists.

## Decision

1. **Artifacts:** bundle-tier models are `pyannote/segmentation-3.0` ONNX (MIT, ≈6 MB) and
   `WeSpeaker ResNet34-LM` ONNX (CC-BY-4.0, ≈26.5 MB, 256-dim), acquired by exact filename and
   sha256 (never by globbing a release tag — the same tags carry non-commercial Rev models), and
   installed through the existing offline model-pack flow with displayed licence and verified
   checksum, like whisper weights. Attribution rows for CNRS, WeSpeaker and VoxCeleb land in
   `NOTICE` and `docs/licensing/third-party-register.md` (including the Model-weights checksum
   table).
2. **Weights-licence posture:** where upstreams disagree about whether VoxCeleb-trained weights
   inherit CC-BY-4.0 (WeSpeaker says yes; Alibaba labels equivalent models Apache-2.0), we comply
   with the stricter claim (CC-BY-4.0 attribution). Models trained on LDC corpora (TitaNet) are
   excluded from bundling regardless of NVIDIA's CC-BY-4.0 label, because the two licence theories
   give opposite answers there and we do not need the model.
3. **Runtime:** `ort` (MIT OR Apache-2.0) + `kaldi-native-fbank` (MIT), with our own windowing,
   AS-Norm and matching (which profile identity needs anyway). Fallback if budgets are missed:
   the first-party `sherpa-onnx` crate's pipeline (Apache-2.0) for the diarization stages.
   Inference over untrusted audio runs out of process per ADR-0015 via
   `plaudon_security::sandbox`.
4. **The ADR-0400 gate is extended, not bypassed:** before the real pipeline enters
   `VALIDATED_PROVIDERS`, the EPIC must record on reference hardware (both platforms): peak RSS,
   startup, RTF; DER on labelled fixtures including overlap
   (`docs/benchmarks/diarization-<provider>-der.json`); and EER/DET for matching
   (`docs/benchmarks/speaker-match-<model>-eer.json`). No published Windows x64 CPU figures exist
   for any candidate; measuring them is a release gate, and single-machine research measurements
   are not release evidence.

   *Implementation (2026-10-12):* the artifact's shape is
   `schemas/speaker-match-report.v1.schema.json` and it is produced by
   `plaudon-eval speaker-match` (`crates/eval/src/speaker/`, documented in
   [docs/benchmarks/speaker-match-eval.md](../benchmarks/speaker-match-eval.md)). The gate's
   conditions are *computed* into every artifact as `release_evidence.blockers`, so a document
   that is not evidence cannot be mistaken for one. No checkpoint has been measured yet: the
   committed `speaker-match-scripted-synthetic-eer.json` runs on scripted embeddings, lists both
   the missing model measurement and the missing runtime figures as blockers, and its derived
   thresholds are explicitly not shippable defaults.

## Rejected alternatives

- **SpeechBrain ECAPA-TDNN (cleanest licence label).** Rejected for V1: no official ONNX export
  (open upstream issue); self-exporting makes us the party asserting numerical fidelity of a
  biometric model, a validation burden with no accuracy payoff over WeSpeaker (0.80% vs 0.797%
  VoxCeleb1-O EER as shipped).
- **NVIDIA TitaNet-L (best headline EER among permissively-labelled models).** Rejected:
  Fisher/Switchboard/NIST-SRE training corpora are per-seat LDC licences; under the
  dataset-taint theory the weights are not redistributable at all, and 101 MB includes a dead
  classifier head.
- **speakrs / pyannote-rs (pure-Rust pipelines).** Rejected for now: speakrs publishes strong
  CoreML numbers but no CPU-mode DER/RTF (the Windows case) and its model mirror carries no
  licence tag with compliance disclaimed onto the integrator; pyannote-rs has third-party-measured
  DER of 80% and emits nothing on most test files. Re-evaluate speakrs when CPU figures and a
  tagged model provenance exist.
- **Gated pyannote `community-1` pipeline as the default.** Rejected for bundling: Hugging Face
  gate (account, token, terms) violates the no-token rule for a default path (ADR-0400 condition 3,
  §16.4); permitted later as an explicit user-download accuracy tier with the user's own token,
  never stored.
- **Whisper-encoder embedding reuse (no new model).** Rejected: not speaker-discriminative without
  training a new head; published attempts reach worse EER than a 26 MB purpose-built model at far
  higher compute.

## Consequences

- Two new model artifacts and two new native-inference dependencies enter the supply chain:
  `deny.toml`, the third-party register, SBOM and licence-audit CI all gain entries; an
  unreviewed licence fails the build, as today.
- The product inherits CC-BY-4.0 attribution obligations (NOTICE text) — accepted, trivial.
- Accuracy claims are bounded by honest protocol reporting: VoxCeleb headline EERs must never be
  quoted as meeting-audio accuracy; the committed harness artifacts are the only shippable claims.
- If the gate is not passed on reference hardware, the feature does not ship; the allowlist keeps
  only the fake provider and the UI never offers enrolment. That outcome is acceptable and
  planned for (VP-042).

## Amendment, 2026-10-12 (P2.5, #248): the runtime decision, measured

Decision §3 named `ort` + `kaldi-native-fbank`, with the first-party `sherpa-onnx` crate as the
fallback. Building both on the reference Apple Silicon machine changed half of that, and the
numbers are the reason rather than the illustration (full record:
[docs/hardware/macos-apple-silicon.md](../hardware/macos-apple-silicon.md) §"Speaker embedding on
this platform"):

- **`ort` 2.0.0-rc.14 is the runtime.** Clean release build in **15 s** at `CARGO_BUILD_JOBS=2`,
  15 crates in a normal build, ONNX Runtime linked **statically** into one binary — nothing extra
  to install, sign or notarize. Two costs are accepted explicitly: the 2.0 line is still a release
  candidate, so the dependency is pinned with `=`; and `download-binaries` fetches the prebuilt
  runtime over TLS during the build, which a release build must replace with `ORT_LIB_LOCATION`
  pointing at a vendored, checksummed artifact. That replacement is a release-evidence item, not a
  nice-to-have: a build that reaches a CDN is not a build we can reproduce offline.
- **`sherpa-rs` 0.6.8 stays the fallback, and is more expensive than assumed.** 80 s clean build,
  a bindgen/libclang and CMake requirement on every build host, and **three dylibs** (26 MB) to
  redistribute, with ONNX Runtime pinned at 1.17.1. It remains the answer if `ort` cannot be made
  to work on Windows x64 — which is not yet measured.
- **`kaldi-native-fbank` is dropped; the feature front end is first-party.** The only maintained
  Rust binding (`knf-rs` → `knf-rs-sys`) vendors the C++ sources and builds them with CMake *and*
  bindgen, so a 300-line DSP dependency would impose a C++ toolchain on every build host and put a
  second native parser of untrusted audio in the process (ADR-0015). `crates/ai-speaker/src/fbank.rs`
  computes the same features in dependency-free Rust and was cross-checked against
  `kaldi-native-fbank` on a 3-second broadband signal with the same window: identical frame and bin
  counts, **max absolute difference 5.6 × 10⁻⁴, RMS 1.2 × 10⁻⁵**. This is the `opus-pure` decision
  again (O1.6), for the same reasons.

The §4 gate is **not** passed, and the feature flag therefore stays off. What now exists:
`docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json` — WeSpeaker ResNet34-LM over
LibriSpeech `test-clean`-derived fixtures on macOS arm64, in five conditions including a MUSAN
noise mix at 10 dB SNR (#284), pooled **EER 0.67%** (300 target / 3 300 non-target trials),
startup **103 ms**, RTF **0.0143**, peak RSS **87 MiB**. What does not:
Windows x64 figures of any kind, a real diarization provider's DER, and out-of-process inference
on Windows. Each is an item with an owner and a next step in
[docs/release/voice-profiles-evidence.md](../release/voice-profiles-evidence.md). Legal review is
not among them: the software ships under the MIT licence, "AS IS", with no warranty of legal
compliance, and is used at the user's own risk.
