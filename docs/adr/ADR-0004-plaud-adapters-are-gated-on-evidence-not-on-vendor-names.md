# ADR-0004: PLAUD adapters are gated on evidence, not on vendor names

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §2.5A, §2.5B, §7.9, App. B.2

## Context

Upstream projects demonstrate a legacy BLE protocol on Linux and an undocumented web API, neither of which proves that our targets work. A product claim derived from a repository README is a claim we cannot support.

## Decision

Every device, firmware, host OS, transport and authentication combination is a row in the committed `compatibility.yaml`. The interface generates its compatibility strings from that register and from nothing else. A tuple that matches no row fails closed, and a row cannot claim `supported` without an archived `test_run_id`, a `last_verified_at`, a firmware range, a host OS and a recorded verified import.

## Rejected alternatives

- **Advertising broad PLAUD support and handling failures as bug reports.** spec §1.5 sets zero combinations advertised as Supported without reproducible validation.
- **Matching the closest known profile when a tuple is unknown.** a resemblance between firmware strings is not evidence; §2.5B requires failing closed.
- **Treating the upstream Linux result as cross-platform proof.** a cross-platform BLE crate proves central discovery exists, not that the PLAUD protocol succeeds.

## Consequences

- Direct-device support can stay unavailable while the core product ships.
- Promoting a cell is a documented procedure with an archived evidence record, not a code change.
- A firmware change demotes the affected cell rather than silently keeping a stale claim.
