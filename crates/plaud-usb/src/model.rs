//! Vendor model support matrix for USB file access (spec 7.8, App. B.2).

use serde::{Deserialize, Serialize};

/// A PLAUD hardware model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VendorModel {
    /// The original PLAUD Note.
    Note,
    /// PLAUD Note Pro.
    NotePro,
    /// PLAUD NotePin S.
    NotePinS,
    /// A model this build does not recognize.
    #[serde(other)]
    Unknown,
}

/// Whether a model exposes recordings over USB mass storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsbSupport {
    /// Vendor documentation says recordings are reachable, subject to the
    /// device's own "Access via USB" setting, and we have a profile for the
    /// layout. Still requires a per-cell validation record before use.
    ConditionalOnDeviceSetting,
    /// Vendor documentation says USB file access does not exist for this
    /// model. We must not suggest mounting it.
    KnownUnsupported {
        /// Short machine-readable reason.
        reason: &'static str,
    },
    /// We have no evidence either way.
    NotEvaluated,
}

impl VendorModel {
    /// Every model this build has a verdict for.
    pub const ALL: &'static [Self] = &[Self::Note, Self::NotePro, Self::NotePinS, Self::Unknown];

    /// Recognize a model from a volume label or device identifier.
    ///
    /// Matching is exact and case-insensitive. A near-miss is `Unknown`, never
    /// the closest model: spec 2.5B requires an unmatched tuple to fail
    /// closed.
    #[must_use]
    pub fn from_volume_label(label: &str) -> Self {
        match label.trim().to_ascii_uppercase().as_str() {
            "PLAUD_NOTE" => Self::Note,
            "PLAUD_NOTE_PRO" => Self::NotePro,
            "PLAUD_NOTEPIN_S" => Self::NotePinS,
            _ => Self::Unknown,
        }
    }

    /// USB file-access verdict for this model.
    #[must_use]
    pub const fn usb_support(self) -> UsbSupport {
        match self {
            Self::Note => UsbSupport::ConditionalOnDeviceSetting,
            Self::NotePro => UsbSupport::KnownUnsupported {
                reason: "vendor documents USB as charging only for Note Pro",
            },
            Self::NotePinS => UsbSupport::KnownUnsupported {
                reason: "vendor documents no USB recording access for NotePin S",
            },
            Self::Unknown => UsbSupport::NotEvaluated,
        }
    }

    /// Whether the application may offer a USB import flow for this model.
    #[must_use]
    pub const fn may_offer_usb_import(self) -> bool {
        matches!(self.usb_support(), UsbSupport::ConditionalOnDeviceSetting)
    }

    /// Whether the user interface may instruct the user to mount this model.
    ///
    /// Spec 7.8: we must not suggest USB recording access for Note Pro or
    /// NotePin S, and must not guess for an unrecognized model.
    #[must_use]
    pub const fn may_instruct_user_to_mount(self) -> bool {
        self.may_offer_usb_import()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_original_note_may_be_offered_a_usb_flow() {
        assert!(VendorModel::Note.may_offer_usb_import());
        for unsupported in [
            VendorModel::NotePro,
            VendorModel::NotePinS,
            VendorModel::Unknown,
        ] {
            assert!(!unsupported.may_offer_usb_import(), "{unsupported:?}");
            assert!(!unsupported.may_instruct_user_to_mount(), "{unsupported:?}");
        }
    }

    #[test]
    fn note_pro_and_notepin_are_recorded_as_known_unsupported_with_a_reason() {
        for model in [VendorModel::NotePro, VendorModel::NotePinS] {
            let UsbSupport::KnownUnsupported { reason } = model.usb_support() else {
                panic!("{model:?} must be known-unsupported");
            };
            assert_ne!(reason, "");
        }
        assert_eq!(VendorModel::Unknown.usb_support(), UsbSupport::NotEvaluated);
    }

    #[test]
    fn label_matching_is_exact_and_a_near_miss_fails_closed() {
        assert_eq!(
            VendorModel::from_volume_label("PLAUD_NOTE"),
            VendorModel::Note
        );
        assert_eq!(
            VendorModel::from_volume_label(" plaud_note "),
            VendorModel::Note
        );
        assert_eq!(
            VendorModel::from_volume_label("PLAUD_NOTE_PRO"),
            VendorModel::NotePro
        );
        for near_miss in [
            "PLAUD_NOTE2",
            "PLAUDNOTE",
            "PLAUD NOTE",
            "NOTE",
            "PLAUD_NOTE_X",
            "",
        ] {
            assert_eq!(
                VendorModel::from_volume_label(near_miss),
                VendorModel::Unknown,
                "{near_miss:?} must not match the legacy profile by resemblance"
            );
        }
    }

    #[test]
    fn an_unrecognized_model_decodes_as_unknown() {
        let decoded: VendorModel = serde_json::from_str("\"plaud_note_ultra\"").unwrap();
        assert_eq!(decoded, VendorModel::Unknown);
        assert!(!decoded.may_offer_usb_import());
    }
}
