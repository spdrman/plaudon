//! Hostile-input behaviour of the BLE frame layer (spec 18.2A, 7.9).
//!
//! Every test here drives a *rejection* path from real bytes: truncated
//! headers, forged length fields, unknown kinds, forbidden opcodes, duplicated
//! and conflicting chunks, post-terminal frames, budget and window overruns.
//! The decoders are total, so each case asserts the error value rather than
//! relying on a panic.

use plaudon_plaud_protocol::bulk::{AssemblyError, END_OF_TRANSFER};
use plaudon_plaud_protocol::frame::MAX_FRAME_PAYLOAD;
use plaudon_plaud_protocol::profile::{ProfileAuthMode, ProtocolProfile, LEGACY_NOTE_V0095};
use plaudon_plaud_protocol::{
    profile_by_name, profile_for, BulkAssembler, BulkFrame, ControlFrame, ForbiddenOpcode,
    FrameError, FrameKind, ReadOnlyOpcode, StreamError, StreamingAssembler, TransferOutcome,
};

fn chunk(offset: u32, payload: &[u8]) -> BulkFrame {
    BulkFrame::Chunk {
        offset,
        payload: payload.to_vec(),
    }
}

/// A control frame header with an arbitrary declared length.
fn control_header(opcode: u16, declared: u16) -> Vec<u8> {
    let mut bytes = vec![FrameKind::Control.wire_value()];
    bytes.extend_from_slice(&opcode.to_be_bytes());
    bytes.extend_from_slice(&declared.to_be_bytes());
    bytes
}

/// A bulk frame header with an arbitrary declared length.
fn bulk_header(offset: u32, declared: u16) -> Vec<u8> {
    let mut bytes = vec![FrameKind::Bulk.wire_value()];
    bytes.extend_from_slice(&offset.to_be_bytes());
    bytes.extend_from_slice(&declared.to_be_bytes());
    bytes
}

// ------------------------------------------------------------- control frames

#[test]
fn a_control_frame_shorter_than_its_header_is_refused_at_every_length() {
    let full = ControlFrame::new(ReadOnlyOpcode::ListDelta, vec![])
        .expect("an empty payload is in bounds")
        .encode();
    for cut in 0..ControlFrame::HEADER_LEN {
        assert_eq!(
            ControlFrame::decode(&full[..cut]),
            Err(FrameError::TooShort),
            "{cut} header bytes must not decode"
        );
    }
    assert!(ControlFrame::decode(&full).is_ok(), "the whole header does");
}

#[test]
fn an_unknown_leading_byte_is_named_rather_than_guessed() {
    for value in [0x00u8, 0x03, 0x7F, 0x80, 0xFF] {
        let mut bytes = control_header(ReadOnlyOpcode::ListDelta.wire_value(), 0);
        bytes[0] = value;
        assert_eq!(
            ControlFrame::decode(&bytes),
            Err(FrameError::UnknownKind { value })
        );
        assert_eq!(
            BulkFrame::decode(&[value, 0, 0, 0, 0, 0, 0]),
            Err(FrameError::UnknownKind { value })
        );
        assert_eq!(FrameKind::from_wire(value), None);
    }
}

#[test]
fn a_bulk_frame_offered_to_the_control_decoder_is_refused_and_the_reverse() {
    let bulk = BulkFrame::EndOfTransfer.encode();
    assert_eq!(
        ControlFrame::decode(&bulk),
        Err(FrameError::WrongKind {
            expected: FrameKind::Control,
            actual: FrameKind::Bulk,
        })
    );

    let control = ControlFrame::new(ReadOnlyOpcode::Authenticate, vec![1, 2, 3])
        .expect("in bounds")
        .encode();
    assert_eq!(
        BulkFrame::decode(&control),
        Err(FrameError::WrongKind {
            expected: FrameKind::Bulk,
            actual: FrameKind::Control,
        })
    );
}

#[test]
fn every_opcode_outside_the_allowlist_decodes_to_a_forbidden_opcode() {
    for value in [
        0x0000u16, 0x0002, 0x0003, 0x0010, 0x0011, 0x0012, 0x0019, 0x001B, 0x001D, 0x0020, 0x0021,
        0x8000, 0xFFFF,
    ] {
        let bytes = control_header(value, 0);
        assert_eq!(
            ControlFrame::decode(&bytes),
            Err(FrameError::ForbiddenOpcode(ForbiddenOpcode { value })),
            "{value:#06x} must not decode"
        );
    }

    // The rejection names the value and says why, without inventing a frame.
    let rendered = FrameError::ForbiddenOpcode(ForbiddenOpcode { value: 0x0002 }).to_string();
    assert!(rendered.contains("0x0002"), "{rendered}");
    assert!(rendered.contains("allowlist"), "{rendered}");
}

#[test]
fn the_three_allowlisted_opcodes_survive_a_round_trip_at_both_payload_extremes() {
    for opcode in ReadOnlyOpcode::ALL {
        for payload in [Vec::new(), vec![0xAA; 1], vec![0x55; MAX_FRAME_PAYLOAD]] {
            let frame = ControlFrame::new(*opcode, payload.clone()).expect("at the cap");
            let decoded = ControlFrame::decode(&frame.encode()).expect("round trip");
            assert_eq!(decoded.opcode, *opcode);
            assert_eq!(decoded.payload, payload);
        }
    }
}

#[test]
fn a_declared_length_past_the_cap_is_refused_before_anything_is_reserved() {
    // One byte over the cap, with a single byte actually present.
    let declared = u16::try_from(MAX_FRAME_PAYLOAD + 1).expect("fits a u16");
    let mut bytes = control_header(ReadOnlyOpcode::RequestData.wire_value(), declared);
    bytes.push(0x00);
    assert_eq!(
        ControlFrame::decode(&bytes),
        Err(FrameError::LengthExceedsCap {
            declared: MAX_FRAME_PAYLOAD + 1
        })
    );

    // The largest value a u16 can carry is refused the same way.
    let mut bytes = control_header(ReadOnlyOpcode::RequestData.wire_value(), u16::MAX);
    bytes.push(0x00);
    assert_eq!(
        ControlFrame::decode(&bytes),
        Err(FrameError::LengthExceedsCap {
            declared: usize::from(u16::MAX)
        })
    );

    // And a bulk frame is bounded by the same cap.
    let mut bytes = bulk_header(0, declared);
    bytes.push(0x00);
    assert_eq!(
        BulkFrame::decode(&bytes),
        Err(FrameError::LengthExceedsCap {
            declared: MAX_FRAME_PAYLOAD + 1
        })
    );
}

#[test]
fn a_length_field_larger_than_the_bytes_present_is_refused_at_the_boundary() {
    for available in [0usize, 1, 7, 64] {
        let declared = available + 1;
        let mut bytes = control_header(
            ReadOnlyOpcode::ListDelta.wire_value(),
            u16::try_from(declared).expect("small"),
        );
        bytes.extend(std::iter::repeat_n(0x41u8, available));
        assert_eq!(
            ControlFrame::decode(&bytes),
            Err(FrameError::LengthExceedsInput {
                declared,
                available
            })
        );

        // Exactly the available bytes is the first accepted value.
        let mut exact = control_header(
            ReadOnlyOpcode::ListDelta.wire_value(),
            u16::try_from(available).expect("small"),
        );
        exact.extend(std::iter::repeat_n(0x41u8, available));
        assert_eq!(
            ControlFrame::decode(&exact).expect("exact fit").payload,
            vec![0x41; available]
        );
    }
}

#[test]
fn a_length_field_smaller_than_the_payload_never_yields_the_trailing_bytes() {
    let mut bytes = control_header(ReadOnlyOpcode::ListDelta.wire_value(), 2);
    bytes.extend_from_slice(b"ab-smuggled-tail");
    let frame = ControlFrame::decode(&bytes).expect("the declared prefix decodes");
    assert_eq!(frame.payload, b"ab", "only the declared bytes are returned");
}

#[test]
fn a_payload_one_byte_over_the_cap_cannot_be_constructed() {
    assert!(ControlFrame::new(ReadOnlyOpcode::ListDelta, vec![0; MAX_FRAME_PAYLOAD]).is_ok());
    assert_eq!(
        ControlFrame::new(ReadOnlyOpcode::ListDelta, vec![0; MAX_FRAME_PAYLOAD + 1]),
        Err(FrameError::LengthExceedsCap {
            declared: MAX_FRAME_PAYLOAD + 1
        })
    );
}

// ---------------------------------------------------------------- bulk frames

#[test]
fn a_bulk_frame_shorter_than_its_header_is_refused_at_every_length() {
    let full = chunk(0, b"abc").encode();
    for cut in 0..BulkFrame::HEADER_LEN {
        assert_eq!(
            BulkFrame::decode(&full[..cut]),
            Err(FrameError::TooShort),
            "{cut} bytes"
        );
    }
}

#[test]
fn a_bulk_length_past_the_bytes_present_is_refused_before_the_sentinel_check() {
    let mut bytes = bulk_header(0, 9);
    bytes.extend_from_slice(b"abc");
    assert_eq!(
        BulkFrame::decode(&bytes),
        Err(FrameError::LengthExceedsInput {
            declared: 9,
            available: 3,
        })
    );

    // Even the terminal marker must not carry a forged length.
    let mut sentinel = bulk_header(END_OF_TRANSFER, 4);
    sentinel.push(0x00);
    assert_eq!(
        BulkFrame::decode(&sentinel),
        Err(FrameError::LengthExceedsInput {
            declared: 4,
            available: 1,
        })
    );
}

#[test]
fn a_terminal_marker_carrying_a_payload_is_still_only_a_terminal_marker() {
    let mut bytes = bulk_header(END_OF_TRANSFER, 4);
    bytes.extend_from_slice(b"junk");
    assert_eq!(
        BulkFrame::decode(&bytes).expect("decodes"),
        BulkFrame::EndOfTransfer,
        "a sentinel never smuggles bytes into the transfer"
    );
}

#[test]
fn a_zero_length_chunk_decodes_as_an_empty_chunk_rather_than_as_an_end() {
    let bytes = bulk_header(0, 0);
    assert_eq!(
        BulkFrame::decode(&bytes).expect("decodes"),
        chunk(0, b""),
        "an empty chunk is not the terminal marker"
    );
}

#[test]
fn the_offset_just_below_the_sentinel_is_an_ordinary_chunk() {
    let mut bytes = bulk_header(END_OF_TRANSFER - 1, 1);
    bytes.push(0x7A);
    assert_eq!(
        BulkFrame::decode(&bytes).expect("decodes"),
        chunk(END_OF_TRANSFER - 1, &[0x7A])
    );
}

// ------------------------------------------------------------ buffering assembly

#[test]
fn a_conflicting_retransmission_fails_the_transfer_but_an_identical_one_does_not() {
    let mut assembler = BulkAssembler::with_budget(1_024);
    assembler.accept(&chunk(0, b"abc")).expect("first");
    assembler.accept(&chunk(0, b"abc")).expect("identical");
    assert_eq!(assembler.received_bytes(), 3);
    assert_eq!(
        assembler.accept(&chunk(0, b"abd")),
        Err(AssemblyError::ConflictingChunk { offset: 0 }),
        "one differing byte is still a conflict"
    );
}

#[test]
fn the_byte_budget_is_exact_at_its_boundary() {
    let mut assembler = BulkAssembler::with_budget(8);
    assembler.accept(&chunk(0, b"abcd")).expect("4 of 8");
    assembler.accept(&chunk(4, b"efgh")).expect("8 of 8");
    assert_eq!(assembler.received_bytes(), 8);
    assert_eq!(
        assembler.accept(&chunk(8, b"i")),
        Err(AssemblyError::BudgetExceeded { budget_bytes: 8 })
    );
}

#[test]
fn a_zero_byte_budget_refuses_the_very_first_chunk() {
    let mut assembler = BulkAssembler::with_budget(0);
    assert_eq!(
        assembler.accept(&chunk(0, b"a")),
        Err(AssemblyError::BudgetExceeded { budget_bytes: 0 })
    );
    assert_eq!(assembler.received_bytes(), 0);
    assert!(!assembler.ended());
}

#[test]
fn the_zero_progress_tolerance_fires_on_its_exact_frame_count() {
    let mut assembler = BulkAssembler::with_budget(1_024);
    assembler.accept(&chunk(0, b"abc")).expect("progress");
    let limit = BulkAssembler::DEFAULT_MAX_ZERO_PROGRESS_FRAMES;
    for frame in 1..limit {
        assert_eq!(
            assembler.accept(&chunk(0, b"abc")),
            Ok(TransferOutcome::InProgress),
            "duplicate {frame} is still tolerated"
        );
    }
    assert_eq!(
        assembler.accept(&chunk(0, b"abc")),
        Err(AssemblyError::ZeroProgress { frames: limit })
    );
}

#[test]
fn an_empty_chunk_counts_as_no_progress_rather_than_as_data() {
    let mut assembler = BulkAssembler::with_budget(1_024);
    assert_eq!(
        assembler.accept(&chunk(7, b"")),
        Ok(TransferOutcome::InProgress)
    );
    assert_eq!(assembler.received_bytes(), 0);
    assembler.accept(&BulkFrame::EndOfTransfer).expect("ends");
    assert_eq!(
        assembler.outcome(),
        TransferOutcome::CompleteLengthUnverified { received_bytes: 0 },
        "an empty chunk at offset 7 never became part of the range"
    );
}

#[test]
fn an_offset_that_would_run_past_the_addressable_range_is_refused_at_the_boundary() {
    let mut assembler = BulkAssembler::with_budget(1_024);
    // The last four addressable bytes are accepted.
    assembler
        .accept(&chunk(u32::MAX - 4, b"abcd"))
        .expect("exactly at the end of the range");
    // One byte further is not.
    assert_eq!(
        assembler.accept(&chunk(u32::MAX - 3, b"abcd")),
        Err(AssemblyError::OffsetOverflow {
            offset: u32::MAX - 3
        })
    );
}

#[test]
fn a_declared_length_shorter_than_the_received_bytes_is_not_complete() {
    let mut assembler = BulkAssembler::with_declared_length(1_024, 3);
    assembler.accept(&chunk(0, b"abcdef")).expect("accepted");
    assert_eq!(
        assembler.accept(&BulkFrame::EndOfTransfer).expect("ends"),
        TransferOutcome::IncompleteAtEnd {
            first_missing_offset: 6
        },
        "more bytes than declared is a disagreement, not a success"
    );
    assert!(assembler.finish().is_none());
}

#[test]
fn a_transfer_of_zero_declared_bytes_completes_with_no_chunks_at_all() {
    let mut assembler = BulkAssembler::with_declared_length(1_024, 0);
    assert_eq!(assembler.outcome(), TransferOutcome::InProgress);
    assert!(!assembler.ended());
    assert_eq!(
        assembler.accept(&BulkFrame::EndOfTransfer).expect("ends"),
        TransferOutcome::Complete
    );
    assert!(assembler.ended());
    assert_eq!(assembler.finish(), Some(Vec::new()));
}

#[test]
fn a_hole_at_the_very_first_byte_is_reported_as_offset_zero() {
    let mut assembler = BulkAssembler::with_declared_length(1_024, 6);
    assembler.accept(&chunk(3, b"def")).expect("accepted");
    assert_eq!(
        assembler.accept(&BulkFrame::EndOfTransfer).expect("ends"),
        TransferOutcome::IncompleteAtEnd {
            first_missing_offset: 0
        }
    );
    assert!(assembler.finish().is_none());
}

#[test]
fn a_frame_arriving_after_the_terminal_marker_is_refused_whatever_it_carries() {
    for late in [chunk(0, b"abc"), chunk(0, b""), chunk(99, b"x")] {
        let mut assembler = BulkAssembler::with_budget(1_024);
        assembler.accept(&BulkFrame::EndOfTransfer).expect("ends");
        assert_eq!(assembler.accept(&late), Err(AssemblyError::ChunkAfterEnd));
    }
}

// ------------------------------------------------------------ streaming assembly

#[test]
fn the_streaming_assembler_refuses_an_offset_past_the_addressable_range() {
    let mut assembler = StreamingAssembler::with_budget(1_024);
    assert_eq!(
        assembler.accept(&chunk(u32::MAX - 3, b"abcd")),
        Err(StreamError::Assembly(AssemblyError::OffsetOverflow {
            offset: u32::MAX - 3
        }))
    );
    assert_eq!(assembler.written_bytes(), 0);
}

#[test]
fn a_duplicate_of_a_held_chunk_is_tolerated_and_a_conflicting_one_is_not() {
    let mut assembler = StreamingAssembler::with_budget(1_024);
    assembler.accept(&chunk(8, b"abcd")).expect("held");
    assert_eq!(assembler.buffered_bytes(), 4);

    let step = assembler.accept(&chunk(8, b"abcd")).expect("identical");
    assert!(step.ready.is_empty(), "a duplicate releases nothing");
    assert_eq!(assembler.buffered_bytes(), 4, "held once, not twice");

    assert_eq!(
        assembler.accept(&chunk(8, b"abce")),
        Err(StreamError::Assembly(AssemblyError::ConflictingChunk {
            offset: 8
        }))
    );
}

#[test]
fn a_retransmission_that_ends_exactly_at_the_cursor_is_dropped_not_reapplied() {
    let mut assembler = StreamingAssembler::with_declared_length(1_024, 6);
    assembler.accept(&chunk(0, b"abc")).expect("written");
    // Ends exactly on the cursor: fully behind it, so it is only noise.
    let step = assembler.accept(&chunk(0, b"abc")).expect("duplicate");
    assert!(step.ready.is_empty(), "written bytes are not re-emitted");
    assert_eq!(assembler.written_bytes(), 3);

    // One byte past the cursor cannot be verified, so it fails the transfer.
    assert_eq!(
        assembler.accept(&chunk(2, b"cd")),
        Err(StreamError::ChunkBehindCursor {
            offset: 2,
            cursor: 3
        })
    );
}

#[test]
fn the_reorder_window_is_exact_at_its_boundary() {
    let mut assembler = StreamingAssembler::with_budget(1_000_000).with_reorder_window(8);
    assembler
        .accept(&chunk(100, b"01234567"))
        .expect("exactly the window");
    assert_eq!(assembler.buffered_bytes(), 8);
    assert_eq!(
        assembler.accept(&chunk(200, b"8")),
        Err(StreamError::ReorderWindowExceeded { window_bytes: 8 })
    );

    let rendered = StreamError::ReorderWindowExceeded { window_bytes: 8 }.to_string();
    assert!(rendered.contains('8'), "{rendered}");
}

#[test]
fn a_zero_width_reorder_window_refuses_any_out_of_order_chunk() {
    let mut assembler = StreamingAssembler::with_budget(1_024).with_reorder_window(0);
    // Nothing may be held, so the first chunk that cannot drain immediately
    // is refused rather than buffered.
    assert_eq!(
        assembler.accept(&chunk(4, b"a")),
        Err(StreamError::ReorderWindowExceeded { window_bytes: 0 })
    );
}

#[test]
fn the_streaming_budget_counts_written_and_held_bytes_at_its_boundary() {
    let mut assembler = StreamingAssembler::with_budget(8);
    assembler.accept(&chunk(0, b"abcd")).expect("written");
    assembler.accept(&chunk(4, b"efgh")).expect("written");
    assert_eq!(assembler.written_bytes(), 8);
    assert_eq!(
        assembler.accept(&chunk(8, b"i")),
        Err(StreamError::Assembly(AssemblyError::BudgetExceeded {
            budget_bytes: 8
        }))
    );
}

#[test]
fn an_empty_chunk_makes_no_progress_in_the_stream_either() {
    let mut assembler = StreamingAssembler::with_budget(1_024);
    let step = assembler.accept(&chunk(0, b"")).expect("tolerated once");
    assert!(step.ready.is_empty(), "an empty chunk releases nothing");
    assert_eq!(step.outcome, TransferOutcome::InProgress);
    assert_eq!(assembler.written_bytes(), 0);
    assert_eq!(assembler.buffered_bytes(), 0);
}

#[test]
fn a_stream_that_never_reaches_the_cursor_is_incomplete_at_the_marker() {
    let mut assembler = StreamingAssembler::with_declared_length(1_024, 8);
    assembler.accept(&chunk(4, b"efgh")).expect("held");
    let step = assembler.accept(&BulkFrame::EndOfTransfer).expect("ends");
    assert_eq!(
        step.outcome,
        TransferOutcome::IncompleteAtEnd {
            first_missing_offset: 0
        }
    );
    assert!(step.ready.is_empty(), "the marker releases nothing");
    assert!(assembler.ended());
    assert!(!assembler.is_complete());
}

#[test]
fn a_resumed_stream_refuses_bytes_behind_its_verified_offset() {
    let mut assembler = StreamingAssembler::resuming_at(1_024, 6, 3);
    assert_eq!(assembler.written_bytes(), 3);
    // A chunk entirely behind the verified point is noise.
    let step = assembler.accept(&chunk(0, b"abc")).expect("noise");
    assert!(
        step.ready.is_empty(),
        "bytes behind the verified offset are not re-emitted"
    );
    // A chunk straddling it cannot be checked against bytes we no longer hold.
    assert_eq!(
        assembler.accept(&chunk(1, b"bcde")),
        Err(StreamError::ChunkBehindCursor {
            offset: 1,
            cursor: 3
        })
    );
}

#[test]
fn a_stream_zero_progress_loop_terminates_on_its_exact_frame_count() {
    let mut assembler = StreamingAssembler::with_budget(1_024);
    let limit = BulkAssembler::DEFAULT_MAX_ZERO_PROGRESS_FRAMES;
    for frame in 1..limit {
        assert!(
            assembler.accept(&chunk(0, b"")).is_ok(),
            "empty frame {frame} is tolerated"
        );
    }
    assert_eq!(
        assembler.accept(&chunk(0, b"")),
        Err(StreamError::Assembly(AssemblyError::ZeroProgress {
            frames: limit
        }))
    );
}

#[test]
fn a_declared_length_shorter_than_the_streamed_bytes_is_not_complete() {
    let mut assembler = StreamingAssembler::with_declared_length(1_024, 3);
    assembler.accept(&chunk(0, b"abcdef")).expect("written");
    assert_eq!(
        assembler
            .accept(&BulkFrame::EndOfTransfer)
            .expect("ends")
            .outcome,
        TransferOutcome::IncompleteAtEnd {
            first_missing_offset: 6
        }
    );
    assert!(!assembler.is_complete());
}

// --------------------------------------------------------------------- profiles

#[test]
fn a_blank_or_whitespace_only_firmware_never_matches_a_profile() {
    for firmware in ["", " ", "\t", "\n  \t"] {
        assert!(
            !LEGACY_NOTE_V0095.covers_firmware(firmware),
            "{firmware:?} must not be read as a fingerprint"
        );
        assert!(profile_for("Note", firmware).is_none());
    }
    assert!(LEGACY_NOTE_V0095.covers_firmware("  v0095  "));
}

#[test]
fn a_model_that_merely_contains_the_known_one_is_not_the_known_one() {
    for model in ["", "Note Pro", "NotePin", "XNote", "Note2", "  "] {
        assert!(
            !LEGACY_NOTE_V0095.covers_model(model),
            "{model:?} must not match"
        );
    }
    assert!(LEGACY_NOTE_V0095.covers_model("  NOTE "));
}

#[test]
fn an_unimplemented_auth_mode_is_refused_rather_than_downgraded() {
    assert!(LEGACY_NOTE_V0095.auth_mode_is_implemented());
    let encrypted = ProtocolProfile {
        auth_mode: ProfileAuthMode::EncryptedHandshake,
        ..LEGACY_NOTE_V0095
    };
    assert!(!encrypted.auth_mode_is_implemented());
    assert!(!encrypted.accepts_token_length(0));
}

#[test]
fn an_unknown_profile_name_resolves_to_nothing() {
    assert!(profile_by_name("").is_none());
    assert!(profile_by_name("plaud-note-legacy-v0096").is_none());
    assert_eq!(
        profile_by_name(LEGACY_NOTE_V0095.name).map(|p| p.name),
        Some(LEGACY_NOTE_V0095.name)
    );
}
