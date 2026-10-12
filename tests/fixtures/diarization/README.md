# Diarization Test Fixtures

These fixtures are **synthetic annotations authored for this repository**.  
They contain no third-party corpus data and no borrowed benchmark numbers.

## Derivation

Each fixture includes hand-calculated `expected` values derived from the DER
definition in `crates/ai-diarization/src/der.rs`:

- **Frame grid**: 10 ms frames, half-open time ranges `[start_ms, end_ms)`.
- **Denominator**: total reference speech in milliseconds, counting overlapping
  speech once per simultaneous speaker.
- **Components**: missed speech (reference with no hypothesis), false alarm
  (hypothesis with no reference), and confusion (matched slots with wrong
  cluster mapping).
- **DER**: `(missed + false_alarm + confusion) / total_reference_ms`.

The expected values were computed frame-by-frame by hand and verified against
the implementation. If the implementation disagrees, the discrepancy is
investigated to determine which is correct.

## Fixtures

- **`two-speaker-clean.json`**: Two speakers, no overlap, perfect hypothesis.
  Tests basic DER calculation with zero error.

- **`three-speaker-overlap.json`**: Three speakers with genuine simultaneous
  speech (500 ms each of two overlapping regions). Tests that
  `overlapping_reference_ms` is computed correctly and that the denominator
  counts overlap once per speaker.

- **`single-speaker-silence.json`**: One speaker with hypothesis false alarms
  in silence before and after the reference speech. Tests that false alarms
  contribute to the numerator without inflating the denominator.

## Running the Harness

```bash
# Run the known-answer tests
cargo test -p plaudon-ai-diarization der_known_answers

# Generate quality metrics report
cargo run -q -p plaudon-ai-diarization --bin quality_metrics -- \
  --out quality-report.json

# Verify the emitter
cargo test -p plaudon-ai-diarization quality_metrics_emitter
```

The emitter binary reports DER over these fixtures plus optional tool-quality
scores when a labeled corpus is provided. Without a corpus, tool quality is
reported as `{"status": "skipped", "reason": "corpus_not_available"}`.
