# ADR-0011: Native background work without a web service

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §14.5, §15.3, §1.2(12)

## Context

Transcription is long-running. The user must be able to close the window, sleep the machine and come back to a correct result, without us running a server to achieve it.

## Decision

Jobs are persisted in SQLite with an explicit state machine and checkpoints. Progress reaches the interface as events with sequence numbers, and the interface refetches authoritative state after a gap, so event delivery is never treated as durable storage. Sleep, logout, keychain lock and radio interruption transition a job into a persisted wait state rather than a failure.

## Rejected alternatives

- **Keeping job state in memory.** a crash or a sleep loses the queue, and spec §1.5 requires 100% recovery from simulated crash points.
- **A background service that outlives the application.** contradicts ADR-0007 and surprises the user.
- **Treating frontend events as the source of truth.** a dropped event then silently corrupts the displayed state.

## Consequences

- Every durable transition needs positive, negative and crash-recovery tests.
- The interface can be restarted at any moment without affecting a running job.
- A possibly-billed cloud request is never retried automatically.
