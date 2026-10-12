# ADR-0402: Full-library encryption evaluation

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §13.7, §13.8, §24

## Context

Spec §13.7 states: "We will rely on standard OS disk protection (FileVault/BitLocker) as a base, while avoiding any claim that SQLite files are application-level encrypted by default. We will evaluate optional full-library encryption with OS-backed key wrapping as P1, including recovery and migration." Issue M2.5 records this evaluation ADR with a go/no-go decision.

The threat model (`docs/threat-model/full-library-encryption.md`) clarifies what full-library encryption would and would not defend against. This ADR evaluates the technical options and makes a shipping recommendation.

## Evaluated options

### (a) SQLCipher-class whole-database encryption

- **Approach:** Replace SQLite with SQLCipher or add SQLCipher's encryption extension; the database file is AES-256-CBC encrypted, keyed from a user passphrase via PBKDF2.
- **Licence:** SQLCipher community edition is BSD-style; the `sqlx` integration story requires either the community build or a commercial licence for official builds. Rust `rusqlite` has a `sqlcipher` feature; `sqlx` support is via a separate fork or manual patching.
- **Key wrapping:** Passphrase → PBKDF2 → AES key. Can be combined with OS keychain by storing the derived key or a wrapping key in Keychain/Credential Manager instead of prompting every launch.
- **Performance:** Per-page encryption/decryption overhead; estimates vary 5–15% on read-heavy workloads. VACUUM and large imports take longer. `PRAGMA cipher_page_size` and cache settings affect performance.
- **Migration:** Migrating an existing plaintext library requires `ATTACH` + `sqlcipher_export()` or `PRAGMA rekey`, which effectively rewrites the entire database. Downgrade (encrypted → plaintext) requires explicit export.
- **Integrity:** Page-level HMAC optional (SQLCipher 4+), not default in community edition. No per-transaction or per-record cryptographic integrity by default.
- **Media blobs:** SQLCipher encrypts the database only. Audio files in `media/` remain plaintext unless separately encrypted.
- **Platform:** Cross-platform where SQLCipher builds; requires vendoring or dynamic linking.
- **Measurement:** The measurement harness `scripts/measure-encryption-overhead.sh` (issue M2.4) would record overhead; **no measurement has been taken yet for this build**.

### (b) Per-file envelope encryption of media + encrypted SQLite export

- **Approach:** Encrypt each media file with AES-256-GCM (unique nonce per file), store the wrapped keys in an encrypted key table or OS keychain. SQLite remains plaintext during operation; periodic `VACUUM INTO` an encrypted volume or encrypted archive for at-rest protection.
- **Key wrapping:** Per-file DEK wrapped by a KEK, KEK wrapped by OS (Keychain `kSecAttrAccessibleWhenUnlocked` on macOS, DPAPI `CryptProtectData` with `CRYPTPROTECT_UI_FORBIDDEN` on Windows).
- **Performance:** Encryption cost only on write/read of media; no per-query SQLite overhead. Streaming encryption/decryption with `RustCrypto` aes-gcm crate.
- **Integrity:** GCM provides authenticated encryption; tampered ciphertext fails to decrypt.
- **Migration:** Existing media files are encrypted in place (copy to temp, encrypt, atomic rename). SQLite migration is a `VACUUM INTO` to a protected volume or encrypted container.
- **Partial-file integrity:** Per-file authenticated encryption, not per-row.
- **Platform:** Cross-platform; no SQLCipher dependency.
- **Measurement:** Overhead measured for file I/O, not database queries. **No measurement recorded yet.**

### (c) OS disk protection only (current posture)

- **Approach:** Rely on FileVault (macOS) or BitLocker (Windows). The application does not perform encryption; the OS encrypts the volume.
- **Key wrapping:** OS-managed, unlocked at login. Secure Enclave on Apple Silicon, TPM on Windows if configured.
- **Performance:** No application-level overhead; encryption is transparent.
- **Integrity:** Volume-level, managed by the OS.
- **Migration:** No migration required; already in place.
- **Threat coverage:** Protects against offline disk access (stolen laptop, removed disk, backup copied to unencrypted media) IF the volume is encrypted. Does NOT protect a library on an unencrypted external drive, a cloud-synced folder, or a network share.
- **Limitation:** User may disable FileVault/BitLocker or place the library on an unencrypted volume; the application cannot enforce or detect this reliably across all scenarios.

### (d) Per-record field encryption

- **Approach:** Encrypt specific sensitive columns (e.g., transcript text, speaker names) with application-managed keys.
- **Performance:** Encryption on every read/write of encrypted columns; query overhead for FTS (cannot index ciphertext).
- **Complexity:** High; every query touching encrypted fields must decrypt. FTS5 requires plaintext or separate encrypted index.
- **Integrity:** Per-field, but no cryptographic binding between related fields.
- **Migration:** ALTER to add encrypted columns, backfill, drop plaintext columns; requires application-aware migration.
- **Platform:** Cross-platform.
- **Measurement:** Not estimated; complexity deferred.

## Key-wrapping detail: OS-backed keys

### macOS Keychain

- `kSecAttrAccessibleWhenUnlocked`: key available only when the device is unlocked; inaccessible after lock.
- Secure Enclave (Apple Silicon): Hardware key wrapping; private keys never leave the Secure Enclave.
- API: `SecItemAdd`, `SecItemCopyMatching`, `SecItemUpdate`, `SecItemDelete` for key storage and retrieval.
- User authentication: Optional `kSecAttrAccessControl` with biometric/password; adds an additional unlock step.

### Windows DPAPI / CNG

- `CryptProtectData` / `CryptUnprotectData`: User-scoped or machine-scoped encryption; `CRYPTPROTECT_UI_FORBIDDEN` prevents UI prompts.
- TPM: Optional hardware-backed key protection if TPM is available and configured.
- Key storage: Via Credential Manager (`CredWrite`, `CredRead`) or direct DPAPI calls.
- Limitation: Keys unlocked when user session is unlocked; no per-access authentication by default.

## Recovery and key-loss policy

- **Recovery:** A lost passphrase (user-entered password encrypting the library) is NOT recoverable by the application. No master key, no backdoor, no "reset password" flow. If the key is in the OS keychain and the user loses access to their OS account, the key is lost.
- **User communication:** Onboarding for encryption must explicitly warn that losing the password means losing access to the library, and that backups of the encrypted library are also encrypted with the same key.
- **Keychain unavailable:** If the OS keychain is unavailable (corrupted, inaccessible), the application cannot unlock the library. An error is shown; the user must restore keychain access or use a backup.

## Migration and rollback

- **Plaintext → encrypted:** `ATTACH` and copy (SQLCipher), or `VACUUM INTO` encrypted volume (per-file approach). Media files encrypted in place or copied to encrypted archive.
- **Encrypted → plaintext downgrade:** Requires explicit user action ("Remove encryption") and sufficient disk space for a plaintext copy. One-way by default; user must confirm.
- **Rollback after enabling:** Downgrade path must be tested and documented; user must be able to export a plaintext backup if they choose.

## Portable archive contract (§13.8)

Spec §13.8 requires that portable archives can be encrypted with a user-chosen password (not dependent on the original machine's keychain), and that restoration works across macOS ↔ Windows. Archive encryption is separate from library encryption:

- Library encryption protects the working library on disk.
- Archive encryption protects the exported backup.

The two are independent. An encrypted library can export a plaintext archive, or an encrypted archive. A plaintext library can export an encrypted archive.

## Measured overhead

The measurement harness `scripts/measure-encryption-overhead.sh` (issue M2.4, assigned to another agent) is intended to record:

- SQLCipher: per-query and large-import overhead.
- Per-file AES-GCM: streaming encryption/decryption throughput.
- OS-volume overhead (baseline; typically negligible).

Results land in `docs/benchmarks/encryption-overhead.md`. **No measurement has been recorded yet for this build.** Any decision to ship encryption in a future release must cite actual measured overhead, not estimates.

## Decision

**Recommendation: NOT YET.** Keep OS disk protection (FileVault/BitLocker) as the shipped posture. Recommend that users who require additional protection enable full-disk encryption and, for highly sensitive libraries, place the library on an encrypted volume or use an encrypted external drive. Revisit full-library encryption after measurement and once the portable archive contract (§13.8) is stable and tested.

### Rationale

1. **Measurement absent.** No overhead measurement exists; shipping encryption without knowing the cost violates spec §12's measurement-before-claim rule.
2. **Complexity vs. threat coverage.** The threat model shows that application-level encryption does not defend against malware in the user session, a debugger, memory scraping, or a Tauri/WebView compromise. It adds narrow additional protection for offline disk access only in scenarios where FileVault/BitLocker are not enabled or the library is on an unencrypted external volume.
3. **Portable archive priority.** Spec §13.8's encrypted portable archive provides user-controlled, cross-machine backup encryption. Delivering that first (with measured, tested password-based encryption) satisfies the "encrypt sensitive backups" need without the operational complexity of encrypting the working library.
4. **Key recovery.** A lost password is unrecoverable; users must understand this before enabling. Encryption adoption requires clear onboarding, backup warnings, and a tested recovery/downgrade path.
5. **SQLCipher integration.** The `sqlx` ecosystem's SQLCipher support is less mature than mainline SQLite; a full evaluation requires integration testing, schema migration testing, and performance validation.

### When to revisit

A future EPIC should revisit full-library encryption when:

- Measurement of SQLCipher and per-file envelope overhead is complete and acceptable.
- Portable encrypted archives (§13.8) are shipped, tested, and proven across macOS ↔ Windows.
- User research shows demand for application-level encryption beyond encrypted backups.
- A design for onboarding, key-loss warnings, downgrade, and recovery is tested with users.

## Rejected alternatives

- **Shipping SQLCipher without measurement.** Violates spec §12 and risks performance regressions without known bounds.
- **Mandatory encryption for all users.** Adds complexity and key-loss risk for users who rely on OS disk encryption and do not need application-level encryption.
- **Per-record field encryption.** High complexity, no FTS on encrypted text, and unclear threat-model benefit over volume or per-file encryption.
- **Claiming that local processing is "encrypted".** Spec §16.6 prohibits misleading privacy claims; offline processing is not the same as encrypted storage.

## Consequences

- The application ships without full-library encryption; OS disk protection (FileVault/BitLocker) is the recommended posture.
- The threat model document (`docs/threat-model/full-library-encryption.md`) explicitly states what encryption would and would not protect.
- The portable archive contract (§13.8) includes encrypted backup/restore, which provides cross-machine encrypted export for sensitive users.
- A future EPIC that adds full-library encryption must include measurement, migration testing, key-loss warnings, and a documented recovery/downgrade path.
- No claim is made that the application encrypts the library at rest; the settings UI recommends enabling FileVault/BitLocker and using an encrypted volume for sensitive data.
