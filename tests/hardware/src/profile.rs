//! Named device profiles (`PLAUDON_HARDWARE_PROFILE`).
//!
//! A profile name selects exactly one cell of the committed
//! `compatibility.yaml` for the current host, so the evidence a run produces
//! describes a cell that exists and can be promoted. An unknown name grants
//! nothing (fail closed, spec §2.5B).

use plaudon_source_plaud_device::register::{CompatibilityCell, CompatibilityRegister};

/// A resolved profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfile {
    /// The name the operator gave.
    pub name: String,
    /// The register cell it selects.
    pub cell: CompatibilityCell,
}

/// Why a profile name was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The name is not `<source_type>:<model>:<auth_mode>`.
    #[error("the profile {0:?} is not of the form <source_type>:<model>:<auth_mode>")]
    Malformed(String),
    /// No register cell matches the name on this host.
    #[error("no compatibility cell matches the profile {name:?} on host {host_os}")]
    Unknown {
        /// The name.
        name: String,
        /// Host OS family.
        host_os: String,
    },
}

/// The canonical profile name of a cell.
#[must_use]
pub fn profile_name(cell: &CompatibilityCell) -> String {
    format!(
        "{}:{}:{}",
        cell.source_type,
        slug(&cell.device_model),
        cell.auth_mode
    )
}

fn slug(model: &str) -> String {
    model
        .trim()
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Resolve `name` for `host_os` against `register`.
pub fn resolve(
    name: &str,
    host_os: &str,
    register: &CompatibilityRegister,
) -> Result<ResolvedProfile, ProfileError> {
    if name.split(':').count() != 3 || name.split(':').any(str::is_empty) {
        return Err(ProfileError::Malformed(name.to_owned()));
    }
    let wanted = name.to_ascii_lowercase();
    register
        .cells
        .iter()
        .find(|cell| cell.host_os.eq_ignore_ascii_case(host_os) && profile_name(cell) == wanted)
        .map(|cell| ResolvedProfile {
            name: name.to_owned(),
            cell: cell.clone(),
        })
        .ok_or_else(|| ProfileError::Unknown {
            name: name.to_owned(),
            host_os: host_os.to_owned(),
        })
}
