# Local STT backend benchmarks (whisper.cpp)

Spec references: §12 (evaluate CPU / Apple Metal / Windows GPU "without making
GPU support a universal requirement"), §17.2 (STT metrics), §17.4 (compare
real-time factor across backends), §18.4 (platform tests for GPU fallback),
task F2.5.

## Why this document exists and what it does not claim

It records **measurements**, not targets. §17.6 forbids deriving a quality or
speed claim before the numbers exist, so this file starts with the method and
an empty results table; each row is filled in by a real run on named hardware.
A missing row means "not measured", never "assumed equivalent".

## Method

Reproduce a row with:

```bash
scripts/fetch-stt-fixtures.sh                 # once, seeds the model cache
cargo test -p plaudon-integration-tests --test f_local_stt -- --nocapture
```

Fixed parameters, all recorded on every transcript revision so a result can be
tied back to the run that produced it:

| Parameter | Value | Why fixed |
|---|---|---|
| model | `ggml-tiny.en` | smallest honest English model; keeps CI cheap |
| threads | 4 | whisper.cpp reductions are order-dependent, so a varying thread count makes transcripts irreproducible |
| beam size | 1 (greedy) | deterministic decode |
| temperature | 0.0 | deterministic decode |
| canonical audio | 16 kHz mono `f32le` | `CanonicalPcmProfile::whisper_mono_16k` |

The backend is chosen by `plaudon_ai_whisper::backend::select_backend`, which
asks ggml's own `print_system_info()` rather than inferring from the OS
version. `BackendPreference::Require(…)` is used for benchmarking so a run can
never silently measure the CPU while claiming a GPU number.

## Honest limitation

whisper.cpp falls back to its CPU kernels internally when a GPU backend
initialises but cannot execute a given op. We record the backend we
*requested* and whether context creation succeeded; we do not claim per-op
placement. Treat a GPU row as "GPU offload enabled", not "100 % of compute on
the GPU".

## Results

| Date | Host | OS / build | Backend | Model | Audio duration | Wall clock | Real-time factor | Peak RSS |
|---|---|---|---|---|---|---|---|---|
| _(unmeasured)_ | | | `cpu` | | | | | |
| _(unmeasured)_ | | | `metal` | | | | | |
| _(unmeasured)_ | | | `vulkan` | | | | | |

## Fallback behaviour verified in CI

`tests/integration/tests/f_local_stt.rs` asserts, on every hermetic run:

* a forced-unavailable GPU completes on the CPU and produces **identical**
  transcript content to the CPU baseline;
* the fallback reason is recorded in `engine_params["backend_fallback"]` and
  the active backend in `TranscriptDraft::backend`;
* only one local workload runs at a time, and a second request is refused with
  a busy error rather than queued invisibly.
