# Full-library encryption threat model

Spec: §16.1 (threats and mitigations), §16.2 (threat table), §13.7 (encryption posture), §13.8 (portable archive encryption).

This document describes what optional full-library encryption would and would not defend against, to inform the decision in ADR-0402 and to set user expectations.

---

## What full-library encryption would protect

Full-library encryption (encrypting the SQLite database and media files with application-managed keys, wrapped by the OS keychain) would protect against:

1. **Offline disk access by an adversary without OS credentials.** If a laptop is stolen while powered off, or a disk is removed, an adversary without the user's OS login cannot read the library. This is incremental protection **beyond FileVault/BitLocker** only in these scenarios:

   - The library is on an **unencrypted external drive** or USB stick.
   - The library is in a **cloud-synced folder** (iCloud Drive, OneDrive, Google Drive) and the cloud provider's at-rest encryption is not trusted.
   - The OS volume encryption (FileVault/BitLocker) is disabled.
   - A **backup of the library folder** is copied to an unencrypted destination.

2. **Cold-boot attacks after the machine is powered off.** If the OS volume is encrypted and the machine is fully shut down, the keys are not in RAM. Application-level encryption adds no benefit here; FileVault/BitLocker already cover this.

3. **Backup copied to unencrypted media.** If a user copies the library folder to an unencrypted external drive or cloud storage, application-level encryption protects the backup. (Note: spec §13.8 provides encrypted portable archives for this use case, which is a separate mechanism.)

---

## What full-library encryption would NOT protect

Full-library encryption does **not** defend against the following threats. Spec §16.1 and §16.6 require that limitations be stated explicitly.

### 1. Malware or a compromised process in the user session

While the application is running and the library is unlocked, **any process running as the same OS user can read the data**:

- The application holds decrypted data in memory (transcript text, audio buffers, database rows).
- A malicious process can attach a debugger, use `ptrace` (Linux/macOS), read `/proc/[pid]/mem` (Linux), or call `ReadProcessMemory` (Windows) to scrape memory.
- A compromised dependency linked into the process — the application is one native binary since the egui cutover (ADR-0024), so a malicious crate in the graph runs with the application's own authority — can read any command response or key the process holds.
- A keylogger or screen recorder running as the user can capture plaintext displayed on screen.

**Conclusion:** Application-level encryption does not defend against malware in the user session. A compromised user account can read decrypted data.

### 2. A debugger or memory dump while the app is running

- A privileged debugger (`lldb`, `gdb`, `WinDbg`) can attach to the application process and read decrypted memory.
- A core dump or crash dump (macOS `*.crash`, Windows `*.dmp`) may contain decrypted transcript text, audio, or keys.
- The application cannot reliably prevent debugging; `ptrace` anti-debugging is bypassable on macOS, and Windows allows kernel-mode debugging.

**Conclusion:** Encryption protects data at rest, not data in use. An attacker with debug/memory-read access while the app is unlocked can read plaintext.

### 3. A compromise of the application process itself

- If the application binary or any crate linked into it is compromised (a vulnerability, a supply-chain attack, a tampered build), the attacker can dispatch any command, read any data the process can reach, or extract keys from memory. This section used to name a Tauri plugin and a WebView renderer; those are gone with the webview, and the remaining surface is one Rust process.
- Encryption does not sandbox the application's own code; it protects against external offline access, not internal compromise.

**Conclusion:** Full-library encryption does not defend against a compromise of the application itself.

### 4. Data exfiltration via a compromised cloud provider or OpenRouter

- If the user enables cloud transcription/summarization and grants consent, audio and transcripts are sent to OpenRouter and the downstream provider.
- Application-level encryption of the local library does not prevent cloud exfiltration **after consent is granted**.
- Local-only Mode (spec §16.2A) refuses cloud dispatch; this is a separate control from encryption.

**Conclusion:** Encryption protects the local library; it does not control what the user authorizes to be sent to the cloud.

### 5. Data already written to plaintext logs, caches, or temp files

- The application writes no API keys or transcript excerpts to logs (spec §16.6), but diagnostic logs may include metadata (recording IDs, job statuses, error messages).
- If a transcript is exported to a plaintext file (TXT, Markdown, JSON), that export is not encrypted by library encryption.
- Temporary audio files during import (in `fsguard::PrivateTempDir`) are on the same volume as the library; if the volume is unencrypted, temp files are unencrypted during import.

**Conclusion:** Encryption protects committed data in the library; exported files and temp files during processing are outside the scope unless the OS volume is encrypted.

### 6. The library on a network share or remote volume

- If the library is on a network share (SMB, NFS), application-level encryption protects the data at rest on the remote storage, but does **not** encrypt data in transit over the network.
- The network share protocol (SMB3, NFS) may have its own encryption; application-level encryption is separate.

**Conclusion:** Application-level encryption does not provide network transport encryption; it only protects data at rest.

### 7. FileVault/BitLocker already enabled

- On a Mac with FileVault enabled, the volume is encrypted with AES-XTS-128 or AES-XTS-256 (depending on model), keyed from the user's login password or recovery key, wrapped by the Secure Enclave on Apple Silicon.
- On Windows with BitLocker enabled, the volume is encrypted with AES-128 or AES-256 (configurable), keyed from the user's password/PIN, optionally sealed by the TPM.
- If FileVault/BitLocker is enabled and the library is on that volume, **application-level encryption adds no additional protection against offline disk access**, because the disk is already encrypted.

**Conclusion:** For a library on an OS-encrypted volume, application-level encryption is redundant for offline threats. It only adds protection if the library is on an unencrypted external drive, network share, or cloud-synced folder.

### 8. A lost or stolen device while the OS is unlocked

- If the device is stolen while logged in and the screen is unlocked (or the attacker unlocks it via shoulder-surfing the PIN), the OS keychain is unlocked.
- Application-level keys wrapped by `kSecAttrAccessibleWhenUnlocked` (macOS) or `CryptProtectData` (Windows) are accessible while the session is unlocked.
- The attacker can launch the application, or copy the library files and extract the keys from the keychain.

**Conclusion:** Encryption protects against offline access; it does not protect a live, unlocked session.

### 9. Backup software or cloud sync before encryption is enabled

- If a user enabled cloud sync (iCloud Drive, OneDrive, Dropbox) **before** enabling application-level encryption, plaintext copies of the library may already exist in cloud backups.
- Enabling encryption does not retroactively encrypt those backups.

**Conclusion:** Encryption is forward-looking; existing backups remain in whatever form they were created.

### 10. A compromised OS keychain

- If the macOS Keychain or Windows Credential Manager is compromised (malware, privilege escalation, OS vulnerability), the attacker can retrieve the application's encryption keys.
- Application-level encryption depends on the OS keychain for key protection; it cannot defend against a keychain compromise.

**Conclusion:** The security of application-level encryption depends on the OS keychain's security.

---

## Recommended posture

As of ADR-0402, the application **does not encrypt the library at rest** by default. The recommended posture is:

1. **Enable FileVault (macOS) or BitLocker (Windows)** for the system volume. This protects the library against offline disk access.
2. **For highly sensitive libraries**, place the library on an **encrypted external volume** or use the encrypted portable archive feature (spec §13.8) for backups.
3. **Use Local-only Mode** (spec §16.2A) to refuse all cloud dispatch, including OpenRouter, update checks, and telemetry.

If full-library encryption is added in a future release, users must understand:

- It protects against offline disk access when FileVault/BitLocker is disabled or the library is on an unencrypted external drive.
- It does **not** protect against malware in the user session, memory scraping, debugger access, or a compromised application.
- A lost password is **not recoverable**; the library is permanently inaccessible.

---

## Residual risks for an unlocked session

Even with full-library encryption enabled, while the application is running and the library is unlocked:

- Any process running as the user can read memory.
- Any privileged process (root, admin) can attach a debugger or read memory.
- Any screen recording or keylogger can capture displayed content.
- Any code running inside the application process — including a compromised dependency — can dispatch a command and read its response.
- Any OS-level backup or snapshot captures the library in whatever state it is in (unlocked keys in keychain, temp files on disk).

Encryption protects data **at rest** (disk powered off, device stolen, backup copied to unencrypted media). It does not protect data **in use** (app running, user logged in, session active).

---

## Threat model summary

| Threat | FileVault/BitLocker | App-level encryption | Notes |
|---|:---:|:---:|---|
| Stolen powered-off laptop | ✓ | ✓ | Both protect; redundant if FileVault/BitLocker enabled |
| Library on unencrypted external drive | ✗ | ✓ | App-level encryption required |
| Cloud-synced library folder | ✗ | ✓ | Cloud provider's encryption may not be trusted |
| Backup copied to unencrypted media | ✗ | ✓ | Or use encrypted portable archive (§13.8) |
| Malware in user session | ✗ | ✗ | Neither protects; OS-level security required |
| Memory scraping / debugger | ✗ | ✗ | Data in use is not encrypted |
| Compromised application process | ✗ | ✗ | Internal compromise bypasses encryption |
| Cloud exfiltration after consent | ✗ | ✗ | Controlled by Local-only Mode, not encryption |
| Lost password | N/A | ⚠ | Unrecoverable; library permanently inaccessible |

**Legend:** ✓ = protects, ✗ = does not protect, ⚠ = risk introduced by the control.

---

## Compliance and regulatory claims

The application **does not claim regulatory compliance** (GDPR, HIPAA, PIPEDA, etc.) solely because processing occurs locally or because the library is encrypted. Spec §1.4 states: "We will not claim regulatory compliance solely because processing occurs locally."

Encryption is a technical control; compliance requires operational, legal, and procedural controls beyond the application's scope.
