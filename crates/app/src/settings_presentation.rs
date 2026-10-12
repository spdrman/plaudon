//! The rules the Settings screen renders, with no view attached.
//!
//! Matrix rows C1–C5, A9, A13, J16 and the N2.2 update state. Everything here
//! is a pure function over a DTO, so the honest-label obligations — unknown is
//! "unknown" and never zero, a revoked grant stays listed, a denied permission
//! says the activity history is still complete — are asserted by ordinary unit
//! tests instead of by rendering a window.
//!
//! The split with `cloud_presentation` (O2.2) is deliberate: that module owns
//! price, route and verification copy for the *cloud* surfaces, this one owns
//! the wording the settings screen alone uses.

use plaudon_app_core::models::{ModelEntry, ModelLicense};
use serde::{Deserialize, Serialize};

use crate::dto::{
    Appearance, Capabilities, ConsentSummary, Contrast, LocalOnlyModeResponse,
    OsNotificationPermission, StorageCapability,
};

/// Byte units, largest first, mirroring `features/settings/bytes.ts`.
const UNITS: &[(&str, u64)] = &[
    ("TB", 1024 * 1024 * 1024 * 1024),
    ("GB", 1024 * 1024 * 1024),
    ("MB", 1024 * 1024),
    ("kB", 1024),
];

/// What the interface says when it does not know a quantity.
pub const UNKNOWN: &str = "unknown";

/// Human byte size, or [`UNKNOWN`] when the quantity was never measured.
///
/// A missing size renders as "unknown", never as `0 bytes`: the cross-cutting
/// invariant that an unmeasured quantity is never shown as a zero.
#[must_use]
pub fn format_bytes(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return UNKNOWN.to_owned();
    };
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    for &(label, size) in UNITS {
        if bytes >= size {
            let value = bytes as f64 / size as f64;
            return if value < 10.0 {
                format!("{value:.1} {label}")
            } else {
                format!("{value:.0} {label}")
            };
        }
    }
    format!("{bytes} bytes")
}

/* ------------------------------------------------------------- appearance */

/// Label of an appearance choice.
#[must_use]
pub const fn appearance_label(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::System => "Follow the system",
        Appearance::Light => "Light",
        Appearance::Dark => "Dark",
    }
}

/// Label of a contrast choice.
#[must_use]
pub const fn contrast_label(contrast: Contrast) -> &'static str {
    match contrast {
        Contrast::Standard => "Standard",
        Contrast::High => "High contrast",
    }
}

/* ---------------------------------------------------------------- privacy */

/// The standing explanation of what Local-only Mode protects.
///
/// The *enforced* policy is never composed here: it comes back from
/// `set_local_only_mode` as [`LocalOnlyModeResponse::description`], and
/// [`local_only_status`] renders that, so the interface cannot claim a posture
/// the gate did not confirm.
pub const LOCAL_ONLY_PROSE: &str = "Recordings, transcripts and summaries stay on this computer. \
Cloud AI is opt-in: Plaudon sends nothing to a provider until you choose one and consent for that \
purpose.";

/// Label of the Local-only toggle.
pub const LOCAL_ONLY_TOGGLE: &str = "Local-only mode (refuse all outbound network access)";

/// Status badge for the Local-only switch: label plus a glyph, never colour
/// alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalOnlyStatus {
    /// Text a screen reader announces.
    pub label: &'static str,
    /// Glyph shown beside it.
    pub glyph: &'static str,
}

/// The badge for `enabled`.
#[must_use]
pub const fn local_only_status(enabled: bool) -> LocalOnlyStatus {
    if enabled {
        LocalOnlyStatus {
            label: "Local only",
            glyph: "✓",
        }
    } else {
        LocalOnlyStatus {
            label: "Network allowed for providers you enable",
            glyph: "!",
        }
    }
}

/// What the gate reported, as lines to render.
///
/// `None` until a change has been confirmed: before that the interface knows
/// the stored preference but not the enforced policy, and saying otherwise
/// would be inventing it.
#[must_use]
pub fn enforced_policy_lines(policy: Option<&LocalOnlyModeResponse>) -> Vec<String> {
    let Some(policy) = policy else {
        return Vec::new();
    };
    let mut lines = vec![format!("Enforced policy: {}", policy.description)];
    if policy.allowed_hosts.is_empty() {
        lines.push("No hosts are reachable.".to_owned());
    } else {
        lines.extend(policy.allowed_hosts.iter().cloned());
    }
    lines
}

/* --------------------------------------------------------------- AI key */

/// Badge for the OpenRouter key. The key itself is never a value here — only
/// its presence (spec §16.4).
#[must_use]
pub const fn openrouter_key_status(present: bool) -> LocalOnlyStatus {
    if present {
        LocalOnlyStatus {
            label: "Key saved in the system keychain",
            glyph: "✓",
        }
    } else {
        LocalOnlyStatus {
            label: "No key saved",
            glyph: "–",
        }
    }
}

/// The standing note under the key field.
pub const OPENROUTER_KEY_NOTE: &str = "Plaudon never displays a saved key and never returns it \
from the backend. Having a key does not grant cloud processing on its own.";

/// Disclosure shown before the key is removed.
pub const OPENROUTER_REMOVE_DISCLOSURE: &str = "The key is deleted from the system keychain. \
Cloud requests stop working until you add a key again. Local transcription is unaffected.";

/* ---------------------------------------------------------- notifications */

/// What the operating system currently permits, in the user's terms.
///
/// Every branch repeats that the in-app activity history is unaffected,
/// because spec §5.5 makes that the point: a denied permission must not look
/// like lost events.
#[must_use]
pub const fn notification_permission_copy(
    permission: Option<OsNotificationPermission>,
) -> &'static str {
    match permission {
        Some(OsNotificationPermission::Granted) => {
            "Operating system status: notifications are permitted."
        }
        Some(OsNotificationPermission::Denied) => {
            "Operating system status: notifications are blocked for this app. The activity history \
             still records every event. To change it, allow notifications for Plaudon in your \
             operating system's notification settings."
        }
        Some(OsNotificationPermission::Unsupported) => {
            "Operating system status: this system does not support desktop notifications. The \
             activity history still records every event."
        }
        Some(OsNotificationPermission::NotRequested) => {
            "Operating system status: permission has not been requested yet. It is requested the \
             first time a notification is sent."
        }
        None => "Operating system status: unknown.",
    }
}

/// The standing note above the notification switches.
pub const NOTIFICATIONS_NOTE: &str = "Notifications are opt-in and use privacy-safe text. \
Everything listed here is also recorded in the activity history, which stays complete whatever \
you choose.";

/* ---------------------------------------------------------------- storage */

/// One read-only storage fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// Term.
    pub label: &'static str,
    /// Value, already rendered.
    pub value: String,
}

/// The read-only storage facts (matrix row C4).
#[must_use]
pub fn storage_facts(storage: Option<&StorageCapability>) -> Vec<Fact> {
    let Some(storage) = storage else {
        return Vec::new();
    };
    vec![
        Fact {
            label: "Library location",
            value: storage.library_path.clone(),
        },
        Fact {
            label: "Free space",
            value: format_bytes(Some(storage.free_bytes)),
        },
        Fact {
            label: "Total space",
            value: format_bytes(Some(storage.total_bytes)),
        },
    ]
}

/// Shown instead of the facts when `get_capabilities` has not answered yet.
pub const STORAGE_UNAVAILABLE: &str = "Storage facts are not available yet.";

/// Why the library location is read-only in this build.
///
/// There is no command that moves the library, so the section says so rather
/// than offering a control that would do nothing.
pub const STORAGE_LOCATION_FIXED: &str = "The library location is chosen at first run and is not \
movable from here. To move it, quit Plaudon and move the folder, then start Plaudon with the new \
location.";

/* ---------------------------------------------------------------- consent */

/// Human label for a `ConsentSummary::purpose`, which arrives as the stored
/// wire string.
///
/// An unrecognised value is shown verbatim rather than guessed at.
#[must_use]
pub fn consent_purpose_label(purpose: &str) -> &str {
    match purpose {
        "transcription" => "Transcription",
        "summarization" => "Summarization",
        "transcript_tool" => "Transcript tool",
        "credential_validation" => "Credential validation",
        other => other,
    }
}

/// Human label for a `ConsentSummary::content_type`.
#[must_use]
pub fn consent_content_label(content_type: &str) -> &str {
    match content_type {
        "audio_bytes" => "Audio bytes",
        "transcript_text" => "Transcript text",
        "metadata_only" => "Metadata only",
        other => other,
    }
}

/// Human label for a `ConsentSummary::scope_kind`.
#[must_use]
pub fn consent_scope_label(scope_kind: &str) -> &str {
    match scope_kind {
        "ask_every_time" => "Ask every time",
        "recording" => "This recording only",
        "model_policy" => "This model policy",
        other => other,
    }
}

/// Human label for a `ConsentSummary::required_retention`.
#[must_use]
pub fn consent_retention_label(retention: Option<&str>) -> String {
    match retention {
        None => "Not recorded".to_owned(),
        Some("zero_retention") => "Zero retention required".to_owned(),
        Some("provider_default") => "Provider default retention".to_owned(),
        Some(other) => other.to_owned(),
    }
}

/// The sentence that must appear above the consent register.
///
/// Spec §16.3: revoking is forward-looking, and the interface never implies
/// that content already sent can be recalled.
pub const CONSENT_REVOCATION_NOTE: &str = "Revoking stops future cloud runs under that grant. It \
does not recall content already sent or delete anything a provider already holds.";

/// Shown when no grant has ever been made.
pub const CONSENT_EMPTY: &str = "No cloud consent has been granted.";

/// One consent grant, reduced to what the register renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentRow {
    /// Grant identity, for the revoke command.
    pub consent_id: String,
    /// Model the grant names.
    pub model_id: String,
    /// Whether the grant is still in force.
    pub active: bool,
    /// Badge label.
    pub status_label: &'static str,
    /// Badge glyph, so status is never colour alone.
    pub status_glyph: &'static str,
    /// The facts the grant was given under.
    pub facts: Vec<Fact>,
}

/// Render every grant, revoked ones included.
///
/// Spec §4.4 #6: a withdrawn grant stays visible, marked revoked with the time
/// it happened, so the record of what was once allowed is never quietly erased.
#[must_use]
pub fn consent_rows(grants: &[ConsentSummary]) -> Vec<ConsentRow> {
    grants
        .iter()
        .map(|grant| {
            let active = grant.revoked_at.is_none();
            let mut facts = vec![
                Fact {
                    label: "Purpose",
                    value: consent_purpose_label(&grant.purpose).to_owned(),
                },
                Fact {
                    label: "Data allowed",
                    value: consent_content_label(&grant.content_type).to_owned(),
                },
                Fact {
                    label: "Scope",
                    value: match &grant.recording_id {
                        Some(recording_id) => format!(
                            "{} ({recording_id})",
                            consent_scope_label(&grant.scope_kind)
                        ),
                        None => consent_scope_label(&grant.scope_kind).to_owned(),
                    },
                },
                Fact {
                    label: "Downstream provider",
                    value: grant
                        .downstream_provider
                        .clone()
                        .unwrap_or_else(|| "Not disclosed".to_owned()),
                },
                Fact {
                    label: "Required retention",
                    value: consent_retention_label(grant.required_retention.as_deref()),
                },
                Fact {
                    label: "Granted",
                    value: grant.granted_at.clone(),
                },
                Fact {
                    label: "Expires",
                    value: grant
                        .expires_at
                        .clone()
                        .unwrap_or_else(|| "No expiry recorded".to_owned()),
                },
            ];
            if let Some(revoked_at) = &grant.revoked_at {
                facts.push(Fact {
                    label: "Revoked",
                    value: revoked_at.clone(),
                });
            }
            facts.push(Fact {
                label: "Disclosure version",
                value: grant.disclosure_version.clone(),
            });
            ConsentRow {
                consent_id: grant.consent_id.clone(),
                model_id: grant.model_id.clone(),
                active,
                status_label: if active { "Active" } else { "Revoked" },
                status_glyph: if active { "✓" } else { "✕" },
                facts,
            }
        })
        .collect()
}

/* ---------------------------------------------------------------- engines */

/// One engine as the engine pickers render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineRow {
    /// Engine identity.
    pub engine_id: String,
    /// Name shown in the picker.
    pub display_name: String,
    /// Validation status, rendered from the backend's own word.
    pub status_label: String,
    /// Glyph for the status, so it is never colour alone.
    pub status_glyph: &'static str,
    /// Why it is unavailable, when the backend said.
    pub unavailable_reason: Option<String>,
    /// Whether the engine may be chosen as a default.
    pub selectable: bool,
    /// Whether using it needs the network.
    pub requires_network: bool,
    /// Installed models, already rendered.
    pub installed_models: Vec<String>,
    /// Download requirement, honestly unknown when it is.
    pub download_requirement: String,
}

/// Glyph for a capability status word.
#[must_use]
pub const fn status_glyph(status: &str) -> &'static str {
    match status.as_bytes() {
        b"supported" => "✓",
        b"experimental" => "~",
        b"unsupported" => "✕",
        _ => "?",
    }
}

/// Title-case the backend's status word without inventing one.
#[must_use]
pub fn status_label(status: &str) -> String {
    let mut chars = status.chars();
    match chars.next() {
        None => UNKNOWN.to_owned(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// Engine rows for one stage, with the free-space figure the disclosure needs.
#[must_use]
pub fn engine_rows(capabilities: &Capabilities, stage: crate::dto::EngineStage) -> Vec<EngineRow> {
    let free_bytes = capabilities.storage.free_bytes;
    capabilities
        .engines
        .iter()
        .filter(|engine| engine.stage == stage)
        .map(|engine| {
            let status = serde_json::to_value(engine.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| UNKNOWN.to_owned());
            EngineRow {
                engine_id: engine.engine_id.clone(),
                display_name: engine.display_name.clone(),
                status_glyph: status_glyph(&status),
                selectable: status != "unsupported",
                status_label: status_label(&status),
                unavailable_reason: engine.unavailable_reason.clone(),
                requires_network: engine.requires_network,
                installed_models: engine
                    .installed_models
                    .iter()
                    .map(|model| {
                        format!(
                            "{} — {} · {}",
                            model.display_name,
                            format_bytes(Some(model.size_bytes)),
                            if model.installed {
                                "installed"
                            } else {
                                "not installed"
                            }
                        )
                    })
                    .collect(),
                download_requirement: match engine.download_bytes_required {
                    Some(bytes) => format!(
                        "Needs {}, {} free on this disk.",
                        format_bytes(Some(bytes)),
                        format_bytes(Some(free_bytes))
                    ),
                    None => "Download size unknown.".to_owned(),
                },
            }
        })
        .collect()
}

/* ----------------------------------------------------------- local models */

/// Why a model may not be installed right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "reason")]
pub enum InstallBlocker {
    /// The licence has not been accepted.
    LicenceNotAccepted {
        /// SPDX identifier the user must accept.
        spdx_id: String,
    },
    /// The volume does not have room, including the installer's headroom.
    NotEnoughDisk {
        /// Bytes needed.
        required_bytes: u64,
        /// Bytes available.
        available_bytes: u64,
    },
    /// The machine has less RAM than the model declares it needs.
    NotEnoughMemory {
        /// Bytes the model needs.
        required_bytes: u64,
        /// Bytes the machine has.
        available_bytes: u64,
    },
    /// The model is already installed.
    AlreadyInstalled,
}

impl InstallBlocker {
    /// The sentence shown beside the blocked install.
    #[must_use]
    pub fn explanation(&self) -> String {
        match self {
            Self::LicenceNotAccepted { spdx_id } => {
                format!("Accept the {spdx_id} licence before installing.")
            }
            Self::NotEnoughDisk {
                required_bytes,
                available_bytes,
            } => format!(
                "This install needs {} including headroom; {} is free.",
                format_bytes(Some(*required_bytes)),
                format_bytes(Some(*available_bytes))
            ),
            Self::NotEnoughMemory {
                required_bytes,
                available_bytes,
            } => format!(
                "The model requires {} of memory; this machine reports {}.",
                format_bytes(Some(*required_bytes)),
                format_bytes(Some(*available_bytes))
            ),
            Self::AlreadyInstalled => "This model is already installed.".to_owned(),
        }
    }
}

/// Licence facts shown before an install (spec §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenceDisclosure {
    /// SPDX identifier.
    pub spdx_id: String,
    /// Human-readable licence name.
    pub name: String,
    /// Whether the weights may travel inside an offline pack.
    pub redistributable: bool,
    /// Sentence describing what redistribution means for this model.
    pub redistribution_note: String,
    /// Leaf file name of the licence text shipped with the weights.
    pub text_file: String,
}

impl LicenceDisclosure {
    /// Build the disclosure from the manifest's licence block.
    #[must_use]
    pub fn from_license(license: &ModelLicense) -> Self {
        Self {
            spdx_id: license.spdx_id.clone(),
            name: license.name.clone(),
            redistributable: license.redistributable,
            redistribution_note: if license.redistributable {
                "These weights may be redistributed inside an offline pack.".to_owned()
            } else {
                "These weights may not be redistributed; they are download-only.".to_owned()
            },
            text_file: license.text_file.clone(),
        }
    }
}

/// The disk and memory cost of one model, as the gate presents it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FootprintDisclosure {
    /// Exact size of the weights.
    pub size_bytes: u64,
    /// Size plus the installer's 10% headroom, which is what preflight checks.
    pub required_bytes: u64,
    /// Below this, installation is refused.
    pub min_ram_bytes: u64,
    /// Below this, installation warns but proceeds.
    pub recommended_ram_bytes: u64,
    /// The manifest's own hardware note.
    pub notes: String,
}

impl FootprintDisclosure {
    /// Build the footprint from a manifest entry.
    ///
    /// The headroom matches `ModelInstaller::preflight` exactly, so the figure
    /// the gate shows is the figure the installer enforces.
    #[must_use]
    pub fn from_entry(entry: &ModelEntry) -> Self {
        Self {
            size_bytes: entry.size_bytes,
            required_bytes: entry.size_bytes.saturating_add(entry.size_bytes / 10),
            min_ram_bytes: entry.hardware.min_ram_bytes,
            recommended_ram_bytes: entry.hardware.recommended_ram_bytes,
            notes: entry.hardware.notes.clone(),
        }
    }
}

/// The licence-and-footprint gate for one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInstallGate {
    /// Model the gate is about.
    pub model_id: String,
    /// Display name.
    pub display_name: String,
    /// Licence facts the user must see.
    pub licence: LicenceDisclosure,
    /// Disk and memory cost.
    pub footprint: FootprintDisclosure,
    /// Whether the machine is above the recommended memory, which is advice,
    /// not a refusal.
    pub below_recommended_memory: bool,
}

impl ModelInstallGate {
    /// Everything standing between the user and an install.
    ///
    /// `available_disk_bytes` and `total_ram_bytes` are what the host reported;
    /// `licence_accepted` is the user's explicit acceptance. The order matters
    /// to nobody but the reader: all blockers are reported at once so the user
    /// is not told about them one at a time.
    #[must_use]
    pub fn blockers(
        &self,
        licence_accepted: bool,
        available_disk_bytes: u64,
        total_ram_bytes: u64,
        already_installed: bool,
    ) -> Vec<InstallBlocker> {
        let mut blockers = Vec::new();
        if already_installed {
            blockers.push(InstallBlocker::AlreadyInstalled);
        }
        if !licence_accepted {
            blockers.push(InstallBlocker::LicenceNotAccepted {
                spdx_id: self.licence.spdx_id.clone(),
            });
        }
        if available_disk_bytes < self.footprint.required_bytes {
            blockers.push(InstallBlocker::NotEnoughDisk {
                required_bytes: self.footprint.required_bytes,
                available_bytes: available_disk_bytes,
            });
        }
        if total_ram_bytes < self.footprint.min_ram_bytes {
            blockers.push(InstallBlocker::NotEnoughMemory {
                required_bytes: self.footprint.min_ram_bytes,
                available_bytes: total_ram_bytes,
            });
        }
        blockers
    }
}

/// Why this build cannot fetch a model over the network.
///
/// There is no model catalogue endpoint in this build, so the section says so
/// instead of offering a download button that could not work.
pub const MODEL_SOURCE_NOTE: &str = "This build installs models from an offline model pack you \
choose. It ships no model catalogue and downloads nothing on its own, in Local-only mode or out \
of it.";

/* ---------------------------------------------------------------- updates */

/// What the update subsystem can report without making a request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "readiness")]
pub enum UpdateReadiness {
    /// An installation root exists and names the version that will launch.
    Ready {
        /// Version that launches next.
        installed_version: String,
    },
    /// No installation root has been created yet, which is the state of a
    /// build that was not installed by the updater.
    NotInstalledByUpdater {
        /// Non-secret explanation.
        detail: String,
    },
}

/// The update section's state (task N2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// What the installation root says.
    pub readiness: UpdateReadiness,
    /// Whether Local-only Mode suppresses update checks.
    pub checks_suppressed: bool,
    /// The endpoint updates are pinned to, shown so the user can see it.
    pub manifest_endpoint: String,
    /// Identifier of the release signing key updates are verified against.
    pub signing_key_id: String,
}

impl UpdateStatus {
    /// The sentence describing whether a check can happen at all.
    #[must_use]
    pub fn check_policy_copy(&self) -> &'static str {
        if self.checks_suppressed {
            "Local-only mode is on, so Plaudon makes no update request at all — not even to ask \
             whether one exists."
        } else {
            "Plaudon checks for updates only when you ask it to, and installs nothing without \
             verifying the release signature."
        }
    }

    /// The installed version, or an honest statement that there is none.
    #[must_use]
    pub fn installed_version_copy(&self) -> String {
        match &self.readiness {
            UpdateReadiness::Ready { installed_version } => installed_version.clone(),
            UpdateReadiness::NotInstalledByUpdater { .. } => {
                "Not managed by the updater".to_owned()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{EngineInventoryEntry, EngineStage, InstalledModel};
    use plaudon_app_core::models::{HardwareRecommendation, ModelId, ModelSource};
    use plaudon_domain::capability::CapabilityStatus;
    use plaudon_domain::media::Sha256Digest;

    fn storage() -> StorageCapability {
        StorageCapability {
            library_path: "/Users/a/Library/Application Support/Plaudon".to_owned(),
            free_bytes: 42 * 1024 * 1024 * 1024,
            total_bytes: 500 * 1024 * 1024 * 1024,
        }
    }

    #[test]
    fn an_unmeasured_size_is_unknown_and_never_zero() {
        assert_eq!(format_bytes(None), "unknown");
        assert_eq!(format_bytes(Some(0)), "0 bytes");
    }

    #[test]
    fn byte_sizes_match_the_typescript_they_replace() {
        assert_eq!(format_bytes(Some(512)), "512 bytes");
        assert_eq!(format_bytes(Some(1024)), "1.0 kB");
        assert_eq!(format_bytes(Some(1536)), "1.5 kB");
        assert_eq!(format_bytes(Some(20 * 1024)), "20 kB");
        assert_eq!(format_bytes(Some(5 * 1024 * 1024)), "5.0 MB");
        assert_eq!(format_bytes(Some(42 * 1024 * 1024 * 1024)), "42 GB");
        assert_eq!(format_bytes(Some(2 * 1024_u64.pow(4))), "2.0 TB");
    }

    #[test]
    fn the_local_only_badge_carries_a_glyph_as_well_as_a_colour() {
        assert_eq!(local_only_status(true).glyph, "✓");
        assert_eq!(local_only_status(false).glyph, "!");
        assert_ne!(
            local_only_status(true).label,
            local_only_status(false).label
        );
    }

    #[test]
    fn no_policy_is_rendered_before_the_gate_has_answered() {
        assert!(enforced_policy_lines(None).is_empty());
    }

    #[test]
    fn the_rendered_policy_is_the_gates_own_words() {
        let policy = LocalOnlyModeResponse {
            enabled: true,
            description: "All outbound network access is refused.".to_owned(),
            allowed_hosts: Vec::new(),
        };
        let lines = enforced_policy_lines(Some(&policy));
        assert_eq!(
            lines[0],
            "Enforced policy: All outbound network access is refused."
        );
        assert_eq!(lines[1], "No hosts are reachable.");
    }

    #[test]
    fn allowed_hosts_are_listed_verbatim() {
        let policy = LocalOnlyModeResponse {
            enabled: false,
            description: "Providers you enabled are reachable.".to_owned(),
            allowed_hosts: vec!["openrouter.ai".to_owned()],
        };
        assert_eq!(enforced_policy_lines(Some(&policy))[1], "openrouter.ai");
    }

    #[test]
    fn the_key_badge_reports_presence_and_nothing_else() {
        assert_eq!(
            openrouter_key_status(true).label,
            "Key saved in the system keychain"
        );
        assert_eq!(openrouter_key_status(false).label, "No key saved");
    }

    #[test]
    fn every_permission_state_says_the_activity_history_is_unaffected() {
        for permission in [
            OsNotificationPermission::Denied,
            OsNotificationPermission::Unsupported,
        ] {
            assert!(
                notification_permission_copy(Some(permission)).contains("activity history"),
                "{permission:?}"
            );
        }
        assert_eq!(
            notification_permission_copy(None),
            "Operating system status: unknown."
        );
    }

    #[test]
    fn storage_facts_are_three_read_only_rows() {
        let facts = storage_facts(Some(&storage()));
        assert_eq!(facts.len(), 3);
        assert_eq!(facts[0].label, "Library location");
        assert_eq!(facts[1].value, "42 GB");
        assert!(storage_facts(None).is_empty());
    }

    #[test]
    fn a_revoked_grant_stays_listed_and_says_when() {
        let grants = vec![ConsentSummary {
            consent_id: "c1".to_owned(),
            purpose: "summarization".to_owned(),
            content_type: "transcript_text".to_owned(),
            scope_kind: "recording".to_owned(),
            recording_id: Some("r1".to_owned()),
            model_id: "openai/gpt-4o-mini".to_owned(),
            downstream_provider: None,
            required_retention: None,
            disclosure_version: "2".to_owned(),
            granted_at: "2026-01-01T00:00:00Z".to_owned(),
            expires_at: None,
            revoked_at: Some("2026-02-01T00:00:00Z".to_owned()),
        }];
        let rows = consent_rows(&grants);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].active);
        assert_eq!(rows[0].status_label, "Revoked");
        assert_eq!(rows[0].status_glyph, "✕");
        assert!(rows[0]
            .facts
            .iter()
            .any(|fact| fact.label == "Revoked" && fact.value == "2026-02-01T00:00:00Z"));
        assert!(rows[0]
            .facts
            .iter()
            .any(|fact| fact.label == "Downstream provider" && fact.value == "Not disclosed"));
        assert!(rows[0]
            .facts
            .iter()
            .any(|fact| fact.label == "Required retention" && fact.value == "Not recorded"));
    }

    #[test]
    fn an_active_grant_is_marked_active_and_carries_the_scope_recording() {
        let grants = vec![ConsentSummary {
            consent_id: "c2".to_owned(),
            purpose: "transcription".to_owned(),
            content_type: "audio_bytes".to_owned(),
            scope_kind: "recording".to_owned(),
            recording_id: Some("r9".to_owned()),
            model_id: "m".to_owned(),
            downstream_provider: Some("Acme".to_owned()),
            required_retention: Some("zero_retention".to_owned()),
            disclosure_version: "2".to_owned(),
            granted_at: "2026-01-01T00:00:00Z".to_owned(),
            expires_at: Some("2026-03-01T00:00:00Z".to_owned()),
            revoked_at: None,
        }];
        let rows = consent_rows(&grants);
        assert!(rows[0].active);
        assert_eq!(rows[0].status_label, "Active");
        let scope = rows[0]
            .facts
            .iter()
            .find(|fact| fact.label == "Scope")
            .unwrap();
        assert_eq!(scope.value, "This recording only (r9)");
    }

    #[test]
    fn an_unknown_consent_word_is_shown_verbatim_not_guessed_at() {
        assert_eq!(
            consent_purpose_label("quantum_analysis"),
            "quantum_analysis"
        );
        assert_eq!(consent_retention_label(Some("weird")), "weird");
    }

    #[test]
    fn an_unsupported_engine_cannot_be_chosen_as_a_default() {
        let capabilities = capabilities_with_engines(vec![
            engine("whisper.cpp", CapabilityStatus::Supported, EngineStage::Stt),
            engine("broken", CapabilityStatus::Unsupported, EngineStage::Stt),
        ]);
        let rows = engine_rows(&capabilities, EngineStage::Stt);
        assert_eq!(rows.len(), 2);
        assert!(rows[0].selectable);
        assert!(!rows[1].selectable, "an unsupported engine is still listed");
        assert_eq!(rows[1].status_glyph, "✕");
    }

    #[test]
    fn an_engine_with_no_stated_download_size_says_unknown() {
        let capabilities = capabilities_with_engines(vec![engine(
            "whisper.cpp",
            CapabilityStatus::Supported,
            EngineStage::Stt,
        )]);
        let rows = engine_rows(&capabilities, EngineStage::Stt);
        assert_eq!(rows[0].download_requirement, "Download size unknown.");
        assert_eq!(rows[0].installed_models, vec!["tiny — 75 MB · installed"]);
    }

    #[test]
    fn the_text_stage_is_filtered_out_of_the_stt_picker() {
        let capabilities = capabilities_with_engines(vec![
            engine("whisper.cpp", CapabilityStatus::Supported, EngineStage::Stt),
            engine("llama", CapabilityStatus::Supported, EngineStage::Text),
        ]);
        assert_eq!(engine_rows(&capabilities, EngineStage::Text).len(), 1);
        assert_eq!(
            engine_rows(&capabilities, EngineStage::Text)[0].engine_id,
            "llama"
        );
    }

    fn entry() -> ModelEntry {
        ModelEntry {
            id: ModelId::parse("tiny-en").unwrap(),
            engine: "whisper.cpp".to_owned(),
            version: "1".to_owned(),
            display_name: "Tiny (English)".to_owned(),
            file_name: "tiny.bin".to_owned(),
            size_bytes: 1000,
            sha256: Sha256Digest::of(b"tiny"),
            languages: Vec::new(),
            license: ModelLicense {
                spdx_id: "MIT".to_owned(),
                name: "MIT License".to_owned(),
                redistributable: true,
                text_file: "LICENSE".to_owned(),
            },
            source: ModelSource::Pack {
                entry_path: "whisper.cpp/1/tiny.bin".to_owned(),
            },
            hardware: HardwareRecommendation {
                min_ram_bytes: 2000,
                recommended_ram_bytes: 4000,
                notes: "runs in real time on Apple Silicon".to_owned(),
            },
        }
    }

    fn gate() -> ModelInstallGate {
        let entry = entry();
        ModelInstallGate {
            model_id: entry.id.to_string(),
            display_name: entry.display_name.clone(),
            licence: LicenceDisclosure::from_license(&entry.license),
            footprint: FootprintDisclosure::from_entry(&entry),
            below_recommended_memory: false,
        }
    }

    #[test]
    fn the_footprint_matches_the_headroom_the_installer_enforces() {
        let footprint = FootprintDisclosure::from_entry(&entry());
        assert_eq!(footprint.size_bytes, 1000);
        assert_eq!(footprint.required_bytes, 1100);
    }

    #[test]
    fn an_unaccepted_licence_blocks_the_install() {
        let blockers = gate().blockers(false, u64::MAX, u64::MAX, false);
        assert_eq!(
            blockers,
            vec![InstallBlocker::LicenceNotAccepted {
                spdx_id: "MIT".to_owned()
            }]
        );
        assert!(blockers[0].explanation().contains("MIT"));
    }

    #[test]
    fn every_blocker_is_reported_at_once_not_one_at_a_time() {
        let blockers = gate().blockers(false, 10, 10, true);
        assert_eq!(blockers.len(), 4, "{blockers:?}");
        assert!(blockers.contains(&InstallBlocker::AlreadyInstalled));
        assert!(blockers.contains(&InstallBlocker::NotEnoughDisk {
            required_bytes: 1100,
            available_bytes: 10
        }));
        assert!(blockers.contains(&InstallBlocker::NotEnoughMemory {
            required_bytes: 2000,
            available_bytes: 10
        }));
    }

    #[test]
    fn a_satisfiable_install_has_no_blockers() {
        assert!(gate().blockers(true, 2000, 8000, false).is_empty());
    }

    #[test]
    fn a_download_only_licence_says_so() {
        let mut license = entry().license;
        license.redistributable = false;
        let disclosure = LicenceDisclosure::from_license(&license);
        assert!(disclosure.redistribution_note.contains("download-only"));
    }

    #[test]
    fn local_only_mode_makes_the_update_section_say_no_request_is_made() {
        let status = UpdateStatus {
            readiness: UpdateReadiness::Ready {
                installed_version: "0.1.0".to_owned(),
            },
            checks_suppressed: true,
            manifest_endpoint: "https://example.invalid/manifest".to_owned(),
            signing_key_id: "k1".to_owned(),
        };
        assert!(status.check_policy_copy().contains("no update request"));
        assert_eq!(status.installed_version_copy(), "0.1.0");
    }

    #[test]
    fn a_build_the_updater_did_not_install_says_so_instead_of_inventing_a_version() {
        let status = UpdateStatus {
            readiness: UpdateReadiness::NotInstalledByUpdater {
                detail: "no installation root".to_owned(),
            },
            checks_suppressed: false,
            manifest_endpoint: "https://example.invalid/manifest".to_owned(),
            signing_key_id: "k1".to_owned(),
        };
        assert_eq!(
            status.installed_version_copy(),
            "Not managed by the updater"
        );
        assert!(status.check_policy_copy().contains("only when you ask"));
    }

    fn engine(
        engine_id: &str,
        status: CapabilityStatus,
        stage: EngineStage,
    ) -> EngineInventoryEntry {
        EngineInventoryEntry {
            engine_id: engine_id.to_owned(),
            display_name: engine_id.to_owned(),
            stage,
            status,
            unavailable_reason: None,
            installed_models: vec![InstalledModel {
                model_id: "tiny".to_owned(),
                display_name: "tiny".to_owned(),
                size_bytes: 75 * 1024 * 1024,
                installed: true,
            }],
            download_bytes_required: None,
            requires_network: false,
        }
    }

    fn capabilities_with_engines(engines: Vec<EngineInventoryEntry>) -> Capabilities {
        Capabilities {
            os: crate::dto::OsInfo {
                platform: crate::dto::Platform::Macos,
                version: "26.0".to_owned(),
            },
            hardware: crate::dto::HardwareInfo {
                arch: "aarch64".to_owned(),
                logical_cores: 10,
                total_memory_bytes: 32 * 1024 * 1024 * 1024,
            },
            engines,
            storage: storage(),
            integrations: std::collections::BTreeMap::new(),
            permissions: std::collections::BTreeMap::new(),
            preferences: crate::backend::default_preferences(),
            openrouter_key_present: false,
            cloud_consents: Vec::new(),
        }
    }
}
