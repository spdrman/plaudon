# Speaker-matching evaluation harness (VOICE_PROFILES_SPEC §17.2)

The harness lives in `crates/eval/src/speaker/` and is driven by
`plaudon-eval speaker-match`. It turns a fixture plan into the artifact
ADR-0081 §4 names: `docs/benchmarks/speaker-match-<model>-eer.json`, validated
against `schemas/speaker-match-report.v1.schema.json`.

## Status

**One checkpoint has been measured, on one platform.** P2.5 (#248, extended by
#284) produced
[`speaker-match-wespeaker-resnet34-lm-eer.json`](speaker-match-wespeaker-resnet34-lm-eer.json):
WeSpeaker ResNet34-LM over LibriSpeech `test-clean`-derived fixtures — including
the MUSAN noise condition at 10 dB SNR — embedded by the real ONNX engine
(`crates/ai-speaker`) on macOS arm64. Pooled EER
**0.67%** over 300 target and 3 300 non-target trials, with the runtime
footprint beside it (startup 103 ms, RTF 0.0143, peak RSS 87 MiB). The run's
machine facts and exact commands are in
[docs/hardware/macos-apple-silicon.md](../hardware/macos-apple-silicon.md); the
fixture provenance — which LibriSpeech reader stood in for which plan label,
every file's digest, every condition's recipe — is in
[`speaker-match-wespeaker-fixtures.json`](speaker-match-wespeaker-fixtures.json).

The other committed artifact,
[`speaker-match-scripted-synthetic-eer.json`](speaker-match-scripted-synthetic-eer.json),
is still the hermetic CI tier: *scripted* embeddings, generated from a seed and
not extracted from audio. It says so in its own `release_evidence.blockers`.
Its error rates describe the generator and the harness around it, and must
never be quoted as an accuracy claim about any checkpoint.

These remain unmeasured and unclaimed:

- **Windows x64**: no peak RSS, no startup, no RTF, no EER. ADR-0081 §4 is a
  two-platform gate, so the feature stays off;
  [docs/release/voice-profiles-evidence.md](../release/voice-profiles-evidence.md)
  carries the item, its owner and the exact run that would close it;
- **noise beyond MUSAN's `noise` subset**: the measured `noise_musan` condition
  is one additive mixture at a single declared SNR (10 dB). MUSAN's `music` and
  `babble` material, other SNRs and real far-field capture are not measured;
- **the shipped threshold defaults**. The derived `T_suggest`/`T_floor` from
  the measured run describe LibriSpeech read speech at three seconds; nothing
  installs them, and a default operating point needs the Windows leg and
  meeting-like material first.

## Running it

```sh
# The hermetic CI tier: no audio, no model, deterministic bytes.
cargo run -p plaudon-eval --bin plaudon-eval -- speaker-match \
  --plan tests/fixtures/speaker-eval/harness-plan.json \
  --out docs/benchmarks/speaker-match-scripted-synthetic-eer.json

# The measured tier. `--model-pack` is any directory holding an
# `eval-embeddings.json`; `plaudon-speaker-embed` (crates/ai-speaker, feature
# `onnx`) is what produces one from audio and a checkpoint. The runtime
# figures are passed in because the harness does not run the model and must
# not invent them.
cargo run -p plaudon-eval --bin plaudon-eval -- speaker-match \
  --plan tests/fixtures/speaker-eval/librispeech-plan.json \
  --model-pack "$PLAUDON_MODEL_PACK" \
  --out docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json \
  --commit "$(git rev-parse HEAD)" --measured-on "$(date -u +%FT%TZ)" \
  --platform macos-arm64 --host-cpu "Apple M5" \
  --peak-rss-bytes 91242496 --startup-ms 103 --real-time-factor 0.0143
```

`crates/eval/tests/speaker_harness.rs` regenerates the committed artifact and
fails if it differs, so a change in the matching core, the generator or the
metric code shows up as a diff in review rather than as a silent drift.

## What the harness does

1. **Licence-checks the fixture library** (`speaker::fixtures`). Only the
   VP-181 corpora are representable; the VP-182 names are rejected wherever
   they appear, including in free text.
2. **Fetches embeddings** (`speaker::embeddings`) from the scripted generator
   or from a model pack's `eval-embeddings.json`.
3. **Scores with the shipped matching core** (`plaudon_app_core::voice_profiles::matching`):
   real cosine, real AS-Norm against a cohort built from non-roster speakers,
   real multi-exemplar aggregation. Enrolment uses the first *N* utterances of
   each speaker, trials the ones after them — a profile is never scored
   against its own enrolment material.
4. **Builds DET curves** (`speaker::det`), pooled and per condition, from every
   (cluster, profile) score. The first pass runs with an open operating point
   precisely so no score is discarded before the curve is drawn.
5. **Derives the two thresholds** (`speaker::calibration`): `T_suggest` is the
   loosest observed threshold with a false-accept rate ≤ 1% (VP-062), `T_floor`
   the same at ≤ 10%. A trial set with fewer than 100 non-target trials cannot
   resolve 1% and the harness refuses to publish a threshold from it.
6. **Re-bands the trials** with the derived thresholds, using the product's own
   band rule, and pins the suggestions of a golden fixture to the model sha256.
7. **Scores DER** (`crates/ai-diarization::der`) on the synthetic session,
   against a scripted hypothesis. A *provider's* DER on mixed audio is a
   separate run and a separate artifact ([`diarization-eval.md`](diarization-eval.md)).

## The fixture tiers and their licences (VP-181, VP-182)

| Tier | Corpus | Licence | In the repository? |
|---|---|---|---|
| Scripted | none (seeded vectors) | n/a | yes — the CI tier, no audio at all |
| Synthetic multi-speaker | LibriSpeech `test-clean` derived | CC-BY-4.0 | ground truth and digests only; audio built locally |
| Noise variants | MUSAN | CC-BY-4.0 | built locally |
| Zero-human-voices | Kokoro-82M TTS | Apache-2.0 | built locally |
| Accuracy gate | AMI, ICSI | CC-BY-4.0 | **never** — download-on-demand, developer-local |
| Forbidden | VoxCeleb, VoxConverse, AISHELL-4 | not redistributable / ShareAlike | **never**, in repo or CI |

Attribution rows are in
[`docs/licensing/third-party-register.md`](../licensing/third-party-register.md).
Committing audio from the LibriSpeech, MUSAN or Kokoro tiers would also add a
`NOTICE` obligation, which is one more reason the repository carries their
ground truth and checksums rather than their samples.

### Building the audio tiers locally

`plaudon-speaker-embed build-fixtures` (`crates/ai-speaker`, feature `onnx`)
does this for the single-speaker trial material, and the 2026-10-12 measured
run is exactly its output:

```sh
plaudon-speaker-embed build-fixtures \
  --librispeech <dir with LibriSpeech/test-clean> \
  --musan <dir with musan/noise> \
  --plan tests/fixtures/speaker-eval/librispeech-plan.json \
  --out <fixture dir>
```

`--musan` is required whenever the plan lists `noise_musan`, and the tool
refuses the condition without it: an approximation labelled as MUSAN would be a
false provenance claim. `--musan-snr-db` overrides the 10 dB default and the
figure used is written into the manifest, because a noise condition without a
stated SNR reports an error rate that means nothing.

1. Plan labels are assigned to LibriSpeech readers deterministically — both
   sorted, then zipped — and the assignment is written into the output
   manifest, so "which reader is `roster-07`" is a committed fact rather than a
   run-time accident.
2. Each reader's files are consumed in corpus order and cut into fixed-length
   utterances that never overlap, so enrolment material and trial material are
   different speech from the same person. A verification trial that scored a
   profile against its own enrolment audio would measure nothing.
3. The conditions are applied per utterance (table below) and every file's
   sha256, duration and source files land in `fixtures.json`, which is
   committed as `speaker-match-wespeaker-fixtures.json`. The plan's
   `media_sha256` folds the sorted `<token> <sha256>` lines of a condition, so
   the plan pins the audio without carrying it.
4. The audio itself is never committed. Neither are the embeddings.

The session timeline (`speaker::fixtures`) remains the generator's: a seeded
turn permutation with controlled overlap and an exact RTTM. Mixing that
timeline into one multi-speaker wav — the input a *diarization* provider needs
— is `plaudon-speaker-embed build-session`, and the DER a real provider scores
on it is in [`diarization-eval.md`](diarization-eval.md). The DER figure inside
*this* artifact is still the scripted hypothesis, and still measures
`crates/ai-diarization::der` rather than a provider.

### Cross-device conditions (VP-184)

Each condition is one chain applied to the fixture audio before extraction:

| Condition | Chain |
|---|---|
| `clean` | none |
| `noise_musan` | one file of the MUSAN `noise` subset summed at **10 dB SNR**, the noise scaled by the ratio of the two RMS levels and the sum divided by its peak only if it would clip; file and start offset are derived from the sha256 of the fixture token, and both are recorded per fixture in the manifest |
| `opus_low_bitrate` | encode at 12 kbit/s with libopus and decode back (developer-local FFmpeg; the product's own decoder is `crates/audio/src/opus.rs`) |
| `telephone_bandpass` | two-pole RBJ high-pass at 300 Hz and low-pass at 3400 Hz, Q = 0.7071 |
| `room_impulse_response` | convolution with a deterministic image-source response of a 5.0 × 4.0 × 2.7 m room, wall reflection 0.6, 12 image orders, peak-normalised |

The report carries one DET curve, EER and band partition per condition, so the
§6.5 device-variance story is a measurement rather than an assertion. The
scripted tier models the conditions as different within-speaker spreads: enough
to exercise the per-condition reporting path end to end, and no more.

## Supplying real embeddings

Inference runs out of process (ADR-0015), and the harness must stay hermetic,
so the harness never loads an ONNX file. A model pack — or any directory —
supplies `eval-embeddings.json`, which `plaudon-speaker-embed embed` writes:

```json
{
  "schema_version": 1,
  "model": {
    "model_id": "wespeaker-resnet34-lm",
    "model_version": "2026-01",
    "model_sha256": "<64 hex characters of the checkpoint file>"
  },
  "dim": 256,
  "vectors": { "<speaker>/<utterance>/<condition>": [0.0, 0.0] }
}
```

Keys are `SegmentKey::token()`: speaker label, utterance index, condition token.
A missing segment fails the run loudly instead of quietly scoring fewer trials.
The file holds embeddings, so it lives in the model pack beside the weights and
is never committed, exported or logged (VP-002, VP-112); only scores and counts
reach the artifact.

## Reading the artifact

- `release_evidence.qualifies` is the only field that says whether the document
  may be cited. It is computed from the ADR-0081 §4 conditions, not asserted.
- `overall` pools every condition; `conditions[]` is where the honest story is.
- `calibration.status` is `derived` only when the sample could resolve the 1%
  ceiling and the two thresholds did not collapse onto one score.
- `caveats` must travel with any number taken out of the file: accuracy is
  demographically skewed (VP-170), no score ever assigns anything (VP-060), and
  VoxCeleb1-O model-card EERs measure a different protocol (VP-173).
