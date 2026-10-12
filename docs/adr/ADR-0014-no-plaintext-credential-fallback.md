# ADR-0014: No plaintext credential fallback — an unavailable secret store is a locked state

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** SPEC v1.3 (see "Spec references")

## Status

Accepted — 2026-10-11. Implemented in `crates/security/src/secrets.rs`
(locked-state behaviour and device-scoped entries verified; zeroization of
transient buffers and the crash-trace scan remain as findings F-036 and
F-001).

## Context

Two credential families exist (SPEC §16.4, App. B.5): the OpenRouter API key,
and device authorization material — the static per-device BLE token of the
legacy profile and, where a device uses it, mode-B key material. SPEC §16.4
requires the OS credential store for both, prohibits universal tokens shared
across devices, and states that "an unavailable secure store yields an
actionable locked state, not a silent downgrade".

The temptation this ADR exists to foreclose is familiar: the keychain prompt is
denied, or the CI machine has no login keychain, or Windows Credential Manager
returns an error — and the quickest way to keep the feature working is to fall
back to a file in the application support directory, "just for this case".
That fallback is permanent in practice: it is written once and read forever,
and it converts a Critical asset (adversary A1 can read any file the user can
read) from keychain-protected to plaintext-on-disk, invisibly.

An expiring, revocable session token might arguably tolerate degraded storage.
Neither of ours is: the device token is static and long-lived, and the
OpenRouter key is a billable bearer credential.

## Decision

1. The only storage backend for either credential family is the platform
   store: macOS Keychain, Windows Credential Manager, or a platform-backed
   equivalent. There is no file backend, no encrypted-file backend, no
   environment-variable backend and no SQLite column — not in production, and
   not behind a configuration flag.
2. When the store is unavailable, locked, or denies access, the operation
   returns an actionable error (`SECRET_STORE_LOCKED` / `SECRET_STORE_FAILED`,
   already reserved in `crates/security/src/error.rs`) carrying a user action.
   Any job that needed the secret moves to a persisted wait state (§6.3A), not
   to a failed-but-looks-finished state and never to a silent downgrade.
3. Credentials are device-scoped: one vault entry per device identity. A
   universal or shared token is not representable in the API.
4. SQLite and portable backups hold an opaque reference and a non-reversible
   fingerprint only. A backup that contains a credential is a release failure
   (§22.9).
5. Secrets never reach a command line, a subprocess environment, a log line,
   a crash trace or a notification payload. The subprocess boundary already
   scrubs the environment it passes to the CLI.
6. Transient copies in process memory live in zeroizing buffers, and the
   documentation states plainly that this reduces a window rather than
   defeating privileged malware (adversary A2).
7. Tests use an in-memory fake store through the same trait. The fake is a
   test double, never a production fallback, and it is not reachable from a
   release build's wiring.

## Consequences

* Some environments cannot hold credentials at all (a headless CI runner, a
  locked keychain). Those environments simply cannot use the BYOK or
  direct-device features; they can still import files and run local STT. That
  is the intended trade.
* The locked state becomes a first-class UX surface: the UI must present
  "unlock your keychain and retry", which is exactly what the error action is
  for, rather than an unexplained failure.
* The threat model can state without qualification that no credential is at
  rest in our archive, our database or our backups.
* Loss of the OS store (a wiped keychain) means re-entering the key and
  re-authorizing devices. We accept that over holding a recovery copy
  ourselves, because a recovery copy is a plaintext fallback with a nicer name.

## Rejected alternatives

- **Encrypted file fallback with a key derived from a user passphrase.**
  Rejected for V1: it adds a second credential to manage and a second recovery
  path, and users would meet it at the exact moment they are least prepared to
  make a security decision. Revisit only with the §13.8 vault work.
- **Machine-bound encrypted file (DPAPI / `kSecAttrAccessible` file blob).**
  Rejected: on both platforms the honest version of this *is* the platform
  store; a hand-rolled variant adds risk and no capability.
- **Environment variables for CI.** Rejected: §16.4 forbids them explicitly,
  and any test that needs a secret can use the in-memory fake.
- **One shared token for all devices of a user.** Rejected: prohibited by
  §16.4 and it would make the loss of one device's token a compromise of all.

## Spec references

SPEC v1.3 §16.4, §16.2 ("Credential disclosure"), §13.7, §22.9, §22.16,
§6.3A (persisted wait state on keychain lock), App. B.5 and B.7, ADR-003 of
§21.

Matrix rows: TM-001, TM-033, TM-034, TM-035, TM-036 in
[`../threat-model/traceability.json`](../threat-model/traceability.json);
findings F-001, F-035 and F-036 in
[`../threat-model/open-findings.md`](../threat-model/open-findings.md).
