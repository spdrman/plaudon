//! The `0x01` control frame codec (spec 18.2A).
//!
//! Wire layout, as observed for the legacy profile:
//!
//! ```text
//! +------+--------+--------+-----------------+
//! | kind | opcode | length | payload         |
//! | u8   | u16 BE | u16 BE | length bytes    |
//! +------+--------+--------+-----------------+
//! ```
//!
//! The declared length is treated as a claim, never as an allocation
//! instruction: a frame whose declared length exceeds the bytes actually
//! present is [`FrameError::LengthExceedsInput`], and nothing is reserved
//! before the bytes have been seen.

use serde::{Deserialize, Serialize};

use crate::opcode::{ForbiddenOpcode, ReadOnlyOpcode};

/// Leading byte identifying the frame family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    /// `0x01`: a control frame.
    Control,
    /// `0x02`: a bulk data frame.
    Bulk,
}

impl FrameKind {
    /// Wire value.
    #[must_use]
    pub const fn wire_value(self) -> u8 {
        match self {
            Self::Control => 0x01,
            Self::Bulk => 0x02,
        }
    }

    /// Decode a leading byte.
    #[must_use]
    pub const fn from_wire(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Control),
            0x02 => Some(Self::Bulk),
            _ => None,
        }
    }
}

/// Largest payload a single frame may declare.
///
/// Legacy BLE MTUs are far below this; the cap exists so a forged length
/// cannot be used to drive allocation even if the transport changes.
pub const MAX_FRAME_PAYLOAD: usize = 4 * 1024;

/// Why a frame could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    /// Fewer bytes than the fixed header requires.
    #[error("the frame is shorter than its header")]
    TooShort,
    /// The leading byte is not a known frame kind.
    #[error("unknown frame kind {value:#04x}")]
    UnknownKind {
        /// The rejected leading byte.
        value: u8,
    },
    /// The frame kind is not the one the caller expected.
    #[error("expected a {expected:?} frame")]
    WrongKind {
        /// Kind the caller asked for.
        expected: FrameKind,
        /// Kind actually present.
        actual: FrameKind,
    },
    /// The opcode is outside the read-only allowlist.
    #[error(transparent)]
    ForbiddenOpcode(#[from] ForbiddenOpcode),
    /// The declared payload length exceeds the frame cap.
    #[error("declared payload length {declared} exceeds the {MAX_FRAME_PAYLOAD} byte cap")]
    LengthExceedsCap {
        /// Length the frame declared.
        declared: usize,
    },
    /// The declared payload length exceeds the bytes actually present.
    #[error("declared payload length {declared} exceeds the {available} bytes available")]
    LengthExceedsInput {
        /// Length the frame declared.
        declared: usize,
        /// Bytes actually present after the header.
        available: usize,
    },
}

/// A decoded control frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlFrame {
    /// Operation this frame carries. Always allowlisted.
    pub opcode: ReadOnlyOpcode,
    /// Frame payload.
    pub payload: Vec<u8>,
}

impl ControlFrame {
    /// Fixed header size: kind, opcode, length.
    pub const HEADER_LEN: usize = 1 + 2 + 2;

    /// Build a control frame.
    ///
    /// The opcode argument is already an allowlisted value, so there is no
    /// path from an arbitrary `u16` to an outgoing frame.
    pub fn new(opcode: ReadOnlyOpcode, payload: Vec<u8>) -> Result<Self, FrameError> {
        if payload.len() > MAX_FRAME_PAYLOAD {
            return Err(FrameError::LengthExceedsCap {
                declared: payload.len(),
            });
        }
        Ok(Self { opcode, payload })
    }

    /// Encode to the wire.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let length = u16::try_from(self.payload.len()).unwrap_or(u16::MAX);
        let mut out = Vec::with_capacity(Self::HEADER_LEN + self.payload.len());
        out.push(FrameKind::Control.wire_value());
        out.extend_from_slice(&self.opcode.wire_value().to_be_bytes());
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    /// Decode from the wire.
    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < Self::HEADER_LEN {
            return Err(FrameError::TooShort);
        }
        let kind =
            FrameKind::from_wire(bytes[0]).ok_or(FrameError::UnknownKind { value: bytes[0] })?;
        if kind != FrameKind::Control {
            return Err(FrameError::WrongKind {
                expected: FrameKind::Control,
                actual: kind,
            });
        }
        let raw_opcode = u16::from_be_bytes([bytes[1], bytes[2]]);
        let opcode =
            ReadOnlyOpcode::from_wire(raw_opcode).ok_or(ForbiddenOpcode { value: raw_opcode })?;
        let declared = usize::from(u16::from_be_bytes([bytes[3], bytes[4]]));
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
        Ok(Self {
            opcode,
            payload: bytes[Self::HEADER_LEN..Self::HEADER_LEN + declared].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_control_frame_round_trips() {
        let frame = ControlFrame::new(ReadOnlyOpcode::ListDelta, vec![1, 2, 3]).unwrap();
        let encoded = frame.encode();
        assert_eq!(encoded[0], 0x01);
        assert_eq!(&encoded[1..3], &0x001Au16.to_be_bytes());
        assert_eq!(ControlFrame::decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn a_destructive_opcode_cannot_be_decoded() {
        let mut encoded = ControlFrame::new(ReadOnlyOpcode::ListDelta, vec![])
            .unwrap()
            .encode();
        encoded[1..3].copy_from_slice(&0x0002u16.to_be_bytes()); // deleteFile
        assert_eq!(
            ControlFrame::decode(&encoded),
            Err(FrameError::ForbiddenOpcode(ForbiddenOpcode {
                value: 0x0002
            }))
        );
    }

    #[test]
    fn a_forged_length_cannot_drive_allocation() {
        let mut encoded = ControlFrame::new(ReadOnlyOpcode::RequestData, vec![1])
            .unwrap()
            .encode();
        encoded[3..5].copy_from_slice(&4_000u16.to_be_bytes());
        assert_eq!(
            ControlFrame::decode(&encoded),
            Err(FrameError::LengthExceedsInput {
                declared: 4_000,
                available: 1
            })
        );

        encoded[3..5].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(
            ControlFrame::decode(&encoded),
            Err(FrameError::LengthExceedsCap {
                declared: usize::from(u16::MAX)
            })
        );
    }

    #[test]
    fn short_and_misidentified_frames_are_rejected() {
        assert_eq!(ControlFrame::decode(&[]), Err(FrameError::TooShort));
        assert_eq!(
            ControlFrame::decode(&[1, 0, 1, 0]),
            Err(FrameError::TooShort)
        );
        assert_eq!(
            ControlFrame::decode(&[0x09, 0, 1, 0, 0]),
            Err(FrameError::UnknownKind { value: 0x09 })
        );
        assert_eq!(
            ControlFrame::decode(&[0x02, 0, 1, 0, 0]),
            Err(FrameError::WrongKind {
                expected: FrameKind::Control,
                actual: FrameKind::Bulk
            })
        );
    }

    #[test]
    fn an_oversized_payload_cannot_be_constructed() {
        assert_eq!(
            ControlFrame::new(ReadOnlyOpcode::RequestData, vec![0; MAX_FRAME_PAYLOAD + 1]),
            Err(FrameError::LengthExceedsCap {
                declared: MAX_FRAME_PAYLOAD + 1
            })
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

        /// Decoding arbitrary bytes never panics and never allocates more
        /// than the input.
        #[test]
        fn decoding_arbitrary_bytes_is_total(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            if let Ok(frame) = ControlFrame::decode(&bytes) {
                prop_assert!(frame.payload.len() <= bytes.len());
                prop_assert!(frame.payload.len() <= MAX_FRAME_PAYLOAD);
                prop_assert!(ReadOnlyOpcode::ALL.contains(&frame.opcode));
            }
        }

        /// Every allowlisted frame round-trips for any payload inside the cap.
        #[test]
        fn allowlisted_frames_round_trip(
            index in 0usize..3,
            payload in proptest::collection::vec(any::<u8>(), 0..1024),
        ) {
            let opcode = ReadOnlyOpcode::ALL[index];
            let frame = ControlFrame::new(opcode, payload.clone()).unwrap();
            let decoded = ControlFrame::decode(&frame.encode()).unwrap();
            prop_assert_eq!(decoded.opcode, opcode);
            prop_assert_eq!(decoded.payload, payload);
        }

        /// No byte sequence decodes into an opcode outside the allowlist.
        #[test]
        fn no_input_yields_a_forbidden_opcode(
            opcode in any::<u16>(),
            payload in proptest::collection::vec(any::<u8>(), 0..64),
        ) {
            let length = u16::try_from(payload.len()).unwrap();
            let mut bytes = vec![0x01];
            bytes.extend_from_slice(&opcode.to_be_bytes());
            bytes.extend_from_slice(&length.to_be_bytes());
            bytes.extend_from_slice(&payload);

            match ControlFrame::decode(&bytes) {
                Ok(frame) => prop_assert!(ReadOnlyOpcode::ALL.contains(&frame.opcode)),
                Err(FrameError::ForbiddenOpcode(rejected)) => {
                    prop_assert_eq!(rejected.value, opcode);
                    prop_assert!(ReadOnlyOpcode::from_wire(opcode).is_none());
                }
                Err(_) => {}
            }
        }
    }
}
