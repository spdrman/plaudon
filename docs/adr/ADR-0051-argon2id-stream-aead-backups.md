# ADR-0051: Argon2id plus STREAM XChaCha20-Poly1305 for encrypted backups

- **Status:** Accepted
- **Date:** 2026-10-11
- **Spec:** §13.6, §13.7, §13.8, §16.4, ADR-0020 (backups are encrypted by default)

## Context

Spec §13.8 is normative: an encrypted archive uses "a modern authenticated
cipher and a deliberately expensive password KDF (for example Argon2id, tuned
and documented per platform)", records salt, KDF parameters, format version
and nonce scheme in authenticated metadata, uses an audited implementation and
unique nonces, streams, and does not roll its own cryptography. Portability
must not depend on the original machine's keychain, and a lost password cannot
be reconstructed by the application.

## Decision

- **KDF:** RustCrypto `argon2`, Argon2id v0x13. Profiles, documented here and
  in `crates/export/src/archive/crypto.rs`:

  | Profile | m_cost | t_cost | p_cost | Used for |
  |---|---|---|---|---|
  | `Backup` (default) | 64 MiB | 3 | 4 | `Create backup` on macOS and Windows |
  | `Sensitive` | 256 MiB | 4 | 4 | user opt-in |
  | `Test` | 64 KiB | 1 | 1 | tests only; never written by the product |

  The parameters, the 16-byte random salt and the derived key length travel in
  the archive header, so another machine reproduces the key from the password
  alone. On read they are bounded (`m_cost ≤ 1 GiB`, `t_cost ≤ 16`,
  `p_cost ≤ 16`) so a hostile archive cannot turn "deliberately expensive" into
  a denial of service.

- **Cipher:** RustCrypto `chacha20poly1305`'s XChaCha20-Poly1305 in the STREAM
  (BE32) construction, 1 MiB plaintext chunks. STREAM derives each chunk nonce
  from a random 19-byte prefix, a 32-bit counter and a last-block flag, so
  nonces are unique by construction and truncating, reordering or dropping a
  chunk fails authentication.

- **Associated data:** the exact header JSON bytes are the AAD of every chunk,
  so editing the KDF parameters, the salt or the nonce prefix invalidates the
  whole body instead of silently changing how it is read.

- **No recovery path:** no escrow, no hint, no machine-key-wrapped copy. A lost
  password is a lost archive, and the UI says so before the archive is written.

## Rejected alternatives

- **PBKDF2 or bcrypt.** neither is memory-hard; §13.8 names Argon2id.
- **One AEAD over the whole archive.** it would force the entire body into
  memory on both sides, which contradicts the streaming requirement.
- **A hand-rolled chunk nonce (`prefix || counter`).** the STREAM construction
  already solves last-chunk truncation and is audited; writing our own would be
  exactly the "roll your own cryptography" §13.8 forbids.
- **Wrapping the archive key with an OS keychain key.** the archive would stop
  being portable, which is the point of the format.
- **Encrypting only the metadata, storing media in the clear.** the audio is
  the sensitive part.

## Consequences

- An encrypted backup costs roughly a second of key derivation on current
  laptop hardware, once per archive, on both create and restore.
- A wrong password and a tampered body are indistinguishable at the crypto
  layer — by design — and both surface as `ARCHIVE_DECRYPTION_FAILED` with no
  partial plaintext written.
- The default `Create backup` flow is encrypted; a plaintext archive is a
  deliberate choice and the UI shows the plaintext disclosure before writing.
