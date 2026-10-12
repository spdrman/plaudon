//! The encrypted exemplar store: sealing and opening speaker embeddings
//! (`VOICE_PROFILES_SPEC` VP-071, VP-095, VP-132; ADR-0014, ADR-0080;
//! threat-model row TM-045).
//!
//! # What this guarantees, and what it does not
//!
//! An embedding vector reaches persistence only as
//! [`SealedEmbedding`]: XChaCha20-Poly1305 ciphertext, a fresh 24-byte random
//! nonce, and nothing else. The key is a 32-byte value generated locally and
//! held in the OS credential store under
//! [`SecretScope::VoiceProfileKey`]; this type never writes it anywhere else,
//! and there is no plaintext fallback of any kind — a locked or missing vault
//! yields [`VoiceProfileError::StoreLocked`], which the UI renders as a locked
//! *biometric tier* while the name-only roster keeps working.
//!
//! Each ciphertext is bound to its [`ExemplarBinding`] — person, model id,
//! model version and dimensionality — as additional authenticated data. A
//! ciphertext therefore cannot be moved to another person's row, replayed
//! under a different model version, or silently reinterpreted at another
//! dimensionality: all three fail the tag check rather than decrypting into a
//! plausible wrong vector. Matching only ever compares equal model versions
//! (VP-075), and this is the mechanism that makes a mismatch loud.
//!
//! Deleting the key ([`EmbeddingCipher::destroy_key`]) is the "forget
//! everyone" control: every remaining ciphertext, including any inside an
//! already-written encrypted backup, becomes permanently undecryptable
//! (VP-095). It is the only erasure primitive that reaches bytes we no longer
//! control.
//!
//! This is at-rest protection. While a job runs, vectors exist in process
//! memory in plaintext; the documents say so rather than implying otherwise.

use std::sync::Arc;

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use plaudon_domain::ids::PersonId;
use plaudon_security::secrets::{
    SecretScope, SecretStore, SecretStoreError, SecretString, StoreKind,
};
use rand::RngCore;
use zeroize::Zeroizing;

/// Length of the feature-scoped data key.
pub const EMBEDDING_KEY_BYTES: usize = 32;

/// Length of an XChaCha20-Poly1305 nonce.
pub const EMBEDDING_NONCE_BYTES: usize = 24;

/// Largest embedding this build will seal or open. Speaker embeddings are
/// 192-512 dimensional (ADR-0081); the bound exists so a corrupt header cannot
/// make the decoder allocate unboundedly.
pub const MAX_EMBEDDING_DIM: usize = 8192;

/// Result alias for voice-profile operations.
pub type VoiceProfileResult<T> = Result<T, VoiceProfileError>;

/// Why a biometric operation could not happen.
///
/// Every variant is biometric-tier only: none of them is reachable from roster
/// CRUD, which is the point of the two-tier split (spec §12.2A).
#[derive(Debug, thiserror::Error)]
pub enum VoiceProfileError {
    /// The OS credential store is locked, unavailable or unsupported.
    ///
    /// This locks the biometric tier — enrol, embed, match, decrypt — and
    /// nothing else. The roster, manual assignment and every existing name
    /// snapshot keep working, because none of them needs a key (ADR-0014 +
    /// spec §12.2A).
    #[error("the voice-profile key store is locked: {0}")]
    StoreLocked(#[source] SecretStoreError),
    /// The credential store failed for a reason that is not a locked state.
    #[error("the voice-profile key store failed: {0}")]
    StoreFailed(#[source] SecretStoreError),
    /// A stored key is not a 32-byte value: refuse rather than pad or truncate.
    #[error("the stored voice-profile key is malformed")]
    MalformedKey,
    /// The ciphertext, its nonce or its binding did not authenticate.
    ///
    /// A tampered ciphertext, a ciphertext moved to another person, a changed
    /// model binding and a wrong key are deliberately indistinguishable here:
    /// that is what authenticated encryption promises.
    #[error("the stored embedding did not authenticate")]
    Tampered,
    /// The vector or its parameters are outside what this build will handle.
    #[error("the embedding is malformed: {0}")]
    Malformed(String),
    /// The library failed.
    ///
    /// Present because the tier's lifecycle operations — wholesale erasure
    /// (VP-095) and retention expiry (VP-130) — touch rows as well as the key,
    /// and a caller handling one failure mode should not have to handle two
    /// unrelated error types to do it.
    #[error(transparent)]
    Persistence(#[from] plaudon_persistence::PersistenceError),
}

impl VoiceProfileError {
    /// Whether this is the locked-store state the UI renders as "the biometric
    /// tier is unavailable; the roster is not".
    #[must_use]
    pub const fn is_store_locked(&self) -> bool {
        matches!(self, VoiceProfileError::StoreLocked(_))
    }

    fn from_store(error: SecretStoreError) -> Self {
        if error.is_locked_state() {
            Self::StoreLocked(error)
        } else {
            Self::StoreFailed(error)
        }
    }
}

impl From<VoiceProfileError> for plaudon_domain::error::AppError {
    /// Map a biometric-tier failure onto the stable error vocabulary.
    ///
    /// A locked vault is its own code because the UI reacts to it differently:
    /// it is a "unlock your credential store" state, not a failure of the
    /// feature — the roster keeps working either way (ADR-0014).
    fn from(error: VoiceProfileError) -> Self {
        use plaudon_domain::error::AppErrorCode;
        let code = match &error {
            VoiceProfileError::StoreLocked(_) => AppErrorCode::SecretStoreLocked,
            VoiceProfileError::StoreFailed(_)
            | VoiceProfileError::MalformedKey
            | VoiceProfileError::Malformed(_) => AppErrorCode::InternalError,
            VoiceProfileError::Tampered => AppErrorCode::IntegrityFailed,
            VoiceProfileError::Persistence(inner) => inner.code(),
        };
        Self::new(code, error.to_string())
    }
}

/// What a ciphertext is bound to. Carried as AEAD additional data, so the
/// binding cannot be edited in the database without destroying the ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExemplarBinding {
    /// Person the exemplar belongs to.
    pub person_id: PersonId,
    /// Embedding model identity.
    pub model_id: String,
    /// Embedding model version.
    pub model_version: String,
    /// Dimensionality of the vector.
    pub embedding_dim: u32,
}

impl ExemplarBinding {
    /// The exact bytes authenticated alongside the ciphertext.
    ///
    /// Fields are separated by `0x1f`, which cannot occur in a UUID, a model id
    /// or a decimal number, so no two distinct bindings can produce the same
    /// additional data.
    fn aad(&self) -> Vec<u8> {
        let mut aad = Vec::with_capacity(96);
        aad.extend_from_slice(AAD_DOMAIN);
        aad.push(FIELD_SEPARATOR);
        aad.extend_from_slice(self.person_id.to_string().as_bytes());
        aad.push(FIELD_SEPARATOR);
        aad.extend_from_slice(self.model_id.as_bytes());
        aad.push(FIELD_SEPARATOR);
        aad.extend_from_slice(self.model_version.as_bytes());
        aad.push(FIELD_SEPARATOR);
        aad.extend_from_slice(self.embedding_dim.to_string().as_bytes());
        aad
    }
}

/// Domain separator, so a ciphertext from this construction can never verify
/// under another part of the application.
const AAD_DOMAIN: &[u8] = b"plaudon.voice-profile.exemplar.v1";

/// Field separator inside the additional data.
const FIELD_SEPARATOR: u8 = 0x1f;

/// A sealed embedding, exactly as it goes into `voice_exemplars`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedEmbedding {
    /// XChaCha20-Poly1305 ciphertext of the little-endian `f32` vector.
    pub cipher: Vec<u8>,
    /// The nonce it was sealed with.
    pub nonce: Vec<u8>,
    /// Dimensionality, repeated in the clear for query planning. It is also
    /// authenticated through [`ExemplarBinding`], so a tampered copy of this
    /// number cannot survive an [`EmbeddingCipher::open`].
    pub embedding_dim: u32,
}

/// Seals and opens speaker embeddings under the library's data key.
#[derive(Clone)]
pub struct EmbeddingCipher {
    store: Arc<dyn SecretStore>,
}

impl std::fmt::Debug for EmbeddingCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddingCipher")
            .field("store", &self.store.kind())
            .finish()
    }
}

impl EmbeddingCipher {
    /// Build a cipher over an OS credential store.
    #[must_use]
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }

    /// Which credential backend holds the key.
    #[must_use]
    pub fn store_kind(&self) -> StoreKind {
        self.store.kind()
    }

    /// Whether the biometric tier is usable right now, i.e. whether the vault
    /// answers at all. Never creates a key.
    ///
    /// # Errors
    ///
    /// [`VoiceProfileError::StoreLocked`] when the vault is locked or absent.
    pub fn check_available(&self) -> VoiceProfileResult<bool> {
        match self.store.status(&SecretScope::VoiceProfileKey) {
            Ok(presence) => Ok(presence.present),
            Err(error) => Err(VoiceProfileError::from_store(error)),
        }
    }

    /// Seal one embedding.
    ///
    /// # Errors
    ///
    /// [`VoiceProfileError::StoreLocked`] when the vault is unavailable — the
    /// biometric tier is then locked and nothing is written —
    /// [`VoiceProfileError::Malformed`] for an empty or over-long vector.
    pub fn seal(
        &self,
        embedding: &[f32],
        binding: &ExemplarBinding,
    ) -> VoiceProfileResult<SealedEmbedding> {
        if embedding.is_empty() {
            return Err(VoiceProfileError::Malformed(
                "an embedding has at least one dimension".to_owned(),
            ));
        }
        if embedding.len() > MAX_EMBEDDING_DIM {
            return Err(VoiceProfileError::Malformed(format!(
                "an embedding has at most {MAX_EMBEDDING_DIM} dimensions, got {}",
                embedding.len()
            )));
        }
        let dim = u32::try_from(embedding.len()).unwrap_or(u32::MAX);
        if binding.embedding_dim != dim {
            return Err(VoiceProfileError::Malformed(format!(
                "the binding says {} dimensions, the vector has {dim}",
                binding.embedding_dim
            )));
        }

        let key = self.load_or_create_key()?;
        let cipher = XChaCha20Poly1305::new(key.as_slice().into());
        let mut nonce = [0u8; EMBEDDING_NONCE_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut nonce);

        let plaintext = Zeroizing::new(encode_vector(embedding));
        let aad = binding.aad();
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_slice(),
                    aad: &aad,
                },
            )
            .map_err(|_| VoiceProfileError::Tampered)?;

        Ok(SealedEmbedding {
            cipher: ciphertext,
            nonce: nonce.to_vec(),
            embedding_dim: dim,
        })
    }

    /// Open one sealed embedding.
    ///
    /// # Errors
    ///
    /// [`VoiceProfileError::StoreLocked`] when the vault is unavailable,
    /// [`VoiceProfileError::Tampered`] when the ciphertext, nonce or binding
    /// does not authenticate, [`VoiceProfileError::Malformed`] when the
    /// plaintext is not a whole `f32` vector of the bound length.
    pub fn open(
        &self,
        sealed: &SealedEmbedding,
        binding: &ExemplarBinding,
    ) -> VoiceProfileResult<Zeroizing<Vec<f32>>> {
        if sealed.nonce.len() != EMBEDDING_NONCE_BYTES {
            return Err(VoiceProfileError::Malformed(format!(
                "a nonce is {EMBEDDING_NONCE_BYTES} bytes, got {}",
                sealed.nonce.len()
            )));
        }
        if binding.embedding_dim != sealed.embedding_dim {
            return Err(VoiceProfileError::Malformed(
                "the binding and the stored row disagree about dimensionality".to_owned(),
            ));
        }

        let key = self.load_key()?.ok_or(VoiceProfileError::Tampered)?;
        let cipher = XChaCha20Poly1305::new(key.as_slice().into());
        let aad = binding.aad();
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    XNonce::from_slice(&sealed.nonce),
                    Payload {
                        msg: sealed.cipher.as_slice(),
                        aad: &aad,
                    },
                )
                .map_err(|_| VoiceProfileError::Tampered)?,
        );

        let vector = decode_vector(&plaintext)?;
        if vector.len() != sealed.embedding_dim as usize {
            return Err(VoiceProfileError::Malformed(
                "the decrypted vector is not the length it was stored with".to_owned(),
            ));
        }
        Ok(Zeroizing::new(vector))
    }

    /// Destroy the data key: cryptographic erasure of every ciphertext that
    /// exists anywhere, including inside backups already written (VP-095).
    ///
    /// Destroying a key that was never created is not an error — "forget
    /// everyone" must be idempotent, and a user who runs it twice has the
    /// outcome they asked for both times.
    ///
    /// # Errors
    ///
    /// [`VoiceProfileError::StoreLocked`] when the vault is unavailable.
    pub fn destroy_key(&self) -> VoiceProfileResult<()> {
        match self.store.delete(&SecretScope::VoiceProfileKey) {
            Ok(()) | Err(SecretStoreError::NotFound) => Ok(()),
            Err(error) => Err(VoiceProfileError::from_store(error)),
        }
    }

    /// Read the key, or `None` when none has been created yet.
    ///
    /// The vault stores text, so the key travels as lowercase hex: 64
    /// characters in, 32 bytes out, and anything else is refused rather than
    /// padded into something that would decrypt into noise.
    fn load_key(&self) -> VoiceProfileResult<Option<Zeroizing<Vec<u8>>>> {
        match self.store.load(&SecretScope::VoiceProfileKey) {
            Ok(secret) => {
                let decoded =
                    hex::decode(secret.expose()).map_err(|_| VoiceProfileError::MalformedKey)?;
                let bytes = Zeroizing::new(decoded);
                if bytes.len() != EMBEDDING_KEY_BYTES {
                    return Err(VoiceProfileError::MalformedKey);
                }
                Ok(Some(bytes))
            }
            Err(SecretStoreError::NotFound) => Ok(None),
            Err(error) => Err(VoiceProfileError::from_store(error)),
        }
    }

    /// Read the key, generating one on first use.
    ///
    /// Generation happens here rather than at activation so that a library that
    /// never enrols anybody never has a voice-profile key at all.
    fn load_or_create_key(&self) -> VoiceProfileResult<Zeroizing<Vec<u8>>> {
        if let Some(existing) = self.load_key()? {
            return Ok(existing);
        }
        let mut fresh = Zeroizing::new(vec![0u8; EMBEDDING_KEY_BYTES]);
        rand::rngs::OsRng.fill_bytes(fresh.as_mut_slice());
        let secret = SecretString::new(Zeroizing::new(hex::encode(fresh.as_slice())).as_str());
        self.store
            .store(&SecretScope::VoiceProfileKey, &secret)
            .map_err(VoiceProfileError::from_store)?;
        Ok(fresh)
    }
}

/// Little-endian `f32` encoding. Explicit, so the bytes do not depend on the
/// host's endianness: a library copied between machines still decrypts.
fn encode_vector(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for value in embedding {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

fn decode_vector(bytes: &[u8]) -> VoiceProfileResult<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(VoiceProfileError::Malformed(
            "the decrypted vector is not a whole number of f32 values".to_owned(),
        ));
    }
    if bytes.len() / 4 > MAX_EMBEDDING_DIM {
        return Err(VoiceProfileError::Malformed(
            "the decrypted vector exceeds the supported dimensionality".to_owned(),
        ));
    }
    Ok(bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect())
}
