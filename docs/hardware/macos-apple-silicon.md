# Hardware test runner: macOS on Apple Silicon

Opt-in real-hardware suite for direct PLAUD device import (spec §18.3A,
§18.6A, §7.9 "Hardware shipping gate", App. B.9). Implemented in
`tests/hardware` (package `plaudon-hardware-tests`). The suite **never runs in
default CI**: every hardware test is `#[ignore]` and also checks an explicit
environment gate.

## What counts as evidence

- A run on this page is evidence for **macOS on Apple Silicon only**. Linux `bluer` peripheral
  bootstrap results are **not accepted as proof of Mac or Windows pairing**
  (§18.3A); a result from another host never promotes a cell for this one.
- A simulator result is never hardware evidence. A record whose `source` is
  `simulator` cannot promote a `compatibility.yaml` cell.
- A cell becomes `supported` only through `EvidenceRecord::promoted`: real
  hardware, every required check `passed`, a verified transfer, a read-only
  opcode trace, an observed non-destructive scan and a network-denied run. A
  missing or failed check keeps the cell at its previous, lower status. Repeat
  the whole setup on a clean machine before changing the register (§7.9).

## Prerequisites

- An Apple Silicon Mac (arm64) on the macOS version named in the cell's
  `os_version_range`, a PLAUD device on the firmware named in `firmware_range`,
  and a Rust toolchain matching `rust-toolchain.toml`.
- **Bluetooth permission.** The first scan triggers the system prompt. The
  prompt only appears for a process whose app bundle declares
  `NSBluetoothAlwaysUsageDescription` in its `Info.plist`; an unbundled
  binary launched from Terminal is attributed to Terminal, so grant Terminal
  (or your IDE) under System Settings → Privacy & Security → Bluetooth when
  running the harness, and run the `bluetooth_permission` check against the
  **packaged app bundle** as well. A denied or missing description must produce
  a clear in-app error, not a hang.
- Close the PLAUD mobile app and turn off phone Bluetooth, except while
  deliberately running `host_ble_exclusivity_with_mobile_app` and
  `listing_delta_before_after_phone_sync`.
- A scratch volume small enough to fill for `full_disk`, and a firewall (for
  example Little Snitch or `pf`) able to deny PLAUD and OpenRouter hosts for
  `network_denied`.
- Sleep/wake needs the machine to be allowed to sleep (`pmset` / Energy
  settings).

## Opting in

The suite is off unless **all** of these are set; only the exact value `1` opts in.

| Variable | Meaning |
| --- | --- |
| `PLAUDON_HARDWARE_TESTS=1` | Explicit opt-in. |
| `PLAUDON_HARDWARE_PROFILE` | Named device profile `<source_type>:<model>:<auth_mode>`, for example `plaud_device_ble:note:legacy_static_token`. It must select a cell of `compatibility.yaml` for this host; an unknown name grants nothing. |
| `PLAUDON_HARDWARE_OBSERVATIONS` | Path of the observations file (below). |
| `PLAUDON_HARDWARE_EVIDENCE_OUT` | Where `emit_evidence_record` writes the evidence JSON. |

```
PLAUDON_HARDWARE_TESTS=1 \
PLAUDON_HARDWARE_PROFILE=plaud_device_ble:note:legacy_static_token \
PLAUDON_HARDWARE_OBSERVATIONS=$HOME/plaud-run/observations.json \
PLAUDON_HARDWARE_EVIDENCE_OUT=$HOME/plaud-run/evidence.json \
  cargo test -p plaudon-hardware-tests --features radio --test hardware -- --ignored --test-threads=1
```

Without the gate, `cargo test -p plaudon-hardware-tests` lists the hardware tests
as `ignored`; with `-- --ignored` each prints `SKIPPED` and nothing is touched.

## Driving the radio (`--features radio`)

Built with `--features radio`, the suite drives the BLE radio itself for the
checks software can determine, using the same `DeviceActor`, `RadioTransport`
and `StagingAcquisition` the application uses. Without the feature the default
build has no Bluetooth stack and only reads your observations file.

| Check | How the run derives it |
| --- | --- |
| `discover_only` | a scan finds candidates while nothing has been connected or written |
| `bluetooth_permission` | `failed` when the OS denies the scan; a pass also needs your attestation for the packaged app bundle |
| `device_name_collision`, `two_devices_in_range` | from the candidates seen; with more than one in range the suite refuses to auto-select and needs `PLAUDON_HARDWARE_PERIPHERAL` |
| `authorization_failure` | a random, well-formed credential is authenticated once and must be refused |
| `encrypted_handshake_detected` | authentication after a good probe answered "encrypted mode required"; the legacy path is not retried |
| `missing_secret` | a vault lookup for an unprovisioned identity fails closed and writes no frame |
| `authorization_success` | the credential already provisioned in the OS vault for this device authenticates |
| `non_destructive_scan_observed` | two back-to-back read-only listings compared by `NonDestructiveAudit` |
| `listing_delta_before_after_phone_sync` | the listing before and after the phone sync you perform (below) |
| `transfer_pull_*`, `transfer_verified`, `imported_bytes_verified`, `opus_decode_measurement` | real pulls streamed to a temporary staging file, re-hashed from disk, bitstream duration measured, Ogg Opus remux attempted |
| `no_device_write_opcodes` | every frame the actor actually wrote |

Notes:

- **The credential must already be in the OS vault** for this device (provision
  it with the application first). The suite only reads it; it never stores,
  prints or records it. Without one, `authorization_success` is `skipped` and
  nothing after it runs.
- **`authorization_failure` sends one random credential to your device.** That
  is the App. B.9 test; if the device counts failed attempts, expect one.
- Pull length buckets are by measured duration: 15 s is 5-60 s, 5 min is
  3-10 min, 30 min is 20-60 min, multi-hour is 2 h or more. A bucket with no
  matching recording is `skipped`. `PLAUDON_HARDWARE_MAX_PULLS` (default 4) and
  `PLAUDON_HARDWARE_MAX_PULL_BYTES` (default 512 MiB) bound the run. Staged
  files live in a temporary directory and are deleted when the run ends.
- `opus_decode_measurement` measures the bitstream; this build bundles no PCM
  decoder and the detail says so.
- **Phone sync hand-off.** Set `PLAUDON_HARDWARE_PHONE_SYNC_FLAG` to a path. After
  the first listings the run prints `ACTION: ...` and waits (default 600 s,
  `PLAUDON_HARDWARE_PHONE_SYNC_WAIT_SECS`) for you to sync the phone and create
  that file, then lists again and records what changed. Without the variable the
  check is `skipped`.
- The radio is driven **once per process** and every hardware test reads the
  same result, so run with `--features radio -- --ignored --test-threads=1`.
- `PLAUDON_HARDWARE_SCAN_SECS` (default 8) sets the scan length.
- Live results win: the observations file cannot supply or override a check
  the run owns. The file still carries what software cannot know: whether the
  packaged app was granted Bluetooth, sleep/wake, disconnects by link loss,
  battery state, full disk, USB, process kill, timestamps against a trusted
  source and the firewall (`network_denied_run`).

## The observations file

The suite does not invent results. You follow the App. B.9 procedure with the
app under test and write down what happened; the suite validates it, derives
what can be derived, and refuses unsafe content.

```json
{
  "observed_firmware": "V0095",
  "emitted_opcodes": ["authenticate", "listDelta", "requestData"],
  "non_destructive_scan_observed": true,
  "network_denied_run": true,
  "transfer": { "device_recording_id": "...", "acquired_mime": "audio/opus",
    "quality_class": "compressed_device_transfer", "received_bytes": 4096,
    "device_declared_byte_length": 4096, "terminal_marker_observed": true,
    "sha256_hex": "<64 hex>", "measured_duration_ms": 15000 },
  "checks": { "discover_only": { "outcome": "passed", "detail": "one device found, nothing connected" } }
}
```

- `observed_firmware` is read from the device, never assumed.
- A check you did not run is simply absent and is recorded `skipped`, which
  blocks promotion. Unknown check ids are rejected so a typo cannot hide a gap.
- `no_device_write_opcodes`, `transfer_verified` and
  `disconnect_during_transfer` are **derived**: from the opcode trace (only the
  read-only allowlist passes; anything else fails closed), from the transfer
  proof (terminal marker, declared length equal to received bytes, SHA-256) and
  from the six `disconnect_at_*` stages. You cannot assert them.
- `imported_bytes_verified` and `network_denied` are downgraded to `failed`
  when no verified transfer or no network-denied run backs them.
- `detail` must contain no credential and no recording content. Anything that
  looks like a secret assignment, a bearer credential, a long opaque token or
  transcript-length text is replaced by a fixed message and the check fails.

## Checks

`bluetooth_permission`, `discover_only`, `authorization_success`,
`authorization_failure`, `device_name_collision`, `missing_secret`,
`encrypted_handshake_detected`, `host_ble_exclusivity_with_mobile_app`,
`two_devices_in_range`, `listing_delta_before_after_phone_sync`,
`transfer_pull_15s`, `transfer_pull_5min`, `transfer_pull_30min`,
`transfer_pull_multi_hour` (where storage permits), `transfer_verified`,
`opus_decode_measurement`, `timestamp_verified_against_trusted_source`,
`disconnect_at_connecting`, `disconnect_at_authenticating`,
`disconnect_at_listing`, `disconnect_at_requesting`,
`disconnect_at_streaming`, `disconnect_at_finalizing`,
`disconnect_during_transfer`, `sleep_wake`, `resume_or_restart`,
`low_battery`, `full_disk`, `imported_bytes_verified`,
`no_device_write_opcodes`, `usb_mount_and_eject`, `network_denied`,
`recovery_after_process_kill`.

The ones needed for promotion are `EvidenceRecord::REQUIRED_CHECKS`; the rest
are archived with the record.

## What the suite does and does not touch

- It reads your observations file and the committed `compatibility.yaml`, and
  writes one evidence JSON at the path you chose.
- Without `--features radio` it opens no radio, USB volume or vault. With it,
  it scans, connects to the one peripheral you selected, and sends only the
  three allowlisted read-only opcodes (authenticate, list, request data). It
  never deletes, clears, controls or writes to the device: the opcode type has
  no write, delete, clear, settings, OTA or sync-acknowledge operation.
- With `--features radio` it reads one device credential from the OS vault to
  authenticate, holds it in memory only for that call, and never stores,
  prints or records it. Device secrets stay in the OS credential vault.

## Privacy

**No secrets and no recording media leave the test machine by default.** The
evidence record holds metadata, a digest and counts only. Nothing is uploaded;
attaching a record to a release is a deliberate, manual act. Review the JSON
before sharing it.

## Evidence output

`PLAUDON_HARDWARE_EVIDENCE_OUT` receives a pretty-printed JSON document that
satisfies `schemas/device-evidence.v1.schema.json`: `test_run_id`,
`recorded_at`, `source`, `source_type`, `vendor`, `device_model`,
`observed_firmware`, `host_os`, `host_os_build`, `transport`, `auth_mode`,
`provisioning_route`, `transfer`, `non_destructive_scan_observed`,
`emitted_opcodes`, `network_denied_run` and `checks` (`id`, `outcome`
`passed|failed|skipped`, `detail`). `test_run_id` and `recorded_at` are what
`compatibility.yaml` cites as `test_run_id` and `last_verified_at`.

## Speaker embedding on this platform (EPIC P, ADR-0081 §4)

Everything above is about the opt-in PLAUD device suite. This section is the
other kind of hardware evidence the same platform owes: the measured runtime
footprint and accuracy of the speaker-embedding checkpoint, which ADR-0081 §4
requires **on both reference platforms** before the voice-profiles feature may
ship. macOS arm64 is measured below. Windows x64 is not measured anywhere and
is a release-evidence item
([docs/release/voice-profiles-evidence.md](../release/voice-profiles-evidence.md)).

### The machine

| Fact | Value |
|---|---|
| CPU | Apple M5, 10 logical cores |
| Memory | 34 359 738 368 bytes (32 GiB) |
| OS | macOS 27.0.1, build 26A434 |
| Target | `aarch64-apple-darwin` |
| Toolchain | `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0 (5f94df478 2026-08-27)` |
| Build | `--release`, `CARGO_BUILD_JOBS=2` (workstation resource policy) |
| Runtime | `ort` 2.0.0-rc.14 linking a prebuilt ONNX Runtime **statically**; 1 intra-op thread, 1 inter-op thread |
| Checkpoint | `wespeaker_en_voxceleb_resnet34_LM.onnx`, sha256 `e9848563…c39012`, 256-dimensional |
| Date | 2026-10-12 |

No accelerator is used: this is CPU-only inference, which is what the Windows
leg will also be.

### The commands, exactly

```sh
export SDKROOT="$(xcrun --show-sdk-path)"
scripts/fetch-onnxruntime.sh   # verifies and extracts the vendored runtime
export ORT_LIB_LOCATION="$PWD/vendor/onnxruntime/aarch64-apple-darwin"
scripts/fetch-speaker-models.sh --dir /tmp/speaker-models

# LibriSpeech test-clean, 346 663 984 bytes,
# sha256 39fde525e59672dc6d1551919b1478f724438a95aa55f874b576be21967e6c23
curl -L -o test-clean.tar.gz https://www.openslr.org/resources/12/test-clean.tar.gz
tar xzf test-clean.tar.gz -C /tmp/p284-corpus

# MUSAN, 11 086 114 085 bytes,
# sha256 86d1061c7e15b5c9e906777685c519701df51bfde3001e1070dcc9ffac955ee1
# (openslr.org and its ELDA mirror serve byte-identical files; only the
# `noise` subset is unpacked, 930 wavs, 686 MiB)
curl -L -o musan.tar.gz https://www.openslr.org/resources/17/musan.tar.gz
tar xzf musan.tar.gz -C /tmp/p284-musan musan/noise

cargo build -j2 --release -p plaudon-ai-speaker --features onnx \
  --bin plaudon-speaker-embed

./target/release/plaudon-speaker-embed build-fixtures \
  --librispeech /tmp/p284-corpus \
  --musan /tmp/p284-musan/musan \
  --plan tests/fixtures/speaker-eval/librispeech-plan.json \
  --out /tmp/p284-fixtures

/usr/bin/time -l ./target/release/plaudon-speaker-embed embed \
  --model /tmp/speaker-models/wespeaker_en_voxceleb_resnet34_LM.onnx \
  --fixtures /tmp/p284-fixtures \
  --plan tests/fixtures/speaker-eval/librispeech-plan.json \
  --out /tmp/p284-run/eval-embeddings.json \
  --stats /tmp/p284-run/runtime-stats.json --threads 1

cargo run -j2 --release -q -p plaudon-eval --bin plaudon-eval -- speaker-match \
  --plan tests/fixtures/speaker-eval/librispeech-plan.json \
  --model-pack /tmp/p284-run \
  --out docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json \
  --commit "$(git rev-parse HEAD)" --measured-on "$(date -u +%FT%TZ)" \
  --platform macos-arm64 --host-cpu "Apple M5" \
  --peak-rss-bytes 91242496 --startup-ms 103 --real-time-factor 0.0143
```

`eval-embeddings.json` is **not** committed and must not be: it is 372 voice
embeddings of real readers (VP-002, VP-112). The committed outputs are the
report and the fixture manifest, neither of which contains a vector.

### Runtime footprint

| Figure | Value | How it was taken |
|---|---|---|
| Startup | **103 ms** | Digest verification of the 26 530 550-byte checkpoint plus ONNX Runtime session creation, timed inside `SpeakerEmbeddingModel::open`, CPU execution provider. |
| Real-time factor, embedding only | **0.0143** | 15.922 s of CPU-bound work for 1 116.0 s of audio (372 windows of 3 s). |
| Real-time factor, decode + embed | **0.0145** | 16.218 s for the same audio, including WAV decode through `plaudon-audio`. |
| Per window | **42.8 ms** mean, **67.5 ms** max | 372 windows, one intra-op thread. |
| Peak RSS | **91 242 496 bytes (87 MiB)** | `/usr/bin/time -l`, whole process, across the full 372-window run. |
| Fixture build | 22.1 s | 372 files: FLAC decode, concatenation, MUSAN mixing, band-pass, impulse-response convolution, Opus round trip. |

A 60-minute recording with 25% speech in 3-second windows is roughly 300
windows: about 13 s of embedding work on this machine, bounded in memory by the
window rather than the recording, because the canonical PCM derivative lives on
disk and windows are read by seeking.

#### The provider this table describes, and what #283 changed

These figures are **in-process inference pinned to the CPU execution
provider**, which is what `SpeakerEmbeddingModel::open` has done since #283:
CoreML's model builder needs a working directory inside the user's temporary
directory and the sandboxed helper's `(deny default)` profile does not grant
one, so the provider choice was made for the helper and applies everywhere.
The earlier 312-window run (#248) predates that and used ONNX Runtime's own
provider selection — CoreML on this machine — which is why its start-up was
481 ms (an Apple model compilation, not a graph load) and its resident set
214 MiB against 87 MiB here.

Measured here on 2026-10-12, release build, same checkpoint, one 3 s window,
best of five:

| Figure | Pre-#283: in process, CoreML | In process, CPU | Through the sandboxed helper |
|---|---|---|---|
| Session start-up | 442 ms cold, 147 ms with a warm model cache | 75–89 ms | 80 ms (reported by the helper) |
| One 3 s window | 13.0–16.3 ms | 48.4–54.8 ms | 76.8 ms (86.2 ms for the first) |
| Real-time factor | 0.0054 | 0.018 | 0.026 |
| Resident set | 92 MB | 92 MB | 92 MB in the helper, which is where the model now lives |

Two consequences, stated rather than averaged away:

* **The provider does not move the accuracy figures.** The two providers'
  vectors for the same input agree to a cosine similarity of **0.999974**
  (largest component difference 1.2e-3), and the #284 re-run confirms it end to
  end: the CPU-provider embeddings give exactly the per-condition EERs the
  CoreML run gave on the same fixtures (pooled 0.67%, clean 0.30%).
* **The table above is still not the shipped configuration's.** The product
  runs the model in `plaudon-speaker-helper`; `plaudon-speaker-embed` opens it
  in process. The honest replacement is a re-run of the 372-window harness
  through the helper; it is listed as an open item in
  [`docs/release/voice-profiles-evidence.md`](../release/voice-profiles-evidence.md).
  The per-window and start-up measurements in this sub-section are
  microbenchmarks on one synthetic window, taken by
  `crates/ai-speaker/tests/sandboxed_inference.rs`, and are not a substitute
  for that run.

### Accuracy on licence-clean audio

From `docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json`: 12 enrolled
speakers, 24 AS-Norm cohort speakers, 300 target and 3 300 non-target trials,
LibriSpeech `test-clean` derived, 3-second utterances, enrolment and trial
material disjoint.

| Condition | EER | Trials (target / non-target) |
|---|---|---|
| Pooled | **0.67%** | 300 / 3 300 |
| Clean | 0.30% | 60 / 660 |
| MUSAN noise at 10 dB SNR | 1.52% | 60 / 660 |
| Telephone band-pass (300–3400 Hz) | 0.45% | 60 / 660 |
| Opus 12 kbit/s round trip | 0.61% | 60 / 660 |
| Room impulse response | 0.61% | 60 / 660 |

Derived operating point: `T_suggest` 5.038 (false accept 1.00%, false reject
0.67%), `T_floor` 1.693 at a 10% false-accept rate.

Three honest limits on these numbers:

1. **LibriSpeech is read speech by native-English readers in quiet.** Meeting
   audio is not. These figures bound what the checkpoint can do on easy
   material; they are not a product accuracy claim, and VP-170/VP-171 forbid
   quoting a pooled figure as an advertised accuracy.
2. **`noise_musan` is one SNR and one noise subset.** The condition mixes a
   single MUSAN `noise` file into each trial utterance at 10 dB; MUSAN's music
   and babble material, lower SNRs and real far-field capture are not measured,
   and the 1.52% figure is the error rate of that one mixture, not of "noisy
   audio" in general.
3. **The DER in the same artifact is not a diarizer's.** It scores a scripted
   hypothesis against exact synthetic ground truth, which measures
   `crates/ai-diarization::der`. No real diarization provider exists yet.

### Why the runtime is `ort` and the features are first-party

Measured on this machine, at `CARGO_BUILD_JOBS=2`, from a clean target
directory:

| Candidate | Clean build | Normal-build crates | What ships |
|---|---|---|---|
| `ort` 2.0.0-rc.14 + first-party fbank | **15 s** | 15 | one self-contained binary; ONNX Runtime linked statically |
| `sherpa-rs` 0.6.8 (the ADR-0081 fallback) | **80 s** | 15 | three dylibs — `libsherpa-onnx-c-api` (3.2 MB), `libsherpa-onnx-cxx-api` (94 KB), `libonnxruntime.1.17.1.dylib` (23 MB) — plus a bindgen/libclang and CMake requirement on every build host |
| `ort` + `knf-rs` (the ADR's literal wording) | 15 s | 17 | as the first row, plus vendored kaldi-native-fbank C++ built with CMake and bindgen |

The first-party feature front end was cross-checked against
`kaldi-native-fbank` through `knf-rs` 0.3.2 on a 3-second broadband signal with
the same window function: identical frame and bin counts (298 × 80),
**maximum absolute difference 5.6 × 10⁻⁴, RMS 1.2 × 10⁻⁵** — f32 rounding, not
a behavioural difference. `knf-rs` is a verification tool here and is not a
dependency of this repository.
