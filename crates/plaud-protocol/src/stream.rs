//! Streaming reassembly with a bounded reorder window (spec App. B.4, §7.7).
//!
//! [`crate::bulk::BulkAssembler`] keeps every chunk, which is correct for a
//! unit test and wrong for a multi-hour recording: App. B.4 requires bounded
//! buffers and streaming to disk rather than "collecting entire audio blobs in
//! RAM".
//!
//! [`StreamingAssembler`] therefore holds only what a radio link actually
//! forces it to hold: chunks that arrived ahead of the write cursor. Anything
//! contiguous is handed back immediately for the caller to write to staging
//! and is then forgotten. The out-of-order window is capped, so a device that
//! never sends the missing chunk cannot grow our memory without bound — it
//! fails with [`StreamError::ReorderWindowExceeded`] instead.

use crate::bulk::{AssemblyError, BulkFrame, TransferOutcome};

/// Default cap on bytes held for out-of-order chunks.
///
/// Legacy chunks are 80 bytes, so this window tolerates several hundred
/// outstanding frames while staying a rounding error in RAM.
pub const DEFAULT_REORDER_WINDOW_BYTES: usize = 256 * 1024;

/// Why streaming reassembly failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// A rule shared with the buffering assembler was violated.
    #[error(transparent)]
    Assembly(#[from] AssemblyError),
    /// A chunk landed before the write cursor, so it cannot be verified
    /// against what was already written.
    #[error("chunk at offset {offset} arrived after offset {cursor} was already written")]
    ChunkBehindCursor {
        /// Offset the chunk claimed.
        offset: u64,
        /// Bytes already handed to the sink.
        cursor: u64,
    },
    /// Too many bytes are held waiting for a gap to be filled.
    #[error("the out-of-order window of {window_bytes} bytes was exceeded")]
    ReorderWindowExceeded {
        /// Configured window.
        window_bytes: usize,
    },
}

/// What one accepted frame produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamStep {
    /// Bytes that are now contiguous and must be appended to staging, in
    /// order. Empty when the frame only filled the reorder window.
    pub ready: Vec<u8>,
    /// Transfer state after this frame.
    pub outcome: TransferOutcome,
}

/// Reassembles bulk frames into an ordered byte stream with bounded memory.
#[derive(Debug)]
pub struct StreamingAssembler {
    /// Offset of the next byte that may be handed to the sink.
    cursor: u64,
    /// Chunks ahead of the cursor, keyed by offset.
    pending: std::collections::BTreeMap<u64, Vec<u8>>,
    pending_bytes: usize,
    window_bytes: usize,
    budget_bytes: u64,
    declared_total_bytes: Option<u64>,
    zero_progress_frames: u32,
    max_zero_progress_frames: u32,
    ended: bool,
}

impl StreamingAssembler {
    /// Create an assembler with a hard byte budget and no declared length.
    #[must_use]
    pub fn with_budget(budget_bytes: u64) -> Self {
        Self {
            cursor: 0,
            pending: std::collections::BTreeMap::new(),
            pending_bytes: 0,
            window_bytes: DEFAULT_REORDER_WINDOW_BYTES,
            budget_bytes,
            declared_total_bytes: None,
            zero_progress_frames: 0,
            max_zero_progress_frames: crate::bulk::BulkAssembler::DEFAULT_MAX_ZERO_PROGRESS_FRAMES,
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

    /// Resume from a verified checkpoint.
    ///
    /// Spec App. B.6 permits this only after the caller has confirmed the
    /// recording identity, the expected size and the digest of the bytes
    /// already staged; this constructor records the position, it does not
    /// authorize the resume.
    #[must_use]
    pub fn resuming_at(budget_bytes: u64, declared_total_bytes: u64, verified_bytes: u64) -> Self {
        Self {
            cursor: verified_bytes,
            ..Self::with_declared_length(budget_bytes, declared_total_bytes)
        }
    }

    /// Set the out-of-order window.
    #[must_use]
    pub const fn with_reorder_window(mut self, window_bytes: usize) -> Self {
        self.window_bytes = window_bytes;
        self
    }

    /// Bytes handed to the sink so far.
    #[must_use]
    pub const fn written_bytes(&self) -> u64 {
        self.cursor
    }

    /// Bytes currently held for out-of-order chunks.
    #[must_use]
    pub const fn buffered_bytes(&self) -> usize {
        self.pending_bytes
    }

    /// Whether the terminal marker has been seen.
    #[must_use]
    pub const fn ended(&self) -> bool {
        self.ended
    }

    /// Accept one frame.
    pub fn accept(&mut self, frame: &BulkFrame) -> Result<StreamStep, StreamError> {
        match frame {
            BulkFrame::EndOfTransfer => {
                self.ended = true;
                Ok(StreamStep {
                    ready: Vec::new(),
                    outcome: self.outcome(),
                })
            }
            BulkFrame::Chunk { offset, payload } => {
                if self.ended {
                    return Err(AssemblyError::ChunkAfterEnd.into());
                }
                let offset = u64::from(*offset);
                if offset
                    .checked_add(payload.len() as u64)
                    .is_none_or(|end| end > u64::from(u32::MAX))
                {
                    return Err(AssemblyError::OffsetOverflow {
                        offset: u32::try_from(offset).unwrap_or(u32::MAX),
                    }
                    .into());
                }
                if payload.is_empty() {
                    self.note_zero_progress()?;
                    return Ok(StreamStep {
                        ready: Vec::new(),
                        outcome: TransferOutcome::InProgress,
                    });
                }
                if offset < self.cursor {
                    // A retransmission of bytes already written. We cannot
                    // compare it against what we wrote, because we no longer
                    // hold those bytes, so we neither re-apply nor silently
                    // accept it: it counts as no progress.
                    if offset + payload.len() as u64 <= self.cursor {
                        self.note_zero_progress()?;
                        return Ok(StreamStep {
                            ready: Vec::new(),
                            outcome: TransferOutcome::InProgress,
                        });
                    }
                    return Err(StreamError::ChunkBehindCursor {
                        offset,
                        cursor: self.cursor,
                    });
                }
                if let Some(existing) = self.pending.get(&offset) {
                    if existing == payload {
                        self.note_zero_progress()?;
                        return Ok(StreamStep {
                            ready: Vec::new(),
                            outcome: TransferOutcome::InProgress,
                        });
                    }
                    return Err(AssemblyError::ConflictingChunk {
                        offset: u32::try_from(offset).unwrap_or(u32::MAX),
                    }
                    .into());
                }
                let projected = self
                    .cursor
                    .saturating_add(self.pending_bytes as u64)
                    .saturating_add(payload.len() as u64);
                if projected > self.budget_bytes {
                    return Err(AssemblyError::BudgetExceeded {
                        budget_bytes: self.budget_bytes,
                    }
                    .into());
                }

                self.pending.insert(offset, payload.clone());
                self.pending_bytes += payload.len();
                if self.pending_bytes > self.window_bytes {
                    return Err(StreamError::ReorderWindowExceeded {
                        window_bytes: self.window_bytes,
                    });
                }
                self.zero_progress_frames = 0;

                let ready = self.drain_contiguous();
                Ok(StreamStep {
                    ready,
                    outcome: TransferOutcome::InProgress,
                })
            }
        }
    }

    /// The current outcome.
    #[must_use]
    pub fn outcome(&self) -> TransferOutcome {
        if !self.ended {
            return TransferOutcome::InProgress;
        }
        if self.pending_bytes > 0 {
            return TransferOutcome::IncompleteAtEnd {
                first_missing_offset: self.cursor,
            };
        }
        match self.declared_total_bytes {
            Some(declared) if declared == self.cursor => TransferOutcome::Complete,
            Some(_) => TransferOutcome::IncompleteAtEnd {
                first_missing_offset: self.cursor,
            },
            None => TransferOutcome::CompleteLengthUnverified {
                received_bytes: self.cursor,
            },
        }
    }

    /// Whether the stream may be committed as a complete asset.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.outcome() == TransferOutcome::Complete
    }

    fn drain_contiguous(&mut self) -> Vec<u8> {
        let mut ready = Vec::new();
        while let Some(payload) = self.pending.remove(&self.cursor) {
            self.pending_bytes -= payload.len();
            self.cursor += payload.len() as u64;
            ready.extend_from_slice(&payload);
        }
        ready
    }

    fn note_zero_progress(&mut self) -> Result<(), StreamError> {
        self.zero_progress_frames += 1;
        if self.zero_progress_frames >= self.max_zero_progress_frames {
            return Err(AssemblyError::ZeroProgress {
                frames: self.zero_progress_frames,
            }
            .into());
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
    fn contiguous_chunks_stream_straight_through_without_buffering() {
        let mut assembler = StreamingAssembler::with_declared_length(1_024, 6);
        let step = assembler.accept(&chunk(0, b"abc")).unwrap();
        assert_eq!(step.ready, b"abc");
        assert_eq!(assembler.buffered_bytes(), 0);
        let step = assembler.accept(&chunk(3, b"def")).unwrap();
        assert_eq!(step.ready, b"def");
        assert_eq!(
            assembler.accept(&BulkFrame::EndOfTransfer).unwrap().outcome,
            TransferOutcome::Complete
        );
        assert_eq!(assembler.written_bytes(), 6);
    }

    #[test]
    fn an_out_of_order_chunk_is_held_then_released_in_order() {
        let mut assembler = StreamingAssembler::with_declared_length(1_024, 6);
        let step = assembler.accept(&chunk(3, b"def")).unwrap();
        assert!(step.ready.is_empty());
        assert_eq!(assembler.buffered_bytes(), 3);

        let step = assembler.accept(&chunk(0, b"abc")).unwrap();
        assert_eq!(step.ready, b"abcdef", "the gap release is ordered");
        assert_eq!(assembler.buffered_bytes(), 0);
    }

    #[test]
    fn the_reorder_window_is_bounded() {
        let mut assembler = StreamingAssembler::with_budget(1_000_000).with_reorder_window(64);
        // Never send offset 0, so nothing can ever drain.
        let mut last = Ok(StreamStep {
            ready: Vec::new(),
            outcome: TransferOutcome::InProgress,
        });
        for index in 1..32u32 {
            last = assembler.accept(&chunk(index * 8, b"01234567"));
            if last.is_err() {
                break;
            }
        }
        assert_eq!(
            last,
            Err(StreamError::ReorderWindowExceeded { window_bytes: 64 })
        );
    }

    #[test]
    fn a_retransmission_of_written_bytes_is_not_reapplied() {
        let mut assembler = StreamingAssembler::with_declared_length(1_024, 6);
        assembler.accept(&chunk(0, b"abc")).unwrap();
        let step = assembler.accept(&chunk(0, b"abc")).unwrap();
        assert!(
            step.ready.is_empty(),
            "a duplicate must not be written twice"
        );
        assert_eq!(assembler.written_bytes(), 3);
    }

    #[test]
    fn a_chunk_straddling_the_cursor_is_refused_rather_than_guessed() {
        let mut assembler = StreamingAssembler::with_declared_length(1_024, 6);
        assembler.accept(&chunk(0, b"abc")).unwrap();
        assert_eq!(
            assembler.accept(&chunk(1, b"xyz")),
            Err(StreamError::ChunkBehindCursor {
                offset: 1,
                cursor: 3
            })
        );
    }

    #[test]
    fn a_hole_at_the_terminal_marker_is_never_complete() {
        let mut assembler = StreamingAssembler::with_declared_length(1_024, 9);
        assembler.accept(&chunk(0, b"abc")).unwrap();
        assembler.accept(&chunk(6, b"ghi")).unwrap();
        let step = assembler.accept(&BulkFrame::EndOfTransfer).unwrap();
        assert_eq!(
            step.outcome,
            TransferOutcome::IncompleteAtEnd {
                first_missing_offset: 3
            }
        );
        assert!(!assembler.is_complete());
    }

    #[test]
    fn without_a_declared_length_the_stream_stays_unverified() {
        let mut assembler = StreamingAssembler::with_budget(1_024);
        assembler.accept(&chunk(0, b"abcdef")).unwrap();
        let step = assembler.accept(&BulkFrame::EndOfTransfer).unwrap();
        assert_eq!(
            step.outcome,
            TransferOutcome::CompleteLengthUnverified { received_bytes: 6 }
        );
        assert!(!assembler.is_complete());
    }

    #[test]
    fn a_truncated_tail_is_detected_against_the_declared_length() {
        let mut assembler = StreamingAssembler::with_declared_length(1_024, 9);
        assembler.accept(&chunk(0, b"abcdef")).unwrap();
        assert_eq!(
            assembler.accept(&BulkFrame::EndOfTransfer).unwrap().outcome,
            TransferOutcome::IncompleteAtEnd {
                first_missing_offset: 6
            }
        );
    }

    #[test]
    fn a_resumed_stream_starts_at_the_verified_offset() {
        let mut assembler = StreamingAssembler::resuming_at(1_024, 6, 3);
        let step = assembler.accept(&chunk(3, b"def")).unwrap();
        assert_eq!(step.ready, b"def");
        assert_eq!(
            assembler.accept(&BulkFrame::EndOfTransfer).unwrap().outcome,
            TransferOutcome::Complete
        );
    }

    #[test]
    fn a_chunk_after_the_terminal_marker_is_refused() {
        let mut assembler = StreamingAssembler::with_budget(1_024);
        assembler.accept(&BulkFrame::EndOfTransfer).unwrap();
        assert_eq!(
            assembler.accept(&chunk(0, b"abc")),
            Err(StreamError::Assembly(AssemblyError::ChunkAfterEnd))
        );
    }

    #[test]
    fn a_zero_progress_loop_terminates() {
        let mut assembler = StreamingAssembler::with_budget(1_024);
        let mut last = Ok(StreamStep {
            ready: Vec::new(),
            outcome: TransferOutcome::InProgress,
        });
        for _ in 0..=crate::bulk::BulkAssembler::DEFAULT_MAX_ZERO_PROGRESS_FRAMES {
            last = assembler.accept(&chunk(0, b""));
            if last.is_err() {
                break;
            }
        }
        assert!(matches!(
            last,
            Err(StreamError::Assembly(AssemblyError::ZeroProgress { .. }))
        ));
    }

    #[test]
    fn the_budget_counts_buffered_and_written_bytes_together() {
        let mut assembler = StreamingAssembler::with_budget(8);
        assembler.accept(&chunk(0, b"abcd")).unwrap();
        assert_eq!(
            assembler.accept(&chunk(4, b"efghij")),
            Err(StreamError::Assembly(AssemblyError::BudgetExceeded {
                budget_bytes: 8
            }))
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        /// A shuffled stream reproduces the original bytes exactly, and the
        /// reorder window never holds more than the configured cap.
        #[test]
        fn a_shuffled_stream_reproduces_the_original_bytes(
            data in proptest::collection::vec(any::<u8>(), 1..512),
            chunk_size in 1usize..32,
            rotate in 0usize..8,
        ) {
            let mut frames: Vec<BulkFrame> = data
                .chunks(chunk_size)
                .enumerate()
                .map(|(index, slice)| chunk(u32::try_from(index * chunk_size).unwrap(), slice))
                .collect();
            let shift = rotate.min(frames.len().saturating_sub(1));
            frames.rotate_left(shift);

            let mut assembler =
                StreamingAssembler::with_declared_length(4096, data.len() as u64);
            let mut written = Vec::new();
            for frame in &frames {
                let step = assembler.accept(frame).unwrap();
                written.extend_from_slice(&step.ready);
                prop_assert!(assembler.buffered_bytes() <= DEFAULT_REORDER_WINDOW_BYTES);
            }
            prop_assert_eq!(
                assembler.accept(&BulkFrame::EndOfTransfer).unwrap().outcome,
                TransferOutcome::Complete
            );
            prop_assert_eq!(written, data);
        }

        /// Dropping any chunk always prevents completion.
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
                StreamingAssembler::with_declared_length(4096, data.len() as u64);
            for (index, frame) in frames.iter().enumerate() {
                if index == drop_index {
                    continue;
                }
                // A chunk after the gap is held, which is allowed; a chunk
                // before the cursor cannot occur in an ordered stream.
                let _ = assembler.accept(frame);
            }
            prop_assert_ne!(
                assembler.accept(&BulkFrame::EndOfTransfer).unwrap().outcome,
                TransferOutcome::Complete
            );
            prop_assert!(!assembler.is_complete());
        }
    }
}
