# Apple native AI: measured results and compatibility cells

**Spec:** §11.5, §17.1, §17.2, §22(4), §22(17)
**Produced by:** `tests/hardware/tests/apple_compatibility.rs` (opt-in) and
`crates/ai-apple/src/bin/probe.rs --json`
**Consumed by:** `plaudon_ai_apple::compatibility::CompatibilityMatrix`, which
derives the interface labels and the release evidence bundle.

A cell is `Supported` only when a real-hardware record shows the feature
producing output with zero network requests. Simulated evidence reaches
`Experimental` at best, documentation alone reaches nothing. The gate is
`CompatibilityMatrix::gate_failures`, and it is unit-tested against synthetic
records so the rule holds without hardware.

## Cell 1 — Apple Silicon, Apple Intelligence ready

**Measured 2026-10-11.** Run id `apple-apple_silicon_ready-2026-10-12T03:50Z`.

| | |
|---|---|
| OS | macOS 27.0.1, build 26A434 |
| Hardware | Apple M5 |
| Toolchain | Apple Swift 6.4, bridge target `arm64-apple-macosx13.0` |
| Apple Intelligence | ready |
| Locale | `en-US`, installed |
| Process | a plain Rust test binary linking the Swift static library — **not** an application bundle |

### `probeSpeech`

`SpeechTranscriber.isAvailable` true. 45 supported locales; 9 installed, all
English regions. Concurrency ceiling (`AssetInventory.maximumReservedLocales`)
5. Accepted analyzer formats: 8 kHz and 16 kHz mono.

Probe cost, measured end to end including the Foundation Models probe:

| | |
|---|---|
| Cold, in-process | 17.9 ms – 147 ms (first call in a process pays framework initialization) |
| Cached read | 3.5 µs – 7.0 µs |
| Application budget | 100 ms (`plaudon_ai_apple::probe::PROBE_BUDGET`) |

A cold probe can exceed the budget on a first call. That is handled rather
than ignored: the budget overrun is reported as `engine_timeout`, the
capability maps to `Unknown`, and the engine policy selects whisper.cpp. The
cache then makes every subsequent ask free.

### `transcribeFile`

Fixture: 7.636 s, 16 kHz mono WAV, synthesized speech.

| | |
|---|---|
| Wall clock | 400 ms (≈19× real time) |
| Segments | 4, all finalized |
| Alignment origin | `native` |
| Confidence | reported per segment, 0.53 – 0.97 |
| Word timings | present via the `audioTimeRange` attribute |
| Network requests | 0 |

Transcript, verbatim:

```text
Hello, team. This is a short meeting recording. We decided to ship the Apple
Bridge on Friday. Anna will send the release notes.
```

### `generateText`

Guided generation against a JSON Schema, converted to a
`DynamicGenerationSchema`:

```json
{"summary": "We decided to ship on Friday. Anna sends the notes."}
```

197 input tokens, 25 output tokens, reported by `LanguageModelSession.Usage`
(macOS 27+). A free-form guarded prompt on an earlier run produced the same
shape in 3.3 s for a two-field schema with a nested array.

The framework reports **no context window**. `max_context_tokens` is `null`,
and the only place a real number appears is inside
`LanguageModelError.contextSizeExceeded.contextSize`. The chunk planner
therefore starts from a labelled conservative bound
(`ContextOrigin::Conservative`) and calls `ContextBudget::shrunk` with the
observed figure after the first overflow.

### Guardrails

A deliberately unsafe prompt was refused by the platform with
`LanguageModelError` ("May contain unsafe content"). It is classified as
`guardrail_violation`, surfaces as `TextModelError::ProviderRejected`, and
cannot reach a `StructuredResponse`.

## Cell 2 — Apple Silicon, Apple Intelligence disabled or unready

**Not measured on hardware.** No machine in this state was available.

The mapping is implemented and unit-tested from the platform's own
unavailability reasons: `appleIntelligenceNotEnabled` → `disabled_by_user`,
`modelNotReady` → `model_asset_not_ready`, `deviceNotEligible` →
`hardware_unsupported`. The engine policy and the fallback to whisper.cpp are
covered hermetically in
`tests/integration/tests/h_apple_bridge.rs`.

Cell status: **Unsupported** for Apple AI, with no record yet showing the
local fallback completing on such a machine. `gate_failures()` reports this,
and it is the honest state until someone runs the suite on one.

## Cell 3 — Intel Mac or otherwise ineligible

**Not measured on hardware.** No such host was available.

The non-macOS path *is* measured: with the `apple-bridge` feature off, the
probe answers `platform_unsupported` with no Apple framework loaded, on every
platform, and `scripts/check-no-apple-symbols.sh` verifies the artifact
contains no Apple symbol.

Cell status: **Unsupported** for Apple AI, same gap as cell 2.

## Comparison against whisper.cpp (§17.2)

**Not yet run.** `ComparisonMetrics` is recorded by the hardware suite and
carries the fixture, both wall-clock figures, both transcript digests and a
token-level agreement figure. The Apple leg is measured above; the
whisper.cpp leg needs an installed model pack on the same machine.

The agreement figure is deliberately named *agreement*, not accuracy: neither
engine is ground truth, and §1.5 forbids an accuracy claim without a
reference corpus.

## Reproducing

```sh
# Probe only, no transcription, no download.
scripts/probe-apple.sh

# Full measurement with an evidence record.
PLAUDON_APPLE_HARDWARE_TESTS=1 \
PLAUDON_APPLE_AUDIO_FIXTURE=/path/to/speech-16k.wav \
PLAUDON_TEST_RUN_ID=apple-$(date -u +%Y%m%dT%H%M%SZ) \
  cargo test -p plaudon-hardware-tests --features apple-bridge \
    --test apple_compatibility -- --nocapture

# Records land in target/hardware-evidence/ for the release bundle.
```

Make the fixture with:

```sh
say -o /tmp/speech.aiff "…"
afconvert -f WAVE -d LEI16@16000 -c 1 /tmp/speech.aiff /tmp/speech-16k.wav
```
