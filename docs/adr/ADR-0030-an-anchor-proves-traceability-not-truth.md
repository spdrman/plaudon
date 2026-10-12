# ADR-0030: An anchor proves traceability, not truth

- **Status:** Accepted
- **Spec:** PLAUD BYOK Desktop SPEC v1.3 (Consensus) — §9.1, §9.5, §9.6, §9.6A, §17.3
- **Date:** 2026-10-11
- **EPIC:** I — Meeting intelligence & source grounding

## Context

A summarization pipeline that returns citations invites a comfortable mistake:
treating a resolving citation as evidence that the claim is correct. Spec §9.6A
states the opposite explicitly — "a segment link establishes that the model
referenced existing text, **not** that the text entails the claim or the
meeting participant actually agreed to it".

We also have to survive hostile input. Spec §9.6 requires that transcript
content be treated as untrusted even when it contains instructions, and §18.6A
fixes the observable outcome for the AI cluster: a prompt injection, a
falsified owner or date, or a fabricated anchor must "fail grounded validation
or label the claim unverified".

## Decision

Grounding is modelled as three separate questions, each answered by a different
mechanism, and none of them by the model's own assertion.

1. **Does the citation resolve?** `plaudon_domain::anchor::SourceAnchor::resolve`
   compares the quoted excerpt against the exact character span of the exact
   immutable revision the run consumed. It performs no I/O and no model call,
   so a stored verification result is reproducible offline. A fabricated
   revision, segment, span or quote is a typed `AnchorRejection`, recorded
   rather than discarded.
2. **Does the cited text support the claim?** A conservative lexical
   spot-check (`spot_check_entailment`) asserts that the claim's distinguishing
   tokens — numbers, dates, proper nouns — actually occur in the cited span.
   It is named a spot-check because that is what it is: it catches a falsified
   owner, amount or date, and it does not pretend to be natural-language
   inference.
3. **Has a human agreed?** `ClaimState::HumanConfirmed` is reachable only
   through `Claim::review`, which requires a `HumanConfirmation` carrying a
   reviewer and an instant. A model response cannot construct one: the model's
   schema is `ClaimDraft`, which has no state field at all, and the database
   `CHECK` on `output_claims` rejects `human_confirmed` without a reviewer.

High-consequence categories (financial amounts, obligations, deadlines, safety
commitments) are classified from the item's structure and wording and default
to **Review required**; they never satisfy `permits_automatic_dispatch`.

Prompt-injection defense is structural rather than filter-based: instructions
and transcript never share a region, the fence markers are neutralized inside
the data, the application exposes no tool, URL, file or command surface to the
model at all, and `scan_response` refuses a response that tries anyway.
`detect_directives` exists only so an attempt is *counted and shown*; the
output is safe whether or not it fires, which is why the fixture corpus
includes benign controls that must not fire.

## Consequences

- A "verified decisions" view can only ever contain human-confirmed claims.
  `SourceLinked` is a traceability badge, and the UI says so.
- Unsupported-claim rate and anchor correctness are computed from our own
  verification, so §17.3 metrics are independent of the model under test.
- Every anchor rejection is persisted with its reason code, which makes
  grounding failure visible in the interface instead of a shorter list.
- The entailment spot-check will produce false negatives on heavy paraphrase.
  That is the conservative direction: the claim is marked for review, not
  silently accepted.

## Rejected alternatives

- **Trust the model's own `verified` flag.** Rejected: §9.6 forbids relying on
  a single LLM assertion to prove citation accuracy, and a model that invents a
  quote will equally happily invent a confidence score.
- **Ask a second model to judge entailment.** Rejected for V1: it doubles cost
  and cloud exposure for an unmeasured benefit, and it replaces one
  unverifiable assertion with another. A deterministic lexical check plus human
  review is honest about what it does.
- **Store anchors as foreign keys into `transcript_segments`.** Rejected: a
  fabricated citation names a segment that does not exist, and a foreign key
  would make the most important case unrepresentable rather than recorded.
- **Filter injected instructions out of the transcript before prompting.**
  Rejected: it corrupts the user's recording, it cannot be complete, and it
  would make the summary of a meeting *about* prompt injection wrong.
