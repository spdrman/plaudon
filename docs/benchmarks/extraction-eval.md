# Extraction Evaluation Suite

**EPIC I Task I2.5** — Hermetic, offline evaluation harness for the meeting extraction pipeline.

## Purpose

This evaluation suite measures the intrinsic quality of the extraction harness and grounding infrastructure using synthetic reference corpora with hand-verified evidence anchors. It is **not** a claim about any provider's quality.

The suite runs entirely offline, with no network access and no real text model. It uses a reference "perfect" model that returns exactly what the corpus reference data specifies, so the measured metrics reflect the harness itself, not an LLM.

## What is Measured

The §17.3 metrics suite:

1. **Decision precision, recall, and F1**
2. **Action item precision, recall, and F1**
3. **Owner accuracy** (over matched action items)
4. **Due date accuracy** (over matched action items)
5. **Schema validity rate** (fraction of model items accepted by `OutputValidator`)
6. **Anchor correctness** (from the pipeline's `GroundingSummary::anchor_correctness()`)
7. **Unsupported claim rate** (from the pipeline's `GroundingSummary::unsupported_claim_rate()`)

Every metric is `Option<f64>`: an undefined metric (e.g., no reference decisions) is `null` in the report, never `0.0`.

## Matching Rule

Two items (predicted vs. reference) are matched when their evidence pointers reference overlapping segments. Specifically:

- Extract the `seq_index` from each item's anchors
- Match if any `seq_index` appears in both sets

This is a simple, deterministic rule that does not require text similarity or NLP. It is intentionally conservative: items must cite the same region of the transcript to count as a match.

## Mutation Profiles

The reference model can be wrapped in mutation profiles to inject controlled defects, so precision/recall are actually exercised:

- **Perfect**: No defects. Baseline measurements.
- **DropItems{fraction}**: Drop a fraction of items (tests recall).
- **FabricateItems{count}**: Add fabricated items (tests precision).
- **FabricateAnchor**: Anchor pointing to nonexistent segment 999.
- **ShiftSpan{delta}**: Shift character offsets by N (tests anchor verification).
- **FalsifyOwner**: Replace owner with a wrong value.
- **FalsifyDueDate**: Replace due date with a wrong value.
- **InvalidDate**: Use `2025-02-30` (tests date validation).
- **OversizedArray**: Duplicate items to exceed schema bounds.
- **MissingAnchor**: Remove all anchors (tests grounding failure).

Each profile is deterministic given a seed.

## Corpus Fixtures

Location: `tests/fixtures/ai/corpus/*.json`

Five synthetic meeting transcripts:
- `general_meeting_1min.json` (9 segments, general_meeting template)
- `project_meeting_10min.json` (47 segments, project_meeting template)
- `client_call_60min.json` (131 segments, client_call template)
- `interview_180min.json` (221 segments, interview template)
- `lecture_notes_10min.json` (43 segments, lecture template)

**Locale**: All fixtures are `en-US`.

**Provenance**: All fixtures are synthetic content authored specifically for this repository. No real meeting content.

Every `evidence` pointer in the reference data is validated at load time to be an exact substring of the cited segment. A corpus file with invalid evidence is a hard error.

## Baseline Measurements (Perfect Profile)

The following numbers were measured with the Perfect mutation profile on the initial corpus set. These are **harness measurements**, not quality claims.

| Metric                  | Value  | Sample Size |
|-------------------------|--------|-------------|
| Decision Precision      | 1.0000 | TBD         |
| Decision Recall         | 1.0000 | TBD         |
| Decision F1             | 1.0000 | TBD         |
| Action Item Precision   | 1.0000 | TBD         |
| Action Item Recall      | 1.0000 | TBD         |
| Action Item F1          | 1.0000 | TBD         |
| Owner Accuracy          | 1.0000 | TBD         |
| Due Date Accuracy       | 1.0000 | TBD         |
| Schema Validity Rate    | 1.0000 | TBD         |
| Anchor Correctness      | 1.0000 | TBD         |
| Unsupported Claim Rate  | 0.0000 | TBD         |

**Update these numbers after the first successful run.**

## Thresholds (§17.6)

Configured in `docs/benchmarks/extraction-eval-thresholds.json`.

The gate fails when:
- **Anchor correctness** for the Perfect profile falls below `1.0`
- **Schema validity rate** for the Perfect profile falls below `1.0`

These thresholds are derived from the measured Perfect-profile baseline above. They are **not** arbitrary quality targets; they are regression detectors. A Perfect-profile run that produces incorrect anchors or invalid schema indicates a bug in the harness, not a provider issue.

## Usage

### Run the Suite

```sh
bash scripts/run-extraction-eval.sh
```

Writes `target/eval/extraction-eval.json`.

### Run with Threshold Gate

```sh
cargo run -p plaudon-eval --bin extraction-eval -- \
    --out target/eval/extraction-eval.json \
    --check
```

Exits non-zero if the gate fails.

### Inspect the Report

The report is JSON. Schema: `docs/benchmarks/extraction-eval-report.v1.schema.json`.

Example:

```json
{
  "schema_version": "1",
  "generated_at": "2026-10-11T12:34:56Z",
  "pipeline_version": "1.0.0",
  "template_id": "mixed",
  "template_version": "1.0.0",
  "prompt_version": "1.0.0",
  "locale": "en-US",
  "per_corpus": {
    "general_meeting_1min": {
      "corpus_id": "general_meeting_1min",
      "template_id": "general_meeting",
      "metrics": { ... }
    }
  },
  "aggregate": {
    "decision_precision": 1.0,
    "decision_recall": 1.0,
    ...
  },
  "fixture_provenance": {
    "general_meeting_1min.json": "abc123..."
  }
}
```

## CI Integration

See `.github/workflows/extraction-eval.yml`. The workflow:
- Runs on every push and PR
- Uses the pinned Rust toolchain from `rust-toolchain.toml`
- Executes `scripts/run-extraction-eval.sh`
- Uploads the report as an artifact

No real provider. No network.

## Limitations

- **Synthetic data**: These fixtures are authored for testing, not sampled from real usage.
- **Harness measurements**: Metrics reflect the evaluation harness and pipeline infrastructure, not any provider's capabilities.
- **Matching rule**: The segment-overlap rule is simple and may under-count matches for items whose evidence spans are disjoint but semantically equivalent.
- **Locale**: All fixtures are `en-US`. Other locales are not yet covered.

## Maintenance

When updating the corpus:
1. Validate all evidence pointers programmatically (the loader does this automatically).
2. Re-run the Perfect profile to update baseline numbers in this document.
3. Update thresholds in `extraction-eval-thresholds.json` if the baseline changes.
4. Document the provenance and locale of any new fixtures.

---

**Last Updated**: 2026-10-11  
**Fixture Count**: 5 corpus files, 75+ verified evidence pointers  
**Sample Size (aggregate)**: TBD after first run
