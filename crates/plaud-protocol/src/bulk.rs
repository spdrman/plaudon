//! The `0x02` bulk frame codec and transfer assembler (spec 18.2A, 7.9).
//!
//! Wire layout:
//!
//! ```text
//! +------+--------+--------+-----------------+
//! | 0x02 | offset | length | payload         |
//! | u8   | u32 BE | u16 BE | length bytes    |
//! +------+--------+--------+-----------------+
//! ```
//!
//! `offset == 0xFFFF_FFFF` is the end-of-transfer sentinel and carries no
//! payload. A transfer is only complete when that sentinel has actually been
//! seen: spec 7.9 forbids inferring completeness from our own digest alone.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::frame::{FrameError, FrameKind, MAX_FRAME_PAYLOAD};

/// Offset value that marks the end of a transfer.
pub const END_OF_TRANSFER: u32 = 0xFFFF_FFFF;

/// A decoded bulk frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkFrame {
    /// A chunk at an absolute byte offset.
    Chunk {
        /// Absolute offset of the first payload byte.
        offset: u32,
        /// Chunk payload.
        payload: Vec<u8>,
    },
    /// The terminal marker.
    EndOfTransfer,
}

impl BulkFrame {
    /// Fixed header size: kind, offset, length.
    pub const HEADER_LEN: usize = 1 + 4 + 2;

    /// Encode to the wire.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let (offset, payload): (u32, &[u8]) = match self {
            Self::Chunk { offset, payload } => (*offset, payload),
            Self::EndOfTransfer => (END_OF_TRANSFER, &[]),
        };
        let length = u16::try_from(payload.len()).unwrap_or(u16::MAX);
        let mut out = Vec::with_capacity(Self::HEADER_LEN + payload.len());
        out.push(FrameKind::Bulk.wire_value());
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(payload);
        out
    }

    /// Decode from the wire.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < Self::HEADER_LEN {
            return Err(FrameError::TooShort);
        }
        let kind =
            FrameKind::from_wire(bytes[0]).ok_or(FrameError::UnknownKind { value: bytes[0] })?;
        if kind != FrameKind::Bulk {
            return Err(FrameError::WrongKind {
                expected: FrameKind::Bulk,
                actual: kind,
            });
        }
        let offset = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
        let declared = usize::from(u16::from_be_bytes([bytes[5], bytes[6]]));
        if declared > MAX_FRAME_PAYLOAD {
            return Err(FrameError::LengthExceedsCap { declared });
        }
        let available = bytes.len() - Self::HEADER_LEN;
        if declared > available {
            return Err(FrameError::LengthExceedsInput {
                declared,
                available,
            });
        }
        if offset == END_OF_TRANSFER {
            return Ok(Self::EndOfTransfer);
        }
        Ok(Self::Chunk {
            offset,
            payload: bytes[Self::HEADER_LEN..Self::HEADER_LEN + declared].to_vec(),
        })
    }
}

/// How a transfer ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferOutcome {
    /// Still receiving.
    InProgress,
    /// The terminal marker arrived and every byte from zero is present.
    Complete,
    /// The terminal marker arrived but the byte range has a hole.
    IncompleteAtEnd {
        /// First byte offset that was never received.
        first_missing_offset: u64,
    },
    /// The terminal marker arrived and the received bytes are contiguous from
    /// zero, but the device never declared a total length.
    ///
    /// Spec 7.9 forbids inferring completeness from our own byte count alone,
    /// because a truncated tail is indistinguishable from a complete file
    /// without a declared length or CRC. Such a transfer stays `unverified`.
    CompleteLengthUnverified {
        /// Bytes actually received.
        received_bytes: u64,
    },
}

/// Why assembly rejected a frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AssemblyError {
    /// The same offset arrived twice with different bytes.
    #[error("conflicting data received for offset {offset}")]
    ConflictingChunk {
        /// Offset that conflicted.
        offset: u32,
    },
    /// A chunk arrived after the terminal marker.
    #[error("a chunk arrived after the end-of-transfer marker")]
    ChunkAfterEnd,
    /// The transfer exceeded its byte budget.
    #[error("the transfer exceeded its {budget_bytes} byte budget")]
    BudgetExceeded {
        /// Configured budget.
        budget_bytes: u64,
    },
    /// Too many frames arrived without advancing the received byte count.
    #[error("the transfer made no progress across {frames} frames")]
    ZeroProgress {
        /// Number of non-advancing frames seen.
        frames: u32,
    },
    /// A chunk's offset plus length overflows the addressable range.
    #[error("chunk at offset {offset} overflows the addressable range")]
    OffsetOverflow {
        /// Offending offset.
        offset: u32,
    },
}

/// Reassembles bulk chunks into a contiguous byte range.
///
/// Out-of-order and duplicate chunks are tolerated, because a radio link
/// produces both. Conflicting duplicates, post-terminal chunks, budget
/// overruns and zero-progress loops are not.
#[derive(Debug)]
pub struct BulkAssembler {
    chunks: BTreeMap<u32, Vec<u8>>,
    received_bytes: u64,
    budget_bytes: u64,
    zero_progress_frames: u32,
    max_zero_progress_frames: u32,
    declared_total_bytes: Option<u64>,
    ended: bool,
}

impl BulkAssembler {
    /// Default tolerance for frames that add no new bytes.
    pub const DEFAULT_MAX_ZERO_PROGRESS_FRAMES: u32 = 64;

    /// Create an assembler with a hard byte budget and no declared length.
    ///
    /// Without a device-declared length a finished transfer can only reach
    /// [`TransferOutcome::CompleteLengthUnverified`].
    #[must_use]
    pub fn with_budget(budget_bytes: u64) -> Self {
        Self {
            chunks: BTreeMap::new(),
            received_bytes: 0,
            budget_bytes,
            zero_progress_frames: 0,
            max_zero_progress_frames: Self::DEFAULT_MAX_ZERO_PROGRESS_FRAMES,
            declared_total_bytes: None,
            ended: false,
        }
    }

    /// Create an assembler for a transfer whose total length the device
    /// declared, which is the only way to reach [`TransferOutcome::Complete`].
    #[must_use]
    pub fn with_declared_length(budget_bytes: u64, declared_total_bytes: u64) -> Self {
        Self {
            declared_total_bytes: Some(declared_total_bytes),
            ..Self::with_budget(budget_bytes)
        }
    }

    /// Bytes accepted so far, counting each offset once.
    #[must_use]
    pub const fn received_bytes(&self) -> u64 {
        self.received_bytes
    }

    /// Whether the terminal marker has been seen.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.ended
    }

    /// Accept one frame.
    pub fn accept(&mut self, frame: &BulkFrame) -> Result<TransferOutcome, AssemblyError> {
        match frame {
            BulkFrame::EndOfTransfer => {
                self.ended = true;
                Ok(self.outcome())
            }
            BulkFrame::Chunk { offset, payload } => {
                if self.ended {
                    return Err(AssemblyError::ChunkAfterEnd);
                }
                if u64::from(*offset) + payload.len() as u64 > u64::from(u32::MAX) {
                    return Err(AssemblyError::OffsetOverflow { offset: *offset });
                }
                match self.chunks.get(offset) {
                    Some(existing) if existing == payload => {
                        self.note_zero_progress()?;
                        return Ok(TransferOutcome::InProgress);
                    }
                    Some(_) => return Err(AssemblyError::ConflictingChunk { offset: *offset }),
                    None => {}
                }
                if payload.is_empty() {
                    self.note_zero_progress()?;
                    return Ok(TransferOutcome::InProgress);
                }
                let next_total = self.received_bytes + payload.len() as u64;
                if next_total > self.budget_bytes {
                    return Err(AssemblyError::BudgetExceeded {
                        budget_bytes: self.budget_bytes,
                    });
                }
                self.chunks.insert(*offset, payload.clone());
                self.received_bytes = next_total;
                self.zero_progress_frames = 0;
                Ok(TransferOutcome::InProgress)
            }
        }
    }

    /// The current outcome.
    #[must_use]
    pub fn outcome(&self) -> TransferOutcome {
        if !self.ended {
            return TransferOutcome::InProgress;
        }
        let mut expected: u64 = 0;
        for (offset, payload) in &self.chunks {
            if u64::from(*offset) != expected {
                return TransferOutcome::IncompleteAtEnd {
                    first_missing_offset: expected,
                };
            }
            expected += payload.len() as u64;
        }
        match self.declared_total_bytes {
            Some(declared) if declared == expected => TransferOutcome::Complete,
            Some(_) => TransferOutcome::IncompleteAtEnd {
                first_missing_offset: expected,
            },
            None => TransferOutcome::CompleteLengthUnverified {
                received_bytes: expected,
            },
        }
    }

    /// The assembled bytes, only when the transfer is verifiably complete.
    ///
    /// Returns `None` while in progress or when the range has a hole, so a
    /// partial transfer cannot be committed as an asset.
    #[must_use]
    pub fn finish(self) -> Option<Vec<u8>> {
        if self.outcome() != TransferOutcome::Complete {
            return None;
        }
        let mut out = Vec::with_capacity(usize::try_from(self.received_bytes).ok()?);
        for payload in self.chunks.into_values() {
            out.extend_from_slice(&payload);
        }
        Some(out)
    }

    fn note_zero_progress(&mut self) -> Result<(), AssemblyError> {
        self.zero_progress_frames += 1;
        if self.zero_progress_frames >= self.max_zero_progress_frames {
            return Err(AssemblyError::ZeroProgress {
                frames: self.zero_progress_frames,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn chunk(offset: u32, payload: &[u8]) -> BulkFrame {
        BulkFrame::Chunk {
            offset,
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn bulk_frames_round_trip_including_the_sentinel() {
        let data = chunk(0, b"abc");
        assert_eq!(BulkFrame::decode(&data.encode()).unwrap(), data);
        assert_eq!(
            BulkFrame::decode(&BulkFrame::EndOfTransfer.encode()).unwrap(),
            BulkFrame::EndOfTransfer
        );
        assert_eq!(
            &BulkFrame::EndOfTransfer.encode()[1..5],
            &END_OF_TRANSFER.to_be_bytes()
        );
    }

    #[test]
    fn a_complete_in_order_transfer_assembles() {
        let mut assembler = BulkAssembler::with_declared_length(1_024, 6);
        assembler.accept(&chunk(0, b"abc")).unwrap();
        assembler.accept(&chunk(3, b"def")).unwrap();
        assert_eq!(
            assembler.accept(&BulkFrame::EndOfTransfer).unwrap(),
            TransferOutcome::Complete
        );
        assert_eq!(assembler.finish().unwrap(), b"abcdef");
    }

    #[test]
    fn out_of_order_chunks_assemble_correctly() {
        let mut assembler = BulkAssembler::with_declared_length(1_024, 6);
        assembler.accept(&chunk(3, b"def")).unwrap();
        assembler.accept(&chunk(0, b"abc")).unwrap();
        assembler.accept(&BulkFrame::EndOfTransfer).unwrap();
        assert_eq!(assembler.finish().unwrap(), b"abcdef");
    }

    #[test]
    fn an_identical_duplicate_is_tolerated_but_a_conflicting_one_is_not() {
        let mut assembler = BulkAssembler::with_budget(1_024);
        assembler.accept(&chunk(0, b"abc")).unwrap();
        assembler.accept(&chunk(0, b"abc")).unwrap();
        assert_eq!(
            assembler.received_bytes(),
            3,
            "a duplicate must not be counted twice"
        );
        assert_eq!(
            assembler.accept(&chunk(0, b"xyz")),
            Err(AssemblyError::ConflictingChunk { offset: 0 })
        );
    }

    #[test]
    fn a_missing_final_frame_never_reports_complete() {
        let mut assembler = BulkAssembler::with_budget(1_024);
        assembler.accept(&chunk(0, b"abc")).unwrap();
        assert_eq!(assembler.outcome(), TransferOutcome::InProgress);
        assert!(
            assembler.finish().is_none(),
            "no sentinel means no committed bytes"
        );
    }

    #[test]
    fn a_hole_in_the_byte_range_is_reported_rather_than_silently_closed() {
        let mut assembler = BulkAssembler::with_budget(1_024);
        assembler.accept(&chunk(0, b"abc")).unwrap();
        assembler.accept(&chunk(10, b"def")).unwrap();
        assert_eq!(
            assembler.accept(&BulkFrame::EndOfTransfer).unwrap(),
            TransferOutcome::IncompleteAtEnd {
                first_missing_offset: 3
            }
        );
        assert!(assembler.finish().is_none());
    }

    #[test]
    fn without_a_declared_length_a_finished_transfer_stays_unverified() {
        let mut assembler = BulkAssembler::with_budget(1_024);
        assembler.accept(&chunk(0, b"abcdef")).unwrap();
        assert_eq!(
            assembler.accept(&BulkFrame::EndOfTransfer).unwrap(),
            TransferOutcome::CompleteLengthUnverified { received_bytes: 6 }
        );
        assert!(
            assembler.finish().is_none(),
            "our own byte count is not proof of completeness (spec 7.9)"
        );
    }

    #[test]
    fn a_truncated_tail_is_detected_against_the_declared_length() {
        let mut assembler = BulkAssembler::with_declared_length(1_024, 9);
        assembler.accept(&chunk(0, b"abc")).unwrap();
        assembler.accept(&chunk(3, b"def")).unwrap();
        assert_eq!(
            assembler.accept(&BulkFrame::EndOfTransfer).unwrap(),
            TransferOutcome::IncompleteAtEnd {
                first_missing_offset: 6
            }
        );
        assert!(assembler.finish().is_none());
    }

    #[test]
    fn a_chunk_after_the_terminal_marker_is_refused() {
        let mut assembler = BulkAssembler::with_budget(1_024);
        assembler.accept(&BulkFrame::EndOfTransfer).unwrap();
        assert_eq!(
            assembler.accept(&chunk(0, b"abc")),
            Err(AssemblyError::ChunkAfterEnd)
        );
    }

    #[test]
    fn a_forged_length_cannot_exceed_the_transfer_budget() {
        let mut assembler = BulkAssembler::with_budget(8);
        assembler.accept(&chunk(0, b"abcd")).unwrap();
        assert_eq!(
            assembler.accept(&chunk(4, b"efghij")),
            Err(AssemblyError::BudgetExceeded { budget_bytes: 8 })
        );
    }

    #[test]
    fn a_zero_progress_loop_terminates() {
        let mut assembler = BulkAssembler::with_budget(1_024);
        let mut last = Ok(TransferOutcome::InProgress);
        for _ in 0..=BulkAssembler::DEFAULT_MAX_ZERO_PROGRESS_FRAMES {
            last = assembler.accept(&chunk(0, b""));
            if last.is_err() {
                break;
            }
        }
        assert!(matches!(last, Err(AssemblyError::ZeroProgress { .. })));
    }

    #[test]
    fn an_offset_that_overflows_the_address_space_is_refused() {
        let mut assembler = BulkAssembler::with_budget(1_024);
        assert_eq!(
            assembler.accept(&chunk(u32::MAX - 1, b"abcd")),
            Err(AssemblyError::OffsetOverflow {
                offset: u32::MAX - 1
            })
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        /// Decoding arbitrary bytes as a bulk frame never panics.
        #[test]
        fn decoding_arbitrary_bytes_is_total(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            if let Ok(BulkFrame::Chunk { payload, .. }) = BulkFrame::decode(&bytes) {
                prop_assert!(payload.len() <= bytes.len());
                prop_assert!(payload.len() <= MAX_FRAME_PAYLOAD);
            }
        }

        /// A shuffled, duplicated chunk stream still assembles to the original
        /// bytes, and never to anything else.
        #[test]
        fn shuffled_and_duplicated_streams_assemble_identically(
            data in proptest::collection::vec(any::<u8>(), 1..512),
            chunk_size in 1usize..64,
            rotate in 0usize..8,
        ) {
            let mut frames: Vec<BulkFrame> = data
                .chunks(chunk_size)
                .enumerate()
                .map(|(index, slice)| {
                    let offset = u32::try_from(index * chunk_size).unwrap();
                    chunk(offset, slice)
                })
                .collect();
            // Retransmission is normal on a radio link, but a long unbroken
            // run of duplicates is a stalled transfer, so stay inside the
            // zero-progress tolerance.
            let duplicate_budget =
                usize::try_from(BulkAssembler::DEFAULT_MAX_ZERO_PROGRESS_FRAMES - 1).unwrap();
            let duplicates: Vec<BulkFrame> =
                frames.iter().take(duplicate_budget).cloned().collect();
            let shift = rotate.min(frames.len().saturating_sub(1));
            frames.rotate_left(shift);
            frames.extend(duplicates);

            let mut assembler =
                BulkAssembler::with_declared_length(4096, data.len() as u64);
            for frame in &frames {
                assembler.accept(frame).unwrap();
            }
            prop_assert_eq!(assembler.accept(&BulkFrame::EndOfTransfer).unwrap(), TransferOutcome::Complete);
            prop_assert_eq!(assembler.finish().unwrap(), data);
        }

        /// Dropping any single chunk always prevents completion.
        #[test]
        fn a_dropped_chunk_always_prevents_completion(
            data in proptest::collection::vec(any::<u8>(), 8..256),
            chunk_size in 2usize..16,
            drop_index in 0usize..16,
        ) {
            let frames: Vec<BulkFrame> = data
                .chunks(chunk_size)
                .enumerate()
                .map(|(index, slice)| chunk(u32::try_from(index * chunk_size).unwrap(), slice))
                .collect();
            prop_assume!(frames.len() > 1);
            let drop_index = drop_index % frames.len();

            let mut assembler =
                BulkAssembler::with_declared_length(4096, data.len() as u64);
            for (index, frame) in frames.iter().enumerate() {
                if index == drop_index {
                    continue;
                }
                assembler.accept(frame).unwrap();
            }
            let outcome = assembler.accept(&BulkFrame::EndOfTransfer).unwrap();
            prop_assert_ne!(outcome, TransferOutcome::Complete);
            prop_assert!(assembler.finish().is_none());
        }
    }
}
