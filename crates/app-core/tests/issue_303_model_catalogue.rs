//! Issue #303: the model catalogue, a cancellable verified download and the
//! air-gapped "import a model file I already have" path.
//!
//! Every test here is about what reaches the model root: a catalogue entry
//! that is not pinned, a cancelled transfer that leaves a partial file, or an
//! imported file that was never checked against the catalogue would each put
//! something on disk the user did not agree to.

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use plaudon_app_core::models::catalogue;
use plaudon_app_core::models::{
    import_model_file, ExplicitApprovalFetchPolicy, HardwareRecommendation, InstallOrigin,
    InstallRequest, ModelByteSource, ModelEntry, ModelError, ModelId, ModelInstaller, ModelLicense,
    ModelManifest, ModelRegistry, ModelSource, NullInstallObserver, SystemResources, TransferError,
    MANIFEST_SCHEMA_VERSION,
};
use plaudon_domain::media::Sha256Digest;

/* ------------------------------------------------------------ catalogue */

#[test]
fn issue_303_the_committed_catalogue_offers_whisper_models() {
    let whisper = catalogue::for_engine("whisper.cpp");
    assert!(
        whisper.len() >= 3,
        "the catalogue must offer a choice of whisper.cpp models, found {}",
        whisper.len()
    );
    let ids: Vec<&str> = whisper.iter().map(|entry| entry.id.as_str()).collect();
    for wanted in ["whisper-tiny-en", "whisper-base-en", "whisper-small-en"] {
        assert!(ids.contains(&wanted), "{wanted} is missing from {ids:?}");
    }
}

#[test]
fn issue_303_every_catalogue_entry_is_checksum_pinned_to_one_revision() {
    let manifest = catalogue::committed();
    assert!(!manifest.models.is_empty(), "the catalogue is empty");
    plaudon_app_core::models::validate_manifest(manifest).expect("the catalogue validates");
    assert_eq!(catalogue::CATALOGUE_REVISION.len(), 40);
    for entry in &manifest.models {
        let ModelSource::Remote { url } = &entry.source else {
            panic!("{} is not a download", entry.id);
        };
        let expected = format!(
            "https://{}/ggerganov/whisper.cpp/resolve/{}/{}",
            catalogue::CATALOGUE_HOST,
            catalogue::CATALOGUE_REVISION,
            entry.file_name
        );
        assert_eq!(url, &expected, "{} is not pinned to the revision", entry.id);
        assert_ne!(
            entry.sha256,
            Sha256Digest::from_bytes([0; 32]),
            "{} has no checksum",
            entry.id
        );
        // The version is the directory the whisper engine scans, so it must
        // be unique per model or two installs would share one folder.
        assert_eq!(entry.version, entry.id.as_str());
    }
}

#[test]
fn issue_303_catalogue_sizes_and_checksums_are_the_real_upstream_figures() {
    // Taken from the Hugging Face LFS pointers at the pinned revision.
    let tiny = catalogue::find(&ModelId::parse("whisper-tiny-en").unwrap()).expect("tiny.en");
    assert_eq!(tiny.size_bytes, 77_704_715);
    assert_eq!(
        tiny.sha256.to_hex(),
        "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f"
    );
    let small = catalogue::find(&ModelId::parse("whisper-small-en").unwrap()).expect("small.en");
    assert_eq!(small.size_bytes, 487_614_201);
    assert_eq!(
        small.sha256.to_hex(),
        "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d"
    );
    for entry in &catalogue::committed().models {
        assert!(
            entry.size_bytes > 10 * 1024 * 1024,
            "{} declares an implausible size",
            entry.id
        );
    }
}

#[test]
fn issue_303_every_catalogue_entry_states_its_licence() {
    for entry in &catalogue::committed().models {
        assert_eq!(entry.license.spdx_id, "MIT", "{}", entry.id);
        assert_eq!(entry.license.name, "MIT License", "{}", entry.id);
        assert!(!entry.hardware.notes.is_empty(), "{}", entry.id);
        assert!(entry.hardware.min_ram_bytes > 0, "{}", entry.id);
    }
}

#[test]
fn issue_303_the_smallest_whisper_download_is_the_tiny_model() {
    let smallest = catalogue::smallest_for_engine("whisper.cpp").expect("a whisper model");
    assert_eq!(smallest.size_bytes, 77_691_713, "{}", smallest.id);
    assert!(catalogue::smallest_for_engine("apple.speech").is_none());
}

/* ------------------------------------------------------------- cancel */

const WEIGHTS: &[u8] = b"pretend these are whisper weights, long enough to chunk";

fn entry_for(bytes: &[u8], id: &str, file: &str) -> ModelEntry {
    ModelEntry {
        id: ModelId::parse(id).unwrap(),
        engine: "whisper.cpp".into(),
        version: id.into(),
        display_name: format!("Test {id}"),
        file_name: file.into(),
        size_bytes: bytes.len() as u64,
        sha256: Sha256Digest::of(bytes),
        languages: Vec::new(),
        license: ModelLicense {
            spdx_id: "MIT".into(),
            name: "MIT License".into(),
            redistributable: true,
            text_file: "MIT.txt".into(),
        },
        source: ModelSource::Remote {
            url: format!("https://huggingface.co/example/{file}"),
        },
        hardware: HardwareRecommendation {
            min_ram_bytes: 1,
            recommended_ram_bytes: 2,
            notes: "test".into(),
        },
    }
}

#[derive(Debug, Clone, Copy)]
struct Plenty;

impl SystemResources for Plenty {
    fn available_disk_bytes(&self, _path: &std::path::Path) -> Result<u64, ModelError> {
        Ok(1 << 40)
    }
    fn total_ram_bytes(&self) -> Result<u64, ModelError> {
        Ok(1 << 34)
    }
}

/// Yields the weights one byte at a time and raises the cancel flag after the
/// first few, the way a user pressing "Cancel download" mid-transfer would.
struct CancellingReader {
    bytes: Vec<u8>,
    position: usize,
    cancel: Arc<AtomicBool>,
    cancel_after: usize,
}

impl Read for CancellingReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.bytes.len() || buf.is_empty() {
            return Ok(0);
        }
        buf[0] = self.bytes[self.position];
        self.position += 1;
        if self.position == self.cancel_after {
            self.cancel.store(true, Ordering::SeqCst);
        }
        Ok(1)
    }
}

struct CancellingSource {
    cancel: Arc<AtomicBool>,
    opens: AtomicUsize,
}

impl ModelByteSource for CancellingSource {
    fn open(
        &self,
        _source: &ModelSource,
        _resume_from: u64,
    ) -> Result<Box<dyn Read + Send>, TransferError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CancellingReader {
            bytes: WEIGHTS.to_vec(),
            position: 0,
            cancel: Arc::clone(&self.cancel),
            cancel_after: 5,
        }))
    }
    fn supports_resume(&self, _source: &ModelSource) -> bool {
        true
    }
}

#[test]
fn issue_303_a_cancelled_download_stops_and_removes_the_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ModelRegistry::open(dir.path()).unwrap();
    let entry = entry_for(WEIGHTS, "test-model", "ggml-test.bin");
    let cancel = Arc::new(AtomicBool::new(false));
    let source = CancellingSource {
        cancel: Arc::clone(&cancel),
        opens: AtomicUsize::new(0),
    };
    let policy = ExplicitApprovalFetchPolicy::default().approving(&entry.id);
    let installer = ModelInstaller::new(&policy, &source, &Plenty, &NullInstallObserver)
        .with_cancellation(&cancel);

    let error = installer
        .install(
            &mut registry,
            &entry,
            &InstallRequest::new(entry.id.clone()),
        )
        .expect_err("a cancelled download must not install");

    assert!(matches!(error, ModelError::Cancelled), "{error}");
    assert_eq!(
        registry.installed().len(),
        0,
        "a cancelled download installed"
    );
    assert_eq!(
        registry.disk_usage().staging_bytes,
        0,
        "the partial file of a cancelled download was left behind"
    );
    assert!(
        !dir.path()
            .join("whisper.cpp/test-model/ggml-test.bin")
            .exists(),
        "a cancelled download reached the install directory"
    );
}

/* ------------------------------------------------------------- import */

fn catalogue_of(entries: Vec<ModelEntry>) -> ModelManifest {
    ModelManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        models: entries,
    }
}

#[test]
fn issue_303_importing_a_file_that_matches_the_catalogue_installs_it() {
    let models = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let mut registry = ModelRegistry::open(models.path()).unwrap();
    let other = b"some other model, same engine, different bytes";
    let catalogue = catalogue_of(vec![
        entry_for(other, "other-model", "ggml-other.bin"),
        entry_for(WEIGHTS, "test-model", "ggml-test.bin"),
    ]);
    let file = downloads.path().join("whatever-the-user-named-it.bin");
    std::fs::write(&file, WEIGHTS).unwrap();

    let installed = import_model_file(&mut registry, &catalogue, &file, None, true)
        .expect("a matching file is imported");

    assert_eq!(installed.id.as_str(), "test-model");
    assert_eq!(installed.installed_from, InstallOrigin::ImportedFile);
    assert!(installed.keep_offline);
    // Where the whisper engine looks: <root>/whisper.cpp/<model>/<weights>.
    let target = models.path().join("whisper.cpp/test-model/ggml-test.bin");
    assert_eq!(std::fs::read(&target).unwrap(), WEIGHTS);
    assert!(registry.verify(&installed.id).unwrap().intact);
    // The user's own file is copied, never moved or altered.
    assert_eq!(std::fs::read(&file).unwrap(), WEIGHTS);
    assert_eq!(registry.disk_usage().staging_bytes, 0);
}

#[test]
fn issue_303_importing_a_file_that_matches_nothing_is_refused_and_leaves_nothing() {
    let models = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let mut registry = ModelRegistry::open(models.path()).unwrap();
    let catalogue = catalogue_of(vec![entry_for(WEIGHTS, "test-model", "ggml-test.bin")]);
    // Same length as the catalogue entry, different content: only the
    // checksum can tell them apart.
    let tampered: Vec<u8> = WEIGHTS.iter().map(|byte| byte ^ 0x20).collect();
    let file = downloads.path().join("ggml-test.bin");
    std::fs::write(&file, &tampered).unwrap();

    let error = import_model_file(&mut registry, &catalogue, &file, None, false)
        .expect_err("a file with the wrong checksum must be refused");

    assert!(
        matches!(error, ModelError::NoCatalogueMatch { .. }),
        "{error}"
    );
    assert_eq!(registry.installed().len(), 0);
    assert_eq!(
        registry.disk_usage().staging_bytes,
        0,
        "the copy was left behind"
    );
    assert!(!models.path().join("whisper.cpp/test-model").exists());
    assert_eq!(
        std::fs::read(&file).unwrap(),
        tampered,
        "the user's file was touched"
    );
}

#[test]
fn issue_303_importing_for_a_named_model_checks_that_models_checksum() {
    let models = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let mut registry = ModelRegistry::open(models.path()).unwrap();
    let catalogue = catalogue_of(vec![entry_for(WEIGHTS, "test-model", "ggml-test.bin")]);
    let tampered: Vec<u8> = WEIGHTS.iter().map(|byte| byte ^ 0x01).collect();
    let file = downloads.path().join("ggml-test.bin");
    std::fs::write(&file, &tampered).unwrap();
    let id = ModelId::parse("test-model").unwrap();

    let error = import_model_file(&mut registry, &catalogue, &file, Some(&id), false)
        .expect_err("the named model's checksum must match");
    assert!(
        matches!(error, ModelError::ChecksumMismatch { .. }),
        "{error}"
    );
    assert_eq!(registry.installed().len(), 0);
    assert_eq!(registry.disk_usage().staging_bytes, 0);

    std::fs::write(&file, WEIGHTS).unwrap();
    let installed = import_model_file(&mut registry, &catalogue, &file, Some(&id), false)
        .expect("the right bytes import");
    assert_eq!(installed.id, id);
}

#[test]
fn issue_303_importing_a_file_of_the_wrong_size_for_a_named_model_is_refused() {
    let models = tempfile::tempdir().unwrap();
    let downloads = tempfile::tempdir().unwrap();
    let mut registry = ModelRegistry::open(models.path()).unwrap();
    let catalogue = catalogue_of(vec![entry_for(WEIGHTS, "test-model", "ggml-test.bin")]);
    let file = downloads.path().join("short.bin");
    std::fs::write(&file, &WEIGHTS[..10]).unwrap();
    let id = ModelId::parse("test-model").unwrap();

    let error = import_model_file(&mut registry, &catalogue, &file, Some(&id), false)
        .expect_err("a truncated file must be refused");
    assert!(matches!(error, ModelError::SizeMismatch { .. }), "{error}");
    assert_eq!(registry.installed().len(), 0);
}
