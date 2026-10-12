# ADR-0010: An import never deletes or modifies the source

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §7.10, §7.8, App. A.4

## Context

One reviewed watcher deletes the original after processing. A user who points us at their only copy of a recording must not lose it because our pipeline decided it was done.

## Decision

Import copies. No code path in the application deletes, moves, truncates or rewrites a user-selected file, a watched-folder file or a file on a mounted device volume. Device volumes are opened read-only, and the USB scan has no write function at all. Reconciliation removes only application-owned staging files, and only after a grace period.

## Rejected alternatives

- **Deleting the source after a verified import, behind a setting.** a setting does not make an irreversible action safe, and the failure is silent and total.
- **Moving the source into an 'imported' folder.** still mutates a directory the user owns, and breaks anything else that references the file.
- **Trusting the operating system's read-only mount.** the host may still mount read-write; we assert our own behaviour instead of the platform's.

## Consequences

- A failed import costs the user nothing.
- We cannot claim 'zero physical writes' where the host mounts a volume read-write, and the register records that honestly.
- Deduplication is about our archive, never about the user's own files.
