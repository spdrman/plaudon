# Architecture decision records

Numbers **0001-0012** are reserved by spec §21 and their meanings are fixed
there; do not reuse them. Later ranges are allocated per work stream so two
authors never mint the same number: 0013-0019 security and privacy, 0020-0029
desktop shell, 0030-0039 job engine, 0040-0049 AI adapters, 0050-0059 sources and ingestion,
0060-0069 direct-device import, 0070-0079 export, backup and deletion, 0080+
unallocated — claim a range in this file before using it.

Every record states context, the decision, the alternatives that were rejected
and why, and the consequences we accept.

## Spec §21 initial set

- [ADR-0001: Rust core with a Tauri 2 shell](ADR-0001-rust-core-with-a-tauri-2-shell.md) — Accepted, 2026-10-11 (spec §1, §6.1, §6.3(1), §2.4)
- [ADR-0002: OpenRouter is the only cloud AI gateway in V1](ADR-0002-openrouter-is-the-only-cloud-ai-gateway-in-v1.md) — Accepted, 2026-10-11 (spec §1.2(3), §2.2, §10, §8.6)
- [ADR-0003: Local-first processing with explicit, scoped cloud consent](ADR-0003-local-first-processing-with-explicit-scoped-cloud-consent.md) — Accepted, 2026-10-11 (spec §1.1, §3, §4.4, §16.3, §10.8)
- [ADR-0004: PLAUD adapters are gated on evidence, not on vendor names](ADR-0004-plaud-adapters-are-gated-on-evidence-not-on-vendor-names.md) — Accepted, 2026-10-11 (spec §2.5A, §2.5B, §7.9, App. B.2)
- [ADR-0005: A minimal Apple bridge, not a Swift application](ADR-0005-a-minimal-apple-bridge-not-a-swift-application.md) — Accepted, 2026-10-11 (spec §2.3, §11.1, §11.2, §11.6)
- [ADR-0006: Immutable originals, versioned derived outputs](ADR-0006-immutable-originals-versioned-derived-outputs.md) — Accepted, 2026-10-11 (spec §6.3B, §8.5, §8.6A, §13.4)
- [ADR-0007: No service backend, no daemon, no listening socket](ADR-0007-no-service-backend-no-daemon-no-listening-socket.md) — Accepted, 2026-10-11 (spec §1.3, §6.3(12), §7.10)
- [ADR-0008: Content-addressed archive with a durable import journal](ADR-0008-content-addressed-archive-with-a-durable-import-journal.md) — Accepted, 2026-10-11 (spec §6.3B, §13.3A, §13.4)
- [ADR-0009: We adopt designs, not code, from AGPL and license-absent sources](ADR-0009-designs-not-code-from-agpl-and-unlicensed-sources.md) — Accepted, 2026-10-11 (spec §2.5, §2.5A, App. B.11)
- [ADR-0010: An import never deletes or modifies the source](ADR-0010-an-import-never-deletes-or-modifies-the-source.md) — Accepted, 2026-10-11 (spec §7.10, §7.8, App. A.4)
- [ADR-0011: Native background work without a web service](ADR-0011-native-background-work-without-a-web-service.md) — Accepted, 2026-10-11 (spec §14.5, §15.3, §1.2(12))
- [ADR-0012: A portable backup is not an export](ADR-0012-a-portable-backup-is-not-an-export.md) — Accepted, 2026-10-11 (spec §13.5, §13.6, §13.8)

## Security and privacy (0013-0019)

- [ADR-0013: A single Rust-owned egress gate, enforced by an unforgeable ticket](ADR-0013-single-egress-gate.md) — Accepted, 2026-10-11 (spec §6.3 inv. 5/15, §16.2A)
- [ADR-0014: No plaintext credential fallback — an unavailable secret store is a locked state](ADR-0014-no-plaintext-credential-fallback.md) — Accepted, 2026-10-11 (spec §16.4, §22.16)
- [ADR-0015: Untrusted media decoding and model inference run out of process](ADR-0015-out-of-process-untrusted-input.md) — Accepted, 2026-10-11 (spec §6.3A, §16.2 "Malicious audio decoder input")

## Desktop shell (0020-0029)

- [ADR-0020: Desktop command surface and the two additions to the §15.2 table](ADR-0020-command-surface.md) — Accepted, 2026-10-11 (spec §15.2)
- [ADR-0021: Native notification backends and privacy-safe payloads](ADR-0021-notifications.md) — Accepted, 2026-10-11 (spec §5.5, §16.6)

## Job engine (0030-0039)

- [ADR-0030: Job engine durability, idempotency and crash recovery](0030-job-engine-durability.md) — Accepted, 2026-10-11 (spec §14)

## Export, backup and deletion (0070-0079)

- [ADR-0070: An explicitly versioned `.recording-archive` container, not a ZIP](ADR-0070-recording-archive-container.md) — Accepted, 2026-10-11 (spec §13.6, §13.8, §16.5)
- [ADR-0071: Argon2id plus STREAM XChaCha20-Poly1305 for encrypted backups](ADR-0071-argon2id-stream-aead-backups.md) — Accepted, 2026-10-11 (spec §13.7, §13.8)
- [ADR-0072: Deletion is a journalled operation with a confirmed disclosure](ADR-0072-deletion-disclosure-and-tombstones.md) — Accepted, 2026-10-11 (spec §4.6, §7.10, §13.5)
