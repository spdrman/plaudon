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

use super::manifest::{parse_manifest, ModelEntry, ModelId, ModelManifest, ModelSource};

/// The one host every catalogue download is requested from.
pub const CATALOGUE_HOST: &str = "huggingface.co";

/// The host suffix `huggingface.co` hands a file over from. Its resolver
/// answers a download with a redirect to a server under this domain, so a
/// grant for one download has to allow that one hop, and the disclosure says
/// so before anything is fetched.
pub const DELIVERY_HOST_SUFFIX: &str = "hf.co";

/// The upstream revision every catalogue URL is pinned to.
pub const CATALOGUE_REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

/// The catalogue document, compiled in so it cannot change after review.
const DOCUMENT: &[u8] = include_bytes!("../../catalogue/whisper-cpp.v1.json");

static COMMITTED: LazyLock<ModelManifest> = LazyLock::new(|| {
    // A malformed committed catalogue is a build defect, caught by this
    // crate's tests before anything ships; there is no runtime input here.
    parse_manifest(DOCUMENT).expect("the committed model catalogue is valid")
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

/// The host and URL path of a catalogue download, as the disclosure names
/// them: `("huggingface.co", "/ggerganov/whisper.cpp/resolve/<rev>/<file>")`.
///
/// `None` for an entry that is not a download.
#[must_use]
pub fn host_and_file(entry: &ModelEntry) -> Option<(String, String)> {
    let ModelSource::Remote { url } = &entry.source else {
        return None;
    };
    let rest = url.strip_prefix("https://")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    Some((host.to_ascii_lowercase(), path.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_and_file_split_a_catalogue_url_into_what_the_disclosure_names() {
        let entry = smallest_for_engine("whisper.cpp").expect("a whisper model");
        let (host, file) = host_and_file(entry).expect("a download");
        assert_eq!(host, CATALOGUE_HOST);
        assert_eq!(
            file,
            format!("/ggerganov/whisper.cpp/resolve/{CATALOGUE_REVISION}/ggml-tiny.bin")
        );
    }
}
