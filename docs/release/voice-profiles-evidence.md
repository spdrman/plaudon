# Voice profiles: release-evidence register (EPIC P, ADR-0081 §4)

Spec §24 makes a claimed configuration without its evidence a release failure,
and ADR-0081 §4 adds five conditions before the real speaker-embedding pipeline
may enter `VALIDATED_PROVIDERS` at all. This file is the register of those
items: what exists, what does not, who owns the gap and what the next step is.
`scripts/collect-evidence.sh` copies it into the bundle's `risk-exceptions`
category, and `docs/benchmarks/` goes into `benchmarks` wholesale, so the
bundle carries both the artifacts and the list of what is still missing.

**Nothing in this file is a plan to ship.** The feature flag stays off until
every row below is `done`. A row that cannot be done in a development
environment — a second machine, a lawyer — says so plainly rather than being
quietly satisfied by something else.

Owners are roles, not people: this repository has one maintainer today
(`spdrman`), and naming the role is what makes it transferable.

## Artifacts that exist

| Item | Artifact | Status | Owner |
|---|---|---|---|
| EER/DET and derived operating point for WeSpeaker ResNet34-LM on licence-clean audio, macOS arm64 | [`docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json`](../benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json) | **done** (2026-10-12, Apple M5, macOS 27.0.1) | maintainer |
| Fixture provenance for that run: reader assignment, per-file digests, condition recipes | [`docs/benchmarks/speaker-match-wespeaker-fixtures.json`](../benchmarks/speaker-match-wespeaker-fixtures.json) | **done** | maintainer |
| Harness self-test on scripted vectors (no model, hermetic CI tier) | [`docs/benchmarks/speaker-match-scripted-synthetic-eer.json`](../benchmarks/speaker-match-scripted-synthetic-eer.json) | **done** | maintainer |
| Runtime footprint on macOS arm64: RTF, peak RSS, startup, with the exact commands and machine facts | [`docs/hardware/macos-apple-silicon.md`](../hardware/macos-apple-silicon.md) §"Speaker embedding (EPIC P)" | **done** | maintainer |
| Model licence review, checksum table and attribution | [`docs/licensing/third-party-register.md`](../licensing/third-party-register.md), `NOTICE` | **done** | maintainer |

## Items this environment cannot produce

| Item | Why it is missing | Exact next step | Status | Owner |
|---|---|---|---|---|
| **Windows x64 runtime and accuracy figures** (RTF, peak RSS, startup, EER) | No native Windows x86-64 machine exists in this project's tooling; `docs/PRECOMMIT.md` §3 already records the four CI jobs that need one. A figure from another host is not evidence for this one (§18.3A). | On a Windows 11 x64 machine with the pinned toolchain: `scripts/fetch-speaker-models.sh`; `cargo build --release -p plaudon-ai-speaker --features onnx --bin plaudon-speaker-embed`; `plaudon-speaker-embed build-fixtures` against a local LibriSpeech `test-clean`; `plaudon-speaker-embed embed --stats`; then `plaudon-eval speaker-match --platform windows-x64 --host-cpu <cpu> --peak-rss-bytes <n> --startup-ms <n> --real-time-factor <f>`, committing `docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.windows-x64.json`. Measure peak RSS with `Measure-Command` plus `Get-Process -Id $PID \| Select-Object PeakWorkingSet64`, not with a Unix tool's output pasted in. **Also verify the AVX2 assumption**: `ort`'s prebuilt x86-64 binaries require AVX2 and warn at startup when it is absent, so a CPU without it must be either a documented unsupported configuration or a reason to build ONNX Runtime without that requirement. | **blocked: needs hardware** | release engineering |
| **Privacy-counsel review of VOICE_PROFILES_SPEC §12** (BIPA §15(a)-(b) written policy and retention schedule, Illinois/Texas/Washington consent mechanics, the §20(1) CONTESTED items) | Legal review is not an engineering task and must not be simulated. The spec itself flags these as counsel questions (§12.1, §20(1), VP-131). | Send §12, §20(1) and [`docs/public/privacy-disclosure.md`](../public/privacy-disclosure.md) §"Voice profiles" to privacy counsel; ask specifically about (a) whether the in-product retention text satisfies BIPA §15(a) as a *written policy* when the controller is the user rather than us, (b) the transfer language for TX CUBI and WA My Health My Data, (c) whether "suggestion-only, never auto-assigned" changes the analysis. File the signed opinion as `ci-artifacts/evidence/risk-exceptions/privacy-counsel-review.pdf` and flip this row. | **blocked: needs external review** | product owner |
| **DER of a real diarization provider** | The committed DER scores a scripted hypothesis against exact synthetic ground truth: it measures `crates/ai-diarization::der`, not a provider. ADR-0400 condition 4 wants a provider's DER on labelled audio including overlap, and no provider exists yet — the segmentation checkpoint is licensed and pinned here, but nothing decodes its powerset output into turns. | Implement the segmentation pipeline (sliding-window inference, powerset decode, embedding-based clustering) behind the same off-by-default feature, then run the DER harness on the overlap-bearing synthetic session and commit `docs/benchmarks/diarization-<provider>-der.json`. | **not started** | maintainer |
| **Out-of-process inference (ADR-0015)** | `OnnxSpeakerEmbedder` runs ONNX Runtime in the application process today. ADR-0081 §3 requires inference over untrusted audio to run under `plaudon_security::sandbox`. | Move `SpeakerEmbeddingModel` behind `SandboxedHelper` with the window as the request and the vector as the response; keep the trait unchanged so nothing above it moves. | **not started** | maintainer |
| **Hermetic, pinned inference runtime** | `ort`'s `download-binaries` fetches a prebuilt ONNX Runtime over TLS during the build. A release build must not depend on a third-party CDN at build time. | Vendor the ONNX Runtime archive for both targets, record its digest in the register, and build with `ORT_LIB_LOCATION`; verify `cargo build --offline` then succeeds with the feature on. | **not started** | release engineering |
| **MUSAN noise condition** | The measured run omits `noise_musan`: MUSAN is an 11 GB download that was not taken, and an approximation labelled as MUSAN would be a false provenance claim. | Fetch MUSAN (openslr.org/17), extend `plaudon-speaker-embed build-fixtures` with the mixing step at a stated SNR, and re-run the harness with the condition in the plan. | **not started** | maintainer |

## What the measured artifact does and does not say

The macOS arm64 run is a real measurement of a real checkpoint on real speech,
and `release_evidence.qualifies` in that document is `true`. That field means
"this document is usable as evidence for the platform named in its `runtime`
block" — it does not mean the ADR-0081 §4 gate is passed, because the gate is a
two-platform gate and the second platform is the first row of the table above.
The artifact says so itself in its standing caveats.
