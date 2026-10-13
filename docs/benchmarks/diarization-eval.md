# Diarization error rate of a real provider (ADR-0400 condition 4)

ADR-0400 ships the diarization plugin contract with only the deterministic fake
in `VALIDATED_PROVIDERS`, and lists five conditions a real provider must meet
before it may join. The fourth is a measurement:

> **DER measurement recorded.** The provider passes the DER harness on labelled
> fixtures including overlapping speech, with sample sizes and hardware noted,
> and the measurement artifact committed under
> `docs/benchmarks/diarization-<provider>-der.json`.

This document is that measurement's record. The artifact is
[`diarization-onnx-pyannote-wespeaker-der.json`](diarization-onnx-pyannote-wespeaker-der.json),
validated against `schemas/diarization-der-report.v1.schema.json`.

## Status

**Condition 4 is measured. The provider is still not validated**, and
`release_evidence.qualifies` in the artifact is `false`, because four of the
five conditions are open — Windows x64 is unmeasured, inference still runs in
the application process (ADR-0015), the corpus is synthetic rather than
meeting-domain, and there is no user-facing model-pack download or disclosure
flow for diarization. `crates/ai-speaker/src/diarize.rs` has a test asserting
that `onnx-pyannote-wespeaker` is absent from `VALIDATED_PROVIDERS`, so the
measurement cannot quietly become a shipping decision.

## The provider

`plaudon_ai_speaker::diarize::OnnxDiarizationProvider` (feature `onnx`, off by
default, wired into no composition root). It is a first-party implementation of
the pipeline `pyannote/speaker-diarization-3.1` describes, over the two
bundle-tier checkpoints this workspace already pins:

1. **Sliding-window segmentation.** `pyannote-segmentation-3.0.onnx` decides,
   for each 10-second window, which of up to three local speakers is active in
   each of its 589 frames. The window advances 59 frames (0.996 s), a tenth of
   the window, so every frame is decided by roughly ten windows.
2. **Powerset decoding.** The model's seven classes are the representable sets
   of simultaneous speakers; the decision is the argmax, so "both of them" is a
   class rather than two independent thresholds.
3. **Per-track embedding.** Each (window, local speaker) pair is embedded by
   WeSpeaker ResNet34-LM from the frames where that speaker is the *only* voice
   (`embedding_exclude_overlap`); a track with no exclusive speech is embedded
   from the mixture and barred from shaping the clustering.
4. **Constrained clustering.** Centroid-linkage agglomerative clustering over
   cosine distance at the published threshold, with two tracks of one window
   never allowed to merge, clusters below `min_cluster_size` dissolved into the
   nearest cluster, and short tracks assigned afterwards.
5. **Aggregation.** Per-window decisions are averaged over the windows that
   covered each frame with a Hamming taper, binarised at 0.5, and turned into
   turns.

Steps 2, 4 and 5 need no inference runtime and live in
`plaudon_ai_diarization::pipeline`, where they are unit-tested in the default
build. Only steps 1 and 3 — two forward passes — are behind the `onnx` feature.

The operating point is **not tuned on these sessions**: `threshold = 0.7045`
and `min_cluster_size = 12` are the values `pyannote/speaker-diarization-3.1`
publishes for this exact checkpoint pair. Every parameter the run used is in
the artifact's `provider.parameters`, each with its provenance.

## The fixtures

`plaudon-speaker-embed build-session` mixes the deterministic session timelines
of [`tests/fixtures/speaker-eval/diarization-session-plans.json`](../../tests/fixtures/speaker-eval/diarization-session-plans.json)
into multi-speaker audio. The timeline *is* the ground truth — `SessionPlan`
lays the turns out from a fixed seed — so the RTTM is exact by construction and
nothing is annotated.

| Session | Speakers | LibriSpeech readers | Audio | Reference speech | Overlap |
|---|---|---|---|---|---|
| `librispeech-session-01` | 4 | 61 (M), 121 (F), 237 (F), 260 (M) | 102.0 s | 96.0 s | 3.0 s |
| `librispeech-session-02` | 3 | 672 (M), 908 (M), 1089 (M) | 74.4 s | 81.6 s | 10.8 s |
| `librispeech-session-03` | 3 | 672 (M), 908 (M), 1089 (M) | 90.9 s | 84.0 s | none |

Reference speech counts overlapped time once per simultaneous speaker, which is
also the DER denominator. Session 03 is session 02 with the overlap removed and
nothing else changed — same readers, same seed, same turn order — so the
difference between them is what *overlap* costs this provider rather than what
three similar voices cost it.

Each session is built in four channel conditions (`clean`,
`telephone_bandpass`, `opus_low_bitrate`, `room_impulse_response`), the same
recipes the speaker-matching fixtures use, applied to the finished mix. Twelve
measurements in total, 1 046.4 s of reference speech, 110.4 s of it
overlapping.

Two properties of the mix are deliberate and both are in the artifact's
`corpus.recipe`:

- **Each utterance is scaled to a common RMS before mixing**, so a loud reader
  cannot mask a quiet one and the measurement stays about diarization.
- **The corpus material is silence-trimmed before it is cut into turns.** The
  ground truth says "this speaker is active from here to here"; LibriSpeech
  read speech is about a fifth pauses, and scoring those pauses as missed
  speech would measure the fixture rather than the provider. The session is
  therefore denser than natural conversation, which the caveats repeat.

The audio is never committed (VP-181): the repository carries the plan, the
digests and the RTTM, and a contributor rebuilds the wav from their own
LibriSpeech copy.

## Running it

```sh
export SDKROOT="$(xcrun --show-sdk-path)"
export CXXFLAGS="-isystem $SDKROOT/usr/include/c++/v1"
scripts/fetch-speaker-models.sh --dir /tmp/speaker-models

# LibriSpeech test-clean (CC-BY-4.0, openslr.org/12), 346 663 984 bytes
curl -L -o test-clean.tar.gz https://openslr.elda.org/resources/12/test-clean.tar.gz
tar xzf test-clean.tar.gz -C /tmp/p282-corpus

cargo build -j2 --release -p plaudon-ai-speaker --features onnx \
  --bin plaudon-speaker-embed

./target/release/plaudon-speaker-embed build-session \
  --librispeech /tmp/p282-corpus \
  --plan tests/fixtures/speaker-eval/librispeech-plan.json \
  --sessions tests/fixtures/speaker-eval/diarization-session-plans.json \
  --out /tmp/p282-sessions \
  --conditions clean,telephone_bandpass,opus_low_bitrate,room_impulse_response

/usr/bin/time -l ./target/release/plaudon-speaker-embed diarize \
  --segmentation /tmp/speaker-models/pyannote-segmentation-3.0.onnx \
  --embedding /tmp/speaker-models/wespeaker_en_voxceleb_resnet34_LM.onnx \
  --sessions /tmp/p282-sessions \
  --out docs/benchmarks/diarization-onnx-pyannote-wespeaker-der.json \
  --peak-rss-bytes <from /usr/bin/time -l> \
  --commit "$(git rev-parse HEAD)" --measured-on "$(date -u +%FT%TZ)"
```

`--hypothesis-rttm <dir>` additionally writes each run's turns as RTTM and its
per-track cluster assignments as JSON; neither is committed, both are what you
need to see *where* a run is wrong rather than how wrong it is.

The run is deterministic: the same audio and the same build produce the same
DER to every digit, which is what makes a regression visible.

## Results

macOS 27.0.1 (build 26A434) on an Apple M5, one intra-op thread, CPU execution
provider (#283's policy: a measured operating point belongs to one numerical
engine), release build linking the vendored ONNX Runtime,
2026-10-13. Zero collar, overlapping speech included, no speaker-count hint.

### Pooled over all twelve runs

| Scope | DER | Missed | False alarm | Confusion | Reference speech |
|---|---|---|---|---|---|
| Overall | **32.65%** | 19.9 s | 69.6 s | 252.1 s | 1 046.4 s |
| Single-speaker frames | 33.40% | 14.4 s | 68.1 s | 230.1 s | 936.0 s |
| Overlapping frames | 26.29% | 5.5 s | 1.5 s | 22.0 s | 110.4 s |

### By channel condition

| Condition | DER | Single-speaker | Overlapping |
|---|---|---|---|
| `clean` | **25.51%** | 25.77% | 23.30% |
| `telephone_bandpass` | 29.72% | 29.75% | 29.42% |
| `opus_low_bitrate` | 32.13% | 32.43% | 29.60% |
| `room_impulse_response` | 43.24% | 45.65% | 22.83% |

### By session, clean condition

| Session | DER | Missed | False alarm | Confusion | Clusters found |
|---|---|---|---|---|---|
| `librispeech-session-01` (4 speakers, mixed sexes, 3% overlap) | **11.76%** | 1.3 s | 2.7 s | 7.2 s | 4 of 4 |
| `librispeech-session-02` (3 male speakers, 13% overlap) | 31.67% | 1.2 s | 7.3 s | 17.4 s | 3 of 3 |
| `librispeech-session-03` (the same three, no overlap) | 35.24% | 5.2 s | 4.5 s | 19.8 s | 3 of 3 |

### Runtime footprint

| Figure | Value | How it was taken |
|---|---|---|
| Startup | **75 ms** | Digest verification of both checkpoints plus two ONNX Runtime sessions, timed in the tool. |
| Real-time factor | **0.142** | 152.0 s of work for 1 069.2 s of audio, segmentation and embedding together, one thread, CPU execution provider. |
| Peak RSS | **318 439 424 bytes (304 MiB)** | `/usr/bin/time -l`, whole process, over all twelve runs. Two identical runs peaked at 304 and 286 MiB; the artifact records the larger. |
| Per session | 9.6–14.4 s | 75–103 windows of segmentation plus 112–203 embeddings each. |

## What these numbers say

**Speaker similarity dominates, not overlap.** The three-male-reader session
scores worse *without* overlap (35.24%) than with it (31.67%), and the
four-speaker mixed-sex session scores 11.76%. Confusion — speech given to the
wrong cluster — is 83% of all error. The segmentation model also reuses one
local slot for two consecutive similar voices inside a window, so some tracks
are acoustically mixed before clustering ever sees them.

**Overlap is handled, not ignored.** Overlapping frames score *better* than
single-speaker frames here (26.29% against 33.40%), because the error in the
overlap regions is mostly the same confusion the rest of the session has, while
the detection of simultaneous speech itself works: 5.5 s of 110.4 s of
overlapped reference speech is missed. A provider that could not represent
overlap at all would miss all 110.4 s.

**Room acoustics cost more than the codec or the band.** Reverberation
(43.24%) hurts about twice as much as a 12 kbit/s Opus round trip (32.13%),
and it hurts the single-speaker frames far more than the overlapping ones.

**None of this is a meeting.** The sessions are read speech, close-talk, one
channel, silence-trimmed, with no interruptions, backchannels or Lombard
effect. The published pyannote numbers for the same checkpoints — roughly
20-22% on AMI, 11% on VoxConverse — are measured on real conversations with a
tuned pipeline and a different scoring protocol; they are not comparable to
these figures in either direction.

## Caveats that travel with any number from this file

The artifact carries them in `caveats`, and they are not optional:

- the corpus is a synthetic mix, so these figures are an upper bound on field
  quality rather than a prediction of it;
- the scoring uses a zero collar and includes overlap, so figures quoted
  elsewhere with a 250 ms collar or with overlap excluded are not comparable;
- the clustering threshold is a published default, not a value derived from
  these sessions;
- a cluster is not an identity (VOICE_PROFILES_SPEC §17.6);
- accuracy on read English by LibriSpeech readers does not transfer to other
  languages, accents or recording chains (VP-170).

## What is still open

[`docs/release/voice-profiles-evidence.md`](../release/voice-profiles-evidence.md)
owns the list; for this provider it is: the Windows x64 leg, out-of-process
inference, meeting-domain audio (AMI/ICSI, download-on-demand only), and a
user-facing model-pack flow with the licence disclosure ADR-0400 conditions 2
and 3 require. Until those close, the provider stays out of the allowlist and
`run_guarded` refuses it.
