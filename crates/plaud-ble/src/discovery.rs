//! Advertisement evaluation (spec 7.7, 7.9, 18.6A "BLE authentication").
//!
//! An advertisement is self-reported, unauthenticated and trivially spoofed.
//! The policy here therefore produces a *candidate*, never an identity, and a
//! candidate is only worth probing; it never justifies connecting
//! automatically or reusing a stored credential.

use serde::{Deserialize, Serialize};

/// What a scan actually saw.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedAdvertisement {
    /// Host-assigned peripheral identifier. Stable only for this host.
    pub peripheral_id: String,
    /// Local name, exactly as advertised.
    pub local_name: Option<String>,
    /// Advertised 16-bit service identifiers.
    #[serde(default)]
    pub service_ids: Vec<u16>,
    /// Manufacturer identifiers present in the advertisement.
    #[serde(default)]
    pub manufacturer_ids: Vec<u16>,
    /// Received signal strength, when the host reports it.
    pub rssi: Option<i16>,
}

/// The verdict on one advertisement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum AdvertisementVerdict {
    /// Worth probing. Probing still has to establish model and firmware.
    Candidate(Candidate),
    /// Does not advertise the vendor service.
    NotAVendorService,
    /// Advertises the vendor service but is excluded by policy.
    Excluded {
        /// Short machine-readable reason.
        reason: &'static str,
    },
}

/// A device worth probing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    /// Host-assigned peripheral identifier.
    pub peripheral_id: String,
    /// Name as advertised. Display only; never an identity.
    pub advertised_name: Option<String>,
}

impl Candidate {
    /// Whether this candidate may be connected to without the user asking.
    ///
    /// Spec 7.9 forbids auto-connecting on an advertising name, so this is
    /// always false. It exists so the rule is testable rather than implied by
    /// the absence of code.
    #[must_use]
    pub const fn may_auto_connect(&self) -> bool {
        false
    }
}

/// Discovery policy for the legacy profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryPolicy {
    /// Service identifier a candidate must advertise.
    pub required_service_id: u16,
    /// Minimum signal strength, when the host reports one.
    pub min_rssi: Option<i16>,
}

impl Default for DiscoveryPolicy {
    fn default() -> Self {
        Self {
            required_service_id: plaudon_plaud_protocol::gatt::SERVICE_VENDOR,
            // A faint advertisement is still a legitimate device; filtering on
            // signal strength would hide real hardware, so there is no default.
            min_rssi: None,
        }
    }
}

impl DiscoveryPolicy {
    /// Evaluate one advertisement.
    #[must_use]
    pub fn evaluate(&self, advertisement: &ObservedAdvertisement) -> AdvertisementVerdict {
        // The vendor service identifier is the only signal that is at least
        // protocol-relevant. A name or manufacturer identifier is not: spec
        // 7.7 explicitly refuses to treat `PLAUD_NOTE` plus one manufacturer
        // identifier as a universal identity.
        if !advertisement
            .service_ids
            .contains(&self.required_service_id)
        {
            return AdvertisementVerdict::NotAVendorService;
        }
        if let (Some(minimum), Some(observed)) = (self.min_rssi, advertisement.rssi) {
            if observed < minimum {
                return AdvertisementVerdict::Excluded {
                    reason: "signal_below_threshold",
                };
            }
        }
        if advertisement.peripheral_id.trim().is_empty() {
            return AdvertisementVerdict::Excluded {
                reason: "no_peripheral_identifier",
            };
        }
        AdvertisementVerdict::Candidate(Candidate {
            peripheral_id: advertisement.peripheral_id.clone(),
            advertised_name: advertisement.local_name.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use plaudon_plaud_protocol::gatt::SERVICE_VENDOR;

    use super::*;

    fn advertisement(name: Option<&str>, services: &[u16]) -> ObservedAdvertisement {
        ObservedAdvertisement {
            peripheral_id: "AABBCCDD-0011-2233-4455-667788990011".to_owned(),
            local_name: name.map(str::to_owned),
            service_ids: services.to_vec(),
            manufacturer_ids: vec![0x0499],
            rssi: Some(-55),
        }
    }

    #[test]
    fn a_vendor_service_advertisement_is_a_candidate_not_an_identity() {
        let verdict = DiscoveryPolicy::default()
            .evaluate(&advertisement(Some("PLAUD_NOTE"), &[SERVICE_VENDOR]));
        let AdvertisementVerdict::Candidate(candidate) = verdict else {
            panic!("expected a candidate");
        };
        assert_eq!(candidate.advertised_name.as_deref(), Some("PLAUD_NOTE"));
        assert!(
            !candidate.may_auto_connect(),
            "a name must never trigger a connection"
        );
    }

    #[test]
    fn a_fake_advertiser_using_the_vendor_name_gets_no_special_treatment() {
        // Same name, no vendor service: not even a candidate.
        let verdict =
            DiscoveryPolicy::default().evaluate(&advertisement(Some("PLAUD_NOTE"), &[0x180F]));
        assert_eq!(verdict, AdvertisementVerdict::NotAVendorService);

        // Same name and the vendor service: a candidate, but still only a
        // candidate, and still not auto-connectable.
        let verdict = DiscoveryPolicy::default()
            .evaluate(&advertisement(Some("PLAUD_NOTE"), &[SERVICE_VENDOR]));
        let AdvertisementVerdict::Candidate(candidate) = verdict else {
            panic!("expected a candidate");
        };
        assert!(!candidate.may_auto_connect());
    }

    #[test]
    fn a_device_without_a_name_is_still_a_candidate() {
        let verdict = DiscoveryPolicy::default().evaluate(&advertisement(None, &[SERVICE_VENDOR]));
        assert!(matches!(verdict, AdvertisementVerdict::Candidate(_)));
    }

    #[test]
    fn an_advertisement_without_a_peripheral_identifier_is_excluded() {
        let mut observed = advertisement(Some("PLAUD_NOTE"), &[SERVICE_VENDOR]);
        observed.peripheral_id = "  ".to_owned();
        assert_eq!(
            DiscoveryPolicy::default().evaluate(&observed),
            AdvertisementVerdict::Excluded {
                reason: "no_peripheral_identifier"
            }
        );
    }

    #[test]
    fn a_signal_threshold_is_applied_only_when_both_sides_are_known() {
        let policy = DiscoveryPolicy {
            min_rssi: Some(-60),
            ..DiscoveryPolicy::default()
        };
        let mut weak = advertisement(Some("PLAUD_NOTE"), &[SERVICE_VENDOR]);
        weak.rssi = Some(-90);
        assert_eq!(
            policy.evaluate(&weak),
            AdvertisementVerdict::Excluded {
                reason: "signal_below_threshold"
            }
        );

        weak.rssi = None;
        assert!(matches!(
            policy.evaluate(&weak),
            AdvertisementVerdict::Candidate(_)
        ));
    }

    #[test]
    fn two_devices_reporting_the_same_name_remain_distinguishable_by_peripheral_id() {
        let mut first = advertisement(Some("PLAUD_NOTE"), &[SERVICE_VENDOR]);
        let mut second = first.clone();
        second.peripheral_id = "11112222-3333-4444-5555-666677778888".to_owned();
        first.rssi = Some(-40);

        let policy = DiscoveryPolicy::default();
        let (AdvertisementVerdict::Candidate(a), AdvertisementVerdict::Candidate(b)) =
            (policy.evaluate(&first), policy.evaluate(&second))
        else {
            panic!("expected two candidates");
        };
        assert_ne!(a.peripheral_id, b.peripheral_id);
    }
}
