//! Payload codecs for the three allowlisted operations (spec §18.2A, App. B.3).
//!
//! # Provenance of these layouts
//!
//! Spec App. B.3 records the framing, the GATT identifiers and the three
//! opcodes from upstream reverse-engineering notes. It does **not** publish a
//! per-entry listing layout, and it warns that upstream "includes some
//! protocol parsing comments and fallback heuristics around response layouts;
//! our implementation must replace any unbounded or guessed fallback with a
//! bounded, explicitly tested strategy".
//!
//! The layouts below are therefore **this build's** definition for the named
//! profile, exercised against our own fixtures and `FakeDevice`. They are not
//! a hardware observation, which is why every `compatibility.yaml` cell that
//! would use them is `not_evaluated` until a real device validates it. The
//! value they carry is the bounded, total parser: no heuristic fallback, no
//! unbounded allocation, no trailing-garbage tolerance.
//!
//! ```text
//! authenticate  request : | token bytes (profile-constrained length) |
//! authenticate  response: | status u8 | detail u8 |
//! list delta    request : | (empty) |
//! list delta    response: | count u16 BE | entry * count | truncated u8 |
//!   entry               : | id_len u8 | id | name_len u8 | name
//!                         | size u32 BE | captured_at u32 BE | codec u8 |
//! request data  request : | id_len u8 | id | offset u32 BE | length u32 BE |
//! ```

use serde::{Deserialize, Serialize};

use crate::profile::ProtocolProfile;

/// Longest device-side recording identifier this build accepts.
pub const MAX_RECORDING_ID_LEN: usize = 64;

/// Longest device-reported display name this build accepts.
pub const MAX_DISPLAY_NAME_LEN: usize = 128;

/// Why a payload was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
    /// The payload ended before a declared field.
    #[error("the payload ended before a declared field")]
    Truncated,
    /// Bytes remained after the last declared field.
    #[error("{remaining} bytes remained after the declared payload")]
    TrailingBytes {
        /// Bytes left over.
        remaining: usize,
    },
    /// A declared count or length exceeds the bound for this profile.
    #[error("declared {field} of {declared} exceeds the bound of {bound}")]
    BoundExceeded {
        /// Field that was over the bound.
        field: &'static str,
        /// Value declared.
        declared: usize,
        /// Bound this build enforces.
        bound: usize,
    },
    /// A text field is not valid UTF-8.
    #[error("the {field} field is not valid UTF-8")]
    NotUtf8 {
        /// Field that failed to decode.
        field: &'static str,
    },
    /// A field held a value the profile does not define.
    #[error("the {field} field holds an undefined value")]
    Undefined {
        /// Field that was undefined.
        field: &'static str,
    },
    /// A token length is outside the profile's accepted set.
    #[error("a credential of {bytes} bytes does not match this device profile")]
    TokenLength {
        /// Length that was offered.
        bytes: usize,
    },
}

/// A bounded cursor over a payload.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], PayloadError> {
        let end = self.at.checked_add(count).ok_or(PayloadError::Truncated)?;
        if end > self.bytes.len() {
            return Err(PayloadError::Truncated);
        }
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, PayloadError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PayloadError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, PayloadError> {
        let bytes = self.take(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A `u8`-length-prefixed string with an explicit bound.
    fn short_string(&mut self, field: &'static str, bound: usize) -> Result<String, PayloadError> {
        let length = usize::from(self.u8()?);
        if length > bound {
            return Err(PayloadError::BoundExceeded {
                field,
                declared: length,
                bound,
            });
        }
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| PayloadError::NotUtf8 { field })
    }

    fn finish(self) -> Result<(), PayloadError> {
        let remaining = self.bytes.len() - self.at;
        if remaining == 0 {
            Ok(())
        } else {
            Err(PayloadError::TrailingBytes { remaining })
        }
    }
}

fn push_short_string(
    out: &mut Vec<u8>,
    field: &'static str,
    bound: usize,
    value: &str,
) -> Result<(), PayloadError> {
    let bytes = value.as_bytes();
    if bytes.len() > bound {
        return Err(PayloadError::BoundExceeded {
            field,
            declared: bytes.len(),
            bound,
        });
    }
    // The bound is <= 255 for every caller, so the cast cannot truncate.
    out.push(u8::try_from(bytes.len()).map_err(|_| PayloadError::BoundExceeded {
        field,
        declared: bytes.len(),
        bound,
    })?);
    out.extend_from_slice(bytes);
    Ok(())
}

// ---------------------------------------------------------------- authenticate

/// How the device answered an authentication attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    /// The credential was accepted.
    Accepted,
    /// The credential was rejected.
    Rejected,
    /// The device requires an encrypted pre-handshake this build does not
    /// implement (spec App. B.5 "mode B"). We fail closed; we never retry in
    /// the legacy mode.
    EncryptedModeRequired,
}

/// A decoded authentication response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthResponse {
    /// Outcome reported by the device.
    pub status: AuthStatus,
    /// Profile-defined detail byte, retained for diagnostics only.
    pub detail: u8,
}

impl AuthResponse {
    /// Encode a response, for fixtures and the simulator.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let status = match self.status {
            AuthStatus::Accepted => 0x00,
            AuthStatus::Rejected => 0x01,
            AuthStatus::EncryptedModeRequired => 0x02,
        };
        vec![status, self.detail]
    }

    /// Decode a response.
    ///
    /// An undefined status byte is an error, never "probably accepted".
    pub fn decode(payload: &[u8]) -> Result<Self, PayloadError> {
        let mut cursor = Cursor::new(payload);
        let status = match cursor.u8()? {
            0x00 => AuthStatus::Accepted,
            0x01 => AuthStatus::Rejected,
            0x02 => AuthStatus::EncryptedModeRequired,
            _ => return Err(PayloadError::Undefined { field: "status" }),
        };
        let detail = cursor.u8()?;
        cursor.finish()?;
        Ok(Self { status, detail })
    }
}

/// Encode the authenticate payload for `profile`.
///
/// The token never passes through a formatting macro, a log line or an error
/// message: a length mismatch is reported as a length, not as the value.
pub fn encode_authenticate(
    profile: &ProtocolProfile,
    token: &[u8],
) -> Result<Vec<u8>, PayloadError> {
    if !profile.accepts_token_length(token.len()) {
        return Err(PayloadError::TokenLength { bytes: token.len() });
    }
    Ok(token.to_vec())
}

// ------------------------------------------------------------------- listing

/// Codec a device reports for one recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceCodec {
    /// The compressed `.ASR` Opus sidecar (spec §2.5A).
    AsrOpus,
    /// A codec this build does not model.
    Unknown,
}

impl DeviceCodec {
    /// Wire value.
    #[must_use]
    pub const fn wire_value(self) -> u8 {
        match self {
            Self::AsrOpus => 0x01,
            Self::Unknown => 0xFF,
        }
    }

    /// Decode a wire value. Anything undefined is [`Self::Unknown`], which is
    /// not a codec we will claim to decode.
    #[must_use]
    pub const fn from_wire(value: u8) -> Self {
        match value {
            0x01 => Self::AsrOpus,
            _ => Self::Unknown,
        }
    }

    /// The media type string recorded in provenance.
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::AsrOpus => "audio/opus",
            Self::Unknown => "application/octet-stream",
        }
    }
}

/// One entry of a device listing. Every field is untrusted until verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedRecording {
    /// Device-scoped identifier.
    pub id: String,
    /// Display name the device reports. Advisory metadata only.
    pub display_name: String,
    /// Byte length the device declares. A claim, not a measurement.
    pub declared_byte_length: u64,
    /// Capture time the device reports, as a Unix timestamp in seconds.
    pub captured_at_unix: u32,
    /// Codec the device declares.
    pub codec: DeviceCodec,
}

/// A decoded delta listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaListing {
    /// Entries the device reported.
    pub entries: Vec<ListedRecording>,
    /// Whether the device indicated more entries than it returned.
    pub truncated: bool,
}

impl DeltaListing {
    /// Encode a listing, for fixtures and the simulator.
    pub fn encode(&self, profile: &ProtocolProfile) -> Result<Vec<u8>, PayloadError> {
        if self.entries.len() > profile.max_listed_recordings {
            return Err(PayloadError::BoundExceeded {
                field: "count",
                declared: self.entries.len(),
                bound: profile.max_listed_recordings,
            });
        }
        let count = u16::try_from(self.entries.len()).map_err(|_| PayloadError::BoundExceeded {
            field: "count",
            declared: self.entries.len(),
            bound: profile.max_listed_recordings,
        })?;
        let mut out = Vec::new();
        out.extend_from_slice(&count.to_be_bytes());
        for entry in &self.entries {
            push_short_string(&mut out, "id", MAX_RECORDING_ID_LEN, &entry.id)?;
            push_short_string(
                &mut out,
                "display_name",
                MAX_DISPLAY_NAME_LEN,
                &entry.display_name,
            )?;
            let size = u32::try_from(entry.declared_byte_length).map_err(|_| {
                PayloadError::BoundExceeded {
                    field: "declared_byte_length",
                    declared: usize::MAX,
                    bound: u32::MAX as usize,
                }
            })?;
            out.extend_from_slice(&size.to_be_bytes());
            out.extend_from_slice(&entry.captured_at_unix.to_be_bytes());
            out.push(entry.codec.wire_value());
        }
        out.push(u8::from(self.truncated));
        Ok(out)
    }

    /// Decode a listing against `profile`'s bounds.
    ///
    /// The declared entry count is a claim: it is checked against the profile
    /// bound *before* any allocation, and the payload must end exactly after
    /// the last entry, so a forged count cannot drive allocation and a short
    /// payload cannot silently yield a partial listing.
    pub fn decode(profile: &ProtocolProfile, payload: &[u8]) -> Result<Self, PayloadError> {
        let mut cursor = Cursor::new(payload);
        let declared = usize::from(cursor.u16()?);
        if declared > profile.max_listed_recordings {
            return Err(PayloadError::BoundExceeded {
                field: "count",
                declared,
                bound: profile.max_listed_recordings,
            });
        }
        // Each entry occupies at least seven bytes, so a count that cannot fit
        // in the remaining payload is rejected before anything is reserved.
        const MIN_ENTRY_BYTES: usize = 1 + 1 + 4 + 4 + 1;
        let available = payload.len().saturating_sub(2);
        if declared.saturating_mul(MIN_ENTRY_BYTES) > available {
            return Err(PayloadError::Truncated);
        }
        let mut entries = Vec::with_capacity(declared);
        for _ in 0..declared {
            let id = cursor.short_string("id", MAX_RECORDING_ID_LEN)?;
            let display_name = cursor.short_string("display_name", MAX_DISPLAY_NAME_LEN)?;
            let declared_byte_length = u64::from(cursor.u32()?);
            let captured_at_unix = cursor.u32()?;
            let codec = DeviceCodec::from_wire(cursor.u8()?);
            if id.is_empty() {
                return Err(PayloadError::Undefined { field: "id" });
            }
            entries.push(ListedRecording {
                id,
                display_name,
                declared_byte_length,
                captured_at_unix,
                codec,
            });
        }
        let truncated = match cursor.u8()? {
            0 => false,
            1 => true,
            _ => return Err(PayloadError::Undefined { field: "truncated" }),
        };
        cursor.finish()?;
        Ok(Self { entries, truncated })
    }
}

// -------------------------------------------------------------- request data

/// A request for one byte range of one recording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataRequest {
    /// Device-scoped recording identifier.
    pub id: String,
    /// First byte requested.
    pub offset: u32,
    /// Bytes requested. Zero means "to the end of the file".
    pub length: u32,
}

impl DataRequest {
    /// Encode a data request.
    pub fn encode(&self) -> Result<Vec<u8>, PayloadError> {
        let mut out = Vec::new();
        push_short_string(&mut out, "id", MAX_RECORDING_ID_LEN, &self.id)?;
        out.extend_from_slice(&self.offset.to_be_bytes());
        out.extend_from_slice(&self.length.to_be_bytes());
        Ok(out)
    }

    /// Decode a data request, for fixtures and the simulator.
    pub fn decode(payload: &[u8]) -> Result<Self, PayloadError> {
        let mut cursor = Cursor::new(payload);
        let id = cursor.short_string("id", MAX_RECORDING_ID_LEN)?;
        let offset = cursor.u32()?;
        let length = cursor.u32()?;
        cursor.finish()?;
        if id.is_empty() {
            return Err(PayloadError::Undefined { field: "id" });
        }
        Ok(Self { id, offset, length })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::profile::LEGACY_NOTE_V0095;

    fn entry(id: &str) -> ListedRecording {
        ListedRecording {
            id: id.to_owned(),
            display_name: format!("{id}.ASR"),
            declared_byte_length: 4096,
            captured_at_unix: 1_760_000_000,
            codec: DeviceCodec::AsrOpus,
        }
    }

    #[test]
    fn an_auth_response_round_trips_and_undefined_status_is_refused() {
        for status in [
            AuthStatus::Accepted,
            AuthStatus::Rejected,
            AuthStatus::EncryptedModeRequired,
        ] {
            let response = AuthResponse { status, detail: 7 };
            assert_eq!(AuthResponse::decode(&response.encode()).unwrap(), response);
        }
        assert_eq!(
            AuthResponse::decode(&[0x09, 0x00]),
            Err(PayloadError::Undefined { field: "status" })
        );
        assert_eq!(AuthResponse::decode(&[0x00]), Err(PayloadError::Truncated));
        assert_eq!(
            AuthResponse::decode(&[0x00, 0x00, 0x00]),
            Err(PayloadError::TrailingBytes { remaining: 1 })
        );
    }

    #[test]
    fn a_token_outside_the_profile_length_set_is_refused_without_echoing_it() {
        let error = encode_authenticate(&LEGACY_NOTE_V0095, &[0xAB; 8]).unwrap_err();
        assert_eq!(error, PayloadError::TokenLength { bytes: 8 });
        let rendered = error.to_string();
        assert!(!rendered.contains("ab"), "{rendered} must not echo the token");

        assert_eq!(
            encode_authenticate(&LEGACY_NOTE_V0095, &[0xAB; 16]).unwrap(),
            vec![0xAB; 16]
        );
    }

    #[test]
    fn a_listing_round_trips() {
        let listing = DeltaListing {
            entries: vec![entry("REC0001"), entry("REC0002")],
            truncated: true,
        };
        let encoded = listing.encode(&LEGACY_NOTE_V0095).unwrap();
        assert_eq!(
            DeltaListing::decode(&LEGACY_NOTE_V0095, &encoded).unwrap(),
            listing
        );
    }

    #[test]
    fn an_empty_listing_is_valid_and_is_not_a_completeness_claim() {
        let encoded = DeltaListing {
            entries: vec![],
            truncated: false,
        }
        .encode(&LEGACY_NOTE_V0095)
        .unwrap();
        let decoded = DeltaListing::decode(&LEGACY_NOTE_V0095, &encoded).unwrap();
        assert!(decoded.entries.is_empty());
        assert!(
            LEGACY_NOTE_V0095.listing_is_delta_only,
            "an empty delta listing never means the device has no recordings"
        );
    }

    #[test]
    fn a_forged_entry_count_cannot_drive_allocation() {
        // Claim 60000 entries in a nine byte payload.
        let mut payload = 60_000u16.to_be_bytes().to_vec();
        payload.extend_from_slice(&[0u8; 7]);
        assert_eq!(
            DeltaListing::decode(&LEGACY_NOTE_V0095, &payload),
            Err(PayloadError::BoundExceeded {
                field: "count",
                declared: 60_000,
                bound: LEGACY_NOTE_V0095.max_listed_recordings,
            })
        );

        // Inside the profile bound, but still more than the payload holds.
        let mut payload = 400u16.to_be_bytes().to_vec();
        payload.extend_from_slice(&[0u8; 7]);
        assert_eq!(
            DeltaListing::decode(&LEGACY_NOTE_V0095, &payload),
            Err(PayloadError::Truncated)
        );
    }

    #[test]
    fn an_oversized_name_is_refused_rather_than_truncated() {
        let mut long = entry("REC0001");
        long.display_name = "x".repeat(MAX_DISPLAY_NAME_LEN + 1);
        assert_eq!(
            DeltaListing {
                entries: vec![long],
                truncated: false
            }
            .encode(&LEGACY_NOTE_V0095),
            Err(PayloadError::BoundExceeded {
                field: "display_name",
                declared: MAX_DISPLAY_NAME_LEN + 1,
                bound: MAX_DISPLAY_NAME_LEN,
            })
        );
    }

    #[test]
    fn trailing_bytes_after_a_listing_are_refused() {
        let mut encoded = DeltaListing {
            entries: vec![entry("REC0001")],
            truncated: false,
        }
        .encode(&LEGACY_NOTE_V0095)
        .unwrap();
        encoded.push(0xFF);
        assert_eq!(
            DeltaListing::decode(&LEGACY_NOTE_V0095, &encoded),
            Err(PayloadError::TrailingBytes { remaining: 1 })
        );
    }

    #[test]
    fn an_unknown_codec_is_not_claimed_to_be_decodable() {
        let mut listing = DeltaListing {
            entries: vec![entry("REC0001")],
            truncated: false,
        };
        listing.entries[0].codec = DeviceCodec::Unknown;
        let encoded = listing.encode(&LEGACY_NOTE_V0095).unwrap();
        let decoded = DeltaListing::decode(&LEGACY_NOTE_V0095, &encoded).unwrap();
        assert_eq!(decoded.entries[0].codec, DeviceCodec::Unknown);
        assert_eq!(
            DeviceCodec::Unknown.mime(),
            "application/octet-stream",
            "an unmodelled codec is bytes, not audio we claim to understand"
        );
    }

    #[test]
    fn a_data_request_round_trips_and_an_empty_identifier_is_refused() {
        let request = DataRequest {
            id: "REC0001".into(),
            offset: 1024,
            length: 80,
        };
        assert_eq!(DataRequest::decode(&request.encode().unwrap()).unwrap(), request);

        let empty = DataRequest {
            id: String::new(),
            offset: 0,
            length: 0,
        };
        assert_eq!(
            DataRequest::decode(&empty.encode().unwrap()),
            Err(PayloadError::Undefined { field: "id" })
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        /// Decoding arbitrary bytes as a listing is total and bounded.
        #[test]
        fn listing_decode_is_total(bytes in proptest::collection::vec(any::<u8>(), 0..1024)) {
            if let Ok(listing) = DeltaListing::decode(&LEGACY_NOTE_V0095, &bytes) {
                prop_assert!(listing.entries.len() <= LEGACY_NOTE_V0095.max_listed_recordings);
                prop_assert!(listing.entries.len() <= bytes.len());
                for entry in &listing.entries {
                    prop_assert!(entry.id.len() <= MAX_RECORDING_ID_LEN);
                    prop_assert!(entry.display_name.len() <= MAX_DISPLAY_NAME_LEN);
                }
            }
        }

        /// Decoding arbitrary bytes as an auth response or data request is total.
        #[test]
        fn other_payload_decoders_are_total(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
            let _ = AuthResponse::decode(&bytes);
            if let Ok(request) = DataRequest::decode(&bytes) {
                prop_assert!(!request.id.is_empty());
                prop_assert!(request.id.len() <= MAX_RECORDING_ID_LEN);
            }
        }
    }
}
