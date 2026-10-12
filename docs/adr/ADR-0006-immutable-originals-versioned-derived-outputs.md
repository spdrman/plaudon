# ADR-0006: Immutable originals, versioned derived outputs

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §6.3B, §8.5, §8.6A, §13.4

## Context

A user must be able to trust that the recording they imported is the recording they still have, and that correcting a transcript does not destroy the machine output it came from.

## Decision

A committed media asset is immutable and content addressed by `sha256` plus byte length. Transcript revisions are immutable; a human edit, a derived alignment pass and an overlap reconciliation each produce a child revision that references an untouched parent. Derived outputs record the exact input revision, so a stale summary is detectable rather than merely suspected.

## Rejected alternatives

- **Editing a transcript in place with an undo buffer.** the original machine output is gone after a restart, and provenance claims become unverifiable.
- **Storing only the latest revision plus a diff.** reconstructing an old revision then depends on diff correctness, which is a worse failure mode than storing the text.
- **Re-encoding imported audio to a canonical format on import.** destroys the original and makes a digest meaningless as identity.

## Consequences

- Storage grows with revisions; the archive is content addressed so identical bytes are stored once.
- A summary built on a superseded revision is labelled stale by construction.
- Reprocessing appends a sibling revision and can never overwrite a human edit.
