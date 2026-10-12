# ADR-0012: A portable backup is not an export

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §13.5, §13.6, §13.8

## Context

A text export is useful for reading elsewhere. It is not a backup: it cannot restore a library, and treating it as one is how people discover they have lost their recordings.

## Decision

Export produces human-readable artifacts: TXT, Markdown, JSON, and captions only where real timestamps exist. Backup produces a versioned `.recording-archive` with a manifest, original media, transcript revisions, generated outputs and per-entry digests plus an overall integrity digest, and it has a tested restore procedure against a clean installation. Secrets, tokens, signed URLs and permission handles are excluded from the archive.

## Rejected alternatives

- **Calling the JSON export a backup.** it has no media, no manifest and no verified restore path.
- **A database file copy as the backup format.** it is version-locked to one schema and silently drops the media archive.
- **Signing the manifest and calling it authenticated.** an integrity digest is not authenticity; claiming otherwise would be a security claim we cannot support.

## Consequences

- Restore is tested across operating systems, with media digests and structured records compared against the manifest.
- A restored library carries no credentials, so the user re-authorizes deliberately.
- Deleting local content cannot be claimed to remove it from user-created backups or prior cloud submissions.
