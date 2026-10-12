# ADR-0042: Grounded output surface — three commands for actions, decisions and review

- **Status:** accepted
- **Date:** 2026-10-11
- **Deciders:** EPIC I (meeting intelligence)
- **Spec:** §9.2 (output types), §9.6A (grounding), §17.3 (review requirements), task I2.3

## Context

§9.2 defines structured output types (summary statements, decisions, action items, risks, questions, topics) with required fields: action items carry owner and due date, decisions carry participants and time references, risks carry consequence and mitigation. §9.6A requires source anchors for every extracted item and defines three claim states: `source_linked` (at least one anchor resolved), `human_confirmed` (a reviewer accepted it), and `unverified` (grounding failed or reviewer rejected it). §17.3 marks high-consequence claims (financial amounts, obligations, deadlines, safety commitments) as **review required** and excludes them from automatic downstream dispatch until a human acts.

The §15.2 command table has no read path for generated outputs, no write path for human review decisions, and no regeneration trigger. The existing `get_recording` command returns only recording metadata and transcript revisions; it does not expose the structured outputs the pipeline produces. A reviewer cannot see which action items lack dates, which decisions have unverified anchors, or which claims require explicit confirmation before being trusted.

The review panel must render the §9.2 fields with their exact sentinels (`"Unassigned"` for missing owner, `"Not specified"` for missing due date) rather than inventing values, and it must respect the anchor verification outcome: when `seek_ms` is absent, no seek affordance is shown and no timestamp is synthesized (spec §8.6A: a revision with `alignment_origin: none` yields `seek_ms: None`). The only path to `human_confirmed` state is an explicit review action; no DTO lets the frontend assert a state.

## Decision

Add three commands to the desktop IPC surface:

- **`list_outputs(recording_id) -> ListOutputsResponse`**  
  **Spec:** Returns every generated output of the recording, with id, type, template id+version, pipeline version, status (`complete` | `needs_review`), coverage (`full` | `model_reported_partial` with chunk indices), staleness (when superseded), creation timestamp, and all claims. Each claim carries: id, item_path (JSON Pointer), kind, category, text, state, `review_required` boolean, `review_reasons` array, entailment check result, rendered §9.2 fields, and anchors. An anchor carries `seq_index`, `char_start`, `char_end`, `excerpt`, `verified` boolean, optional `rejection_code`, optional `matched_text`, `alignment_origin`, and optional `seek_ms` — present **only when the backend produced one**. The backend substitutes `"Unassigned"` for missing owners and `"Not specified"` for missing due dates before serialization, so the sentinel is part of the contract rather than a UI accident.

- **`review_claim(output_id, claim_id, accepted: bool, note?: string) -> ReviewClaimResponse`**  
  **Spec:** Records a human review decision on one claim. `accepted: true` moves the claim to `human_confirmed`; `accepted: false` moves it to `unverified`. Returns the updated claim state. This is the **only** path to `human_confirmed`: the persistence layer enforces that a claim cannot be `human_confirmed` without a recorded reviewer and instant, and the DTO types provide no shortcut. The `output_id` is validated but not otherwise used (it's present for consistency and to support future output-level review actions).

- **`regenerate_output(output_id, acknowledged: bool) -> RegenerateOutputResponse`**  
  **Spec:** Triggers regeneration of a stale output. Refuses without `acknowledged: true` (returns `CONFIRMATION_REQUIRED`), and never mutates the existing output; regeneration writes a new row with a supersession link. Returns a job id. The implementation is stubbed with `CAPABILITY_UNVERIFIED` until the pipeline is wired; the command surface is stable and the stub is replaced without a schema change.

All three follow the existing one-request-struct / typed-response / §15.4-error-envelope pattern. The allowlist test enforces that only these 19 commands (the 16 from ADR-0020 plus these three) are reachable from JavaScript.

## Rejected alternatives

- **Embedding outputs in `get_recording` response.** Rejected: a recording with multiple regenerated outputs and hundreds of claims would bloat every detail-screen load; the review panel fetches outputs on demand.
- **A generic `update_claim(claim_id, state)` command.** Rejected: it would let the frontend assert `human_confirmed` without a reviewer, defeating the §9.6A invariant that only an explicit human action can confirm a claim.
- **Regenerate as a background cron job triggered by staleness.** Rejected: §13.2 requires explicit user action for any billable operation; automatic regeneration on every transcript edit would silently consume quota.
- **Computing `seek_ms` in the frontend from segment times.** Rejected: §8.6A forbids inventing timestamps when `alignment_origin: none`; the backend produces `seek_ms` only when the revision permits it, and the frontend shows no seek affordance when the field is absent.

## Consequences

- The versioned schemas under `schemas/commands/v1/` and the generated command table in `crates/app/src/command.rs` enumerate the same 19 commands; drift fails CI. (The TypeScript contract and the `#[tauri::command]` allowlist that used to be the other two copies of this list were deleted at the EPIC O cutover, ADR-0024.)
- A claim can reach `human_confirmed` **only** through `review_claim`; the database schema enforces that `state = 'human_confirmed'` requires non-null `reviewer` and `reviewed_at`, so a DTO that omits those fields cannot persist a confirmed claim.
- The frontend never computes a seek position: when `seek_ms` is absent, no seek button is rendered and no time is invented. This preserves the §8.6A guarantee that a revision with no alignment never exposes a fabricated timeline.
- Sentinel values (`"Unassigned"`, `"Not specified"`) are rendered in Rust and appear in the wire DTO, so the frontend cannot mistake a missing field for "none required" and the UI always shows the explicit sentinel.
- The review panel is keyboard-navigable and every control has an accessible name (§5.3 accessibility requirement); status is conveyed by text badges, not color alone.
