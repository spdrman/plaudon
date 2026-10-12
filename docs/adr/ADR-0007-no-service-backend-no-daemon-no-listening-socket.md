# ADR-0007: No service backend, no daemon, no listening socket

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §1.3, §6.3(12), §7.10

## Context

Two of the reviewed projects require a server, a database container or a Python watcher daemon. Each is an operational burden for a single-user desktop product and an attack surface on the user's own machine.

## Decision

The application is a desktop binary plus a local SQLite file and a managed media archive. There is no HTTP listener, no Docker requirement, no SMTP, no webhook receiver and no background daemon separate from the application process.

## Rejected alternatives

- **A local HTTP API for automation.** spec §1.3 explicitly excludes a public localhost HTTP API; automation is a narrow local command line instead.
- **A bundled Postgres or similar.** an enormous dependency for single-user, single-writer workloads that SQLite handles.
- **A helper daemon for folder watching.** the watcher runs in the application process; a daemon that outlives the application is a surprise to the user.

## Consequences

- Backup is a file-level operation the user can understand and verify.
- Everything is testable without a service fixture.
- Features that would need a server, such as team synchronization, are out of scope rather than half-built.
