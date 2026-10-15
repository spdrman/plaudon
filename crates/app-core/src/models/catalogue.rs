//! The committed, checksum-pinned model catalogue (issue #303).
//!
//! The catalogue is a [`ModelManifest`] compiled into the binary from
//! `crates/app-core/catalogue/whisper-cpp.v1.json`. Every entry names the
//! exact size and SHA-256 of the weights at one pinned upstream revision, so a
//! download or an imported file is accepted only when it is byte-for-byte the
//! file the catalogue describes. Nothing here touches the network: the
//! catalogue is data, and fetching anything it names still goes through the
//! installer, the egress gate and a per-action grant.

use std::sync::LazyLock;

use super::manifest::{ModelEntry, ModelId, ModelManifest, MANIFEST_SCHEMA_VERSION};

/// The one host every catalogue download is requested from.
pub const CATALOGUE_HOST: &str = "huggingface.co";

/// The host suffix `huggingface.co` hands a file over from. Its resolver
/// answers a download with a redirect to a server under this domain, so a
/// grant for one download has to allow that one hop, and the disclosure says
/// so before anything is fetched.
pub const DELIVERY_HOST_SUFFIX: &str = "hf.co";

/// The upstream revision every catalogue URL is pinned to.
pub const CATALOGUE_REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

static COMMITTED: LazyLock<ModelManifest> = LazyLock::new(|| ModelManifest {
    schema_version: MANIFEST_SCHEMA_VERSION,
    models: Vec::new(),
});

/// The catalogue this build ships.
#[must_use]
pub fn committed() -> &'static ModelManifest {
    &COMMITTED
}

/// Look up one catalogue entry.
#[must_use]
pub fn find(id: &ModelId) -> Option<&'static ModelEntry> {
    committed().find(id)
}

/// Every entry for one engine, smallest download first.
#[must_use]
pub fn for_engine(engine: &str) -> Vec<&'static ModelEntry> {
    let mut entries: Vec<&ModelEntry> = committed()
        .models
        .iter()
        .filter(|entry| entry.engine == engine)
        .collect();
    entries.sort_by_key(|entry| (entry.size_bytes, entry.id.clone()));
    entries
}

/// The smallest download that makes `engine` usable, when the catalogue offers one.
#[must_use]
pub fn smallest_for_engine(engine: &str) -> Option<&'static ModelEntry> {
    for_engine(engine).into_iter().next()
}
