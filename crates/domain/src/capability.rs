//! Versioned capability contracts and tri-state capability values (spec 6.6, 8.3).
//!
//! The single rule enforced here is that *unknown is not supported*. A payload
//! produced by a newer contract version, an unrecognized enum value and a
//! missing field all degrade to [`Tri::Unknown`] / [`CapabilityStatus::Unknown`]
//! and can never satisfy a requirement.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Validation status of an integration cell (spec 6.6).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityStatus {
    /// Opt-in diagnostic functionality with incomplete evidence.
    Experimental,
    /// Probed and failed a gate, or uses an unimplemented mode.
    Unsupported,
    /// Repeatable validation plus regression gates passed.
    Supported,
    /// Not probed, or probed with an inconclusive result. Also the decoding of
    /// any status string this build does not recognize.
    #[default]
    #[serde(other)]
    Unknown,
}

impl CapabilityStatus {
    /// Whether engine/source selection policy may choose this capability.
    ///
    /// Fail-closed: only `Supported` is selectable.
    #[must_use]
    pub const fn is_selectable(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Dispatch lifecycle of an outbound inference request (spec 6.6, 10.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    /// Nothing has left the machine.
    NotSent,
    /// Bytes are in flight.
    Sending,
    /// The provider acknowledged receipt.
    AcceptedByProvider,
    /// The connection was lost after transmission; billing is uncertain.
    OutcomeUnknown,
    /// A complete response was received.
    Completed,
    /// The attempt failed before any byte was transmitted.
    FailedBeforeSend,
}

impl DispatchState {
    /// Whether an automatic retry is safe.
    ///
    /// Spec 10.7 forbids automatic retries once an outcome is unknown or the
    /// provider has accepted the request, because that can double-bill.
    #[must_use]
    pub const fn auto_retry_safe(self) -> bool {
        matches!(self, Self::NotSent | Self::FailedBeforeSend)
    }
}

/// A three-valued capability flag where `Unknown` never satisfies a requirement.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Tri {
    /// Observed to be available.
    Yes,
    /// Observed to be unavailable.
    No,
    /// Not observed. Treated as unavailable by every policy decision.
    #[default]
    #[serde(other)]
    Unknown,
}

impl Tri {
    /// True only for [`Tri::Yes`].
    #[must_use]
    pub const fn satisfies(self) -> bool {
        matches!(self, Self::Yes)
    }

    /// Build from an observation, mapping `None` to `Unknown`.
    #[must_use]
    pub const fn from_observation(observed: Option<bool>) -> Self {
        match observed {
            Some(true) => Self::Yes,
            Some(false) => Self::No,
            None => Self::Unknown,
        }
    }
}

/// A value that may not have been observed.
///
/// Serializes as the inner value or `null`; a `null` or absent field decodes to
/// [`Known::Unknown`] rather than a default value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(
    from = "Option<T>",
    into = "Option<T>",
    bound = "T: Clone + Serialize + serde::de::DeserializeOwned"
)]
pub enum Known<T> {
    /// The value was observed.
    Known(T),
    /// The value was not observed.
    Unknown,
}

impl<T> Known<T> {
    /// Borrow the observed value, if any.
    pub const fn get(&self) -> Option<&T> {
        match self {
            Self::Known(value) => Some(value),
            Self::Unknown => None,
        }
    }

    /// Whether a value was observed.
    pub const fn is_known(&self) -> bool {
        matches!(self, Self::Known(_))
    }
}

// Not derivable: `#[derive(Default)]` on a generic enum would demand
// `T: Default`, and an unobserved value has no inner default to fall back on.
#[allow(clippy::derivable_impls)]
impl<T> Default for Known<T> {
    fn default() -> Self {
        Self::Unknown
    }
}

impl<T> From<Option<T>> for Known<T> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(inner) => Self::Known(inner),
            None => Self::Unknown,
        }
    }
}

impl<T> From<Known<T>> for Option<T> {
    fn from(value: Known<T>) -> Self {
        match value {
            Known::Known(inner) => Some(inner),
            Known::Unknown => None,
        }
    }
}

/// Envelope carrying a capability payload plus the contract version that
/// produced it (spec 6.6: capability payloads are versioned independently of
/// the application release).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityEnvelope<T> {
    /// Contract version of the payload.
    pub payload_version: u32,
    /// Validation status of the described capability.
    #[serde(default)]
    pub status: CapabilityStatus,
    /// The payload itself.
    pub payload: T,
    /// Fields present on the wire that this build does not understand.
    ///
    /// They are preserved for diagnostics and round-tripping, and never
    /// interpreted as available functionality.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unknown_fields: BTreeMap<String, serde_json::Value>,
}

impl<T> CapabilityEnvelope<T> {
    /// Contract version understood by this build.
    pub const SUPPORTED_VERSION: u32 = 1;

    /// Wrap a payload produced by this build.
    pub fn current(status: CapabilityStatus, payload: T) -> Self {
        Self {
            payload_version: Self::SUPPORTED_VERSION,
            status,
            payload,
            unknown_fields: BTreeMap::new(),
        }
    }

    /// Effective status after version negotiation.
    ///
    /// A payload from a version this build does not understand is reported as
    /// `Unknown`, never as the status the payload claims.
    #[must_use]
    pub fn effective_status(&self) -> CapabilityStatus {
        if self.payload_version == Self::SUPPORTED_VERSION {
            self.status
        } else {
            CapabilityStatus::Unknown
        }
    }

    /// Whether policy may select the described capability.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        self.effective_status().is_selectable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_capability_status_is_not_selectable() {
        assert!(CapabilityStatus::Supported.is_selectable());
        for status in [
            CapabilityStatus::Unknown,
            CapabilityStatus::Experimental,
            CapabilityStatus::Unsupported,
        ] {
            assert!(!status.is_selectable(), "{status:?} must not be selectable");
        }
    }

    #[test]
    fn unrecognized_status_values_decode_as_unknown() {
        let decoded: CapabilityStatus = serde_json::from_str("\"gold_plated\"").unwrap();
        assert_eq!(decoded, CapabilityStatus::Unknown);
        let decoded: Tri = serde_json::from_str("\"probably\"").unwrap();
        assert_eq!(decoded, Tri::Unknown);
        assert!(!decoded.satisfies());
    }

    #[test]
    fn tri_only_satisfies_on_yes() {
        assert!(Tri::Yes.satisfies());
        assert!(!Tri::No.satisfies());
        assert!(!Tri::Unknown.satisfies());
        assert_eq!(Tri::from_observation(None), Tri::Unknown);
    }

    #[test]
    fn known_round_trips_and_defaults_to_unknown() {
        let value: Known<u64> = serde_json::from_str("null").unwrap();
        assert_eq!(value, Known::Unknown);
        assert_eq!(serde_json::to_string(&Known::Known(7u64)).unwrap(), "7");
        assert_eq!(Known::<u64>::default(), Known::Unknown);
        assert_eq!(Known::Known(7u64).get(), Some(&7));
    }

    #[test]
    fn a_newer_payload_version_degrades_to_unknown() {
        let mut envelope = CapabilityEnvelope::current(CapabilityStatus::Supported, 1u8);
        assert!(envelope.is_selectable());
        envelope.payload_version = 99;
        assert_eq!(envelope.effective_status(), CapabilityStatus::Unknown);
        assert!(!envelope.is_selectable());
    }

    #[test]
    fn unknown_payload_fields_are_preserved_but_not_interpreted() {
        let json = r#"{"payload_version":1,"status":"supported","payload":3,
            "unknown_fields":{"supports_time_travel":true}}"#;
        let envelope: CapabilityEnvelope<u8> = serde_json::from_str(json).unwrap();
        assert_eq!(
            envelope.unknown_fields["supports_time_travel"],
            serde_json::json!(true)
        );
        assert_eq!(envelope.payload, 3);
    }

    #[test]
    fn automatic_retry_is_only_safe_before_transmission() {
        assert!(DispatchState::NotSent.auto_retry_safe());
        assert!(DispatchState::FailedBeforeSend.auto_retry_safe());
        for state in [
            DispatchState::Sending,
            DispatchState::AcceptedByProvider,
            DispatchState::OutcomeUnknown,
            DispatchState::Completed,
        ] {
            assert!(!state.auto_retry_safe(), "{state:?} must not auto-retry");
        }
    }
}
