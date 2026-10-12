# ADR-0005: A minimal Apple bridge, not a Swift application

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §2.3, §11.1, §11.2, §11.6

## Context

Apple's speech and language models are only reachable from Swift, but rewriting the application in Swift would abandon Windows and duplicate the domain logic.

## Decision

The Swift surface is exactly four operations plus cancellation and progress — `probeSpeech`, `transcribeFile`, `probeFoundationModel`, `generateText` — behind a C ABI whose ownership, string encoding, callback thread, cancellation and error serialization are specified in the header. Availability is always a runtime probe. The bridge is macOS-only and a Windows build links no Apple framework.

## Rejected alternatives

- **A full SwiftUI application on macOS.** two products, two sets of bugs, and the Windows build would fall behind immediately.
- **Inferring availability from the macOS version.** spec §11.5 requires a runtime probe; Apple Intelligence can be unavailable on a supported OS version.
- **A richer bridge that also owns chunking and persistence.** duplicates core logic across the boundary, which is the thing most likely to drift.

## Consequences

- Every Apple capability has an explicit unavailable path with a typed reason.
- The bridge is fuzzable and stress-testable in isolation before it ships.
- A guardrail refusal is an error document, never generated text.
