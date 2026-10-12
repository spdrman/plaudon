# ADR-0013: A single Rust-owned egress gate, enforced by an unforgeable ticket

## Status

Accepted — 2026-10-11. Implemented in `crates/security/src/outbound.rs`
(+ `outbound/http.rs`, `outbound/subprocess.rs`).

## Context

SPEC §6.3 invariant 5 requires every cloud request to pass a central policy
check **at dispatch time**, and invariant 15 requires "Local-only" to be
enforced at the Rust network policy boundary rather than as a UI badge.
§16.2A names the subsystems that are network-capable: the PLAUD CLI
integration, the OpenRouter HTTP client, the model manager, the update checker,
telemetry, and any future local helper. §22.15 turns this into a release gate:
Local-only Mode must demonstrably make no app-initiated PLAUD, OpenRouter,
telemetry, update-check or model-download request, and must never launch the
PLAUD Cloud CLI.

The failure mode we are designing against is not a clever attacker; it is
ordinary engineering entropy (adversary A9 in
[`../threat-model/assets-and-adversaries.md`](../threat-model/assets-and-adversaries.md)):
one subsystem that forgets the check, one retry path added later, one helper
that builds its own client. A policy expressed as "call `check_allowed()`
before you send" is a convention, and conventions are not verifiable in CI.

The cloud CLI makes this sharper. It is a separate executable with its own
network stack, so no in-process check can constrain what it does once it runs.
The only enforceable control over it is whether it runs at all.

## Decision

1. One type, `OutboundPolicy`, owns the decision for every destination class.
   Its default is `NetworkMode::LocalOnly`; a permissive mode exists only with
   an explicit `NetworkGrant` recording the approval source, approval time and
   disclosure version.
2. Authorization yields an `EgressTicket` whose constructor is private to the
   module and which is deliberately not `Clone`. The HTTP transport and the
   process launcher both **require** a ticket, so the gate cannot be bypassed
   by forgetting to call it — there is nothing to forget, because without a
   ticket there is no way to express "send".
3. A subprocess that can itself reach the network is a destination class of the
   gate, not an exception to it. In Local-only Mode the PLAUD CLI is never
   launched.
4. Purposes are distinct and separately approvable: a network grant does not
   authorize a model download or a telemetry upload. Each requires its own
   recorded user decision.
5. Every decision — allow and deny — is written to an `EgressObserver` before
   it is returned, so there is no such thing as an unlogged attempt, and the
   §24 evidence bundle can contain a real egress trace.
6. Destination labels are reduced to `scheme://host:port` at construction.
   Paths, query strings and credentials never enter a log line or an error
   envelope.
7. The gate publishes `NOT_GUARANTEED`, the list of things it does not control,
   and that list is part of the policy snapshot returned to the UI. Honesty is
   a data structure, not a doc comment.

## Consequences

* A new network-capable subsystem cannot ship without touching this module:
  it needs a `Subsystem` variant and a purpose, which is a reviewable diff.
* Local-only Mode is testable as a pure function over a cross product of
  (mode × destination × purpose × payload), which is how
  `local_only_denies_every_request_in_the_cross_product` and
  `decision_table_per_destination_class_and_mode` verify it.
* The UI can explain *why* a capability is unavailable, because
  `EffectiveNetworkPolicy` carries a reason per disabled capability instead of
  a boolean.
* The gate constrains our own subsystems. A compromised dependency can still
  call the OS socket API directly (adversary A3), so the gate is complemented
  by an OS-level egress monitor fixture that observes the whole process.
* Redirect handling is a transport concern: each hop must be re-authorized
  through the gate. Until the real HTTP transport exists, that is open finding
  F-011.

## Alternatives considered

* **Per-subsystem policy checks.** Rejected: unverifiable, and the first
  subsystem to forget silently breaks a release gate.
* **A boolean `local_only` flag read at call sites.** Rejected: it makes the
  deny path a convention and cannot distinguish purposes (download vs
  telemetry vs inference) or record a reason.
* **An OS firewall rule or a network namespace.** Rejected as the primary
  control: it is not portable to macOS and Windows user installs, it cannot
  distinguish our purposes, and it would let us claim an assurance level we
  cannot deliver on an ordinary desktop. Retained as a *test* fixture only.
* **Letting the cloud CLI run but blocking its traffic.** Rejected: we cannot
  constrain another process's stack, and §16.2A requires non-invocation.
* **A proxy process that all traffic must route through.** Rejected for V1: it
  adds a privileged long-running component, which §6.2 explicitly avoids, for
  assurance we can largely obtain from the ticket design plus the monitor
  fixture.

## Spec references

SPEC v1.3 §6.2, §6.3 invariants 5 and 15, §6.3A, §16.2A, §16.6, §22.15,
§1.5 ("Direct-device transfer privacy", "Silent cloud upload"), §24 release
evidence bundle, ADR-003 and ADR-015 of §21.

Matrix rows: TM-007, TM-011, TM-014, TM-024, TM-026, TM-027 in
[`../threat-model/traceability.json`](../threat-model/traceability.json).
