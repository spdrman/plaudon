//! Hostile-input behaviour of the payload codecs and the `.ASR` sidecar
//! scanner (spec 18.2A, 7.7, 7.9, App. B.3).
//!
//! Each test feeds real bytes to a real decoder and asserts the rejection it
//! produces. Between them they produce every [`PayloadError`] and every
//! [`AsrError`] variant.

use std::io::{Read, Write};

use plaudon_plaud_protocol::asr::{
    packet_channels, packet_duration_us, scan, ASR_DECODED_SAMPLE_RATE_HZ, MAX_PACKET_BYTES,
    MAX_PACKET_DURATION_US, OPUS_TIMEBASE_HZ,
};
use plaudon_plaud_protocol::payload::{
    encode_authenticate, MAX_DISPLAY_NAME_LEN, MAX_RECORDING_ID_LEN,
};
use plaudon_plaud_protocol::profile::LEGACY_NOTE_V0095;
use plaudon_plaud_protocol::{
    remux_to_ogg_opus, AsrError, AsrScanner, AuthResponse, AuthStatus, DataRequest, DeltaListing,
    DeviceCodec, ListedRecording, PayloadError,
};

// ----------------------------------------------------------------- fixtures

/// One listing entry, built from raw bytes so a declared length can disagree
/// with the value that follows it.
fn raw_entry(id: &[u8], name: &[u8], size: u32, captured: u32, codec: u8) -> Vec<u8> {
    let mut out = vec![u8::try_from(id.len()).expect("test ids stay under 256 bytes")];
    out.extend_from_slice(id);
    out.push(u8::try_from(name.len()).expect("test names stay under 256 bytes"));
    out.extend_from_slice(name);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(&captured.to_be_bytes());
    out.push(codec);
    out
}

fn raw_listing(entries: &[Vec<u8>], truncated: u8) -> Vec<u8> {
    let mut out = u16::try_from(entries.len())
        .expect("test listings stay small")
        .to_be_bytes()
        .to_vec();
    for entry in entries {
        out.extend_from_slice(entry);
    }
    out.push(truncated);
    out
}

fn entry(id: &str) -> ListedRecording {
    ListedRecording {
        id: id.to_owned(),
        display_name: format!("{id}.ASR"),
        declared_byte_length: 4_096,
        captured_at_unix: 1_760_000_000,
        codec: DeviceCodec::AsrOpus,
    }
}

// -------------------------------------------------------- authenticate payload

#[test]
fn every_undefined_authentication_status_byte_is_refused() {
    for status in 3u8..=255 {
        assert_eq!(
            AuthResponse::decode(&[status, 0x00]),
            Err(PayloadError::Undefined { field: "status" }),
            "{status:#04x} must not be read as an outcome"
        );
    }
}

#[test]
fn the_three_defined_statuses_round_trip_at_both_detail_extremes() {
    for status in [
        AuthStatus::Accepted,
        AuthStatus::Rejected,
        AuthStatus::EncryptedModeRequired,
    ] {
        for detail in [0u8, 1, 255] {
            let response = AuthResponse { status, detail };
            let encoded = response.encode();
            assert_eq!(encoded.len(), 2);
            assert_eq!(AuthResponse::decode(&encoded), Ok(response));
        }
    }
}

#[test]
fn an_authentication_response_that_is_short_or_long_is_refused() {
    assert_eq!(AuthResponse::decode(&[]), Err(PayloadError::Truncated));
    assert_eq!(AuthResponse::decode(&[0x00]), Err(PayloadError::Truncated));
    for extra in 1usize..4 {
        let mut bytes = vec![0x00, 0x00];
        bytes.extend(std::iter::repeat_n(0xFFu8, extra));
        assert_eq!(
            AuthResponse::decode(&bytes),
            Err(PayloadError::TrailingBytes { remaining: extra }),
            "{extra} trailing bytes"
        );
    }
}

#[test]
fn a_credential_of_any_unaccepted_length_is_refused_without_echoing_it() {
    for bytes in [0usize, 1, 15, 17, 31, 33, 64, 1_024] {
        let token = vec![0xABu8; bytes];
        let error = encode_authenticate(&LEGACY_NOTE_V0095, &token)
            .expect_err("only the profile's lengths are accepted");
        assert_eq!(error, PayloadError::TokenLength { bytes });
        let rendered = error.to_string();
        assert!(!rendered.contains("ab"), "{rendered} must not echo bytes");
    }
    for bytes in [16usize, 32] {
        let token = vec![0xABu8; bytes];
        assert_eq!(
            encode_authenticate(&LEGACY_NOTE_V0095, &token),
            Ok(token.clone())
        );
    }
}

// ------------------------------------------------------------ listing payload

#[test]
fn a_listing_longer_than_the_profile_bound_cannot_be_encoded() {
    let bound = LEGACY_NOTE_V0095.max_listed_recordings;
    let at_bound = DeltaListing {
        entries: (0..bound).map(|n| entry(&format!("R{n}"))).collect(),
        truncated: true,
    };
    let encoded = at_bound
        .encode(&LEGACY_NOTE_V0095)
        .expect("exactly at bound");
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &encoded),
        Ok(at_bound)
    );

    let over_bound = DeltaListing {
        entries: (0..=bound).map(|n| entry(&format!("R{n}"))).collect(),
        truncated: false,
    };
    assert_eq!(
        over_bound.encode(&LEGACY_NOTE_V0095),
        Err(PayloadError::BoundExceeded {
            field: "count",
            declared: bound + 1,
            bound,
        })
    );
}

#[test]
fn a_declared_byte_length_beyond_the_wire_field_is_refused_rather_than_wrapped() {
    let mut huge = entry("REC0001");
    huge.declared_byte_length = u64::from(u32::MAX) + 1;
    let error = DeltaListing {
        entries: vec![huge],
        truncated: false,
    }
    .encode(&LEGACY_NOTE_V0095)
    .expect_err("a length that does not fit the field is not truncated into it");
    assert!(
        matches!(
            error,
            PayloadError::BoundExceeded {
                field: "declared_byte_length",
                ..
            }
        ),
        "{error:?}"
    );

    // The largest value the field can carry still round trips.
    let mut largest = entry("REC0001");
    largest.declared_byte_length = u64::from(u32::MAX);
    largest.captured_at_unix = u32::MAX;
    let listing = DeltaListing {
        entries: vec![largest],
        truncated: false,
    };
    let encoded = listing.encode(&LEGACY_NOTE_V0095).expect("in range");
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &encoded),
        Ok(listing)
    );
}

#[test]
fn an_identifier_longer_than_the_bound_is_refused_on_both_sides_of_the_wire() {
    let mut long = entry("REC0001");
    long.id = "x".repeat(MAX_RECORDING_ID_LEN + 1);
    assert_eq!(
        DeltaListing {
            entries: vec![long],
            truncated: false
        }
        .encode(&LEGACY_NOTE_V0095),
        Err(PayloadError::BoundExceeded {
            field: "id",
            declared: MAX_RECORDING_ID_LEN + 1,
            bound: MAX_RECORDING_ID_LEN,
        })
    );

    // A device that declares an over-long identifier is refused at decode too,
    // before the bytes it claims are read.
    let mut payload = vec![0x00, 0x01];
    payload.push(u8::try_from(MAX_RECORDING_ID_LEN + 1).expect("65 fits"));
    payload.extend(std::iter::repeat_n(0x41u8, 16));
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &payload),
        Err(PayloadError::BoundExceeded {
            field: "id",
            declared: MAX_RECORDING_ID_LEN + 1,
            bound: MAX_RECORDING_ID_LEN,
        })
    );

    // And the same for a display name.
    let mut payload = vec![0x00, 0x01, 0x02, 0x41, 0x42];
    payload.push(u8::try_from(MAX_DISPLAY_NAME_LEN + 1).expect("129 fits"));
    payload.extend(std::iter::repeat_n(0x41u8, 16));
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &payload),
        Err(PayloadError::BoundExceeded {
            field: "display_name",
            declared: MAX_DISPLAY_NAME_LEN + 1,
            bound: MAX_DISPLAY_NAME_LEN,
        })
    );
}

#[test]
fn a_text_field_that_is_not_utf8_is_refused_rather_than_repaired() {
    let bad_id = raw_listing(&[raw_entry(&[0xFF, 0xFE, 0xFD], b"name", 1, 1, 0x01)], 0);
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &bad_id),
        Err(PayloadError::NotUtf8 { field: "id" })
    );

    let bad_name = raw_listing(&[raw_entry(b"REC1", &[0x80, 0x81], 1, 1, 0x01)], 0);
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &bad_name),
        Err(PayloadError::NotUtf8 {
            field: "display_name"
        })
    );

    // A lone continuation byte appended to valid text is still invalid.
    let truncated_utf8 = raw_listing(&[raw_entry("é".as_bytes(), b"n", 1, 1, 0x01)], 0);
    assert!(DeltaListing::decode(&LEGACY_NOTE_V0095, &truncated_utf8).is_ok());
    let cut = raw_listing(&[raw_entry(&"é".as_bytes()[..1], b"n", 1, 1, 0x01)], 0);
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &cut),
        Err(PayloadError::NotUtf8 { field: "id" })
    );
}

#[test]
fn an_embedded_nul_is_carried_verbatim_rather_than_ending_the_field() {
    let payload = raw_listing(&[raw_entry(b"a\0b", b"n\0m", 1, 1, 0x01)], 0);
    let listing = DeltaListing::decode(&LEGACY_NOTE_V0095, &payload).expect("valid UTF-8");
    assert_eq!(listing.entries[0].id, "a\u{0}b");
    assert_eq!(listing.entries[0].display_name, "n\u{0}m");
}

#[test]
fn an_entry_with_an_empty_identifier_is_refused() {
    let payload = raw_listing(&[raw_entry(b"", b"name", 1, 1, 0x01)], 0);
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &payload),
        Err(PayloadError::Undefined { field: "id" })
    );
}

#[test]
fn an_empty_display_name_is_allowed_because_it_is_not_identity() {
    let payload = raw_listing(&[raw_entry(b"REC1", b"", 0, 0, 0x01)], 0);
    let listing = DeltaListing::decode(&LEGACY_NOTE_V0095, &payload).expect("decodes");
    assert!(
        listing.entries[0].display_name.is_empty(),
        "a blank display name is advisory metadata, not a rejection"
    );
    assert_eq!(listing.entries[0].declared_byte_length, 0);
    assert_eq!(listing.entries[0].captured_at_unix, 0);
}

#[test]
fn every_undefined_truncation_flag_is_refused() {
    for flag in 2u8..=255 {
        let payload = raw_listing(&[raw_entry(b"REC1", b"n", 1, 1, 0x01)], flag);
        assert_eq!(
            DeltaListing::decode(&LEGACY_NOTE_V0095, &payload),
            Err(PayloadError::Undefined { field: "truncated" }),
            "{flag:#04x}"
        );
    }
    for (flag, expected) in [(0u8, false), (1, true)] {
        let payload = raw_listing(&[raw_entry(b"REC1", b"n", 1, 1, 0x01)], flag);
        let listing = DeltaListing::decode(&LEGACY_NOTE_V0095, &payload).expect("defined");
        assert_eq!(listing.truncated, expected);
    }
}

#[test]
fn every_prefix_of_a_valid_listing_is_refused_as_truncated() {
    let full = raw_listing(
        &[
            raw_entry(b"REC1", b"one", 10, 20, 0x01),
            raw_entry(b"REC2", b"two", 30, 40, 0xFF),
        ],
        1,
    );
    assert!(DeltaListing::decode(&LEGACY_NOTE_V0095, &full).is_ok());
    for cut in 0..full.len() {
        assert_eq!(
            DeltaListing::decode(&LEGACY_NOTE_V0095, &full[..cut]),
            Err(PayloadError::Truncated),
            "a {cut} byte prefix must never yield a partial listing"
        );
    }
}

#[test]
fn trailing_bytes_after_a_listing_are_refused_at_every_length() {
    let mut full = raw_listing(&[raw_entry(b"REC1", b"one", 10, 20, 0x01)], 0);
    for extra in 1usize..4 {
        full.push(0xAA);
        assert_eq!(
            DeltaListing::decode(&LEGACY_NOTE_V0095, &full),
            Err(PayloadError::TrailingBytes { remaining: extra })
        );
    }
}

#[test]
fn a_forged_entry_count_is_refused_before_any_allocation() {
    // Beyond the profile bound.
    let bound = LEGACY_NOTE_V0095.max_listed_recordings;
    let declared = u16::try_from(bound + 1).expect("513 fits");
    let mut payload = declared.to_be_bytes().to_vec();
    payload.extend(std::iter::repeat_n(0u8, 16));
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &payload),
        Err(PayloadError::BoundExceeded {
            field: "count",
            declared: bound + 1,
            bound,
        })
    );

    // Inside the bound, but larger than the payload could possibly hold.
    let mut payload = u16::try_from(bound)
        .expect("512 fits")
        .to_be_bytes()
        .to_vec();
    payload.extend(std::iter::repeat_n(0u8, 16));
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &payload),
        Err(PayloadError::Truncated)
    );

    // The smallest over-claim: one entry in a payload that cannot hold one.
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &[0x00, 0x01, 0x00]),
        Err(PayloadError::Truncated)
    );
}

#[test]
fn an_empty_listing_needs_only_its_flag() {
    let decoded =
        DeltaListing::decode(&LEGACY_NOTE_V0095, &[0x00, 0x00, 0x00]).expect("count zero");
    assert!(
        decoded.entries.is_empty(),
        "a count of zero lists no recordings"
    );
    assert!(!decoded.truncated);
    assert_eq!(
        DeltaListing::decode(&LEGACY_NOTE_V0095, &[0x00, 0x00]),
        Err(PayloadError::Truncated),
        "even an empty listing must carry its truncation flag"
    );
}

#[test]
fn every_codec_byte_other_than_the_known_one_is_unknown() {
    assert_eq!(DeviceCodec::from_wire(0x01), DeviceCodec::AsrOpus);
    assert_eq!(DeviceCodec::AsrOpus.wire_value(), 0x01);
    assert_eq!(DeviceCodec::AsrOpus.mime(), "audio/opus");
    assert_eq!(DeviceCodec::Unknown.wire_value(), 0xFF);
    assert_eq!(DeviceCodec::Unknown.mime(), "application/octet-stream");
    for value in 0u8..=255 {
        if value == 0x01 {
            continue;
        }
        assert_eq!(
            DeviceCodec::from_wire(value),
            DeviceCodec::Unknown,
            "{value:#04x} is not a codec this build claims to decode"
        );
    }
}

// ------------------------------------------------------- data request payload

#[test]
fn every_prefix_of_a_data_request_is_refused_as_truncated() {
    let request = DataRequest {
        id: "REC0001".into(),
        offset: u32::MAX,
        length: u32::MAX,
    };
    let encoded = request.encode().expect("in bounds");
    assert_eq!(DataRequest::decode(&encoded), Ok(request));
    for cut in 0..encoded.len() {
        assert_eq!(
            DataRequest::decode(&encoded[..cut]),
            Err(PayloadError::Truncated),
            "{cut} byte prefix"
        );
    }
}

#[test]
fn a_data_request_is_refused_when_its_identifier_is_unusable() {
    // Over the bound, declared but not present.
    let mut payload = vec![u8::try_from(MAX_RECORDING_ID_LEN + 1).expect("65 fits")];
    payload.extend(std::iter::repeat_n(0x41u8, 8));
    assert_eq!(
        DataRequest::decode(&payload),
        Err(PayloadError::BoundExceeded {
            field: "id",
            declared: MAX_RECORDING_ID_LEN + 1,
            bound: MAX_RECORDING_ID_LEN,
        })
    );

    // Not UTF-8.
    let mut payload = vec![0x02, 0xFF, 0xFE];
    payload.extend(std::iter::repeat_n(0u8, 8));
    assert_eq!(
        DataRequest::decode(&payload),
        Err(PayloadError::NotUtf8 { field: "id" })
    );

    // Empty.
    let payload = vec![0x00, 0, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(
        DataRequest::decode(&payload),
        Err(PayloadError::Undefined { field: "id" })
    );

    // Longer than the field allows, on the encode side.
    let request = DataRequest {
        id: "x".repeat(MAX_RECORDING_ID_LEN + 1),
        offset: 0,
        length: 0,
    };
    assert_eq!(
        request.encode(),
        Err(PayloadError::BoundExceeded {
            field: "id",
            declared: MAX_RECORDING_ID_LEN + 1,
            bound: MAX_RECORDING_ID_LEN,
        })
    );
}

#[test]
fn a_data_request_with_trailing_bytes_is_refused() {
    let mut encoded = DataRequest {
        id: "REC1".into(),
        offset: 0,
        length: 0,
    }
    .encode()
    .expect("in bounds");
    encoded.push(0x00);
    assert_eq!(
        DataRequest::decode(&encoded),
        Err(PayloadError::TrailingBytes { remaining: 1 })
    );
}

// ------------------------------------------------------------ `.ASR` framing

/// A packet with an explicit table-of-contents byte and `payload` filler.
fn packet(config: u8, code: u8, stereo: bool, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![(config << 3) | (u8::from(stereo) << 2) | code];
    out.extend_from_slice(payload);
    out
}

/// A 20 ms mono SILK-wideband packet of `filler` extra bytes.
fn mono_20ms(filler: usize) -> Vec<u8> {
    packet(9, 0, false, &vec![0x55; filler])
}

fn framed(packets: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for item in packets {
        out.extend_from_slice(
            &u16::try_from(item.len())
                .expect("test packets fit")
                .to_be_bytes(),
        );
        out.extend_from_slice(item);
    }
    out
}

#[test]
fn an_empty_packet_has_no_table_of_contents_byte() {
    assert_eq!(packet_duration_us(&[]), Err(AsrError::EmptyPacket));
    assert_eq!(packet_channels(&[]), 1, "no byte means no stereo claim");
}

#[test]
fn a_code_three_packet_without_a_frame_count_is_refused() {
    let toc = (9 << 3) | 3;
    assert_eq!(packet_duration_us(&[toc]), Err(AsrError::InvalidFraming));
    assert_eq!(packet_duration_us(&[toc, 0]), Err(AsrError::InvalidFraming));
    for frames in 49u8..=63 {
        assert_eq!(
            packet_duration_us(&[toc, frames]),
            Err(AsrError::InvalidFraming),
            "{frames} frames is past the Opus maximum of 48"
        );
    }
}

#[test]
fn the_opus_duration_ceiling_is_exact() {
    // 48 frames of 2.5 ms is exactly the 120 ms maximum.
    let at_cap = [(16u8 << 3) | 3, 48];
    assert_eq!(packet_duration_us(&at_cap), Ok(MAX_PACKET_DURATION_US));

    // 48 frames of 5 ms is not.
    let over_cap = [(17u8 << 3) | 3, 48];
    assert_eq!(
        packet_duration_us(&over_cap),
        Err(AsrError::PacketTooLong {
            declared_us: 240_000
        })
    );

    // Two 60 ms SILK frames are exactly at the cap; the frame-count codes 1
    // and 2 both mean two frames.
    for code in [1u8, 2] {
        assert_eq!(
            packet_duration_us(&[(3 << 3) | code, 0x00]),
            Ok(MAX_PACKET_DURATION_US)
        );
    }
}

#[test]
fn a_stereo_table_of_contents_byte_is_reported_as_two_channels() {
    let stereo = packet(9, 0, true, &[0x55; 8]);
    assert_eq!(packet_channels(&stereo), 2);
    let mono = packet(9, 0, false, &[0x55; 8]);
    assert_eq!(packet_channels(&mono), 1);

    let summary = scan(&framed(&[stereo])).expect("a valid stereo sidecar");
    assert_eq!(summary.channels, 2);
    assert_eq!(summary.packet_count, 1);
}

#[test]
fn a_sidecar_with_no_packets_is_refused_rather_than_summarised_as_silence() {
    assert_eq!(scan(&[]), Err(AsrError::NoPackets));
    // A length prefix that never completes is held, not summarised.
    assert_eq!(scan(&[0x00]), Err(AsrError::Truncated));
}

#[test]
fn a_zero_length_packet_prefix_is_refused() {
    assert_eq!(scan(&[0x00, 0x00]), Err(AsrError::EmptyPacket));
    let mut after_good = framed(&[mono_20ms(8)]);
    after_good.extend_from_slice(&[0x00, 0x00]);
    assert_eq!(scan(&after_good), Err(AsrError::EmptyPacket));
}

#[test]
fn the_packet_cap_is_exact_and_a_forged_prefix_never_allocates() {
    let at_cap = mono_20ms(MAX_PACKET_BYTES - 1);
    assert_eq!(at_cap.len(), MAX_PACKET_BYTES);
    let summary = scan(&framed(&[at_cap])).expect("exactly at the cap");
    assert_eq!(summary.packet_bytes, MAX_PACKET_BYTES as u64);

    let declared = u16::try_from(MAX_PACKET_BYTES + 1).expect("8193 fits");
    let mut scanner = AsrScanner::new();
    assert_eq!(
        scanner.push(&declared.to_be_bytes()),
        Err(AsrError::PacketTooLarge {
            declared: MAX_PACKET_BYTES + 1
        })
    );
    assert!(
        scanner.buffered_bytes() <= 2,
        "nothing is reserved for a length that was never delivered"
    );
}

#[test]
fn a_failed_scanner_stays_failed_on_every_later_call() {
    let mut scanner = AsrScanner::new();
    let first = scanner.push(&[0x00, 0x00]).expect_err("zero length packet");
    assert_eq!(first, AsrError::EmptyPacket);
    // More bytes do not rehabilitate a stream that already broke its framing.
    assert_eq!(scanner.push(&framed(&[mono_20ms(8)])), Err(first.clone()));
    assert_eq!(scanner.push(&[]), Err(first.clone()));
    assert_eq!(scanner.finish(), Err(first));
}

#[test]
fn a_sidecar_cut_at_any_byte_is_never_summarised() {
    let full = framed(&[mono_20ms(16), mono_20ms(24)]);
    for cut in 1..full.len() {
        let mut scanner = AsrScanner::new();
        scanner
            .push(&full[..cut])
            .expect("a prefix of valid framing is held, never rejected");
        // A prefix either stops inside a packet, which cannot be summarised at
        // all, or on a packet boundary, where it may only account for the
        // packets that actually arrived.
        match scanner.finish() {
            Ok(summary) => assert!(summary.packet_count < 2, "{cut} byte prefix"),
            Err(error) => assert_eq!(error, AsrError::Truncated, "{cut} byte prefix"),
        }
    }
    assert_eq!(scan(&full).expect("whole sidecar").packet_count, 2);
}

#[test]
fn a_measured_sidecar_reports_its_derived_output_sample_count() {
    let summary = scan(&framed(&vec![mono_20ms(40); 50])).expect("valid");
    assert_eq!(summary.duration_ms, 1_000);
    assert_eq!(summary.samples_48k, u64::from(OPUS_TIMEBASE_HZ));
    assert_eq!(
        summary.derived_samples_at_output_rate(),
        u64::from(ASR_DECODED_SAMPLE_RATE_HZ)
    );
}

// --------------------------------------------------------------- Ogg remuxing

/// A reader that hands over at most `step` bytes at a time.
#[derive(Debug)]
struct ChunkedReader {
    data: Vec<u8>,
    at: usize,
    step: usize,
}

impl Read for ChunkedReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let take = self.step.min(buf.len()).min(self.data.len() - self.at);
        buf[..take].copy_from_slice(&self.data[self.at..self.at + take]);
        self.at += take;
        Ok(take)
    }
}

/// A reader that fails the way a disconnected device does.
#[derive(Debug)]
struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("the device went away"))
    }
}

/// A sink that refuses every byte.
#[derive(Debug)]
struct FailingWriter;

impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("no space left"))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The Ogg CRC-32, reimplemented here so the test checks the container rather
/// than trusting the writer's own helper.
fn ogg_crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 == 0 {
                crc << 1
            } else {
                (crc << 1) ^ 0x04C1_1DB7
            };
        }
    }
    crc
}

/// Walk every page, checking the capture pattern and the checksum.
fn verified_pages(stream: &[u8]) -> usize {
    let mut at = 0usize;
    let mut count = 0usize;
    while at < stream.len() {
        assert!(at + 27 <= stream.len(), "page {count} has no header");
        assert_eq!(&stream[at..at + 4], b"OggS", "page {count} capture pattern");
        let segments = usize::from(stream[at + 26]);
        let header_len = 27 + segments;
        assert!(at + header_len <= stream.len());
        let body: usize = stream[at + 27..at + header_len]
            .iter()
            .map(|lace| usize::from(*lace))
            .sum();
        let page_len = header_len + body;
        let mut page = stream[at..at + page_len].to_vec();
        let declared = u32::from_le_bytes([page[22], page[23], page[24], page[25]]);
        page[22..26].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(ogg_crc32(&page), declared, "page {count} checksum");
        at += page_len;
        count += 1;
    }
    count
}

#[test]
fn a_remux_of_nothing_is_refused_rather_than_producing_an_empty_container() {
    let mut source: &[u8] = &[];
    let mut out = Vec::new();
    assert_eq!(
        remux_to_ogg_opus(&mut source, &mut out, 1),
        Err(AsrError::NoPackets)
    );
    assert!(out.is_empty(), "no container is written for no packets");
}

#[test]
fn a_remux_refuses_a_zero_length_or_oversized_packet_declaration() {
    let mut out = Vec::new();
    assert_eq!(
        remux_to_ogg_opus(&mut [0x00u8, 0x00].as_slice(), &mut out, 1),
        Err(AsrError::EmptyPacket)
    );

    let declared = u16::try_from(MAX_PACKET_BYTES + 1).expect("8193 fits");
    let mut out = Vec::new();
    assert_eq!(
        remux_to_ogg_opus(&mut declared.to_be_bytes().as_slice(), &mut out, 1),
        Err(AsrError::PacketTooLarge {
            declared: MAX_PACKET_BYTES + 1
        })
    );
}

#[test]
fn a_remux_refuses_a_packet_whose_framing_is_impossible() {
    // A code-3 packet declaring zero frames: valid length prefix, invalid Opus.
    let bad = framed(&[packet(9, 3, false, &[0x00])]);
    let mut out = Vec::new();
    assert_eq!(
        remux_to_ogg_opus(&mut bad.as_slice(), &mut out, 1),
        Err(AsrError::InvalidFraming)
    );
}

#[test]
fn a_remux_of_a_truncated_sidecar_is_refused() {
    let mut bytes = framed(&[mono_20ms(16)]);
    bytes.truncate(bytes.len() - 3);
    let mut out = Vec::new();
    assert_eq!(
        remux_to_ogg_opus(&mut bytes.as_slice(), &mut out, 1),
        Err(AsrError::Truncated)
    );
}

#[test]
fn a_source_that_fails_mid_transfer_is_reported_as_an_io_error() {
    let mut out = Vec::new();
    assert_eq!(
        remux_to_ogg_opus(&mut FailingReader, &mut out, 1),
        Err(AsrError::Io)
    );
}

#[test]
fn a_sink_that_refuses_bytes_is_reported_as_an_io_error() {
    let bytes = framed(&[mono_20ms(16)]);
    assert_eq!(
        remux_to_ogg_opus(&mut bytes.as_slice(), &mut FailingWriter, 1),
        Err(AsrError::Io)
    );
}

#[test]
fn a_source_delivering_one_byte_at_a_time_still_remuxes_identically() {
    let bytes = framed(&vec![mono_20ms(12); 5]);
    let mut whole = Vec::new();
    let expected = remux_to_ogg_opus(&mut bytes.as_slice(), &mut whole, 42).expect("remux");

    let mut dribbled = Vec::new();
    let mut reader = ChunkedReader {
        data: bytes,
        at: 0,
        step: 1,
    };
    let summary = remux_to_ogg_opus(&mut reader, &mut dribbled, 42).expect("remux");
    assert_eq!(summary, expected);
    assert_eq!(
        dribbled, whole,
        "the container does not depend on read sizes"
    );
}

#[test]
fn packets_at_and_past_the_lacing_boundary_are_segmented_correctly() {
    // 255 bytes needs a terminating zero lacing value; 256 and 600 need a
    // continuation; 254 fits in one.
    for size in [254usize, 255, 256, 509, 510, 600] {
        let bytes = framed(&[mono_20ms(size - 1)]);
        let mut out = Vec::new();
        let summary = remux_to_ogg_opus(&mut bytes.as_slice(), &mut out, 9).expect("remux");
        assert_eq!(summary.packet_bytes, size as u64);
        assert!(
            verified_pages(&out) >= 3,
            "{size} byte packet produced a malformed container"
        );
    }
}

#[test]
fn a_long_sidecar_is_split_into_several_valid_pages() {
    // Enough packets that the lacing table cannot hold them all on one page.
    let bytes = framed(&vec![mono_20ms(20); 600]);
    let mut out = Vec::new();
    let summary = remux_to_ogg_opus(&mut bytes.as_slice(), &mut out, 0x1234_5678).expect("remux");
    assert_eq!(summary.packet_count, 600);
    assert_eq!(summary.duration_ms, 12_000);
    assert!(
        verified_pages(&out) > 4,
        "a long stream must be paginated, not written as one page"
    );
    assert_eq!(scan(&bytes).expect("scan"), summary);
}
