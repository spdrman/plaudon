# ADR-0003: Local-first processing with explicit, scoped cloud consent

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §1.1, §3, §4.4, §16.3, §10.8

## Context

Meeting audio is some of the most sensitive material a user owns. The product must be useful with no account and no network, and any transmission must be a decision the user made knowingly.

## Decision

Offline file import and local transcription work with no account, no key and no network. Any cloud dispatch requires a consent grant scoped to engine, model/provider and privacy policy version, checked immediately before dispatch. Local-only Mode refuses every application-initiated network operation, including model downloads and update checks, and says so rather than failing silently.

## Rejected alternatives

- **Cloud-first with an offline mode bolted on.** the offline path then only works by accident; spec §1.6 makes offline file-to-transcript an unconditional V1 pathway.
- **A single global 'allow cloud' switch.** cannot express 'summarize with this model but never send audio', which is the common request.
- **Implicit consent through a settings default.** spec §16.3 requires explicit scope and revocation.

## Consequences

- Every cloud feature carries a consent check before dispatch, and a revoked grant stops the queue.
- Local-only Mode is verifiable by a network-deny test rather than by assertion.
- We cannot claim to control unrelated operating-system traffic, and we say so.
