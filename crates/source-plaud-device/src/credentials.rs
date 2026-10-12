//! Device-scoped authorization material and provisioning boundaries
//! (spec §7.7, §16.4, §22(16), App. B.5, App. B.7).
//!
//! # Three separate credentials
//!
//! App. B.5 separates **PLAUD account binding** (vendor/mobile setup),
//! **device-local authorization** (the credential the device's own BLE
//! protocol checks) and **cloud AI authorization** (`OpenRouter`). This module
//! owns only the middle one. It has no account concept and no network client.
//!
//! # Where the secret lives
//!
//! In the OS vault and nowhere else. `plaudon_security::secrets` provides the
//! only storage, and [`SecretScope::DeviceAuthorization`] requires a validated
//! device identifier, so a universal "one token for every device" entry is not
//! representable. What this module persists in SQLite or a portable backup is
//! a [`DeviceBinding`]: identifiers, a firmware fingerprint and an opaque
//! vault reference. No token bytes, ever.
//!
//! # Binding, not trusting
//!
//! A credential is looked up by a binding whose `peripheral_id`, model and
//! firmware must still match what the current probe observed. Two devices that
//! advertise the same name and even report the same serial resolve to
//! different vault entries, because the host-assigned peripheral identifier
//! differs; and a device whose firmware changed forces revalidation rather
//! than silent credential reuse (spec §16.2 "Device reconnect wrong identity /
//! replay", App. B.10).

use plaudon_plaud_protocol::profile::ProtocolProfile;
use plaudon_security::secrets::{
    ScopeError, SecretPresence, SecretRef, SecretScope, SecretStore, SecretStoreError, SecretString,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How confident we are that the peer is the device the user selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityAssurance {
    /// Nothing but an advertisement. Trivially spoofed (spec §4.2A.2).
    Unverified,
    /// The device completed the profile's authentication exchange.
    ///
    /// Spec §16.2A: we label this `profile-authenticated` and do **not** call
    /// it cryptographically verified identity, because a copied static token
    /// would also pass.
    ProfileAuthenticated,
}

impl IdentityAssurance {
    /// The exact phrase the interface shows.
    #[must_use]
    pub const fn ui_label(self) -> &'static str {
        match self {
            Self::Unverified => "Unverified identity",
            Self::ProfileAuthenticated => "Profile-authenticated",
        }
    }

    /// Whether this assurance level is strong enough to call the device known.
    #[must_use]
    pub const fn is_authenticated(self) -> bool {
        matches!(self, Self::ProfileAuthenticated)
    }
}

/// How a device credential was, or would be, obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisioningRoute {
    /// The user imports a token they already lawfully hold for their own
    /// device. The only route this build performs (spec §4.2A.4).
    ImportedExistingToken,
    /// First-time pairing through the vendor's mobile application. Real, but
    /// performed outside this application; we can only tell the user to do it.
    VendorMobileAppPairing,
    /// Upstream's Linux `bluer` peripheral emulation. Never shipped as a
    /// macOS or Windows onboarding flow (spec §7.7, App. B.5).
    LinuxPeripheralBootstrap,
    /// Importing a token recovered from an Android HCI snoop log. An advanced,
    /// sensitive path that stays unavailable pending legal and technical
    /// review (spec §7.7).
    AndroidHciCapture,
    /// The provisioning an encrypted-mode device would need. Unimplemented;
    /// we fail closed rather than downgrade (spec §22(16), App. B.5).
    EncryptedModeProvisioning,
}

/// What the application may do with a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteDisposition {
    /// The application performs it.
    Performed,
    /// The user must complete it elsewhere; we explain and wait.
    UserActionElsewhere,
    /// We refuse to perform or instruct it.
    Refused,
}

impl ProvisioningRoute {
    /// Every route this build has a verdict for.
    pub const ALL: &'static [Self] = &[
        Self::ImportedExistingToken,
        Self::VendorMobileAppPairing,
        Self::LinuxPeripheralBootstrap,
        Self::AndroidHciCapture,
        Self::EncryptedModeProvisioning,
    ];

    /// Our verdict on the route.
    #[must_use]
    pub const fn disposition(self) -> RouteDisposition {
        match self {
            Self::ImportedExistingToken => RouteDisposition::Performed,
            Self::VendorMobileAppPairing => RouteDisposition::UserActionElsewhere,
            Self::LinuxPeripheralBootstrap
            | Self::AndroidHciCapture
            | Self::EncryptedModeProvisioning => RouteDisposition::Refused,
        }
    }

    /// Why a route is refused or deferred, for display.
    #[must_use]
    pub const fn explanation(self) -> &'static str {
        match self {
            Self::ImportedExistingToken => {
                "Import an authorization token you already hold for your own device. \
                 It is stored in this computer's credential vault and never leaves it."
            }
            Self::VendorMobileAppPairing => {
                "First-time setup happens in the vendor's own mobile application. \
                 This application cannot perform it and will not ask for your account password."
            }
            Self::LinuxPeripheralBootstrap => {
                "The published token bootstrap emulates a Bluetooth peripheral on Linux. \
                 It is not a macOS or Windows setup flow, so this application does not offer it."
            }
            Self::AndroidHciCapture => {
                "Recovering a token from a phone's Bluetooth capture log is a sensitive, \
                 advanced path. It is unavailable pending legal and technical review."
            }
            Self::EncryptedModeProvisioning => {
                "This firmware requires an encrypted handshake this build does not implement. \
                 It is reported as unsupported; the handshake is never downgraded."
            }
        }
    }

    /// Whether the application may carry the route out itself.
    #[must_use]
    pub const fn is_performed_here(self) -> bool {
        matches!(self.disposition(), RouteDisposition::Performed)
    }
}

/// What a user must be told before using a legacy-profile device (spec §7.9,
/// §16.2A, App. B.5).
pub const LEGACY_DISCLOSURES: &[&str] = &[
    "This device profile exchanges a static token over an unencrypted Bluetooth link. \
     Someone within radio range could record the exchange.",
    "A copied token authenticates as your device, so profile authentication proves the \
     protocol succeeded, not that the peer is cryptographically identified.",
    "Turning on Local-only Mode stops this application from using the internet. \
     It does not encrypt the Bluetooth link.",
];

/// Durable, secret-free record of one authorized device.
///
/// Safe to persist in SQLite and to include in a portable backup: it holds
/// identifiers and an opaque vault reference, never credential material
/// (spec §13.3 `device_auth_refs`, App. B.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceBinding {
    /// Host-assigned peripheral identifier. Stable for this host only.
    pub peripheral_id: String,
    /// Device model established by a probe.
    pub device_model: String,
    /// Firmware fingerprint established by a probe.
    pub firmware_fingerprint: String,
    /// Route the credential came from.
    pub provisioning_route: ProvisioningRoute,
    /// Identity assurance at the time of binding.
    pub identity_assurance: IdentityAssurance,
}

impl DeviceBinding {
    /// Build a binding for a probed device.
    #[must_use]
    pub fn new(
        peripheral_id: impl Into<String>,
        device_model: impl Into<String>,
        firmware_fingerprint: impl Into<String>,
        provisioning_route: ProvisioningRoute,
        identity_assurance: IdentityAssurance,
    ) -> Self {
        Self {
            peripheral_id: peripheral_id.into(),
            device_model: device_model.into(),
            firmware_fingerprint: firmware_fingerprint.into(),
            provisioning_route,
            identity_assurance,
        }
    }

    /// The vault identifier this binding resolves to.
    ///
    /// A salted digest of the host peripheral identifier, the model and the
    /// firmware. Two physical devices cannot collide unless the host assigns
    /// them the same peripheral identifier, and an advertised name is not an
    /// input at all, so renaming a device changes nothing and impersonating a
    /// name grants nothing.
    #[must_use]
    pub fn vault_device_id(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"plaudon.device-binding.v1");
        hasher.update([0x1f]);
        hasher.update(self.peripheral_id.as_bytes());
        hasher.update([0x1f]);
        hasher.update(self.device_model.to_ascii_lowercase().as_bytes());
        hasher.update([0x1f]);
        hasher.update(self.firmware_fingerprint.to_ascii_lowercase().as_bytes());
        let digest = hasher.finalize();
        let mut id = String::with_capacity(4 + 32);
        id.push_str("dev-");
        id.push_str(&hex_lower(&digest[..16]));
        id
    }

    /// The credential scope for this binding.
    pub fn scope(&self) -> Result<SecretScope, ScopeError> {
        SecretScope::device(&self.vault_device_id())
    }

    /// Whether a fresh observation still matches this binding.
    #[must_use]
    pub fn matches_observation(
        &self,
        peripheral_id: &str,
        device_model: &str,
        firmware_fingerprint: &str,
    ) -> bool {
        self.peripheral_id == peripheral_id
            && self.device_model.eq_ignore_ascii_case(device_model)
            && self
                .firmware_fingerprint
                .eq_ignore_ascii_case(firmware_fingerprint)
    }
}

/// Why a credential operation was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// The provisioning route is one this build will not perform.
    #[error("this provisioning route is not available: {explanation}")]
    RouteUnavailable {
        /// Route that was requested.
        route: ProvisioningRoute,
        /// User-facing explanation.
        explanation: &'static str,
    },
    /// The device offered an authentication mode we do not implement.
    #[error("the device authentication mode is not implemented")]
    AuthModeUnsupported,
    /// The credential does not have a shape this profile accepts.
    ///
    /// Reports a length, never the value.
    #[error("the credential does not match this device profile")]
    CredentialShape {
        /// Length that was offered, in bytes.
        offered_bytes: usize,
    },
    /// The device observed now is not the device the binding was made with.
    #[error("this device does not match the stored authorization")]
    IdentityMismatch,
    /// The device identifier could not be used as a vault scope.
    #[error("the device identifier is not usable as a credential scope")]
    Scope(#[from] ScopeError),
    /// The OS vault refused.
    #[error(transparent)]
    Store(#[from] SecretStoreError),
}

impl CredentialError {
    /// A stable machine-readable code for the error envelope (spec §15.4).
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::RouteUnavailable { .. } => "AUTH_BOOTSTRAP_REQUIRED",
            Self::AuthModeUnsupported => "AUTH_MODE_UNSUPPORTED",
            Self::CredentialShape { .. } | Self::IdentityMismatch => "AUTH_REJECTED",
            Self::Scope(_) => "INVALID_ARGUMENT",
            Self::Store(error) => match error {
                SecretStoreError::Locked { .. } | SecretStoreError::Unavailable { .. } => {
                    "SECRET_STORE_LOCKED"
                }
                SecretStoreError::NotFound => "AUTH_BOOTSTRAP_REQUIRED",
                _ => "INTERNAL_ERROR",
            },
        }
    }
}

/// Device authorization over an OS credential vault.
///
/// Holds a borrowed store; it owns no secret state of its own, so there is no
/// in-memory credential cache to leak.
#[derive(Debug)]
pub struct DeviceCredentials<'a> {
    store: &'a dyn SecretStore,
}

impl<'a> DeviceCredentials<'a> {
    /// Wrap an OS credential store.
    #[must_use]
    pub const fn new(store: &'a dyn SecretStore) -> Self {
        Self { store }
    }

    /// Store authorization material for one device.
    ///
    /// Validates the route, the profile's auth mode and the credential shape
    /// before the vault is touched, so a rejected attempt leaves no entry.
    pub fn authorize(
        &self,
        binding: &DeviceBinding,
        profile: &ProtocolProfile,
        token_hex: &str,
    ) -> Result<SecretRef, CredentialError> {
        if !binding.provisioning_route.is_performed_here() {
            return Err(CredentialError::RouteUnavailable {
                route: binding.provisioning_route,
                explanation: binding.provisioning_route.explanation(),
            });
        }
        if !profile.auth_mode_is_implemented() {
            return Err(CredentialError::AuthModeUnsupported);
        }
        let bytes = decode_token_hex(token_hex)?;
        if !profile.accepts_token_length(bytes.len()) {
            return Err(CredentialError::CredentialShape {
                offered_bytes: bytes.len(),
            });
        }
        let scope = binding.scope()?;
        // The canonical lowercase hex form is what the vault holds; the
        // caller's copy is its own responsibility.
        let secret = SecretString::new(hex_lower(&bytes));
        Ok(self.store.store(&scope, &secret)?)
    }

    /// Load the credential for a device that is in front of us right now.
    ///
    /// The observation must match the binding. A device that advertises the
    /// same name, or even reports the same serial, but resolves to a different
    /// peripheral identifier, or whose firmware changed, gets
    /// [`CredentialError::IdentityMismatch`] and no vault read at all.
    pub fn load(
        &self,
        binding: &DeviceBinding,
        observed_peripheral_id: &str,
        observed_model: &str,
        observed_firmware: &str,
    ) -> Result<SecretString, CredentialError> {
        if !binding.matches_observation(observed_peripheral_id, observed_model, observed_firmware) {
            return Err(CredentialError::IdentityMismatch);
        }
        let scope = binding.scope()?;
        Ok(self.store.load(&scope)?)
    }

    /// Whether a credential exists for a binding. Never returns the value.
    pub fn status(&self, binding: &DeviceBinding) -> Result<SecretPresence, CredentialError> {
        let scope = binding.scope()?;
        Ok(self.store.status(&scope)?)
    }

    /// Remove the credential, returning the device to `AUTH_REQUIRED`.
    pub fn revoke(&self, binding: &DeviceBinding) -> Result<(), CredentialError> {
        let scope = binding.scope()?;
        Ok(self.store.delete(&scope)?)
    }
}

/// Decode a hex-encoded token without ever formatting the value.
fn decode_token_hex(token_hex: &str) -> Result<Vec<u8>, CredentialError> {
    let trimmed = token_hex.trim();
    if trimmed.is_empty() || !trimmed.len().is_multiple_of(2) {
        return Err(CredentialError::CredentialShape {
            offered_bytes: trimmed.len() / 2,
        });
    }
    let mut bytes = Vec::with_capacity(trimmed.len() / 2);
    let raw = trimmed.as_bytes();
    for pair in raw.chunks(2) {
        let (Some(high), Some(low)) = (nibble(pair[0]), nibble(pair[1])) else {
            return Err(CredentialError::CredentialShape {
                offered_bytes: trimmed.len() / 2,
            });
        };
        bytes.push(high << 4 | low);
    }
    Ok(bytes)
}

const fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use plaudon_plaud_protocol::profile::{ProfileAuthMode, LEGACY_NOTE_V0095};
    use plaudon_security::secrets::InMemorySecretStore;

    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn binding(peripheral: &str) -> DeviceBinding {
        DeviceBinding::new(
            peripheral,
            "Note",
            "V0095",
            ProvisioningRoute::ImportedExistingToken,
            IdentityAssurance::Unverified,
        )
    }

    #[test]
    fn a_token_is_stored_in_the_vault_and_is_retrievable_for_the_same_device() {
        let store = InMemorySecretStore::new();
        let credentials = DeviceCredentials::new(&store);
        let binding = binding("peripheral-a");

        let reference = credentials
            .authorize(&binding, &LEGACY_NOTE_V0095, TOKEN)
            .unwrap();
        assert!(reference.account.contains(&binding.vault_device_id()));
        assert!(
            !reference.fingerprint.contains(TOKEN),
            "a reference must not embed the credential"
        );

        let loaded = credentials
            .load(&binding, "peripheral-a", "Note", "V0095")
            .unwrap();
        assert_eq!(loaded.expose(), TOKEN);
        assert!(credentials.status(&binding).unwrap().present);
    }

    #[test]
    fn two_devices_sharing_a_name_and_serial_do_not_share_a_credential() {
        let store = InMemorySecretStore::new();
        let credentials = DeviceCredentials::new(&store);
        let first = binding("peripheral-a");
        let second = binding("peripheral-b");
        assert_ne!(first.vault_device_id(), second.vault_device_id());

        credentials
            .authorize(&first, &LEGACY_NOTE_V0095, TOKEN)
            .unwrap();
        assert!(
            !credentials.status(&second).unwrap().present,
            "a clone must not inherit another device's authorization"
        );
        assert!(matches!(
            credentials.load(&second, "peripheral-b", "Note", "V0095"),
            Err(CredentialError::Store(SecretStoreError::NotFound))
        ));
    }

    #[test]
    fn a_mismatched_observation_never_reaches_the_vault() {
        let store = InMemorySecretStore::new();
        let credentials = DeviceCredentials::new(&store);
        let binding = binding("peripheral-a");
        credentials
            .authorize(&binding, &LEGACY_NOTE_V0095, TOKEN)
            .unwrap();

        for (peripheral, model, firmware) in [
            ("peripheral-b", "Note", "V0095"),
            ("peripheral-a", "Note Pro", "V0095"),
            ("peripheral-a", "Note", "V0096"),
        ] {
            assert!(
                matches!(
                    credentials.load(&binding, peripheral, model, firmware),
                    Err(CredentialError::IdentityMismatch)
                ),
                "({peripheral}, {model}, {firmware}) must be refused"
            );
        }
    }

    #[test]
    fn a_firmware_change_invalidates_the_binding_rather_than_reusing_it() {
        let store = InMemorySecretStore::new();
        let credentials = DeviceCredentials::new(&store);
        let original = binding("peripheral-a");
        credentials
            .authorize(&original, &LEGACY_NOTE_V0095, TOKEN)
            .unwrap();

        let upgraded = DeviceBinding::new(
            "peripheral-a",
            "Note",
            "V0096",
            ProvisioningRoute::ImportedExistingToken,
            IdentityAssurance::Unverified,
        );
        assert_ne!(original.vault_device_id(), upgraded.vault_device_id());
        assert!(
            !credentials.status(&upgraded).unwrap().present,
            "new firmware must require fresh authorization, not silent reuse"
        );
    }

    #[test]
    fn revocation_removes_the_credential() {
        let store = InMemorySecretStore::new();
        let credentials = DeviceCredentials::new(&store);
        let binding = binding("peripheral-a");
        credentials
            .authorize(&binding, &LEGACY_NOTE_V0095, TOKEN)
            .unwrap();

        credentials.revoke(&binding).unwrap();
        assert!(!credentials.status(&binding).unwrap().present);
        assert!(matches!(
            credentials.load(&binding, "peripheral-a", "Note", "V0095"),
            Err(CredentialError::Store(SecretStoreError::NotFound))
        ));
        assert!(
            credentials.revoke(&binding).is_err(),
            "revoking nothing must not look like a successful revocation"
        );
    }

    #[test]
    fn an_encrypted_mode_profile_is_never_authorized() {
        let store = InMemorySecretStore::new();
        let credentials = DeviceCredentials::new(&store);
        let encrypted = ProtocolProfile {
            auth_mode: ProfileAuthMode::EncryptedHandshake,
            ..LEGACY_NOTE_V0095
        };
        assert_eq!(
            credentials.authorize(&binding("peripheral-a"), &encrypted, TOKEN),
            Err(CredentialError::AuthModeUnsupported)
        );
        assert!(
            !credentials
                .status(&binding("peripheral-a"))
                .unwrap()
                .present
        );
    }

    #[test]
    fn only_the_reviewed_provisioning_route_is_performed_here() {
        let store = InMemorySecretStore::new();
        let credentials = DeviceCredentials::new(&store);

        assert!(ProvisioningRoute::ImportedExistingToken.is_performed_here());
        for refused in [
            ProvisioningRoute::LinuxPeripheralBootstrap,
            ProvisioningRoute::AndroidHciCapture,
            ProvisioningRoute::EncryptedModeProvisioning,
            ProvisioningRoute::VendorMobileAppPairing,
        ] {
            assert!(!refused.is_performed_here(), "{refused:?}");
            let mut binding = binding("peripheral-a");
            binding.provisioning_route = refused;
            assert!(matches!(
                credentials.authorize(&binding, &LEGACY_NOTE_V0095, TOKEN),
                Err(CredentialError::RouteUnavailable { .. })
            ));
        }
        assert_eq!(
            ProvisioningRoute::LinuxPeripheralBootstrap.disposition(),
            RouteDisposition::Refused
        );
        assert_eq!(
            ProvisioningRoute::VendorMobileAppPairing.disposition(),
            RouteDisposition::UserActionElsewhere
        );
    }

    #[test]
    fn a_credential_of_the_wrong_shape_is_refused_without_echoing_it() {
        let store = InMemorySecretStore::new();
        let credentials = DeviceCredentials::new(&store);
        let binding = binding("peripheral-a");

        for bad in ["", "abc", "zz", "00112233", &"ab".repeat(64)] {
            let error = credentials
                .authorize(&binding, &LEGACY_NOTE_V0095, bad)
                .unwrap_err();
            assert!(
                matches!(error, CredentialError::CredentialShape { .. }),
                "{bad:?} produced {error:?}"
            );
            let rendered = error.to_string();
            assert!(
                !rendered.contains(bad) || bad.is_empty(),
                "the message must not echo the credential"
            );
        }
        assert!(!credentials.status(&binding).unwrap().present);
    }

    #[test]
    fn a_binding_is_safe_to_persist() {
        let binding = binding("peripheral-a");
        let json = serde_json::to_string(&binding).unwrap();
        assert!(!json.contains(TOKEN));
        assert!(!json.to_lowercase().contains("token\":\""));
        assert_eq!(
            serde_json::from_str::<DeviceBinding>(&json).unwrap(),
            binding
        );
    }

    #[test]
    fn identity_assurance_is_never_overstated() {
        assert_eq!(
            IdentityAssurance::Unverified.ui_label(),
            "Unverified identity"
        );
        assert_eq!(
            IdentityAssurance::ProfileAuthenticated.ui_label(),
            "Profile-authenticated"
        );
        assert!(!IdentityAssurance::Unverified.is_authenticated());
        for label in [
            IdentityAssurance::Unverified.ui_label(),
            IdentityAssurance::ProfileAuthenticated.ui_label(),
        ] {
            let lowered = label.to_lowercase();
            assert!(!lowered.contains("verified identity") || lowered.starts_with("unverified"));
            assert!(!lowered.contains("secure"));
            assert!(!lowered.contains("encrypted"));
        }
    }

    #[test]
    fn the_legacy_disclosures_state_the_radio_risk_plainly() {
        let joined = LEGACY_DISCLOSURES.join(" ").to_lowercase();
        assert!(joined.contains("unencrypted"));
        assert!(joined.contains("copied token"));
        assert!(joined.contains("local-only mode"));
        assert!(!joined.contains("end-to-end"));
        assert_eq!(LEGACY_DISCLOSURES.len(), 3);
    }

    #[test]
    fn the_vault_identifier_excludes_the_advertised_name() {
        // Two bindings differing only in a name the caller never supplies.
        let first = binding("peripheral-a");
        let second = binding("peripheral-a");
        assert_eq!(first.vault_device_id(), second.vault_device_id());
        assert!(first.vault_device_id().starts_with("dev-"));
        assert_eq!(first.vault_device_id().len(), 4 + 32);
        // And the identifier is acceptable as a vault scope.
        first
            .scope()
            .expect("the derived identifier is a valid scope");
    }
}

#[cfg(test)]
mod real_vault_tests {
    //! Opt-in checks against the machine's real credential vault (spec §18.4).
    //!
    //! `#[ignore]`d unless the `real-keychain` feature is on, because they
    //! write to the developer's Keychain or Credential Manager and may raise
    //! an unlock prompt. EPIC D covers the store itself; what these cover is
    //! the binding layer on top of it: that a device binding resolves to its
    //! own vault entry on the real platform, and that revocation removes it.

    use plaudon_plaud_protocol::profile::LEGACY_NOTE_V0095;
    use plaudon_security::secrets::os_store;

    use super::*;

    const TOKEN: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f0";

    fn unique(prefix: &str) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        std::time::SystemTime::now().hash(&mut hasher);
        std::process::id().hash(&mut hasher);
        format!("{prefix}-{:016x}", hasher.finish())
    }

    #[test]
    #[cfg_attr(
        not(feature = "real-keychain"),
        ignore = "needs --features real-keychain"
    )]
    fn a_binding_round_trips_through_the_real_os_vault() {
        if !cfg!(any(target_os = "macos", target_os = "windows")) {
            return;
        }
        let store = os_store();
        let credentials = DeviceCredentials::new(&*store);
        let binding = DeviceBinding::new(
            unique("selftest-peripheral"),
            "Note",
            "V0095",
            ProvisioningRoute::ImportedExistingToken,
            IdentityAssurance::Unverified,
        );
        let other = DeviceBinding::new(
            unique("selftest-other"),
            "Note",
            "V0095",
            ProvisioningRoute::ImportedExistingToken,
            IdentityAssurance::Unverified,
        );

        credentials
            .authorize(&binding, &LEGACY_NOTE_V0095, TOKEN)
            .expect("the real vault accepts a device credential");

        let loaded = credentials
            .load(
                &binding,
                &binding.peripheral_id,
                &binding.device_model,
                &binding.firmware_fingerprint,
            )
            .expect("the credential comes back");
        assert_eq!(loaded.expose(), TOKEN);

        // A different device, same model and firmware, resolves elsewhere.
        let cross = credentials.load(
            &other,
            &other.peripheral_id,
            &other.device_model,
            &other.firmware_fingerprint,
        );
        let cleanup = credentials.revoke(&binding);
        assert!(matches!(
            cross,
            Err(CredentialError::Store(SecretStoreError::NotFound))
        ));
        cleanup.expect("revocation removes the entry");
        assert!(
            !credentials
                .status(&binding)
                .expect("status after revocation")
                .present
        );
    }
}
