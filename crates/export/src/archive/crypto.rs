//! Password-based authenticated encryption for portable archives (spec 13.8).
//!
//! We do not invent cryptography here. The construction is:
//!
//! * **Argon2id** (RustCrypto `argon2`) derives a 32-byte key from the user's
//!   password and a 16-byte random salt. The parameters live in the archive
//!   header so a restore on another machine can reproduce the key without any
//!   keychain involvement, and they are bounded on read so a hostile archive
//!   cannot ask us to allocate a terabyte.
//! * **XChaCha20-Poly1305 in the STREAM (BE32) construction** (RustCrypto
//!   `chacha20poly1305`) encrypts the body in fixed-size chunks. STREAM
//!   derives each chunk nonce from a random 19-byte prefix plus a 32-bit
//!   counter plus a last-block flag, so nonces are unique by construction and
//!   truncating, reordering or dropping a chunk fails authentication.
//! * The **header JSON is the AAD of every chunk**, so editing the KDF
//!   parameters, the salt or the nonce prefix invalidates the whole body
//!   instead of silently changing how it is read.
//!
//! A lost password cannot be recovered: there is no escrow, no hint and no
//! key wrapped with a machine key anywhere in this module.

use std::io::{Read, Write};

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::aead::Payload;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{ArchiveRejection, ExportError, ExportResult};

/// Name recorded in the header for the cipher construction.
pub const CIPHER_NAME: &str = "xchacha20poly1305-stream-be32";
/// Name recorded in the header for the key-derivation function.
pub const KDF_NAME: &str = "argon2id";

/// Derived key length in bytes.
pub const KEY_BYTES: usize = 32;
/// Salt length in bytes.
pub const SALT_BYTES: usize = 16;
/// STREAM BE32 nonce prefix length: XChaCha's 24-byte nonce minus the 4-byte
/// counter and the 1-byte last-block flag.
pub const NONCE_PREFIX_BYTES: usize = 19;
/// Poly1305 tag length appended to every chunk.
pub const TAG_BYTES: usize = 16;

/// Plaintext bytes per chunk. 1 MiB keeps peak memory flat and bounds the
/// number of chunks far below the 32-bit STREAM counter for any real archive.
pub const CHUNK_BYTES: usize = 1024 * 1024;

/// Upper bounds accepted from an untrusted archive header.
///
/// Argon2id is *meant* to be expensive; these bounds stop an archive from
/// turning that into a denial of service on the machine doing the restore.
pub const MAX_M_COST_KIB: u32 = 1024 * 1024; // 1 GiB
/// Largest accepted time cost.
pub const MAX_T_COST: u32 = 16;
/// Largest accepted parallelism.
pub const MAX_P_COST: u32 = 16;

/// Tuned Argon2id profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KdfProfile {
    /// Default for `Create backup`: ~64 MiB, three passes. Roughly a second on
    /// a current laptop; documented in `docs/adr/ADR-0071-argon2id-stream-aead-backups.md`.
    Backup,
    /// Deliberately heavier profile for users who want it: 256 MiB.
    Sensitive,
    /// Minimum viable parameters. Tests only — never written by the product.
    #[doc(hidden)]
    Test,
}

impl KdfProfile {
    /// `(m_cost in KiB, t_cost, p_cost)`.
    #[must_use]
    pub const fn parameters(self) -> (u32, u32, u32) {
        match self {
            Self::Backup => (65_536, 3, 4),
            Self::Sensitive => (262_144, 4, 4),
            Self::Test => (64, 1, 1),
        }
    }
}

/// Key-derivation parameters, recorded in authenticated header metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KdfParams {
    /// Always `argon2id`.
    pub algorithm: String,
    /// Argon2 version number (`19` for 0x13).
    pub version: u32,
    /// Memory cost in KiB.
    pub m_cost_kib: u32,
    /// Number of passes.
    pub t_cost: u32,
    /// Degree of parallelism.
    pub p_cost: u32,
    /// Derived key length in bytes.
    pub key_bytes: u32,
    /// Random salt, lowercase hex.
    pub salt_hex: String,
}

impl KdfParams {
    /// Fresh parameters for a profile, with a random salt.
    #[must_use]
    pub fn generate(profile: KdfProfile) -> Self {
        let (m_cost_kib, t_cost, p_cost) = profile.parameters();
        let mut salt = [0u8; SALT_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        Self {
            algorithm: KDF_NAME.to_owned(),
            version: 19,
            m_cost_kib,
            t_cost,
            p_cost,
            key_bytes: KEY_BYTES as u32,
            salt_hex: hex::encode(salt),
        }
    }

    /// Reject parameters we will not run (spec 13.8 resource-abuse guard).
    pub fn validate(&self) -> ExportResult<()> {
        let reject = |detail: String| {
            Err(ExportError::ArchiveRejected(
                ArchiveRejection::KdfParametersRejected(detail),
            ))
        };
        if self.algorithm != KDF_NAME {
            return reject(format!("unsupported algorithm {}", self.algorithm));
        }
        if self.version != 19 {
            return reject(format!("unsupported Argon2 version {}", self.version));
        }
        if self.key_bytes as usize != KEY_BYTES {
            return reject(format!("unsupported key length {}", self.key_bytes));
        }
        if self.m_cost_kib > MAX_M_COST_KIB {
            return reject(format!(
                "memory cost {} KiB exceeds the {MAX_M_COST_KIB} KiB limit",
                self.m_cost_kib
            ));
        }
        if self.t_cost == 0 || self.t_cost > MAX_T_COST {
            return reject(format!("time cost {} is out of range", self.t_cost));
        }
        if self.p_cost == 0 || self.p_cost > MAX_P_COST {
            return reject(format!("parallelism {} is out of range", self.p_cost));
        }
        if self.m_cost_kib < 8 * self.p_cost {
            return reject("memory cost is below the Argon2 minimum".to_owned());
        }
        let salt = hex::decode(&self.salt_hex)
            .map_err(|_| ArchiveRejection::KdfParametersRejected("salt is not hex".to_owned()));
        match salt {
            Ok(salt) if salt.len() >= 8 => Ok(()),
            Ok(_) => reject("salt is too short".to_owned()),
            Err(rejection) => Err(ExportError::ArchiveRejected(rejection)),
        }
    }

    /// Derive the archive key. The password is never retained.
    pub fn derive(&self, password: &str) -> ExportResult<Zeroizing<[u8; KEY_BYTES]>> {
        self.validate()?;
        let salt = hex::decode(&self.salt_hex).map_err(|_| {
            ExportError::ArchiveRejected(ArchiveRejection::KdfParametersRejected(
                "salt is not hex".to_owned(),
            ))
        })?;
        let params = Params::new(self.m_cost_kib, self.t_cost, self.p_cost, Some(KEY_BYTES))
            .map_err(|error| {
                ExportError::ArchiveRejected(ArchiveRejection::KdfParametersRejected(
                    error.to_string(),
                ))
            })?;
        let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key = Zeroizing::new([0u8; KEY_BYTES]);
        argon
            .hash_password_into(password.as_bytes(), &salt, key.as_mut())
            .map_err(|error| {
                ExportError::ArchiveRejected(ArchiveRejection::KdfParametersRejected(
                    error.to_string(),
                ))
            })?;
        Ok(key)
    }
}

/// Everything a reader needs to reproduce the key and the nonce sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncryptionHeader {
    /// Always [`CIPHER_NAME`].
    pub cipher: String,
    /// Key-derivation parameters.
    pub kdf: KdfParams,
    /// Plaintext bytes per chunk.
    pub chunk_bytes: u32,
    /// Random STREAM nonce prefix, lowercase hex.
    pub nonce_prefix_hex: String,
}

impl EncryptionHeader {
    /// Fresh header for a profile.
    #[must_use]
    pub fn generate(profile: KdfProfile) -> Self {
        let mut prefix = [0u8; NONCE_PREFIX_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut prefix);
        Self {
            cipher: CIPHER_NAME.to_owned(),
            kdf: KdfParams::generate(profile),
            chunk_bytes: CHUNK_BYTES as u32,
            nonce_prefix_hex: hex::encode(prefix),
        }
    }

    /// Validate everything an untrusted header declares.
    pub fn validate(&self) -> ExportResult<()> {
        if self.cipher != CIPHER_NAME {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::KdfParametersRejected(format!(
                    "unsupported cipher {}",
                    self.cipher
                )),
            ));
        }
        if self.chunk_bytes == 0 || self.chunk_bytes as usize > 8 * CHUNK_BYTES {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::KdfParametersRejected(format!(
                    "unsupported chunk size {}",
                    self.chunk_bytes
                )),
            ));
        }
        if hex::decode(&self.nonce_prefix_hex).map(|p| p.len()) != Ok(NONCE_PREFIX_BYTES) {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::KdfParametersRejected("nonce prefix is malformed".to_owned()),
            ));
        }
        self.kdf.validate()
    }

    fn nonce_prefix(&self) -> ExportResult<[u8; NONCE_PREFIX_BYTES]> {
        let raw = hex::decode(&self.nonce_prefix_hex).map_err(|_| {
            ExportError::ArchiveRejected(ArchiveRejection::KdfParametersRejected(
                "nonce prefix is malformed".to_owned(),
            ))
        })?;
        let mut prefix = [0u8; NONCE_PREFIX_BYTES];
        if raw.len() != NONCE_PREFIX_BYTES {
            return Err(ExportError::ArchiveRejected(
                ArchiveRejection::KdfParametersRejected("nonce prefix is malformed".to_owned()),
            ));
        }
        prefix.copy_from_slice(&raw);
        Ok(prefix)
    }
}

/// Message of the io error raised when an AEAD tag does not verify.
///
/// A wrong password and a tampered body are indistinguishable at this layer —
/// that is the point of authenticated encryption — so both surface through
/// this marker and the reader turns it into a typed failure.
pub const AEAD_FAILURE: &str = "plaudon-archive-authentication-failed";

/// Message of the io error raised when the body ends early or declares an
/// impossible chunk length.
pub const FRAMING_FAILURE: &str = "plaudon-archive-body-malformed";

fn aead_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, AEAD_FAILURE)
}

fn framing_error() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, FRAMING_FAILURE)
}

/// Framing flags for one encrypted chunk.
const CHUNK_MORE: u8 = 0;
const CHUNK_LAST: u8 = 1;

/// Buffers plaintext and writes authenticated chunks.
///
/// [`EncryptingWriter::finish`] must be called; dropping without finishing
/// produces an unreadable body, which is exactly what an interrupted backup
/// should produce.
pub struct EncryptingWriter<W: Write> {
    inner: W,
    encryptor: Option<EncryptorBE32<XChaCha20Poly1305>>,
    aad: Vec<u8>,
    buffer: Vec<u8>,
    chunk_bytes: usize,
}

impl<W: Write> std::fmt::Debug for EncryptingWriter<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptingWriter")
            .field("chunk_bytes", &self.chunk_bytes)
            .finish_non_exhaustive()
    }
}

impl<W: Write> EncryptingWriter<W> {
    /// Wrap `inner`, deriving the key from `password`.
    pub fn new(
        inner: W,
        header: &EncryptionHeader,
        aad: &[u8],
        password: &str,
    ) -> ExportResult<Self> {
        header.validate()?;
        let key = header.kdf.derive(password)?;
        let cipher = XChaCha20Poly1305::new(key.as_ref().into());
        let prefix = header.nonce_prefix()?;
        Ok(Self {
            inner,
            encryptor: Some(EncryptorBE32::from_aead(cipher, prefix.as_ref().into())),
            aad: aad.to_vec(),
            buffer: Vec::with_capacity(header.chunk_bytes as usize),
            chunk_bytes: header.chunk_bytes as usize,
        })
    }

    fn flush_chunk(&mut self, last: bool) -> ExportResult<()> {
        let Some(encryptor) = self.encryptor.take() else {
            return Err(ExportError::IntegrityFailed);
        };
        let plaintext = std::mem::take(&mut self.buffer);
        let payload = Payload {
            msg: &plaintext,
            aad: &self.aad,
        };
        let (ciphertext, next) = if last {
            (
                encryptor
                    .encrypt_last(payload)
                    .map_err(|_| ExportError::IntegrityFailed)?,
                None,
            )
        } else {
            let mut encryptor = encryptor;
            let ciphertext = encryptor
                .encrypt_next(payload)
                .map_err(|_| ExportError::IntegrityFailed)?;
            (ciphertext, Some(encryptor))
        };
        self.encryptor = next;
        let length = u32::try_from(ciphertext.len()).map_err(|_| ExportError::IntegrityFailed)?;
        self.inner.write_all(&length.to_le_bytes())?;
        self.inner
            .write_all(&[if last { CHUNK_LAST } else { CHUNK_MORE }])?;
        self.inner.write_all(&ciphertext)?;
        Ok(())
    }

    /// Seal the final chunk and return the inner writer.
    pub fn finish(mut self) -> ExportResult<W> {
        self.flush_chunk(true)?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for EncryptingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let take = buf.len().min(self.chunk_bytes - self.buffer.len());
        self.buffer.extend_from_slice(&buf[..take]);
        if self.buffer.len() == self.chunk_bytes {
            self.flush_chunk(false)
                .map_err(|error| std::io::Error::other(error.to_string()))?;
        }
        Ok(take)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // Chunk boundaries are fixed by the construction; a partial chunk is
        // only sealed by `finish`.
        self.inner.flush()
    }
}

/// Reads authenticated chunks and exposes the plaintext body as a `Read`.
pub struct DecryptingReader<R: Read> {
    inner: R,
    decryptor: Option<DecryptorBE32<XChaCha20Poly1305>>,
    aad: Vec<u8>,
    buffer: Vec<u8>,
    position: usize,
    finished: bool,
    max_chunk: usize,
}

impl<R: Read> std::fmt::Debug for DecryptingReader<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecryptingReader")
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl<R: Read> DecryptingReader<R> {
    /// Wrap `inner`, deriving the key from `password`.
    pub fn new(
        inner: R,
        header: &EncryptionHeader,
        aad: &[u8],
        password: &str,
    ) -> ExportResult<Self> {
        header.validate()?;
        let key = header.kdf.derive(password)?;
        let cipher = XChaCha20Poly1305::new(key.as_ref().into());
        let prefix = header.nonce_prefix()?;
        Ok(Self {
            inner,
            decryptor: Some(DecryptorBE32::from_aead(cipher, prefix.as_ref().into())),
            aad: aad.to_vec(),
            buffer: Vec::new(),
            position: 0,
            finished: false,
            max_chunk: header.chunk_bytes as usize + TAG_BYTES,
        })
    }

    fn fill(&mut self) -> std::io::Result<()> {
        if self.finished {
            return Ok(());
        }
        let mut frame = [0u8; 5];
        if !read_exact_or_eof(&mut self.inner, &mut frame)? {
            // The body ended without a sealed final chunk.
            self.finished = true;
            return Err(framing_error());
        }
        let length = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
        let last = frame[4] == CHUNK_LAST;
        if length > self.max_chunk || length < TAG_BYTES {
            self.finished = true;
            return Err(framing_error());
        }
        let mut ciphertext = vec![0u8; length];
        if !read_exact_or_eof(&mut self.inner, &mut ciphertext)? {
            self.finished = true;
            return Err(framing_error());
        }
        let Some(decryptor) = self.decryptor.take() else {
            self.finished = true;
            return Err(framing_error());
        };
        let payload = Payload {
            msg: &ciphertext,
            aad: &self.aad,
        };
        let plaintext = if last {
            let plaintext = decryptor.decrypt_last(payload).map_err(|_| aead_error())?;
            self.finished = true;
            plaintext
        } else {
            let mut decryptor = decryptor;
            let plaintext = decryptor.decrypt_next(payload).map_err(|_| aead_error())?;
            self.decryptor = Some(decryptor);
            plaintext
        };
        self.buffer = plaintext;
        self.position = 0;
        Ok(())
    }
}

impl<R: Read> Read for DecryptingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.position < self.buffer.len() {
                let take = (self.buffer.len() - self.position).min(buf.len());
                buf[..take].copy_from_slice(&self.buffer[self.position..self.position + take]);
                self.position += take;
                return Ok(take);
            }
            if self.finished {
                return Ok(0);
            }
            self.fill()?;
        }
    }
}

fn read_exact_or_eof(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<bool> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = reader.read(&mut buffer[filled..])?;
        if read == 0 {
            if filled == 0 {
                return Ok(false);
            }
            return Err(framing_error());
        }
        filled += read;
    }
    Ok(true)
}

/// The nonce STREAM BE32 uses for chunk `counter`.
///
/// Exposed so the uniqueness invariant can be asserted by a test rather than
/// assumed from the construction's documentation.
#[must_use]
pub fn stream_nonce(prefix: &[u8; NONCE_PREFIX_BYTES], counter: u32, last: bool) -> [u8; 24] {
    let mut nonce = [0u8; 24];
    nonce[..NONCE_PREFIX_BYTES].copy_from_slice(prefix);
    nonce[NONCE_PREFIX_BYTES..NONCE_PREFIX_BYTES + 4].copy_from_slice(&counter.to_be_bytes());
    nonce[23] = u8::from(last);
    nonce
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn test_header() -> EncryptionHeader {
        EncryptionHeader::generate(KdfProfile::Test)
    }

    fn round_trip(plaintext: &[u8], header: &EncryptionHeader, aad: &[u8]) -> Vec<u8> {
        let mut writer = EncryptingWriter::new(Vec::new(), header, aad, "correct horse").unwrap();
        writer.write_all(plaintext).unwrap();
        writer.finish().unwrap()
    }

    fn decrypt(
        body: &[u8],
        header: &EncryptionHeader,
        aad: &[u8],
        password: &str,
    ) -> std::io::Result<Vec<u8>> {
        let mut reader = DecryptingReader::new(body, header, aad, password).unwrap();
        let mut out = Vec::new();
        reader.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn bodies_of_every_size_round_trip() {
        let mut header = test_header();
        header.chunk_bytes = 64;
        for size in [0usize, 1, 63, 64, 65, 200, 1024] {
            let plaintext: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            let body = round_trip(&plaintext, &header, b"header");
            assert_eq!(
                decrypt(&body, &header, b"header", "correct horse").unwrap(),
                plaintext,
                "size {size}"
            );
        }
    }

    #[test]
    fn a_wrong_password_fails_without_returning_plaintext() {
        let header = test_header();
        let body = round_trip(b"secret meeting notes", &header, b"header");
        let error = decrypt(&body, &header, b"header", "wrong").unwrap_err();
        assert!(error.to_string().contains(AEAD_FAILURE));
    }

    #[test]
    fn tampering_with_the_body_or_the_header_fails_authentication() {
        let header = test_header();
        let mut body = round_trip(b"0123456789abcdef0123456789", &header, b"header-bytes");

        let mut flipped = body.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 0x01;
        assert!(decrypt(&flipped, &header, b"header-bytes", "correct horse").is_err());

        // The header is authenticated as AAD: changing one byte of it breaks
        // every chunk even though the ciphertext is untouched.
        assert!(decrypt(&body, &header, b"header-bytez", "correct horse").is_err());

        // Truncation is detected: the final chunk never arrives.
        body.truncate(body.len() - 1);
        assert!(decrypt(&body, &header, b"header-bytes", "correct horse").is_err());
    }

    #[test]
    fn re_marking_a_chunk_as_final_fails_authentication() {
        let mut header = test_header();
        header.chunk_bytes = 16;
        let body = round_trip(&[7u8; 48], &header, b"aad");
        let mut tampered = body.clone();
        // Flip the first chunk's "more" flag to "last".
        tampered[4] = CHUNK_LAST;
        assert!(decrypt(&tampered, &header, b"aad", "correct horse").is_err());
    }

    #[test]
    fn nonces_are_unique_across_chunks_and_across_archives() {
        let prefix = [1u8; NONCE_PREFIX_BYTES];
        let mut seen = BTreeSet::new();
        for counter in 0..10_000u32 {
            assert!(seen.insert(stream_nonce(&prefix, counter, false)));
        }
        assert!(seen.insert(stream_nonce(&prefix, 9_999, true)));

        let a = EncryptionHeader::generate(KdfProfile::Test);
        let b = EncryptionHeader::generate(KdfProfile::Test);
        assert_ne!(a.nonce_prefix_hex, b.nonce_prefix_hex);
        assert_ne!(a.kdf.salt_hex, b.kdf.salt_hex);
    }

    #[test]
    fn abusive_kdf_parameters_are_refused_before_any_work_happens() {
        let mut header = test_header();
        header.kdf.m_cost_kib = MAX_M_COST_KIB + 1;
        let error = header.validate().unwrap_err();
        assert!(matches!(
            error,
            ExportError::ArchiveRejected(ArchiveRejection::KdfParametersRejected(_))
        ));

        let mut header = test_header();
        header.kdf.t_cost = MAX_T_COST + 1;
        assert!(header.validate().is_err());

        let mut header = test_header();
        header.kdf.algorithm = "bcrypt".into();
        assert!(header.validate().is_err());

        let mut header = test_header();
        header.cipher = "aes-ecb".into();
        assert!(header.validate().is_err());
    }

    #[test]
    fn kdf_parameters_round_trip_through_json() {
        let params = KdfParams::generate(KdfProfile::Backup);
        let json = serde_json::to_string(&params).unwrap();
        let back: KdfParams = serde_json::from_str(&json).unwrap();
        assert_eq!(params, back);
        assert_eq!(back.m_cost_kib, 65_536);
        assert_eq!(back.t_cost, 3);
        // An archive that carries an extra field is not silently accepted.
        let extra = json.replace('{', "{\"escrow_key\":\"x\",");
        assert!(serde_json::from_str::<KdfParams>(&extra).is_err());
    }

    #[test]
    fn the_same_password_and_salt_derive_the_same_key_on_any_machine() {
        let params = KdfParams::generate(KdfProfile::Test);
        let a = params.derive("passphrase").unwrap();
        let b = params.derive("passphrase").unwrap();
        assert_eq!(a.as_ref(), b.as_ref());
        let c = params.derive("passphras").unwrap();
        assert_ne!(a.as_ref(), c.as_ref());
    }
}
