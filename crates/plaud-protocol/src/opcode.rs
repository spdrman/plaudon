//! The compile-time acquisition allowlist (spec 7.9).

use serde::{Deserialize, Serialize};

/// Every operation the acquisition service is allowed to send.
///
/// This enum *is* the allowlist. There is no `Other(u16)` variant and no
/// public constructor from a raw value that is not listed here, so an
/// out-of-allowlist command cannot be constructed anywhere in the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadOnlyOpcode {
    /// Authenticate the host to the device.
    Authenticate,
    /// Request the device's unsynchronized-file delta list.
    ListDelta,
    /// Request a byte range of one recording.
    RequestData,
}

impl ReadOnlyOpcode {
    /// Every allowlisted opcode.
    pub const ALL: &'static [Self] = &[Self::Authenticate, Self::ListDelta, Self::RequestData];

    /// The wire value observed for this operation.
    #[must_use]
    pub const fn wire_value(self) -> u16 {
        match self {
            Self::Authenticate => 0x0001,
            Self::ListDelta => 0x001A,
            Self::RequestData => 0x001C,
        }
    }

    /// Decode a wire value.
    ///
    /// Anything outside the allowlist, including every known destructive
    /// opcode, decodes to `None` and is reported as [`ForbiddenOpcode`].
    #[must_use]
    pub const fn from_wire(value: u16) -> Option<Self> {
        match value {
            0x0001 => Some(Self::Authenticate),
            0x001A => Some(Self::ListDelta),
            0x001C => Some(Self::RequestData),
            _ => None,
        }
    }

    /// Whether this operation changes device state.
    ///
    /// None of them do, which is the point of the enum.
    #[must_use]
    pub const fn mutates_device(self) -> bool {
        false
    }
}

/// A wire value that is not in the allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("opcode {value:#06x} is not in the read-only acquisition allowlist")]
pub struct ForbiddenOpcode {
    /// The rejected value.
    pub value: u16,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opcodes named in upstream reverse-engineering notes that we must never
    /// be able to send. The test asserts they do not decode; the type system
    /// already makes them unconstructible.
    const KNOWN_DESTRUCTIVE: &[u16] = &[
        0x0002, // deleteFile
        0x0003, // clearAllFile
        0x0010, // recording control
        0x0011, // settings write
        0x0012, // device privacy toggle
        0x0020, // OTA begin
        0x0021, // OTA data
        0x001B, // sync acknowledgement
        0x001D, // factory reset
        0xFFFF,
    ];

    #[test]
    fn the_allowlist_contains_only_read_only_operations() {
        assert_eq!(ReadOnlyOpcode::ALL.len(), 3);
        for opcode in ReadOnlyOpcode::ALL {
            assert!(!opcode.mutates_device());
        }
    }

    #[test]
    fn allowlisted_opcodes_round_trip_through_their_wire_values() {
        for opcode in ReadOnlyOpcode::ALL {
            assert_eq!(
                ReadOnlyOpcode::from_wire(opcode.wire_value()),
                Some(*opcode)
            );
        }
        assert_eq!(ReadOnlyOpcode::Authenticate.wire_value(), 0x0001);
        assert_eq!(ReadOnlyOpcode::ListDelta.wire_value(), 0x001A);
        assert_eq!(ReadOnlyOpcode::RequestData.wire_value(), 0x001C);
    }

    #[test]
    fn no_destructive_opcode_decodes() {
        for value in KNOWN_DESTRUCTIVE {
            assert_eq!(
                ReadOnlyOpcode::from_wire(*value),
                None,
                "{value:#06x} decoded"
            );
        }
    }

    #[test]
    fn exactly_three_of_the_whole_u16_space_decode() {
        let decodable: Vec<u16> = (0..=u16::MAX)
            .filter(|v| ReadOnlyOpcode::from_wire(*v).is_some())
            .collect();
        assert_eq!(decodable, vec![0x0001, 0x001A, 0x001C]);
    }
}
