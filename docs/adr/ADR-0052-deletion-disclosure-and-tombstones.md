# ADR-0052: Deletion is a journalled operation with a confirmed disclosure

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §4.6, §7.10, §13.5, §18.6A, §22(11)

## Context

Deletion is the one destructive operation we perform on user data. Spec §4.6
requires showing exactly what will be deleted, stating that exports outside our
folder and provider-side copies are unaffected, and refusing to claim forensic
erasure. Spec §7.10 requires a durable tombstone so a later sync cannot
resurrect a recording, and §18.6A requires that a crash never leaves a
half-deleted recording looking intact.

## Decision

Deletion runs as the inverse of the import journal, in four public steps
(`plan`, `remove_rows_for`, `unlink_blobs`, `finish`):

1. `disclose` reads the exact categories, media and byte counts and returns a
   disclosure whose SHA-256 is the confirmation token.
2. `plan` refuses unless the confirmation carries that exact token, then
   commits a `deletion_records` row plus the blob worklist.
3. One transaction writes the tombstone and the content-identity and
   source-identity suppressions, then removes every dependent row and the
   recording itself.
4. Blobs no other recording references are unlinked, then the journal is
   completed. Startup reconciliation finishes any deletion that stopped early;
   every step is idempotent.

Suppressions are keyed by `(sha256, byte_length)` and by
`(source_id, external_id)` — never by filename — and only an explicit
`allow_reimport` clears them.

## Rejected alternatives

- **A single `DELETE` statement plus file unlinks.** SQLite cannot commit a
  file unlink; a crash in the middle would leave a recording that looks intact
  but has no audio.
- **A boolean `deleted` flag only.** the user asked for the data to go; a flag
  keeps transcripts, anchors and audio on disk.
- **Confirming with a yes/no dialog.** the library can change between the
  disclosure and the click; the digest makes a stale disclosure fail closed.
- **Deleting shared media bytes.** another logical meeting references them, and
  content-addressed storage means deleting them would damage that recording.
- **Claiming secure erasure.** on SSDs, snapshotted filesystems and backed-up
  volumes we cannot support that claim, so the disclosure says so.

## Consequences

- A deletion is auditable: the disclosure the user confirmed is stored verbatim.
- A later import or sync that offers the same bytes or the same external id is
  refused until the user explicitly reimports.
- A tombstone and its suppressions survive restart and migration, and a
  restored backup does not resurrect a deleted recording unless the user asks.
