# ADR-0008: Content-addressed archive with a durable import journal

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §6.3B, §13.3A, §13.4

## Context

A SQLite transaction cannot atomically commit a filesystem rename. Any design that pretends otherwise produces half-registered assets after a crash: a database row with no file, or a file with no row.

## Decision

Media lands at `media/sha256/<ab>/<hash>.<ext>`, derived from content identity alone. Import follows the six durable steps of §13.3A — stage, hash, flush, record intent, rename within the same filesystem, transact and close the journal — and a startup reconciler settles every incomplete intent against the bytes actually on disk. A hash mismatch quarantines; a cross-volume staging path is refused rather than copied.

## Rejected alternatives

- **Claiming atomicity across SQLite and the filesystem.** it is not true, and the resulting bug only appears after a crash in the field.
- **Writing the file after the database row.** produces committed records pointing at nothing, which is the worse direction to fail.
- **Naming stored files after the user's filename.** a hostile or merely odd filename then decides where bytes land.

## Consequences

- Every `Completed` import references an existing verified asset, asserted by a crash-injection suite at each step.
- Only application-owned staging files are ever removed, and never after a grace period for a live intent.
- Original filenames survive as metadata only.
