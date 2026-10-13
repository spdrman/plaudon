# Speaker-matching evaluation harness (VOICE_PROFILES_SPEC §17.2)

The harness lives in `crates/eval/src/speaker/` and is driven by
`plaudon-eval speaker-match`. It turns a fixture plan into the artifact
ADR-0081 §4 names: `docs/benchmarks/speaker-match-<model>-eer.json`, validated
against `schemas/speaker-match-report.v1.schema.json`.

## Status

**No model has been measured.** The committed artifact
[`speaker-match-scripted-synthetic-eer.json`](speaker-match-scripted-synthetic-eer.json)
was produced from *scripted* embeddings — vectors generated from a seed, not
extracted from audio — and says so in its own `release_evidence.blockers`. Its
error rates describe the generator and the harness around it. They are not an
accuracy claim about any checkpoint and must never be quoted as one.

Specifically, these remain unmeasured and unclaimed:

- EER/DET for `WeSpeaker ResNet34-LM` or any other real checkpoint, on any
  fixture tier;
- peak RSS, startup and real-time factor on macOS arm64 or Windows x64
  (ADR-0081 §4 requires both platforms; no published figures exist for either);
- DER of a real diarization provider (the committed DER figure scores a
  deliberately imperfect *scripted* hypothesis against exact synthetic ground
  truth, so it measures `crates/ai-diarization::der`, not a provider);
- the shipped threshold defaults. `T_suggest`/`T_floor` in the committed
  artifact are derived from scripted scores, so
  `calibration.is_shippable_default()` is false and nothing installs them.

## Running it

```sh
# The hermetic CI tier: no audio, no model, deterministic bytes.
cargo run -p plaudon-eval --bin plaudon-eval -- speaker-match \
  --plan tests/fixtures/speaker-eval/harness-plan.json \
  --out docs/benchmarks/speaker-match-scripted-synthetic-eer.json

# The real tier, once a model pack carries extracted embeddings.
cargo run -p plaudon-eval --bin plaudon-eval -- speaker-match \
  --plan tests/fixtures/speaker-eval/harness-plan.json \
  --model-pack "$PLAUDON_MODEL_PACK" \
  --out docs/benchmarks/speaker-match-wespeaker-resnet34-lm-eer.json \
  --commit "$(git rev-parse HEAD)" --measured-on "$(date -u +%FT%TZ)" \
  --platform macos-arm64 --host-cpu "Apple M2 Pro" \
  --peak-rss-bytes 412000000 --startup-ms 940 --real-time-factor 0.11
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
7. **Scores DER** (`crates/ai-diarization::der`) on the synthetic session.

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

The generator in `speaker::fixtures` produces the *timeline* — a seeded turn
permutation with controlled overlap and exact RTTM. The audio is assembled from
that timeline:

1. Download LibriSpeech `test-clean` (openslr.org/12) and pick N speakers.
2. For each turn in `SyntheticSession::rttm()`, take the speaker's next
   utterance, trim to the turn length and cross-fade the overlapping region.
   The RTTM is exact by construction: it is what the mixer was told to do.
3. Optional: mix MUSAN noise (openslr.org/17) at a declared SNR.
4. Optional: synthesize the same transcript with Kokoro-82M for the
   zero-human-voices tier.
5. Register each file in a fixture manifest with its sha256, condition and
   recipe. `FixtureManifest::parse` refuses the manifest if any field names a
   corpus we may not use.

### Cross-device conditions (VP-184)

Each condition is one chain applied to the fixture audio before extraction:

| Condition | Chain |
|---|---|
| `clean` | none |
| `noise_musan` | MUSAN mix at a declared SNR |
| `opus_low_bitrate` | encode/decode through low-bitrate Opus (`crates/audio/src/opus.rs`) |
| `telephone_bandpass` | 300–3400 Hz band-pass |
| `room_impulse_response` | convolution with a room impulse response |

The report carries one DET curve, EER and band partition per condition, so the
§6.5 device-variance story is a measurement rather than an assertion. The
scripted tier models the conditions as different within-speaker spreads: enough
to exercise the per-condition reporting path end to end, and no more.

## Supplying real embeddings

Inference runs out of process (ADR-0015), and the harness must stay hermetic,
so the harness never loads an ONNX file. A model pack supplies
`eval-embeddings.json`:

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
