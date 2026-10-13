# ADR-0082: A voice match is a suggestion, never an identity

- **Status:** Proposed
- **Date:** 2026-10-12
- **Spec:** VOICE_PROFILES_SPEC §6 (VP-060–067), §16 (VP-170–173); base spec §8.7, §17.6, §16.6; sibling of ADR-0041

## Context

Open-set speaker recognition has irreducible error, and the error is not uniform: the FAccT '22
study of VoxCeleb-trained systems (the training data behind every bundling candidate) measured
substantially worse error for female and non-US speakers, with some subgroups' false-positive rate
at 13× the average operating point, and found smaller models — what a CPU desktop app ships — can
widen the gap. Error also grows 1.4–3.9× on the short utterances that dominate meetings, and
thresholds are checkpoint-specific with inconsistent polarity conventions across ecosystems.
Meanwhile ADR-0041 already established the house rule for a weaker claim-class: "an anchor proves
traceability, not truth". A voice match is a weaker claim still.

## Decision

1. **No auto-assignment at any score.** Matching produces persisted *suggestions* in two bands
   (suggest / weak); a transcript label changes only on explicit user confirmation. There is no
   high-confidence tier that bypasses review, deliberately: with demographically skewed error, an
   auto-assignment threshold is a policy about whose records get silently corrupted.
2. **Open-set first.** "Matches nobody enrolled" is the expected common case; below-floor scores
   record nothing, and the UI never offers "closest match" labelling of a below-threshold score.
3. **Two thresholds, harness-derived, false-accept-biased.** Operating points come from the
   committed evaluation harness (target ≤1% false-accept at the suggest threshold), not from this
   document or any model card; score polarity is a newtype; thresholds are versioned with the
   checkpoint and never survive a model swap.
4. **Honest wording, enforced by tests.** UI copy says "sounds like" / "suggestion" / "match
   score"; never "identified", "verified", "recognised as", or "is". The speaker display qualifier
   lives in one domain function (`plaudon_domain::speaker::display`), extended with a
   "confirmed by you" provenance form. Scores are shown with a band name and a tooltip that a
   score is not a probability of identity. No single accuracy number is advertised anywhere.
5. **Per-profile calibration only.** The system adapts thresholds per profile from the user's own
   confirm/reject decisions, within harness-validated bounds. It never collects demographic
   attributes to "correct" bias, and attribute inference from voice is prohibited product-wide.

## Rejected alternatives

- **Auto-assign above a high threshold ("it's obviously them").** Rejected: converts measured
  demographic disparity into silently mislabelled records for exactly the users the training data
  under-represents; also forfeits the legal distinction between a suggestion tool and an
  identification system that §12 of the spec depends on.
- **Presenting calibrated percentages as identity confidence.** Rejected: cosine-similarity
  scores, even AS-normalised, are not calibrated probabilities of identity across devices, rooms
  and demographics; a percentage invites exactly the misreading the wording rules exist to
  prevent. Bands plus the raw score are shown instead.
- **A global user-tunable sensitivity slider.** Rejected for V1: a single global knob is a
  known-weak design (per-file tuning admissions in upstream docs); per-profile adaptation from
  confirmations achieves the same end without inviting users to crank false-accepts globally.

## Consequences

- The feature's ceiling is deliberately lower than "magic": every correct match still costs one
  click. Accepted — the click is the control that keeps wrong matches out of records.
- Wording and no-auto-assignment are load-bearing for the legal posture (suggestion tool vs
  biometric identification deployment) and for user trust; kittest string assertions make
  regressions test failures, and changing this behaviour requires superseding this ADR.
- Per-profile calibration adds state and tests, and its fairness benefit is plausible but
  unmeasured for this stack — the harness must report it (unverified-claims register item 11).
