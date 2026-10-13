# Open findings

Every row of [`traceability.json`](traceability.json) whose status is `open` or
`planned`, restated as a finding. SPEC §22.8 and §1.5 require **zero unresolved
critical or high findings at public release**, so this file is the list that
has to be clean (or explicitly accepted with the owning gate deferred) before
the release manifest is signed.

Finding `F-0NN` corresponds to matrix row `TM-0NN`. Finding ids are stable: a
row that reaches `implemented` is removed from this file and the id is retired,
never reused. The checker enforces both directions — an unresolved row missing
from this file fails, and an implemented row mentioned here fails.

## Severity scale

| Severity | Definition |
|---|---|
| **Critical** | Exploitation discloses a credential or user content without any user action, or silently destroys user media. Blocks release unconditionally. |
| **High** | Exploitation discloses user content or credentials given plausible user action or hostile input, or breaks a release-gate promise (Local-only Mode, read-only device access, no plaintext credential fallback). Blocks the gate named in the row. |
| **Medium** | Reduces defence in depth, weakens auditability, or risks an inaccurate claim; needs a decision before release but may be accepted with documentation. |
| **Low** | Hygiene; no plausible path to content or credential disclosure. |

## Conditional gates

SPEC §22.2 and §24 allow the direct-device track (EPIC L) and the cloud-AI
track to ship later than core V1. A finding whose gate is "Direct-device
Supported cell" or "Cloud AI release" blocks **that** track, not the offline
file-to-transcript product. It must still be resolved before the corresponding
route is advertised as anything other than Experimental or Unavailable.

## Findings

| Finding | Row | Severity | Owner | Epic | Blocks | What remains |
|---|---|---|---|---|---|---|
| F-001 | TM-001 | Medium | `crates/security/src/secrets.rs` | D | §22.16 device secrets; §1.5 "Secrets in application logs: 0" | The keychain-backed store, the opaque-reference database contract and redacted `Debug`/`Display` output all exist. Remaining: the crash-report half of the §16.2 verification column, which needs the diagnostics reporter. |
| F-002 | TM-002 | Medium | `crates/plaud-ble/src/lib.rs` | L | Direct-device Supported cell | The radio-layer disclosure is machine-enforced today; what is missing is suppression of raw GATT payload logging and a scan for token signatures in logs and transcripts. |
| F-003 | TM-003 | High | `crates/plaud-ble/src/lib.rs` | L | Direct-device Supported cell | No BLE adapter exists. Needs profile authentication plus fail-closed identity mismatch, proven with a fake-advertiser and clone-identity fixture. |
| F-004 | TM-004 | High | `crates/plaud-protocol/src/lib.rs` | L | Direct-device Supported cell | Needs mode detection with a hard refusal to fall back from an encrypted firmware mode to the legacy cleartext profile, proven with a synthetic encrypted-preamble downgrade fixture. |
| F-005 | TM-005 | High | `crates/plaud-protocol/src/lib.rs` | L | §22.14 non-destructive sync | Needs the compile-time read-only opcode allowlist and an emitted-opcode trace assertion. Until it exists, no device write path may be enabled at all. |
| F-006 | TM-006 | High | `crates/source-plaud-device/src/lib.rs` | L | Direct-device Supported cell | Needs per-device vault binding and identity/firmware revalidation on reconnect, proven with two simulated nearby devices and a mid-transfer disconnect. |
| F-008 | TM-008 | Medium | `crates/source-plaud-device/src/lib.rs` | L | §22.14 truthful device UX | Needs incomplete/`on_device_only` labelling so an incremental delta list is never presented as the device's full history. |
| F-010 | TM-010 | Medium | `crates/security/src/outbound/http.rs` | D | §1.5 "Secrets in application logs: 0" | Redaction holds at the egress boundary. Missing: the repository-wide snapshot/log scanner over emitted diagnostics (`crates/diagnostics`). |
| F-011 | TM-011 | High | `crates/security/src/outbound.rs` | D | Cloud AI release | Destination validation and host allowlisting are in place. Missing: a real HTTP transport that re-authorizes every redirect hop through the gate and bounds the hop count, with mock malicious-redirect fixtures. |
| F-012 | TM-012 | Medium | `crates/security/src/sandbox.rs` | D | §22.1 import of arbitrary files | Out-of-process execution, CPU/wall-clock/memory caps, a deny-by-default grant model (network and further `exec` are not expressible), bounded length-prefixed IPC with no handles, and a kill path that cannot yield a completed outcome are implemented and verified against a real child process; OS-side denial is proven by the same probe succeeding under `ResourceCapsOnly`. Per-platform gaps that remain open by design: macOS relies on the deprecated `sandbox-exec` and samples resident set rather than using a kernel memory cap (durable fix: a signed, entitled helper under the App Sandbox); Windows applies no filesystem or network restriction in V1; other unix platforms get resource limits only. Remaining work for this row: patched-codec policy and an AFL/libFuzzer corpus with sanitizer runs over a real decoder (needs the EPIC F audio pipeline). |
| F-013 | TM-013 | High | `Cargo.lock` | N | §22.8 vulnerability scans | Lockfile and central pinning exist. Missing: SBOM generation, dependency/OSV scanning in CI, and pinned hashes/signatures for downloaded artifacts. |
| F-015 | TM-015 | High | `crates/ai-tools/src/lib.rs` | I | §22.6 grounded extraction | Needs the data-not-instructions handling, schema checks on every model response, and the adversarial transcript suite. |
| F-017 | TM-017 | Medium | `crates/export/src/lib.rs` | J | §22.10 disclosure | Accepted posture for V1 (§13.7 keeps the archive plaintext on disk). Needs the plaintext-backup warning, the §13.8 encrypted portable archive, and OS disk-encryption guidance in user documentation. |
| F-018 | TM-018 | High | `crates/source-folder/src/lib.rs` | K | §22.11 imported files untouched | The staging primitive is in place (copy-only, source bytes untouched, same-volume rename commit, abandoned staging removed). Remaining: the watcher itself — readiness probe, cloud placeholder handling and the mid-copy/rename chaos tests. A watcher that moves or truncates a user's file is user-data loss. |
| F-019 | TM-019 | Medium | `crates/security/src/outbound/subprocess.rs` | D | §22.2 cloud connector | Program pinning already excludes a scraping helper. Missing: the onboarding UI that refuses pasted bearer tokens and browser-extension bridges (EPIC E), and the social-login/region fallback tests. |
| F-020 | TM-020 | High | `crates/source-plaud-cli/src/lib.rs` | K | §22.11 no remote delete cascade | Needs explicit tombstones and cloud trash/reappearance fixtures so a remote deletion can never erase local media. |
| F-021 | TM-021 | Medium | `crates/automation/src/lib.rs` | M | §22.7 | The architecture already excludes a listener. Needs the typed user-local CLI surface plus boundary fuzzing when automation ships. |
| F-022 | TM-022 | Medium | `crates/security/src/fsguard.rs` | D | §22.9 restore | Archive entry validation is implemented and fuzzed: traversal, absolute and device-namespace entries, case/NFC-folded duplicates, compression bombs, and a symlink planted after inspection are all refused without touching the filesystem. Remaining: manifest checksums and backup schema verification on the EPIC J restore path. |
| F-023 | TM-023 | High | `apps/plaudon-egui` | N | §22.8 signed update workflow | Needs signed artifacts, a verified release manifest, a rollback path that does not weaken verification, and a tampered-update rejection test. The packaging scripts fail closed without a credential and report `not_verified` rather than claiming a signature, so nothing publishable exists today: no notarized macOS bundle and no Authenticode-signed Windows installer has been produced, because we hold no Apple or code-signing credentials. |
| F-024 | TM-024 | Medium | `crates/security/src/outbound.rs` | D | §1.5 "Silent cloud upload: 0" | Opt-in is enforced at the gate and nothing in the repository sends telemetry. Needs the aggregate-only payload builder and instrumentation of all outbound requests before any telemetry ships. |
| F-025 | TM-025 | Medium | `crates/ai-openrouter/src/lib.rs` | G | Cloud AI release | Needs user budgets, provider guardrails and uncertain-outcome safeguards (an unknown outcome is recorded as unknown, never silently retried). |
| F-028 | TM-028 | Medium | `crates/security/src/outbound.rs` | D | §22.10 compatibility limitations published | The statement is enforced in the policy snapshot and in these documents. Missing: the Devices screen copy that repeats it at the moment of use (EPIC E / App. B.8). |
| F-029 | TM-029 | Medium | `crates/plaud-ble/src/lib.rs` | L | §22.17 hardware-backed claims | Needs the `profile-authenticated` assurance label in the device model and the compatibility register rule that no cell reaches `Supported` without an evidence record. |
| F-031 | TM-031 | Medium | `crates/security/src/consent.rs` | D | §22.3 consent | Route, destination, content type and retention posture are scope axes today and an unknown posture never satisfies a requirement. Remaining: a named test for re-prompting when only the disclosure version changes. |
| F-035 | TM-035 | Medium | `crates/security/src/outbound/subprocess.rs` | D | §1.5 "Secrets in application logs: 0" | Environment, argv and SQLite are covered by named tests. Remaining: the crash-trace scan, which needs the diagnostics reporter. |
| F-036 | TM-036 | Medium | `crates/security/src/secrets.rs` | D | §22.16 | Needs zeroizing buffers for transient secret material. The accompanying honesty statement (memory cannot be perfectly protected from privileged malware) is already published. |
| F-040 | TM-040 | High | `crates/ui/src/screens/devices/presentation.rs` | E | §22.3; §16.6 operational privacy | The device transfer screen derives its cloud sentence from the DTO's own `cloud_route`, in exactly one place, so no screen can print `Cloud: not used` on its own authority. Remaining: the same per-route labelling on the recording and job surfaces, so a user can tell local from cloud processing for a given recording. |
| F-043 | TM-043 | High | `crates/domain/src/error.rs` | B | §22.8; §1.5 "Secrets in application logs: 0" | Found by D2.4 fuzzing. `plaudon_domain::error::Redactor` only matches a URL scheme at the *start* of a token, so a parenthesised, quoted, angle-bracketed or `url=`-prefixed URL is persisted verbatim with its signed query string; and `looks_like_opaque_secret` rejects any token containing `.`, `+` or `/`, so dotted JWTs and base64 credentials survive. Both run on untrusted vendor text in `AppError::from_unmapped_vendor` and `DiagnosticLog::record`. Regression test `domain_redactor_redacts_separator_bearing_credentials` is committed and `#[ignore]`d against this finding; Team-AB has a confirmed fix on `epic-a-spikes`. Closes when that merges here and the ignore is deleted. |

**Retired, 2026-10-12 (P2.5, #248).** F-045, F-047 and F-048 — embedding
inversion from a stolen profile store, voice-profile poisoning, and
cross-library correlation of voiceprints — are closed. Each had been waiting on
one leg rather than on a design: F-045 on the export exclusion (landed with
P2.3) and on a public document saying plainly that encryption protects data at
rest and not data in use (landed with this change, in
`docs/public/privacy-disclosure.md`); F-047 on the enrolment and suggestion
surfaces, which landed with P1.5 and P2.1 and now say "sounds like" everywhere
a match is shown; F-048 on a test rather than an argument, now
`the_same_voice_in_two_libraries_shares_no_identifier_or_ciphertext`. The
residual risks the rows carry — vectors are plaintext in memory while a job
runs, a user can confirm a wrong suggestion, a user can restore one encrypted
backup into two libraries — stay written down in `traceability.json` rather
than being counted as closed. The remaining voice-profiles gate items are not
findings but release evidence, and live in
`docs/release/voice-profiles-evidence.md`.

**Retired, 2026-10-12.** F-039 — "production build ships devtools or a permissive
content security policy" — is closed by the egui cutover (ADR-0024) rather than
by a fix in the sense it was written. A Rust-only binary has no browser engine,
no content security policy, no remotely hosted UI content and no capability
manifest, so the audit that parsed them (and the `plaudon-tauri-audit` binary
that ran it) was deleted with the shell. The property the finding protected —
a published artifact must not be debuggable or carry an escape hatch out of the
platform's runtime protections — is now the packaging entitlement and manifest
audit, which is `implemented` with named tests in the matrix and therefore does
not belong in this file. The id is retired, not reused.

**Closed, 2026-10-12.** F-044 — "voice profile embedding leaves the device
through any egress path" — is closed by P2.2. The egress gate now has a payload
class it refuses before it looks at the network mode, the grant, the subsystem
or the destination, and that class has no consent content type, so no grant can
name it; the egress-evidence suite drives an enrol-and-match flow and offers a
biometric payload at the OpenRouter, telemetry and cloud-CLI seams under a
local-only **and** a deliberately permissive policy; the diagnostics redactor
gained the embedding shapes and a display-name registry fed from the roster.
Its matrix row is `implemented` with named tests and therefore does not belong
in this file. The id is retired, not reused. What is *not* claimed: the gate
governs app-initiated traffic from this process, and redaction cannot recognise
a name it was never told about.

## Summary by severity

| Severity | Count | Ids |
|---|---|---|
| Critical | 0 | — |
| High | 12 | F-003, F-004, F-005, F-006, F-011, F-013, F-015, F-018, F-020, F-023, F-040, F-043 |
| Medium | 20 | F-001, F-002, F-008, F-010, F-012, F-017, F-019, F-021, F-022, F-024, F-025, F-028, F-029, F-031, F-035, F-036, F-045, F-046, F-047, F-048 |
| Low | 0 | — |

The High set is the §22.8 release blocker list. Of it, F-003, F-004, F-005 and
F-006 block only the direct-device track; F-011 and F-015 block only the
cloud-AI track; the remainder block core V1.

F-045 through F-048 are the remaining controls for the speaker voice-profiles
feature ([`docs/specs/VOICE_PROFILES_SPEC.md`](../specs/VOICE_PROFILES_SPEC.md),
ADR-0080/0081/0082). Each is partially present: the storage, enrolment,
matching, export and egress legs have landed in phases, and each row above says
which part of its control is still missing. They block the voice-profiles
feature gate (EPIC P), not core V1, and each must be re-rated as the subsystem
grows.

No finding is currently rated Critical. The controls that would carry a
Critical rating if they were missing — the OS-backed secret store with no
plaintext fallback, per-device credential scoping, the scoped consent
preflight and deny-by-default egress — are implemented and verified; their
rows in [`traceability.json`](traceability.json) carry named tests. The
remaining High findings are controls whose subsystem has not shipped yet, so
there is no reachable path to credential or content disclosure through them
today. Each rating must be re-examined the moment its subsystem lands.

## Closed fuzzing defects (D2.4)

These are not matrix rows: the matrix tracks controls, and the controls below
are already `implemented` with named tests. They are recorded here because the
§24 evidence bundle should show that a control was *found broken* and that a
committed corpus entry now stops the defect returning. All three were found by
fuzzing, fixed, and are replayed on every pull request by
`cargo test -p plaudon-security --test fuzz_corpus_replay` and under
AddressSanitizer by `scripts/fuzz-ci.sh`.

| # | Defect | Location | Fixed by | Regression corpus | Control it protects |
|---|---|---|---|---|---|
| C-1 | View-reachable panic: `starts_with_ascii_case_insensitive` byte-indexed an untrusted `&str` (`value[..prefix.len()]`), so `reject_raw_path` panicked whenever byte 5 (`file:`) or 6 (`asset:`) fell inside a multi-byte character | `crates/security/src/tauri_guard/validate.rs`, deleted with the Tauri shell at the egui cutover (ADR-0024); the boundary that validates a display or search field is now `crates/app/src/validate.rs` | Team-D.TauriLeastPrivilege — compares `as_bytes()` slices | `tests/fixtures/fuzz-corpus/cli_arg_render/invalid_utf8` and the `a⁦b⁩c` line of `tests/fixtures/hostile-paths/names.txt`, replayed by `cli_arg_render_corpus_holds_its_invariants` and `shared_hostile_names_are_safe_as_cli_arguments` | §16.2 "Unauthorized frontend file access", "Local automation escape" |
| C-2 | Redaction was not idempotent: an emitted `<redacted:…>` marker was re-matched and `trim_trailing_punctuation` stripped its `>`, growing the record one byte per pass — and redaction composes across the logging layer, notification redaction and the crash hook | `crates/diagnostics/src/redact.rs` | Team-D.LogRedaction — markers are inert | `tests/fixtures/fuzz-corpus/redactor/secret_min_length_8`, `.../secret_too_short_7` | §1.5 "Secrets in application logs: 0" |
| C-3 | A registered secret was invisible when it contained `%XX` or `\/`: only the decoded view was scanned, so a secret carrying a percent-escape never matched; a separator-stripped pass also used to swallow the rest of a JSON record | `crates/diagnostics/src/redact.rs` | Team-D.LogRedaction — the untouched record is scanned too whenever decoding changed anything | `tests/fixtures/fuzz-corpus/redactor/secret_contains_percent_escape`, `.../secret_contains_escaped_slash`, `.../secret_in_json_tail_survives` | §16.2 "Credential disclosure", "Signed URL leak"; §1.5 "Secrets in application logs: 0" |

The one fuzzing defect that is **not** closed is F-043 above, whose regression
test is committed and `#[ignore]`d until the upstream fix merges into this
branch.
