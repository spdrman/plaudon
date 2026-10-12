//! Model gating and user-facing USB wording (spec 4.2B, 7.8, App. B.2).
//!
//! Every sentence the application may show about USB access to a PLAUD model
//! is produced here, so a refused model can never be paired with "mount it
//! over USB" instructions: the only way to obtain a [`MountInstruction`] is
//! [`VendorModel::mount_instruction`], which returns a [`UsbRefusal`] for
//! every model that is not the original Note.

use serde::{Deserialize, Serialize};

use crate::model::VendorModel;
use crate::mount::HostWriteBehaviour;

/// Spec 7.8: the host operating system, not this application, may write
/// metadata to a volume it mounts read-write.
pub const HOST_WRITE_NOTICE: &str = "Your computer, not PLAUDON, may write its own metadata (such as Spotlight or Trash folders) to the device when it mounts the device read-write. PLAUDON only reads from the device and never writes to it.";

/// Wording for a volume the host mounted read-only.
pub const HOST_READ_ONLY_NOTICE: &str =
    "Your computer mounted the device read-only. PLAUDON only reads from the device and never writes to it.";

impl HostWriteBehaviour {
    /// The exact user-facing sentence describing who may write to the volume.
    ///
    /// Only a read-only mount earns the read-only wording; every other state,
    /// including an unknown one, carries the host-write notice.
    #[must_use]
    pub const fn disclosure(&self) -> &'static str {
        match self {
            Self::MountedReadOnly => HOST_READ_ONLY_NOTICE,
            Self::MountedReadWriteWithHostMetadata { .. }
            | Self::MountedReadWriteUnobserved
            | Self::Unknown => HOST_WRITE_NOTICE,
        }
    }
}

/// Why a model is not offered a USB import, with the vendor-documented reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct UsbRefusal {
    /// Model the refusal is for.
    pub model: VendorModel,
    /// User-facing explanation. Never contains mounting instructions.
    pub message: &'static str,
}

/// User-facing instruction to mount a device over USB.
///
/// Has no public constructor: obtain one from
/// [`VendorModel::mount_instruction`], which only succeeds for the model
/// vendor documentation says can be mounted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MountInstruction {
    text: &'static str,
}

impl MountInstruction {
    /// The sentence to show the user.
    #[must_use]
    pub const fn text(&self) -> &'static str {
        self.text
    }
}

impl VendorModel {
    /// The refusal for this model, or `None` when a USB flow may be offered.
    #[must_use]
    pub const fn usb_refusal(self) -> Option<UsbRefusal> {
        let message = match self {
            Self::Note => return None,
            Self::NotePro => {
                "PLAUD Note Pro does not support USB file access: PLAUD documents its USB port as charging only. PLAUDON cannot import recordings from it over USB."
            }
            Self::NotePinS => {
                "PLAUD NotePin S does not expose recordings over USB, per PLAUD documentation. PLAUDON cannot import recordings from it over USB."
            }
            Self::Unknown => {
                "This device model is not recognized, so PLAUDON does not offer USB import for it."
            }
        };
        Some(UsbRefusal {
            model: self,
            message,
        })
    }

    /// The instruction to mount this model, or the reason there is none.
    ///
    /// # Errors
    ///
    /// Returns the model's [`UsbRefusal`] for Note Pro, NotePin S and any
    /// unrecognized model.
    pub const fn mount_instruction(self) -> Result<MountInstruction, UsbRefusal> {
        match self.usb_refusal() {
            Some(refusal) => Err(refusal),
            None => Ok(MountInstruction {
                text: "Connect your PLAUD Note by USB and turn on Access via USB in the device settings, then choose the mounted PLAUD_NOTE volume. PLAUDON never changes device settings for you.",
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_original_note_yields_a_mount_instruction() {
        assert!(VendorModel::Note.mount_instruction().is_ok());
        for model in [
            VendorModel::NotePro,
            VendorModel::NotePinS,
            VendorModel::Unknown,
        ] {
            let refusal = model.mount_instruction().expect_err("must refuse");
            assert_eq!(refusal.model, model);
            assert_eq!(model.usb_refusal(), Some(refusal));
        }
    }

    #[test]
    fn refusal_wording_carries_the_vendor_reason_and_never_asks_the_user_to_mount() {
        let pro = VendorModel::NotePro.usb_refusal().unwrap().message;
        assert!(pro.contains("charging only"));
        let pin = VendorModel::NotePinS.usb_refusal().unwrap().message;
        assert!(pin.contains("does not expose recordings over USB"));
        for model in [
            VendorModel::NotePro,
            VendorModel::NotePinS,
            VendorModel::Unknown,
        ] {
            let message = model.usb_refusal().unwrap().message.to_ascii_lowercase();
            assert!(!message.contains("connect your"), "{message}");
            assert!(!message.contains("access via usb"), "{message}");
        }
    }

    #[test]
    fn gating_agrees_with_the_support_matrix() {
        for model in VendorModel::ALL {
            assert_eq!(
                model.usb_refusal().is_none(),
                model.may_offer_usb_import(),
                "{model:?}"
            );
        }
    }

    #[test]
    fn only_a_read_only_mount_gets_the_read_only_wording() {
        assert_eq!(
            HostWriteBehaviour::MountedReadOnly.disclosure(),
            HOST_READ_ONLY_NOTICE
        );
        for behaviour in [
            HostWriteBehaviour::MountedReadWriteUnobserved,
            HostWriteBehaviour::MountedReadWriteWithHostMetadata {
                observed_entries: vec![".DS_Store".to_owned()],
            },
            HostWriteBehaviour::Unknown,
        ] {
            assert_eq!(behaviour.disclosure(), HOST_WRITE_NOTICE);
        }
    }
}
