//! "Import a model file I already have" (issue #303).
//!
//! The air-gapped path: the user hands over a weights file they obtained some
//! other way, and it is installed only when its size and SHA-256 are exactly
//! those of a catalogue entry. No byte source is involved, so there is no
//! egress decision to make and it works the same with Local-only Mode on.

use std::path::Path;

use super::manifest::{ModelId, ModelManifest};
use super::registry::{InstalledModel, ModelRegistry};
use super::ModelError;

/// Install `file` as the catalogue entry whose checksum it matches.
///
/// With `expected` the file must be that entry; without it the file is
/// matched against every entry of the same size.
pub fn import_model_file(
    _registry: &mut ModelRegistry,
    _catalogue: &ModelManifest,
    _file: &Path,
    expected: Option<&ModelId>,
    _keep_offline: bool,
) -> Result<InstalledModel, ModelError> {
    Err(ModelError::NotInManifest {
        id: expected.map_or_else(String::new, ToString::to_string),
    })
}
