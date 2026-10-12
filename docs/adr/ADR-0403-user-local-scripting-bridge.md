# ADR-0403: User-local scripting bridge

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §14.5, App. A.5, App. A.6, §1.3

## Context

Spec §14.5 requires "an optional, explicitly enabled **local scripting bridge**" exposing commands such as `list`, `export`, `process`, and `jobs status` that call the same Rust services the GUI uses. Spec §1.3 explicitly excludes "a public localhost HTTP API, internet-facing webhooks, or an unsupervised autonomous agent". Appendix A.6 rejects the Riffado-style token-authenticated HTTP endpoint architecture. Issue M2.1 records this as the CLI contract.

## Decision

We ship a signed, notarized `plaudon` CLI binary that invokes the same Rust application services (`plaudon_app_core`, `plaudon_jobs`, `plaudon_persistence`) the GUI uses. It opens no network port, runs no server, installs no daemon, and authenticates via OS-user ownership only.

### Enable switch

The bridge is **disabled by default**. Enabling requires:

1. Explicit user action: `plaudon bridge enable --confirm` (the `--confirm` flag prevents accidental enable).
2. The enable state is stored in `automation_bridge_state` table with the OS username and enable timestamp.
3. Every subsequent `plaudon` invocation checks that the current OS user matches the stored owner; if not, the command is refused with an `BRIDGE_DISABLED` or `BRIDGE_WRONG_USER` error.

The bridge can be disabled with `plaudon bridge disable` or via the application settings UI.

### OS-user ownership check

- **Unix (macOS, Linux):** `geteuid()` returns the effective user ID; `getpwuid()` gives the username. The bridge checks that this matches the username stored when the bridge was enabled.
- **Windows:** `GetUserNameW()` returns the current user's username; compared against the stored owner.

This is an **accident/second-account guard**, not authentication:

- It prevents a script run by user `alice` from accessing a library owned and enabled by user `bob`.
- It does NOT defend against `sudo`, privilege escalation, or a process running with the same OS user identity.
- It does NOT replace the application's own authorization: cloud `process` still requires consent granted in the app, and Local-only Mode still refuses cloud dispatch.

### Consent and Local-only enforcement

Every cost-bearing or privacy-sensitive command (`process --run cloud`, `process --stage transcribe --run cloud`, any OpenRouter dispatch) checks:

1. **Bridge enabled and owned by current user** (`crates/automation/src/bridge.rs`).
2. **Consent granted** for the selected model/provider/scope (`crates/automation/src/policy.rs`, reading the same `consent_grants` rows the GUI writes).
3. **Local-only Mode permits the operation** (`crates/automation/src/policy.rs`, reading `app_preferences` for the Local-only Mode flag).

A script cannot bypass consent or Local-only Mode. Revoking consent in the GUI immediately revokes it for CLI invocations.

### JSON envelope contract

Every command prints exactly one JSON document to stdout, with no progress bars, spinners, or log messages. The envelope is versioned:

```json
{
  "status": "ok",
  "contract_version": 1,
  "command": "list",
  "data": { /* command-specific payload */ }
}
```

Or, on failure:

```json
{
  "status": "error",
  "contract_version": 1,
  "command": "process",
  "code": "CONSENT_REQUIRED",
  "message": "Cloud processing for this model requires consent; grant it in Settings → AI Providers",
  "retryable": false
}
}
```

Exit codes:

- **0:** success.
- **2:** refusal or failure (consent denied, Local-only Mode refusal, bridge disabled, invalid argument). Exit code 2 (not 1), so a shell script can distinguish "the CLI refused" from "the CLI could not start at all".

Contract version 1 is the initial version. Future versions may add fields; removing fields or changing types increments the version.

### No secrets in output

The envelope contract forbids API keys, PLAUD session tokens, signed URLs, encryption keys, or transcript excerpts. Every payload is a typed, serializable Rust struct, so accidental secret leakage is prevented by construction. The `output.rs` module is the single place where CLI output is produced.

## Commands

- **`plaudon list`**: List recordings with filters, sort, pagination. Returns recording ID, title, date, duration, source, status.
- **`plaudon export --recording <UUID> --format json|text`**: Export a transcript. `--format json` returns segments with times, speakers, provenance; `--format text` returns plain text (one line per segment).
- **`plaudon process --recording <UUID> --stage transcribe|summarize|diarize --run local|cloud --model <MODEL>`**: Queue a processing job. Returns job ID and initial status.
- **`plaudon jobs status [--recording <UUID>] [--limit N]`**: List jobs with status, type, created/started/finished times, last error.
- **`plaudon bridge status`**: Report whether the bridge is enabled and who owns it.
- **`plaudon bridge enable --confirm`**: Enable the bridge for the current OS user.
- **`plaudon bridge disable`**: Disable the bridge.

Global flags:

- **`--library <PATH>`**: Override the default library path (default: application's configured library). Can also be set via `$PLAUDON_LIBRARY`.
- **`--pretty`**: Print indented JSON instead of one line.

See `docs/automation/plaudon-cli.md` for detailed usage.

## Why NOT an HTTP API

Appendix A.6 explicitly rejects "token-authenticated HTTP endpoints/webhooks" and "a public localhost HTTP API". Reasons:

1. **Unintended exposure.** A localhost HTTP server is visible to every process on the machine. Token auth adds complexity; no token is safe against a process with the same user privileges.
2. **No server guarantee.** The application is a desktop app, not a daemon. An HTTP API would require the app to stay running, or a separate daemon (which spec §7.10 rejects).
3. **Operational burden.** Port conflicts, firewall prompts, TLS certificate management for `localhost`, token rotation/revocation, CORS, CSRF.
4. **No remote access.** Spec §1.3 rejects internet-facing webhooks; a localhost HTTP API invites bypass attempts or third-party "connectors" that expose it remotely.

A CLI binary:

- Is invoked once per command; no persistent process.
- Uses OS user identity, not a token.
- Produces structured output, never HTML or a browsable API.
- Is signed and notarized, so the OS verifies its provenance.

## Why NOT webhooks

Spec App. A.5 notes: "We will not build Riffado-style public token-authenticated HTTP endpoints/webhooks." Reasons:

1. **Outbound webhook delivery.** Requires consent, target validation, secret handling, retries, SSRF review. Out of scope for V1.
2. **Inbound webhook receiver.** Requires authentication, replay protection, rate limiting, origin validation. Also requires a listening socket, which conflicts with the no-daemon/no-server principle.

If webhooks are ever added, they require a separate consent flow, target validation, and explicit documentation that they expose the library to inbound requests.

## Signature and notarization

The `plaudon` CLI binary is part of the signed application bundle on macOS (code-signed and notarized) and the signed installer on Windows (Authenticode). A script that copies the binary to another location loses the signature; the `scripts/verify-cli-signature.sh` script checks that the distributed binary is signed and that the release artifact includes the signature. This is a release gate (issue M2.1).

## Rejected alternatives

- **A loopback HTTP API.** Spec §1.3 explicitly prohibits it; Appendix A.6 rejects the transfer from Riffado.
- **Webhooks for automation.** Out of scope for V1; requires separate consent and security review.
- **A token instead of OS-user check.** A token stored in a file is readable by the same OS user; the OS-user check is simpler and no less secure for single-user desktop use.
- **No enable switch.** The bridge must be opt-in; automation is a power-user feature, not a default.
- **A separate daemon.** Spec §7.10 explicitly rejects "a helper daemon for folder watching"; a daemon for CLI invocation would be the same operational burden.

## Consequences

- Scripts can `list`, `export`, `process` and check `jobs status` without the GUI running, but only if the bridge is explicitly enabled.
- Consent and Local-only Mode are enforced identically to the GUI.
- Every command produces exactly one JSON document; no progress bars or logs to stdout.
- Exit codes distinguish success (0), refusal/failure (2), and "could not start" (any other code from the OS).
- The CLI binary is signed and notarized; tampering is detectable.
- No HTTP server, no webhook receiver, no daemon, no listening socket.
