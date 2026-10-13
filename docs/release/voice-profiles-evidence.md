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
environment — a second machine, for instance — says so plainly rather than
being quietly satisfied by something else.

Legal review is explicitly **not** a release gate for this project. Plaudon is
open-source software distributed under the MIT licence, "AS IS" and without
warranty of any kind, including any warranty of legal compliance; the owner has
accepted the remaining legal uncertainty around biometric and recording-consent
law, and users run the software at their own risk. The engineering safeguards
stay exactly as specified — the voice-profile feature is off by default, data
stays local, and the consent gates and retention policy in
[`docs/public/privacy-disclosure.md`](../public/privacy-disclosure.md) and
README's "No warranty / use at your own risk" section remain mandatory.

Owners are roles, not people: this repository has one maintainer today
(`spdrman`), and naming the role is what makes it transferable.

## Artifacts that exist

| Item | Artifact | Status | Owner |
|---|---|---|---|
| EER/DET and derived operating point for WeSpeaker ResNet34-LM on licence-clean audio, macOS arm64 | [`docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json`](../benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json) | **done** (2026-10-12, Apple M5, macOS 27.0.1) | maintainer |
| Fixture provenance for that run: reader assignment, per-file digests, condition recipes, the MUSAN files and SNR behind the noise condition | [`docs/benchmarks/speaker-match-wespeaker-fixtures.json`](../benchmarks/speaker-match-wespeaker-fixtures.json) | **done** | maintainer |
| MUSAN noise condition: `noise_musan` built from the corpus at 10 dB SNR, with its own DET curve and EER (1.52%) in the measured artifact | [`docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json`](../benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json) | **done** (#284) | maintainer |
| Harness self-test on scripted vectors (no model, hermetic CI tier) | [`docs/benchmarks/speaker-match-scripted-synthetic-eer.json`](../benchmarks/speaker-match-scripted-synthetic-eer.json) | **done** | maintainer |
| Runtime footprint on macOS arm64: RTF, peak RSS, startup, with the exact commands and machine facts | [`docs/hardware/macos-apple-silicon.md`](../hardware/macos-apple-silicon.md) §"Speaker embedding (EPIC P)" | **done** | maintainer |
| Model licence review, checksum table and attribution | [`docs/licensing/third-party-register.md`](../licensing/third-party-register.md), `NOTICE` | **done** | maintainer |
| **Out-of-process inference (ADR-0015, ADR-0081 §3)** | `crates/ai-speaker/src/sandboxed.rs` and the `plaudon-speaker-helper` binary; acceptance test `crates/ai-speaker/tests/sandboxed_inference.rs::an_embedding_is_produced_inside_the_sandboxed_helper` | **done** (2026-10-12, Apple M5, macOS 27.0.1): the helper ran with `filesystem, network, execution and all resource caps applied`, reported 80 ms start-up and a 76.8 ms round trip for a 3 s window, and returned a vector identical to the in-process one to within 1e-6. Windows x64 is covered by the row below. | maintainer |
| **Hermetic, pinned inference runtime** | `scripts/fetch-onnxruntime.sh`, `crates/ai-speaker/src/runtime.rs`, the inference-runtime checksum table in [`docs/licensing/third-party-register.md`](../licensing/third-party-register.md) | **done** for macOS arm64 (2026-10-12): `ort`'s `download-binaries` is off, `cargo build --offline -p plaudon-ai-speaker --features onnx` links the vendored, digest-verified ONNX Runtime 1.31.0, and `ureq`, `ureq-proto`, `socks`, `lzma-rust2` and `hmac-sha256` are gone from `Cargo.lock`. The Windows archive is vendored and digest-verified but has never been linked — see the Windows row below. | release engineering |
| DER of a real diarization provider on overlap-bearing audio (ADR-0400 condition 4), macOS arm64 | [`docs/benchmarks/diarization-onnx-pyannote-wespeaker-der.json`](../benchmarks/diarization-onnx-pyannote-wespeaker-der.json), [`docs/benchmarks/diarization-eval.md`](../benchmarks/diarization-eval.md) | **done** (2026-10-13, Apple M5, macOS 27.0.1; pooled DER 32.65% over 12 runs, 1 046.4 s of reference speech, 110.4 s of it overlapping). The provider stays out of `VALIDATED_PROVIDERS`: ADR-0400 conditions 1, 2, 3 and 5 are open, inference still runs in-process, and the artifact's `release_evidence.qualifies` is `false`. | maintainer |

## Items this environment cannot produce

| Item | Why it is missing | Exact next step | Status | Owner |
|---|---|---|---|---|
| **Windows x64 runtime and accuracy figures** (RTF, peak RSS, startup, EER) | No native Windows x86-64 machine exists in this project's tooling; `docs/PRECOMMIT.md` §3 already records the four CI jobs that need one. A figure from another host is not evidence for this one (§18.3A). | On a Windows 11 x64 machine with the pinned toolchain: `pwsh scripts/fetch-onnxruntime.ps1` and `$env:ORT_LIB_LOCATION` (the archive is vendored and digest-verified, but **this project has never linked it**: `crates/ai-speaker/build.rs` adds the DirectML/Direct3D link libraries `ort-sys` emits only on its own download path, and that branch has never been executed on Windows — if the link fails, that file is the first place to look); `scripts/fetch-speaker-models.sh`; `cargo build --offline --release -p plaudon-ai-speaker --features onnx`; `plaudon-speaker-embed build-fixtures` against a local LibriSpeech `test-clean`; `plaudon-speaker-embed embed --stats`; then `plaudon-eval speaker-match --platform windows-x64 --host-cpu <cpu> --peak-rss-bytes <n> --startup-ms <n> --real-time-factor <f>`, committing `docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.windows-x64.json`. Measure peak RSS with `Measure-Command` plus `Get-Process -Id $PID \| Select-Object PeakWorkingSet64`, not with a Unix tool's output pasted in. **Also verify the AVX2 assumption**: `ort`'s prebuilt x86-64 binaries require AVX2 and warn at startup when it is absent, so a CPU without it must be either a documented unsupported configuration or a reason to build ONNX Runtime without that requirement. | **blocked: needs hardware** | release engineering |
| **Out-of-process inference on Windows x64** | The helper, the protocol and the acceptance test are platform-independent, but the isolation they get is not: `sandbox::windows` applies a job object and **no** filesystem or network restriction in V1 (`NOTE_WINDOWS_NO_FILESYSTEM_RESTRICTION`), and nobody has run the helper there. | On the same Windows machine as the row above: `cargo test --offline -p plaudon-ai-speaker --features onnx --test sandboxed_inference -- --nocapture`, and record what `helper isolation:` prints. A pass with a reduced-isolation report is the expected outcome and must be published as such, not as "sandboxed". | **blocked: needs hardware** | maintainer |
| **Offline build proven on Windows x64** | The build is hermetic by construction on every platform — `download-binaries` is not enabled anywhere — but "it links" is a claim about a linker nobody has run here. The Windows archive's digest is verified; its 359 948 106-byte `onnxruntime.lib` has never been given to `link.exe`. | `pwsh scripts/fetch-onnxruntime.ps1`, then `cargo build --offline -p plaudon-ai-speaker --features onnx`. If the link reports unresolved DirectML or Direct3D symbols, fix the library list in `crates/ai-speaker/build.rs` and record the correction in `crates/ai-speaker/src/runtime.rs`, whose `ArchiveEvidence::FetchedAndListed` for this target becomes `Linked` only when this step passes. | **blocked: needs hardware** | release engineering |
| **Runtime figures for the shipped configuration** (RTF, peak RSS, startup, measured through the sandboxed helper) | The committed macOS figures were taken in the application process under ONNX Runtime's own provider selection — CoreML on this machine. Since #283 inference runs in a sandboxed helper pinned to the CPU provider, so those figures describe an engine the product no longer uses. The *accuracy* figures are unaffected: the two providers' vectors agree to a cosine similarity of 0.999974 ([`docs/hardware/macos-apple-silicon.md`](../hardware/macos-apple-silicon.md) §"What #283 changed about these numbers"). | Extend `plaudon-speaker-embed embed --stats` to run through `plaudon_ai_speaker::sandboxed::SandboxedModel` rather than opening the model in process, re-fetch LibriSpeech `test-clean` and MUSAN, rebuild the fixtures and re-run the 372-window harness; commit the new `runtime` block. A microbenchmark on one synthetic window (80 ms start-up, 76.8 ms per 3 s window) is recorded in the hardware document and is **not** that run. | **not started** | maintainer |

## What the measured artifact does and does not say

The macOS arm64 run is a real measurement of a real checkpoint on real speech,
and `release_evidence.qualifies` in that document is `true`. That field means
"this document is usable as evidence for the platform named in its `runtime`
block" — it does not mean the ADR-0081 §4 gate is passed, because the gate is a
two-platform gate and the second platform is the first row of the table above.
The artifact says so itself in its standing caveats.
