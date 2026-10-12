//! Voice profiles: the policy layer over the people roster and its optional
//! biometric tier (`VOICE_PROFILES_SPEC`, ADR-0080, ADR-0082).
//!
//! # The two tiers, and why the split is in the type system
//!
//! The roster — `plaudon_persistence::people::PeopleRepository` — is a contact
//! list: typed names, aliases, optional email and organisation. It is ordinary
//! personal data (spec §12.2A), it needs no consent gate, no retention clock
//! and no key, and it keeps working when the OS credential store is locked or
//! absent. Nothing in this module is required to use it.
//!
//! The biometric tier — enrolments, exemplars, suggestions — only exists after
//! the consent gate, and an embedding only ever exists at rest as
//! XChaCha20-Poly1305 ciphertext under the library's
//! [`SecretScope::VoiceProfileKey`](plaudon_security::secrets::SecretScope::VoiceProfileKey).
//! That is what [`store`] implements. The key never leaves the OS vault except
//! into a zeroizing buffer, there is no file-backed fallback, and an
//! unavailable vault produces [`VoiceProfileError::StoreLocked`] — ADR-0014's
//! rule, applied to this feature.
//!
//! # Honest limits
//!
//! Encryption here protects data **at rest**, not data in use: while a match
//! job runs, plaintext vectors exist in process memory, and a privileged
//! attacker on an unlocked machine can read them. The control that matters
//! against that attacker is the platform's own (FileVault/BitLocker, ADR-0402),
//! plus the fact that the feature is off by default and computes nothing until
//! a user turns it on.

pub mod store;

pub use store::{
    EmbeddingCipher, ExemplarBinding, SealedEmbedding, VoiceProfileError, VoiceProfileResult,
    EMBEDDING_KEY_BYTES, EMBEDDING_NONCE_BYTES, MAX_EMBEDDING_DIM,
};
