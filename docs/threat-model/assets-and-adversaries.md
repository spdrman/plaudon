# Assets and adversaries

Source: SPEC v1.3 §16.1, with storage locations from §13 and credential rules
from §16.4 and Appendix B.5/B.7. Row ids in the last column refer to
[`traceability.json`](traceability.json); the rendered matrix is in
[`README.md`](README.md).

## Sensitivity scale

| Level | Meaning |
|---|---|
| **Critical** | Disclosure gives an attacker continued access to the user's data or accounts, or costs the user money. Loss of integrity breaks the trust boundary itself. |
| **High** | Disclosure exposes the content of private meetings or the identity of the people in them. Irrecoverable on loss. |
| **Medium** | Disclosure is sensitive in aggregate or reveals behaviour, but not meeting content. Recoverable by re-deriving. |
| **Low** | Public or trivially re-obtainable, but its *integrity* may still matter. |

## Assets

| Asset | Sensitivity | Where it lives | Retention | Rows |
|---|---|---|---|---|
| Original audio files (immutable committed media) | High | Media archive on disk (content-addressed, plaintext for V1 per §13.7); staging copies in a private temp dir during import; bounded buffers in process memory while streaming | Kept until the user deletes the recording. Never rewritten: §6.3 invariant 3 makes committed bytes immutable, so an edit creates a derivative. Source files outside the archive are never moved or deleted. | TM-017, TM-018, TM-022, TM-037, TM-038 |
| Transcripts (text, timestamps, speaker turns) | High | SQLite (revisioned rows), process memory during processing, user-initiated exports | Revisions accumulate; a new transcript never silently overwrites the previous revision. Deleted with the recording. | TM-015, TM-017, TM-020 |
| Client and organisation names | High | SQLite (recording titles, tags, annotations), transcript text, exports, native notification payloads | As long as the recording exists. Must not reach crash reports, telemetry aggregates or notification text that the OS persists. | TM-010, TM-024, TM-040 |
| Speaker names and diarization labels | High | SQLite, transcript revisions, exports | As long as the recording exists. | TM-015, TM-017 |
| Meeting decisions, actions and extracted structure | High | SQLite (derived output revisions with source anchors), exports | Versioned per derivation; deleted with the recording. | TM-015, TM-025 |
| Sensitive attachments and user notes | High | Media archive and SQLite annotations, exports | User-controlled; deleted with the recording. | TM-017, TM-022 |
| Device authorization credentials — static per-device BLE secret (legacy profile) | **Critical** | OS keychain / Credential Manager **only**. SQLite and portable backups hold an opaque reference and a non-reversible fingerprint. Transient copies exist in process memory during a session and are zeroized. **On the radio link the legacy profile may transmit it in cleartext.** | Lives until the user unlinks the device. Never written to a file, an environment variable, a command-line argument or a crash trace. | TM-001, TM-002, TM-033, TM-034, TM-035, TM-036 |
| Device authorization credentials — mode-B (RSA / ChaCha20-Poly1305) key material | **Critical** | OS keychain only, device-scoped, never synchronised across users or devices by this app | Until the device is unlinked. We never downgrade a mode-B device to the legacy cleartext profile to avoid handling this material. | TM-004, TM-034 |
| PLAUD cloud authentication state | **Critical** | Owned by the official PLAUD CLI in its own storage. This application never handles PLAUD account passwords and never copies or pastes bearer tokens. | Out of our retention control; we hold only the fact that a CLI connection was configured. | TM-019, TM-027 |
| OpenRouter API key | **Critical** | OS keychain only. Held in process memory for the duration of a request; placed in a request header, never in a URL, argv or subprocess environment. | Until the user removes it. Its presence never implies consent to send content (§16.3). | TM-001, TM-032, TM-033, TM-035 |
| Signed download URLs (cloud CLI / model artifacts) | **Critical** (short-lived bearer capability) | Ephemeral process memory only. Logs and error envelopes carry a redacted `scheme://host:port` label with no path or query. | Discarded as soon as the transfer finishes or fails. Never persisted. | TM-010 |
| Model downloads (offline STT / LLM artifacts) | Medium (integrity-critical) | Managed model directory on disk; checksum recorded | Kept until the user removes the model. A download is a distinct egress purpose that requires explicit approval even when networking is permitted. | TM-011, TM-012, TM-013 |
| Application binaries and bundled native libraries | Medium (integrity-critical) | Installed application directory, owned by the OS installer | Replaced only by a signature-verified update. | TM-013, TM-023 |
| Update channel (manifest + packages) | Medium (integrity-critical) | Remote; fetched only when networking is permitted | Not retained. A tampered or unsigned manifest must be rejected, and rollback must not weaken verification. | TM-023 |
| Audit metadata (egress decision records, consent grants, job and import journal, provenance) | Medium | Bounded in-memory observer ring for egress decisions; SQLite for consent, jobs, the import journal and device provenance; diagnostics log files | Egress records are bounded and dropped oldest-first; persisted journals live as long as the archive. Must be useful for an audit without containing content, secrets or signed URLs. | TM-010, TM-024, TM-026, TM-035 |

### Derived rules

* No asset above is permitted in a crash report, a telemetry payload, or a
  subprocess environment (§16.4).
* The portable backup contains media, transcripts, revisions and annotations —
  and **no** credential of any kind (§22.9).
* The archive is plaintext on disk for V1 by design (§13.7): disk-level
  confidentiality is the OS's full-disk encryption, which we document rather
  than reimplement, and the encrypted portable archive of §13.8 covers the
  exported copy.

## Adversaries

Capability assumptions are stated so that a control can be judged against them.
Where an adversary is simply stronger than any control we can deploy, that is
stated instead of being mitigated on paper.

### A1. Local malicious process (unprivileged, same user)

**Assumed capable of:** running under the same OS user; reading any file that
user can read; connecting to any local listener; observing process arguments
and environment variables; watching the filesystem.
**Assumed not capable of:** reading another process's memory without the
platform's debug entitlement, or modifying our signed binary in place.
**Controls:** no HTTP listener and no localhost control channel of any kind
(§6.3 invariant 12, TM-021); secrets only in the OS keychain, so a file read
does not yield a credential (TM-033); no secret in argv or the environment,
including for the CLI subprocess (TM-035); restrictive permissions on the
managed data directory (TM-038).
**Honest limit:** the archive is plaintext on disk, so a process running as the
user can read recordings. That is a property of V1's storage posture (§13.7),
not an oversight; see finding F-017.

### A2. Privileged local malware or a user with debug entitlement

**Assumed capable of:** reading and writing our process memory, hooking the
keychain API, replacing our binary.
**Controls:** none that would be honest to claim. Zeroization of transient
buffers reduces the window, nothing more.
**Honest limit:** stated as item 5 of the not-guaranteed list in
[`README.md`](README.md) and in §16.4 — in-process memory cannot be perfectly
protected from privileged malware.

### A3. Compromised plugin or dependency (supply chain)

**Assumed capable of:** executing arbitrary code inside our process at build or
run time; attempting network access from within our address space.
**Controls:** committed lockfile and centrally pinned workspace versions;
SBOM, dependency and OSV scanning in CI (TM-013, EPIC N); the egress gate is
*structural*, so code inside our process still cannot open a socket through our
transport without an `EgressTicket` that only `OutboundPolicy::authorize` can
mint (TM-026).
**Honest limit:** a dependency can call the OS socket API directly. The gate
constrains our own subsystems, not arbitrary linked code; the compensating
controls are review, pinning and scanning, and the OS-level egress monitor
fixture (tests/egress) that observes the whole process rather than the gate.

### A4. Malicious audio or container input

**Assumed capable of:** crafting a file that triggers memory corruption,
unbounded allocation or infinite work in a native decoder; arriving through a
manual import, a watched folder, a vendor USB volume or a BLE transfer.
**Controls:** format sniffing and bounds before decode (`plaudon_audio`,
EPIC F); decoding in a separate OS-sandboxed helper process with resource caps
so a crash is a failed job, not a compromised app (TM-012, ADR-0015); a killed
helper is recorded as a failed job, never as a completed import.
**Honest limit:** Rust memory safety does not make a native codec safe (§6.3A);
the sandbox reduces blast radius and does not eliminate the bug class.

### A5. Fake download redirect / hostile endpoint

**Assumed capable of:** DNS or network-path control; answering with a redirect
to an internal or attacker-controlled host; serving a substituted model or
update artifact.
**Controls:** HTTPS-only destinations with no embedded credentials, IP literals
and loopback/private/mDNS hostnames rejected, and a host allowlist that matches
on label boundaries so `openrouter.ai.evil.example` is not treated as
`openrouter.ai` (TM-011); redirects must be re-authorized per hop and bounded;
artifact checksums and signature verification for models and updates
(TM-013, TM-023).
**Honest limit:** per-hop redirect re-authorization lives in the HTTP transport
implementation, which does not exist yet — see finding F-011.

### A6. Untrusted transcript content (prompt injection)

**Assumed capable of:** placing instructions inside recorded speech, an
imported transcript or a file name, aiming to steer summarization, exfiltrate
other recordings, or cause an automated export.
**Controls:** transcripts are data, never instructions (§6.3 invariant 8);
structured-output schema validation on every model response; no tool-calling
surface that can reach the network or the filesystem from a prompt; extraction
output must carry source anchors or be marked unverified (TM-015).
**Honest limit:** instruction-following models cannot be proven immune to
injection. The structural defence is that a successful injection still cannot
initiate egress or touch a file — it can only produce bad text, which the
source-anchor rule marks as unverified.

### A7. Poisoned model

**Assumed capable of:** shipping a model artifact that emits attacker-chosen
output or exploits the inference runtime.
**Controls:** checksum and license recorded per model artifact (§24 evidence
bundle); downloads are a separately approved egress purpose; inference runs in
the same sandboxed helper as decoding (ADR-0015, TM-012).
**Honest limit:** we can verify that an artifact matches a recorded checksum,
not that its weights are benign.

### A8. Network interception

**Assumed capable of:** observing or modifying traffic on the local network, or
sniffing/replaying a BLE session in radio range.
**Controls (internet):** HTTPS-only, allowlisted hosts, no credential in URLs,
redirect bounds (TM-011).
**Controls (BLE):** no downgrade from an encrypted firmware mode to the legacy
cleartext profile (TM-004); no raw GATT payload logging (TM-002); identity
labelled `profile-authenticated` (TM-029).
**Honest limit:** the examined legacy BLE profile has no application- or
link-layer encryption, so an in-range listener can observe the static token and
the transferred `.ASR` bytes. Local-only Mode does not change this
(App. B.5, not-guaranteed item 4).

### A9. Unintended cloud disclosure (the user's own data going somewhere they did not choose)

**Assumed capable of:** nothing — this is our own defect class: a subsystem that
forgets to ask, a UI that implies local processing while dispatching to a
provider, a cloud-synced watched folder, a background update that uploads
diagnostics.
**Controls:** one central gate at dispatch time that every network-capable
subsystem must pass (§6.3 invariant 5, TM-026); default Local-only; telemetry
and model downloads require their own explicit approval; the CLI subprocess is
never launched in Local-only Mode (TM-027); a cloud-synced user folder is
disabled or labelled (not-guaranteed item 3); scoped consent that fails closed
when destination, purpose or disclosure version changes (TM-030, TM-031).
**Honest limit:** the consent preflight runs *after* the egress gate, so a
grant can never widen Local-only Mode — but it only constrains dispatches that
go through it. A future subsystem that builds its own request path would be
caught by the gate's ticket requirement, not by consent. Re-prompting on a
disclosure-version bump alone is still unverified (finding F-031).
