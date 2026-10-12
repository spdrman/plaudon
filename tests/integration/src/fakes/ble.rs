//! Shared helpers for the direct-device scenario suite (spec §18.3A).
//!
//! The scripts themselves live in `tests/fixtures/device/<profile>/`, keyed by
//! protocol profile and therefore by device model and firmware. These helpers
//! build the synthetic media the scripts carry and the sink the actor streams
//! into; nothing here contacts a radio.

use std::path::{Path, PathBuf};

use plaudon_plaud_protocol::payload::{DeltaListing, DeviceCodec, ListedRecording};
use plaudon_plaud_protocol::profile::LEGACY_NOTE_V0095;

/// Directory holding the versioned wire fixtures for the legacy profile.
#[must_use]
pub fn fixture_dir() -> PathBuf {
    repo_root()
        .join("tests/fixtures/device")
        .join(LEGACY_NOTE_V0095.name)
}

/// Path of one named fixture.
#[must_use]
pub fn fixture(name: &str) -> PathBuf {
    fixture_dir().join(format!("{name}.json"))
}

/// Repository root, from this crate's manifest directory.
#[must_use]
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the integration crate lives two levels below the repository root")
        .to_path_buf()
}

/// One 20 ms mono SILK-wideband Opus packet (RFC 6716 config 9, code 0).
///
/// Synthetic payload bytes: the framing is real, the audio is not a capture.
#[must_use]
pub fn opus_packet(payload_bytes: usize) -> Vec<u8> {
    let mut packet = vec![9 << 3];
    packet.extend(std::iter::repeat_n(0x55u8, payload_bytes));
    packet
}

/// A `.ASR` sidecar of `packets` length-prefixed Opus packets.
#[must_use]
pub fn sidecar(packets: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..packets {
        let packet = opus_packet(40);
        out.extend_from_slice(
            &u16::try_from(packet.len())
                .expect("a synthetic packet is far below the frame cap")
                .to_be_bytes(),
        );
        out.extend_from_slice(&packet);
    }
    out
}

/// A listing entry for a recording of `byte_length` bytes.
#[must_use]
pub fn listed(id: &str, byte_length: u64) -> ListedRecording {
    ListedRecording {
        id: id.to_owned(),
        display_name: format!("{id}.ASR"),
        declared_byte_length: byte_length,
        captured_at_unix: 1_760_000_000,
        codec: DeviceCodec::AsrOpus,
    }
}

/// A delta listing of the given entries.
#[must_use]
pub fn listing(entries: Vec<ListedRecording>, truncated: bool) -> DeltaListing {
    DeltaListing { entries, truncated }
}
