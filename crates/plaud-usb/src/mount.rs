//! Mount detection and honest host-write accounting (spec 7.8).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{UsbSupport, VendorModel};

/// What the host operating system does to a mounted volume.
///
/// Spec 7.8 forbids promising "zero physical writes" where the host mounts
/// the volume read-write, because the operating system writes its own
/// metadata outside our control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostWriteBehaviour {
    /// The volume is mounted read-only; the host cannot write.
    MountedReadOnly,
    /// The volume is mounted read-write and host metadata was observed.
    MountedReadWriteWithHostMetadata {
        /// Metadata entries the host created, as observed.
        observed_entries: Vec<String>,
    },
    /// The volume is mounted read-write; host metadata has not been observed.
    MountedReadWriteUnobserved,
    /// Not established.
    #[serde(other)]
    Unknown,
}

impl HostWriteBehaviour {
    /// Whether the application may claim that nothing was written to the device.
    #[must_use]
    pub const fn permits_zero_write_claim(&self) -> bool {
        matches!(self, Self::MountedReadOnly)
    }

    /// Entries the host operating system is known to create on removable
    /// volumes. Observing one of these is evidence of a host write, not of
    /// an application write.
    pub const KNOWN_HOST_METADATA: &'static [&'static str] = &[
        ".Spotlight-V100",
        ".fseventsd",
        ".Trashes",
        "._.Trashes",
        ".DS_Store",
        "System Volume Information",
        "$RECYCLE.BIN",
    ];

    /// Classify a volume by the entries present at its root.
    #[must_use]
    pub fn from_root_entries(read_only: bool, entries: &[String]) -> Self {
        if read_only {
            return Self::MountedReadOnly;
        }
        let observed: Vec<String> = entries
            .iter()
            .filter(|entry| Self::KNOWN_HOST_METADATA.contains(&entry.as_str()))
            .cloned()
            .collect();
        if observed.is_empty() {
            Self::MountedReadWriteUnobserved
        } else {
            Self::MountedReadWriteWithHostMetadata {
                observed_entries: observed,
            }
        }
    }
}

/// Why a volume was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MountError {
    /// The path does not exist or is not a directory.
    #[error("the volume path is not a mounted directory")]
    NotMounted,
    /// The volume label does not match a model we have a profile for.
    #[error("this volume does not match a validated device profile")]
    UnrecognizedProfile {
        /// Label that was observed.
        observed_label: String,
    },
    /// The model is documented as not exposing recordings over USB.
    #[error("this device model does not expose recordings over USB")]
    ModelDoesNotExposeUsb {
        /// Model that was recognized.
        model: VendorModel,
    },
    /// The expected recording directories are absent.
    #[error("the volume does not contain the expected recording directories")]
    LayoutNotRecognized,
}

/// A volume we are willing to read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountedVolume {
    /// Mount point.
    pub root: PathBuf,
    /// Volume label as observed.
    pub label: String,
    /// Model recognized from the label.
    pub model: VendorModel,
    /// How the host mounted the volume.
    pub host_write_behaviour: HostWriteBehaviour,
}

impl MountedVolume {
    /// Directories the legacy profile stores recordings in.
    pub const RECORDING_DIRECTORIES: &'static [&'static str] = &["NOTES", "CALLS"];

    /// Inspect a mount point.
    ///
    /// This function only reads. It never mounts, never remounts and never
    /// creates anything, which is why it takes an already-mounted path.
    pub fn inspect(root: &Path, label: &str, read_only: bool) -> Result<Self, MountError> {
        if !root.is_dir() {
            return Err(MountError::NotMounted);
        }
        let model = VendorModel::from_volume_label(label);
        match model.usb_support() {
            UsbSupport::ConditionalOnDeviceSetting => {}
            UsbSupport::KnownUnsupported { .. } => {
                return Err(MountError::ModelDoesNotExposeUsb { model })
            }
            UsbSupport::NotEvaluated => {
                return Err(MountError::UnrecognizedProfile {
                    observed_label: label.to_owned(),
                })
            }
        }

        let mut entries = Vec::new();
        let mut has_recording_directory = false;
        if let Ok(read_dir) = std::fs::read_dir(root) {
            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if Self::RECORDING_DIRECTORIES.contains(&name.to_ascii_uppercase().as_str()) {
                    has_recording_directory = true;
                }
                entries.push(name);
            }
        }
        if !has_recording_directory {
            return Err(MountError::LayoutNotRecognized);
        }

        Ok(Self {
            root: root.to_path_buf(),
            label: label.to_owned(),
            model,
            host_write_behaviour: HostWriteBehaviour::from_root_entries(read_only, &entries),
        })
    }

    /// Whether the application may tell the user nothing was written.
    #[must_use]
    pub const fn permits_zero_write_claim(&self) -> bool {
        self.host_write_behaviour.permits_zero_write_claim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note_volume() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("NOTES")).expect("notes");
        std::fs::create_dir_all(dir.path().join("CALLS")).expect("calls");
        let root = dir.path().to_path_buf();
        (dir, root)
    }

    #[test]
    fn a_recognized_note_volume_is_accepted() {
        let (_dir, root) = note_volume();
        let volume = MountedVolume::inspect(&root, "PLAUD_NOTE", true).expect("accepted");
        assert_eq!(volume.model, VendorModel::Note);
        assert!(volume.permits_zero_write_claim());
    }

    #[test]
    fn a_read_write_mount_with_host_metadata_forbids_a_zero_write_claim() {
        let (_dir, root) = note_volume();
        std::fs::create_dir_all(root.join(".Spotlight-V100")).expect("metadata");
        std::fs::write(root.join(".DS_Store"), b"x").expect("metadata");

        let volume = MountedVolume::inspect(&root, "PLAUD_NOTE", false).expect("accepted");
        assert!(!volume.permits_zero_write_claim());
        let HostWriteBehaviour::MountedReadWriteWithHostMetadata { observed_entries } =
            &volume.host_write_behaviour
        else {
            panic!(
                "expected observed host metadata, got {:?}",
                volume.host_write_behaviour
            );
        };
        assert!(observed_entries.contains(&".Spotlight-V100".to_owned()));
        assert!(observed_entries.contains(&".DS_Store".to_owned()));
    }

    #[test]
    fn a_read_write_mount_without_observed_metadata_still_forbids_the_claim() {
        let (_dir, root) = note_volume();
        let volume = MountedVolume::inspect(&root, "PLAUD_NOTE", false).expect("accepted");
        assert_eq!(
            volume.host_write_behaviour,
            HostWriteBehaviour::MountedReadWriteUnobserved
        );
        assert!(
            !volume.permits_zero_write_claim(),
            "a read-write mount can be written by the host at any time"
        );
    }

    #[test]
    fn note_pro_and_notepin_volumes_are_refused_by_model() {
        let (_dir, root) = note_volume();
        for (label, model) in [
            ("PLAUD_NOTE_PRO", VendorModel::NotePro),
            ("PLAUD_NOTEPIN_S", VendorModel::NotePinS),
        ] {
            assert_eq!(
                MountedVolume::inspect(&root, label, true),
                Err(MountError::ModelDoesNotExposeUsb { model })
            );
        }
    }

    #[test]
    fn an_unrecognized_label_fails_closed_rather_than_assuming_the_legacy_profile() {
        let (_dir, root) = note_volume();
        assert_eq!(
            MountedVolume::inspect(&root, "PLAUD_NOTE_V2", true),
            Err(MountError::UnrecognizedProfile {
                observed_label: "PLAUD_NOTE_V2".to_owned()
            })
        );
    }

    #[test]
    fn a_volume_without_the_expected_layout_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            MountedVolume::inspect(dir.path(), "PLAUD_NOTE", true),
            Err(MountError::LayoutNotRecognized)
        );
    }

    #[test]
    fn a_path_that_is_not_mounted_is_refused() {
        assert_eq!(
            MountedVolume::inspect(Path::new("/definitely/not/mounted"), "PLAUD_NOTE", true),
            Err(MountError::NotMounted)
        );
    }
}
