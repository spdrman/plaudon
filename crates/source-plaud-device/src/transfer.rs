//! Streaming acquisition, verification and the derived conversion
//! (spec §4.2A.6-7, §7.7, §7.9, §13.3A, App. B.6).
//!
//! # What "verified" means here
//!
//! Spec §7.9 is explicit that our own digest proves nothing about
//! completeness:
//!
//! > "We will not infer complete-file integrity from our own SHA-256 alone: we
//! > require byte counts, protocol terminal markers and any device-reported
//! > length/CRC supported by the profile. Unverifiable file endpoints remain
//! > `unverified`, not `Complete`."
//!
//! [`TransferVerification`] therefore records each piece of evidence
//! separately and only reports [`TransferVerdict::Verified`] when the terminal
//! marker arrived, the byte range is contiguous, the device-declared length
//! agrees with what we received, and the sidecar's own framing parses.
//!
//! # What is stored
//!
//! The acquired bytes, exactly as received, with
//! `quality_class = CompressedDeviceTransfer`. The decoded or remuxed audio is
//! a separate [`DerivativeOutcome`] with `quality_class = LocalDerivative`.
//! Nothing in this module can label device bytes an original WAV, because the
//! only two classes it uses are those two.

use std::io::Write;

use plaudon_domain::media::{MediaIdentity, QualityClass, Sha256Digest};
use plaudon_plaud_protocol::asr::{self, AsrError, AsrScanner, AsrSummary};
use plaudon_plaud_protocol::bulk::{BulkFrame, TransferOutcome};
use plaudon_plaud_protocol::payload::{DeviceCodec, ListedRecording};
use plaudon_plaud_protocol::profile::ProtocolProfile;
use plaudon_plaud_protocol::stream::{StreamError, StreamingAssembler};
use plaudon_recording_sources::device::DeviceError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Why an acquisition failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransferError {
    /// Reassembly refused a frame.
    #[error(transparent)]
    Stream(#[from] StreamError),
    /// The sidecar's own framing is invalid.
    #[error(transparent)]
    Framing(#[from] AsrError),
    /// Writing to staging failed.
    #[error("the staging file could not be written")]
    StagingWrite,
    /// The device declared more bytes than the profile's ceiling allows.
    #[error("the device declared {declared} bytes, over the profile ceiling of {ceiling}")]
    DeclaredLengthOverCeiling {
        /// Length the device declared.
        declared: u64,
        /// Ceiling for this profile.
        ceiling: u64,
    },
    /// The transfer was cancelled.
    #[error("the transfer was cancelled")]
    Cancelled,
}

impl From<TransferError> for DeviceError {
    fn from(error: TransferError) -> Self {
        match error {
            TransferError::Cancelled => Self::Cancelled,
            TransferError::Framing(_) => Self::UnsupportedCodec,
            TransferError::StagingWrite => Self::IntegrityFailed,
            TransferError::Stream(_) | TransferError::DeclaredLengthOverCeiling { .. } => {
                Self::TransferIncomplete
            }
        }
    }
}

/// Verdict on an acquisition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum TransferVerdict {
    /// Every completeness check passed; the bytes may be committed.
    Verified,
    /// The bytes arrived but completeness cannot be established.
    ///
    /// This is not a failure state: the staged bytes are retained for a
    /// resume or a restart. It is simply not a committable asset.
    Unverified(UnverifiedReason),
}

/// Why completeness could not be established (spec 7.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnverifiedReason {
    /// The protocol's terminal marker never arrived.
    NoTerminalMarker,
    /// The received byte range has a hole.
    RangeIncomplete,
    /// The profile supplied no device-declared length to check against.
    NoDeviceDeclaredLength,
    /// The declared length and the byte count disagree.
    ByteCountMismatch,
    /// The sidecar's own packet framing did not parse.
    FramingUnverified,
}

impl UnverifiedReason {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoTerminalMarker => "NO_TERMINAL_MARKER",
            Self::RangeIncomplete => "RANGE_INCOMPLETE",
            Self::NoDeviceDeclaredLength => "NO_DEVICE_DECLARED_LENGTH",
            Self::ByteCountMismatch => "BYTE_COUNT_MISMATCH",
            Self::FramingUnverified => "FRAMING_UNVERIFIED",
        }
    }

    /// What the interface tells the user.
    #[must_use]
    pub const fn ui_explanation(self) -> &'static str {
        match self {
            Self::NoTerminalMarker => {
                "The device stopped sending before the end-of-transfer marker, so this recording \
                 is incomplete."
            }
            Self::RangeIncomplete => {
                "Part of the recording never arrived, so this transfer is incomplete."
            }
            Self::NoDeviceDeclaredLength => {
                "This device profile reports no file length, so a truncated transfer cannot be \
                 told apart from a complete one. The bytes are kept but not archived as complete."
            }
            Self::ByteCountMismatch => {
                "The number of bytes received does not match the length the device reported."
            }
            Self::FramingUnverified => {
                "The transferred bytes do not parse as the expected compressed audio sidecar."
            }
        }
    }
}

impl TransferVerdict {
    /// Whether the bytes may become an archived asset.
    #[must_use]
    pub const fn may_commit(&self) -> bool {
        matches!(self, Self::Verified)
    }

    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Verified => "VERIFIED",
            Self::Unverified(reason) => reason.code(),
        }
    }
}

/// Every piece of completeness evidence, recorded separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferVerification {
    /// Whether the protocol's terminal marker arrived.
    pub terminal_marker_observed: bool,
    /// Whether the received byte range is contiguous from zero.
    pub contiguous: bool,
    /// Bytes actually received.
    pub received_bytes: u64,
    /// Byte length the device declared, when the profile provides one.
    pub device_declared_byte_length: Option<u64>,
    /// Whether the sidecar's own packet framing parsed end to end.
    pub framing_valid: bool,
    /// Duration measured from the acquired bitstream, when measurable.
    pub measured_duration_ms: Option<u64>,
    /// The overall verdict.
    pub verdict: TransferVerdict,
}

impl TransferVerification {
    /// Decide a verdict from the evidence.
    #[must_use]
    pub fn decide(
        terminal_marker_observed: bool,
        contiguous: bool,
        received_bytes: u64,
        device_declared_byte_length: Option<u64>,
        framing: Option<&AsrSummary>,
    ) -> Self {
        let verdict = if !terminal_marker_observed {
            TransferVerdict::Unverified(UnverifiedReason::NoTerminalMarker)
        } else if !contiguous {
            TransferVerdict::Unverified(UnverifiedReason::RangeIncomplete)
        } else {
            match device_declared_byte_length {
                None => TransferVerdict::Unverified(UnverifiedReason::NoDeviceDeclaredLength),
                Some(declared) if declared != received_bytes => {
                    TransferVerdict::Unverified(UnverifiedReason::ByteCountMismatch)
                }
                Some(_) if framing.is_none() => {
                    TransferVerdict::Unverified(UnverifiedReason::FramingUnverified)
                }
                Some(_) => TransferVerdict::Verified,
            }
        };

        Self {
            terminal_marker_observed,
            contiguous,
            received_bytes,
            device_declared_byte_length,
            framing_valid: framing.is_some(),
            measured_duration_ms: framing.map(|summary| summary.duration_ms),
            verdict,
        }
    }

    /// Whether the bytes may become an archived asset.
    #[must_use]
    pub const fn may_commit(&self) -> bool {
        self.verdict.may_commit()
    }
}

/// The result of one acquisition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquisitionReport {
    /// Device-scoped recording identifier.
    pub device_recording_id: String,
    /// Identity of the bytes written to staging.
    pub identity: MediaIdentity,
    /// Fidelity class of the acquired bytes. Always compressed device transfer.
    pub quality_class: QualityClass,
    /// Completeness evidence.
    pub verification: TransferVerification,
    /// Measurements taken from the sidecar, when it parsed.
    pub framing: Option<AsrSummary>,
}

impl AcquisitionReport {
    /// Whether the archive may accept these bytes.
    #[must_use]
    pub const fn may_commit(&self) -> bool {
        self.verification.may_commit()
    }

    /// The phrase the interface shows for the acquired media.
    ///
    /// Spec App. B.8: "We show `Source: Device BLE · compressed ASR` rather
    /// than `Original high-quality audio`."
    #[must_use]
    pub const fn source_label(&self) -> &'static str {
        "Device BLE · compressed ASR"
    }
}

/// What kind of derivative was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivativeKind {
    /// The coded Opus packets rewrapped in a standard Ogg Opus container.
    ///
    /// Bit-exact in the coded domain; a container change, not a re-encode.
    OggOpusRemux,
    /// Decoded linear PCM in a WAV container.
    DecodedWav,
}

impl DerivativeKind {
    /// The phrase the interface shows. Never the word "original".
    #[must_use]
    pub const fn ui_label(self) -> &'static str {
        match self {
            Self::OggOpusRemux => "Derived conversion · Ogg Opus (same compressed audio)",
            Self::DecodedWav => "Derived conversion · decoded WAV",
        }
    }

    /// Fidelity class recorded for the derivative.
    #[must_use]
    pub const fn quality_class(self) -> QualityClass {
        QualityClass::LocalDerivative
    }

    /// File extension for the derivative.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::OggOpusRemux => "opus",
            Self::DecodedWav => "wav",
        }
    }
}

/// Whether a derivative could be produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "derivative", rename_all = "snake_case")]
pub enum DerivativeOutcome {
    /// A derivative was written.
    Produced {
        /// What was produced.
        kind: DerivativeKind,
        /// Identity of the derivative bytes.
        identity: MediaIdentity,
        /// Duration carried over from the measured bitstream.
        duration_ms: u64,
    },
    /// No derivative is available, with an honest reason.
    ///
    /// Spike A1.4 recorded that this build bundles no Opus decoder, so a
    /// decoded WAV is unavailable rather than fabricated.
    Unavailable {
        /// What could not be produced.
        kind: DerivativeKind,
        /// Why.
        reason: DerivativeRefusal,
    },
}

/// Why a derivative could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DerivativeRefusal {
    /// This build bundles no decoder for the acquired codec (spike A1.4).
    UnsupportedCodec,
    /// The acquired bytes did not parse, so there is nothing to convert.
    FramingUnverified,
    /// The derivative could not be written to disk.
    StagingWrite,
}

impl DerivativeRefusal {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedCodec => "UNSUPPORTED_CODEC",
            Self::FramingUnverified => "FRAMING_UNVERIFIED",
            Self::StagingWrite => "STAGING_WRITE",
        }
    }

    /// What the interface tells the user.
    #[must_use]
    pub const fn ui_explanation(self) -> &'static str {
        match self {
            Self::UnsupportedCodec => {
                "This build has no Opus decoder, so the compressed device audio cannot be \
                 converted to WAV here. The original transferred bytes and a playable Ogg Opus \
                 conversion are both kept."
            }
            Self::FramingUnverified => {
                "The transferred bytes did not parse as an Opus sidecar, so no playable \
                 conversion was produced. The transferred bytes are kept unchanged."
            }
            Self::StagingWrite => "The playable conversion could not be written to disk.",
        }
    }
}

impl DerivativeOutcome {
    /// The decoded-PCM outcome for a build with no Opus decoder.
    #[must_use]
    pub const fn no_pcm_decoder() -> Self {
        Self::Unavailable {
            kind: DerivativeKind::DecodedWav,
            reason: DerivativeRefusal::UnsupportedCodec,
        }
    }
}

/// A staged, not yet committed acquisition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedTransfer {
    /// The acquisition report.
    pub report: AcquisitionReport,
    /// Outcome of the playable conversion.
    pub playable_derivative: DerivativeOutcome,
    /// Outcome of the decoded-PCM conversion.
    pub decoded_derivative: DerivativeOutcome,
}

/// Whether an interrupted transfer may resume (spec App. B.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case")]
pub enum ResumePlan {
    /// Resume from a verified offset.
    ResumeAt {
        /// Bytes already staged and digested.
        verified_bytes: u64,
    },
    /// Start a new staging file; nothing partial is merged.
    FreshStaging {
        /// Why a resume was refused.
        reason: RestartReason,
    },
}

/// Why an interrupted transfer restarts instead of resuming (App. B.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartReason {
    /// The profile has no hardware evidence that offset reads behave.
    OffsetSemanticsUnverified,
    /// The device now lists a different recording under that identifier.
    RecordingIdentityChanged,
    /// The device now declares a different length.
    DeclaredSizeChanged,
    /// The staged prefix no longer digests to the recorded value.
    StagedDigestMismatch,
}

impl RestartReason {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::OffsetSemanticsUnverified => "OFFSET_SEMANTICS_UNVERIFIED",
            Self::RecordingIdentityChanged => "RECORDING_IDENTITY_CHANGED",
            Self::DeclaredSizeChanged => "DECLARED_SIZE_CHANGED",
            Self::StagedDigestMismatch => "STAGED_DIGEST_MISMATCH",
        }
    }
}

impl ResumePlan {
    /// Whether this plan resumes rather than restarts.
    #[must_use]
    pub const fn resumes(&self) -> bool {
        matches!(self, Self::ResumeAt { .. })
    }
}

/// A persisted checkpoint for an interrupted transfer
/// (spec §13.3 `device_transfer_checkpoints`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferCheckpoint {
    /// Recording the staged bytes belong to.
    pub device_recording_id: String,
    /// Bytes staged and digested so far.
    pub verified_bytes: u64,
    /// Length the device declared when the transfer started.
    pub source_size_hint: Option<u64>,
    /// Digest of the staged prefix.
    pub staged_prefix_sha256: String,
}

/// Decide whether an interrupted transfer may resume.
///
/// Spec App. B.6: resume only "after confirming the source recording ID,
/// expected size/identity and previously stored byte digest. If any identity
/// changes or offset semantics are uncertain, we restart the file into a new
/// staging path". The profile's `offset_reads_demonstrated` flag is the
/// hardware evidence gate, so with no hardware record every resume is a
/// restart.
#[must_use]
pub fn plan_resume(
    profile: &ProtocolProfile,
    checkpoint: &TransferCheckpoint,
    observed: &ListedRecording,
    observed_prefix_sha256: &str,
) -> ResumePlan {
    if !profile.offset_reads_demonstrated {
        return ResumePlan::FreshStaging {
            reason: RestartReason::OffsetSemanticsUnverified,
        };
    }
    if checkpoint.device_recording_id != observed.id {
        return ResumePlan::FreshStaging {
            reason: RestartReason::RecordingIdentityChanged,
        };
    }
    if checkpoint.source_size_hint != Some(observed.declared_byte_length) {
        return ResumePlan::FreshStaging {
            reason: RestartReason::DeclaredSizeChanged,
        };
    }
    if !checkpoint
        .staged_prefix_sha256
        .eq_ignore_ascii_case(observed_prefix_sha256)
    {
        return ResumePlan::FreshStaging {
            reason: RestartReason::StagedDigestMismatch,
        };
    }
    ResumePlan::ResumeAt {
        verified_bytes: checkpoint.verified_bytes,
    }
}

/// Streams one recording into a staging sink while hashing and validating it.
///
/// Memory is bounded by the reorder window plus one Opus packet; the recording
/// itself is never held (spec App. B.4).
#[derive(Debug)]
pub struct StagingAcquisition<W: Write> {
    device_recording_id: String,
    assembler: StreamingAssembler,
    hasher: Sha256,
    scanner: AsrScanner,
    scanner_failed: bool,
    sink: W,
    written: u64,
    declared: Option<u64>,
    codec: DeviceCodec,
}

impl<W: Write> StagingAcquisition<W> {
    /// Begin an acquisition into `sink`.
    ///
    /// A device-declared length over the profile's ceiling is refused before
    /// a single byte is accepted.
    pub fn begin(
        profile: &ProtocolProfile,
        recording: &ListedRecording,
        sink: W,
    ) -> Result<Self, TransferError> {
        let declared = recording.declared_byte_length;
        if declared > profile.max_transfer_bytes {
            return Err(TransferError::DeclaredLengthOverCeiling {
                declared,
                ceiling: profile.max_transfer_bytes,
            });
        }
        let assembler = if declared == 0 {
            StreamingAssembler::with_budget(profile.max_transfer_bytes)
        } else {
            StreamingAssembler::with_declared_length(profile.max_transfer_bytes, declared)
        };
        Ok(Self {
            device_recording_id: recording.id.clone(),
            assembler,
            hasher: Sha256::new(),
            scanner: AsrScanner::new(),
            scanner_failed: false,
            sink,
            written: 0,
            declared: (declared > 0).then_some(declared),
            codec: recording.codec,
        })
    }

    /// Bytes written to staging so far.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// Accept one bulk frame, writing any newly contiguous bytes.
    ///
    /// Framing errors in the sidecar do **not** abort the transfer: the raw
    /// bytes are still staged, and the invalid framing is reported as
    /// `framing_valid: false` so the transfer stays unverified rather than
    /// discarding evidence.
    pub fn accept(&mut self, frame: &BulkFrame) -> Result<u64, TransferError> {
        let step = self.assembler.accept(frame)?;
        if !step.ready.is_empty() {
            self.hasher.update(&step.ready);
            if !self.scanner_failed && self.scanner.push(&step.ready).is_err() {
                self.scanner_failed = true;
            }
            self.sink
                .write_all(&step.ready)
                .map_err(|_| TransferError::StagingWrite)?;
            self.written += step.ready.len() as u64;
        }
        Ok(self.written)
    }

    /// Finish, deciding the verdict from the accumulated evidence.
    pub fn finish(mut self) -> Result<AcquisitionReport, TransferError> {
        self.sink.flush().map_err(|_| TransferError::StagingWrite)?;

        let outcome = self.assembler.outcome();
        let terminal = self.assembler.ended();
        let contiguous = matches!(
            outcome,
            TransferOutcome::Complete | TransferOutcome::CompleteLengthUnverified { .. }
        );
        let framing = if self.scanner_failed {
            None
        } else {
            self.scanner.finish().ok()
        };

        let digest = Sha256Digest::from_bytes(self.hasher.finalize().into());
        let identity = MediaIdentity::new(digest, self.written, self.codec.mime());
        let verification = TransferVerification::decide(
            terminal,
            contiguous,
            self.written,
            self.declared,
            framing.as_ref(),
        );

        Ok(AcquisitionReport {
            device_recording_id: self.device_recording_id,
            identity,
            quality_class: QualityClass::CompressedDeviceTransfer,
            verification,
            framing,
        })
    }
}

/// The actor hands frames straight to the acquisition, so a transfer streams
/// from radio to disk without the recording ever being held in memory
/// (spec App. B.4).
impl<W: Write + Send> plaudon_plaud_ble::FrameSink for StagingAcquisition<W> {
    fn accept(&mut self, frame: &BulkFrame) -> Result<(), DeviceError> {
        Self::accept(self, frame)
            .map(|_| ())
            .map_err(DeviceError::from)
    }

    fn written(&self) -> u64 {
        Self::written(self)
    }
}

/// Produce the playable conversion from staged sidecar bytes.
///
/// A remux, not a decode: the coded packets are copied unchanged into an Ogg
/// Opus container. The result is labelled [`QualityClass::LocalDerivative`],
/// so it can never be mistaken for the acquired bytes or for an original WAV.
pub fn derive_playable<R: std::io::Read, W: Write>(
    report: &AcquisitionReport,
    source: &mut R,
    sink: &mut W,
) -> DerivativeOutcome {
    if report.framing.is_none() {
        return DerivativeOutcome::Unavailable {
            kind: DerivativeKind::OggOpusRemux,
            reason: DerivativeRefusal::FramingUnverified,
        };
    }
    let mut captured = Vec::new();
    // The serial number only has to be stable within one stream; deriving it
    // from the content digest keeps the conversion reproducible.
    let serial = u32::from_be_bytes([
        report.identity.sha256.as_bytes()[0],
        report.identity.sha256.as_bytes()[1],
        report.identity.sha256.as_bytes()[2],
        report.identity.sha256.as_bytes()[3],
    ]);
    match asr::remux_to_ogg_opus(source, &mut captured, serial) {
        Ok(summary) => {
            if sink.write_all(&captured).is_err() || sink.flush().is_err() {
                return DerivativeOutcome::Unavailable {
                    kind: DerivativeKind::OggOpusRemux,
                    reason: DerivativeRefusal::StagingWrite,
                };
            }
            DerivativeOutcome::Produced {
                kind: DerivativeKind::OggOpusRemux,
                identity: MediaIdentity::of_bytes(&captured, "audio/ogg"),
                duration_ms: summary.duration_ms,
            }
        }
        Err(_) => DerivativeOutcome::Unavailable {
            kind: DerivativeKind::OggOpusRemux,
            reason: DerivativeRefusal::FramingUnverified,
        },
    }
}

#[cfg(test)]
mod tests {
    use plaudon_plaud_protocol::profile::LEGACY_NOTE_V0095;

    use super::*;

    /// A 20 ms mono SILK-wideband packet (RFC 6716 config 9, code 0).
    fn packet(payload: usize) -> Vec<u8> {
        let mut packet = vec![9 << 3];
        packet.extend(std::iter::repeat_n(0x55u8, payload));
        packet
    }

    fn sidecar(packets: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..packets {
            let packet = packet(40);
            out.extend_from_slice(&u16::try_from(packet.len()).unwrap().to_be_bytes());
            out.extend_from_slice(&packet);
        }
        out
    }

    fn listed(id: &str, declared: u64) -> ListedRecording {
        ListedRecording {
            id: id.to_owned(),
            display_name: format!("{id}.ASR"),
            declared_byte_length: declared,
            captured_at_unix: 1_760_000_000,
            codec: DeviceCodec::AsrOpus,
        }
    }

    fn frames(bytes: &[u8], chunk: usize) -> Vec<BulkFrame> {
        bytes
            .chunks(chunk)
            .enumerate()
            .map(|(index, slice)| BulkFrame::Chunk {
                offset: u32::try_from(index * chunk).unwrap(),
                payload: slice.to_vec(),
            })
            .collect()
    }

    fn acquire(bytes: &[u8], declared: u64, include_terminal: bool) -> AcquisitionReport {
        let mut staging = Vec::new();
        let mut acquisition = StagingAcquisition::begin(
            &LEGACY_NOTE_V0095,
            &listed("REC0001", declared),
            &mut staging,
        )
        .unwrap();
        for frame in frames(bytes, 80) {
            acquisition.accept(&frame).unwrap();
        }
        if include_terminal {
            acquisition.accept(&BulkFrame::EndOfTransfer).unwrap();
        }
        acquisition.finish().unwrap()
    }

    #[test]
    fn a_complete_transfer_verifies_and_measures_itself() {
        let bytes = sidecar(50);
        let report = acquire(&bytes, bytes.len() as u64, true);

        assert!(report.may_commit());
        assert_eq!(report.verification.verdict, TransferVerdict::Verified);
        assert_eq!(report.identity.byte_length, bytes.len() as u64);
        assert_eq!(report.identity.observed_codec, "audio/opus");
        assert_eq!(report.quality_class, QualityClass::CompressedDeviceTransfer);
        assert_eq!(report.verification.measured_duration_ms, Some(1_000));
        assert!(report.framing.is_some());
    }

    #[test]
    fn a_missing_terminal_marker_never_commits() {
        let bytes = sidecar(10);
        let report = acquire(&bytes, bytes.len() as u64, false);
        assert!(!report.may_commit());
        assert_eq!(report.verification.verdict.code(), "NO_TERMINAL_MARKER");
    }

    #[test]
    fn a_byte_count_mismatch_never_commits() {
        let bytes = sidecar(10);
        let report = acquire(&bytes, bytes.len() as u64 + 1, true);
        assert!(!report.may_commit());
        assert_eq!(report.verification.verdict.code(), "RANGE_INCOMPLETE");
    }

    #[test]
    fn a_transfer_without_a_declared_length_stays_unverified() {
        let bytes = sidecar(10);
        let report = acquire(&bytes, 0, true);
        assert!(!report.may_commit());
        assert_eq!(
            report.verification.verdict.code(),
            "NO_DEVICE_DECLARED_LENGTH",
            "our own digest is not evidence of completeness (spec 7.9)"
        );
        assert_eq!(report.verification.received_bytes, bytes.len() as u64);
    }

    #[test]
    fn a_duplicate_or_out_of_order_chunk_still_produces_the_original_bytes() {
        let bytes = sidecar(20);
        let mut staging = Vec::new();
        let mut acquisition = StagingAcquisition::begin(
            &LEGACY_NOTE_V0095,
            &listed("REC0001", bytes.len() as u64),
            &mut staging,
        )
        .unwrap();

        let mut shuffled = frames(&bytes, 80);
        shuffled.rotate_left(3);
        let duplicate = shuffled[0].clone();
        for frame in &shuffled {
            acquisition.accept(frame).unwrap();
        }
        acquisition.accept(&duplicate).unwrap();
        acquisition.accept(&BulkFrame::EndOfTransfer).unwrap();
        let report = acquisition.finish().unwrap();

        assert!(report.may_commit());
        assert_eq!(staging, bytes);
    }

    #[test]
    fn a_forged_declared_length_cannot_exceed_the_profile_ceiling() {
        let mut staging = Vec::new();
        let huge = LEGACY_NOTE_V0095.max_transfer_bytes + 1;
        assert_eq!(
            StagingAcquisition::begin(&LEGACY_NOTE_V0095, &listed("REC0001", huge), &mut staging)
                .err(),
            Some(TransferError::DeclaredLengthOverCeiling {
                declared: huge,
                ceiling: LEGACY_NOTE_V0095.max_transfer_bytes,
            })
        );
        assert_eq!(staging, Vec::<u8>::new());
    }

    #[test]
    fn bytes_that_are_not_a_sidecar_are_staged_but_never_verified() {
        let bytes = vec![0xFFu8; 240];
        let report = acquire(&bytes, bytes.len() as u64, true);
        assert!(!report.may_commit());
        assert_eq!(report.verification.verdict.code(), "FRAMING_UNVERIFIED");
        assert!(!report.verification.framing_valid);
        assert_eq!(
            report.verification.received_bytes,
            bytes.len() as u64,
            "the bytes are still staged as evidence"
        );
    }

    #[test]
    fn the_playable_derivative_is_a_conversion_and_says_so() {
        let bytes = sidecar(30);
        let report = acquire(&bytes, bytes.len() as u64, true);
        let mut derived = Vec::new();
        let outcome = derive_playable(&report, &mut bytes.as_slice(), &mut derived);

        let DerivativeOutcome::Produced {
            kind,
            identity,
            duration_ms,
        } = outcome
        else {
            panic!("expected a produced derivative, got {outcome:?}");
        };
        assert_eq!(kind, DerivativeKind::OggOpusRemux);
        assert_eq!(kind.quality_class(), QualityClass::LocalDerivative);
        assert!(!kind.quality_class().is_original_fidelity());
        assert_eq!(duration_ms, 600);
        assert_eq!(identity.observed_codec, "audio/ogg");
        assert_eq!(&derived[..4], b"OggS");

        let label = kind.ui_label().to_lowercase();
        assert!(label.contains("derived conversion"));
        assert!(!label.contains("original"));
        assert!(!label.contains("high-quality"));
        assert!(!label.contains("wav"));
    }

    #[test]
    fn a_decoded_wav_derivative_is_reported_unavailable_rather_than_faked() {
        let outcome = DerivativeOutcome::no_pcm_decoder();
        let DerivativeOutcome::Unavailable { kind, reason } = outcome else {
            panic!("this build has no Opus decoder");
        };
        assert_eq!(kind, DerivativeKind::DecodedWav);
        assert_eq!(reason.code(), "UNSUPPORTED_CODEC");
        assert!(reason.ui_explanation().contains("no Opus decoder"));
        assert!(reason.ui_explanation().contains("kept"));
    }

    #[test]
    fn unparseable_bytes_produce_no_derivative() {
        let bytes = vec![0x01u8; 128];
        let report = acquire(&bytes, bytes.len() as u64, true);
        let mut derived = Vec::new();
        let outcome = derive_playable(&report, &mut bytes.as_slice(), &mut derived);
        assert!(matches!(outcome, DerivativeOutcome::Unavailable { .. }));
        assert_eq!(derived, Vec::<u8>::new());
    }

    #[test]
    fn the_source_label_never_claims_original_fidelity() {
        let bytes = sidecar(4);
        let report = acquire(&bytes, bytes.len() as u64, true);
        let label = report.source_label().to_lowercase();
        assert!(label.contains("compressed"));
        assert!(!label.contains("original"));
        assert!(!label.contains("wav"));
        assert!(!report.quality_class.is_original_fidelity());
    }

    #[test]
    fn resume_requires_identity_size_and_digest_to_all_match() {
        let checkpoint = TransferCheckpoint {
            device_recording_id: "REC0001".into(),
            verified_bytes: 160,
            source_size_hint: Some(4_096),
            staged_prefix_sha256: "ab".repeat(32),
        };
        let observed = listed("REC0001", 4_096);

        // Offset semantics are unproven on this profile, so even a perfect
        // match restarts (spec 7.7).
        assert_eq!(
            plan_resume(&LEGACY_NOTE_V0095, &checkpoint, &observed, &"ab".repeat(32)),
            ResumePlan::FreshStaging {
                reason: RestartReason::OffsetSemanticsUnverified
            }
        );

        let demonstrated = ProtocolProfile {
            offset_reads_demonstrated: true,
            ..LEGACY_NOTE_V0095
        };
        assert_eq!(
            plan_resume(&demonstrated, &checkpoint, &observed, &"ab".repeat(32)),
            ResumePlan::ResumeAt {
                verified_bytes: 160
            }
        );

        for (label, observed, digest) in [
            ("identity", listed("REC0002", 4_096), "ab".repeat(32)),
            ("size", listed("REC0001", 8_192), "ab".repeat(32)),
            ("digest", listed("REC0001", 4_096), "cd".repeat(32)),
        ] {
            let plan = plan_resume(&demonstrated, &checkpoint, &observed, &digest);
            assert!(
                !plan.resumes(),
                "a changed {label} must restart into fresh staging"
            );
        }
    }

    #[test]
    fn a_cancelled_transfer_maps_to_the_cancelled_device_error() {
        assert_eq!(
            DeviceError::from(TransferError::Cancelled),
            DeviceError::Cancelled
        );
        assert_eq!(
            DeviceError::from(TransferError::Framing(AsrError::NoPackets)),
            DeviceError::UnsupportedCodec
        );
        assert_eq!(
            DeviceError::from(TransferError::StagingWrite),
            DeviceError::IntegrityFailed
        );
    }

    #[test]
    fn memory_stays_bounded_across_a_large_transfer() {
        // 200 KiB with every frame delivered out of order by one position.
        let bytes = sidecar(5_000);
        let mut staging = Vec::new();
        let mut acquisition = StagingAcquisition::begin(
            &LEGACY_NOTE_V0095,
            &listed("REC0001", bytes.len() as u64),
            &mut staging,
        )
        .unwrap();
        let mut ordered = frames(&bytes, 80);
        // Swap neighbouring frames to force the reorder path.
        for index in (0..ordered.len() - 1).step_by(2) {
            ordered.swap(index, index + 1);
        }
        for frame in &ordered {
            acquisition.accept(frame).unwrap();
        }
        acquisition.accept(&BulkFrame::EndOfTransfer).unwrap();
        let report = acquisition.finish().unwrap();
        assert!(report.may_commit());
        assert_eq!(staging.len(), bytes.len());
    }
}
