//! Local speaker-embedding engine (EPIC P phase 2, ADR-0081).
//!
//! `plaudon-app-core` owns the [`SpeakerEmbedder`] seam and the deterministic
//! double behind it. This crate is the other implementation: a real ONNX
//! checkpoint, its feature front end, and the facts about the two bundle-tier
//! model artifacts — exact file name, exact digest, licence, attribution.
//!
//! Three properties are worth stating before the modules:
//!
//! * **The checkpoint facts are compiled in, and the documents must agree.**
//!   [`checkpoint`] is a plain data module with no feature gate, so the
//!   licence register, `NOTICE` and the fetch script are checked against it by
//!   `plaudon-integration-tests` even in a build that has no inference runtime
//!   at all.
//! * **The runtime is optional and off.** [`onnx`] exists only under the
//!   `onnx` feature, which no shipped configuration enables. ADR-0081 §4 lets
//!   it ship when RTF, peak RSS, startup, DER and EER are recorded on both
//!   reference platforms; one of the two is measured today.
//! * **The front end is first-party.** [`fbank`] computes the Kaldi-compatible
//!   log-Mel features the checkpoint was exported with, in Rust, with no FFI
//!   and no build-time C++ — the measured reason is in that module's
//!   documentation and in `docs/hardware/macos-apple-silicon.md`.
//!
//! [`SpeakerEmbedder`]: plaudon_app_core::voice_profiles::embedder::SpeakerEmbedder

pub mod checkpoint;
pub mod fbank;

#[cfg(feature = "onnx")]
pub mod onnx;
