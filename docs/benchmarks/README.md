# Local speech-to-text benchmarks (task A1.1)

Spec §17.5 says concrete transcription-speed and memory targets are set **only
after** this spike establishes hardware baselines. This document is that
baseline record.

## Status

**No baseline has been measured yet.** `docs/benchmarks/results/` is empty, so
no performance target may be quoted anywhere in the product, the
specification or marketing material. `a1_1_whisper_backend.rs::no_performance_target_is_asserted_before_a_run_exists`
enforces that: the moment a result file lands, the claim in this section must
be replaced with the measured numbers.

What *is* implemented and tested without hardware:

- the backend probe to `availability_reason` mapping, including the rule that
  an unavailable accelerator never silently becomes "CPU, as chosen";
- deterministic backend selection with a rejection reason per candidate;
- model manifest verification, which refuses a tampered, truncated or
  extended weights file before it is loaded;
- the result schema and its citability rule.

## Running a benchmark

```sh
# macOS
scripts/bench-whisper.sh --model ggml-base.en-q5_1 --backend metal --audio tests/fixtures/audio/valid/tone-mono-16k.wav

# Windows (PowerShell)
scripts\bench-whisper.ps1 -Model ggml-base.en-q5_1 -Backend cpu -Audio tests\fixtures\audio\valid\tone-mono-16k.wav
```

Each run appends one JSON document to `docs/benchmarks/results/`, validated
against `schemas/benchmark-result.v1.schema.json`. Commit the result file
together with the exact host identification the script captures; a result
whose host is not identified is evidence of nothing and the schema rejects it.

## Matrix to fill

Spec §2.6 asks for "Windows and macOS whisper.cpp CPU/GPU paths, binaries and
model redistribution licenses, and hardware-specific benchmarks". The matrix
is therefore at least:

| Host | Backend | Models | Measurements |
|---|---|---|---|
| macOS, Apple Silicon | Metal, CPU | two sizes | real-time factor, peak RSS, wall clock, cancellation latency, 60-minute stability |
| Windows 11, x86-64 | CPU, and the GPU backend the build actually contains | two sizes | the same |

Also record per run: whether the engine reported segment boundaries, whether
it reported word timings, and the observed cancellation latency. Those three
populate `SttCapabilities.supports_segments`,
`supports_word_timestamps` and `can_cancel`, and spec §8.3 says an unknown
value is not a supported value.

## Licensing findings

Recorded in `docs/licensing/third-party-register.md`. In summary: whisper.cpp
itself is MIT and we build the binary we ship; the original Whisper weights
are MIT but **each redistributed conversion must be checked individually**,
and any conversion whose terms are not clearly MIT is marked not bundlable and
is user-downloaded only. `ModelManifestEntry::may_bundle` defaults to false
for exactly that reason.
