# ADR-0009: We adopt designs, not code, from AGPL and license-absent sources

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §2.5, §2.5A, App. B.11

## Context

Riffado is AGPL-3.0, `landoncrabtree/applaud` displayed no license, and the official PLAUD npm package listing declares none. `plaude-cli` is MIT but its protocol evidence comes from reverse engineering. Copying from any of these without a decision would be a licensing and provenance problem.

## Decision

We read those projects for behaviour and failure modes and write our own implementation. No AGPL or license-absent code is copied, adapted or vendored. MIT-licensed protocol constants that are facts about a device — service and characteristic identifiers, frame layout, opcode values — are used with attribution recorded in `docs/licensing/third-party-register.md`. We neither bundle nor redistribute the official PLAUD CLI.

## Rejected alternatives

- **Vendoring the AGPL implementation.** would oblige us to release the whole application under AGPL, which contradicts the distribution model.
- **Copying from the license-absent project on the assumption it is permissive.** absence of a license means no permission, not implied permission.
- **Avoiding those projects entirely.** discards genuinely useful negative findings, such as the watcher patterns spec §2.5 tells us to avoid.

## Consequences

- Our implementations are written from documented behaviour and are ours to license.
- Every third-party component has a recorded license, redistribution decision and attribution plan, enforced by `scripts/license-audit.sh`.
- An unreviewed dependency fails the build rather than shipping silently.
