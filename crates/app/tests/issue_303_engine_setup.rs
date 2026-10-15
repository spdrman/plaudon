//! Issue #303: engine setup from first-run step 3 and Settings, end to end.
//!
//! Driven through [`AppEngine`] and its dispatcher, the path the egui shell
//! takes, so a command that validates in isolation but is wired to the wrong
//! port fails here. The download transport, the catalogue and the directories
//! searched for node/npm/plaud are substituted: everything else (Local-only
//! Mode, the per-action grant, the consent log, the installer, the registry)
//! is the production code.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use plaudon_app::backend::models::{ModelDownloadTransport, ModelSetupSeams};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    Capabilities, EngineInventoryEntry, ImportModelFileRequest, InstallModelDownloadRequest,
    InstallPlaudCliRequest, ModelOriginDto, SetLocalOnlyModeRequest,
};
use plaudon_app::notify::UnsupportedNotifier;
use plaudon_app::runtime::{AppEngine, EngineConfig};
use plaudon_app::secrets::InMemorySecretStore;
use plaudon_app_core::models::{
    HardwareRecommendation, ModelEntry, ModelId, ModelLicense, ModelManifest, ModelSource,
    TransferError, MANIFEST_SCHEMA_VERSION,
};
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_domain::media::Sha256Digest;
use plaudon_security::outbound::action::ActionEgressGrant;

const WEIGHTS: &[u8] = b"whisper weights the test catalogue pins by size and checksum";
const HOST: &str = "huggingface.co";
const FILE: &str =
    "/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-test.bin";

/* ------------------------------------------------------------- the seams */

/// A transport that counts every request and serves fixed bytes.
///
/// It also checks the grant it was handed, the way the real transport must:
/// a request the grant does not cover is a test failure, not a silent pass.
#[derive(Debug)]
struct CountingTransport {
    opens: AtomicUsize,
    bytes: Vec<u8>,
    urls: Mutex<Vec<String>>,
}

impl CountingTransport {
    fn serving(bytes: &[u8]) -> Arc<Self> {
        Arc::new(Self {
            opens: AtomicUsize::new(0),
            bytes: bytes.to_vec(),
            urls: Mutex::new(Vec::new()),
        })
    }

    fn opens(&self) -> usize {
        self.opens.load(Ordering::SeqCst)
    }
}

impl ModelDownloadTransport for CountingTransport {
    fn open(
        &self,
        grant: &ActionEgressGrant,
        url: &str,
        _resume_from: u64,
    ) -> Result<Box<dyn Read + Send>, TransferError> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        self.urls.lock().unwrap().push(url.to_owned());
        grant
            .authorize_url(url)
            .map_err(|denied| TransferError::Unavailable(denied.to_string()))?;
        Ok(Box::new(std::io::Cursor::new(self.bytes.clone())))
    }
}

fn test_entry() -> ModelEntry {
    ModelEntry {
        id: ModelId::parse("whisper-test").unwrap(),
        engine: "whisper.cpp".into(),
        version: "whisper-test".into(),
        display_name: "Whisper test model".into(),
        file_name: "ggml-test.bin".into(),
        size_bytes: WEIGHTS.len() as u64,
        sha256: Sha256Digest::of(WEIGHTS),
        languages: Vec::new(),
        license: ModelLicense {
            spdx_id: "MIT".into(),
            name: "MIT License".into(),
            redistributable: true,
            text_file: "MIT.txt".into(),
        },
        source: ModelSource::Remote {
            url: format!("https://{HOST}{FILE}"),
        },
        hardware: HardwareRecommendation {
            min_ram_bytes: 1,
            recommended_ram_bytes: 2,
            notes: "test".into(),
        },
    }
}

fn test_catalogue() -> ModelManifest {
    ModelManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        models: vec![test_entry()],
    }
}

struct Harness {
    engine: AppEngine,
    transport: Arc<CountingTransport>,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn with(transport: Arc<CountingTransport>, toolchain_dirs: Vec<PathBuf>) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let seams = ModelSetupSeams {
            transport: Some(Arc::clone(&transport) as Arc<dyn ModelDownloadTransport>),
            catalogue: Some(test_catalogue()),
            toolchain_dirs: Some(toolchain_dirs),
        };
        let engine = AppEngine::start(
            EngineConfig::new(dir.path().to_path_buf())
                .with_secrets(Arc::new(InMemorySecretStore::new()))
                .with_notifier(Arc::new(UnsupportedNotifier))
                .with_model_setup(seams)
                .with_worker_threads(2),
        )
        .expect("engine starts");
        Self {
            engine,
            transport,
            _dir: dir,
        }
    }

    fn start() -> Self {
        Self::with(CountingTransport::serving(WEIGHTS), Vec::new())
    }

    fn call(&self, command: AppCommand) -> AppResponse {
        self.engine
            .dispatcher()
            .call_blocking(command)
            .expect("command succeeds")
    }

    fn reject(&self, command: AppCommand) -> ErrorEnvelope {
        self.engine
            .dispatcher()
            .call_blocking(command)
            .expect_err("command is refused")
    }

    fn local_only(&self, enabled: bool) {
        self.call(AppCommand::SetLocalOnlyMode(SetLocalOnlyModeRequest {
            enabled,
            confirmed: true,
        }));
    }

    fn capabilities(&self) -> Capabilities {
        match self.call(AppCommand::GetCapabilities) {
            AppResponse::GetCapabilities(capabilities) => capabilities,
            other => panic!("unexpected response: {other:?}"),
        }
    }

    fn installed_ids(&self) -> Vec<String> {
        match self.call(AppCommand::ListLocalModels) {
            AppResponse::ListLocalModels(list) => list
                .models
                .into_iter()
                .map(|model| model.model_id)
                .collect(),
            other => panic!("unexpected response: {other:?}"),
        }
    }
}

fn download_request() -> InstallModelDownloadRequest {
    InstallModelDownloadRequest {
        model_id: "whisper-test".to_owned(),
        licence_accepted_spdx_id: "MIT".to_owned(),
        footprint_acknowledged: true,
        approved_host: HOST.to_owned(),
        approved_file: FILE.to_owned(),
        keep_offline: false,
    }
}

fn whisper_row(capabilities: &Capabilities) -> &EngineInventoryEntry {
    capabilities
        .engines
        .iter()
        .find(|engine| engine.engine_id == "whisper.cpp")
        .expect("the whisper.cpp row")
}

/* --------------------------------------------------------- local-only */

#[test]
fn issue_303_local_only_mode_refuses_a_download_and_makes_no_request() {
    let harness = Harness::start();
    harness.local_only(true);

    let envelope = harness.reject(AppCommand::InstallModelDownload(download_request()));

    assert_eq!(envelope.code, AppErrorCode::LocalOnlyMode, "{envelope:?}");
    assert_eq!(
        harness.transport.opens(),
        0,
        "Local-only Mode made a request"
    );
    assert!(harness.installed_ids().is_empty());
    assert!(
        harness.capabilities().preferences.local_only_mode,
        "a refused download must never turn Local-only Mode off"
    );
    assert!(
        harness.capabilities().cloud_consents.is_empty(),
        "nothing was approved, so nothing may be recorded as a grant"
    );
}

#[test]
fn issue_303_local_only_mode_refuses_the_guided_cli_install_without_launching_anything() {
    let harness = Harness::start();
    harness.local_only(true);
    let envelope = harness.reject(AppCommand::InstallPlaudCli(InstallPlaudCliRequest {
        approved_host: "registry.npmjs.org".to_owned(),
        approved_package: "@plaud-ai/cli@0.3.15".to_owned(),
    }));
    assert_eq!(envelope.code, AppErrorCode::LocalOnlyMode, "{envelope:?}");
    assert!(harness.capabilities().cloud_consents.is_empty());
}

/* --------------------------------------------------- per-action approval */

#[test]
fn issue_303_a_download_whose_approval_names_another_file_is_refused_before_any_request() {
    let harness = Harness::start();
    harness.local_only(false);

    let mut wrong_file = download_request();
    wrong_file.approved_file = "/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin".into();
    let envelope = harness.reject(AppCommand::InstallModelDownload(wrong_file));
    assert_eq!(envelope.code, AppErrorCode::EgressDenied, "{envelope:?}");

    let mut wrong_host = download_request();
    wrong_host.approved_host = "example.org".into();
    let envelope = harness.reject(AppCommand::InstallModelDownload(wrong_host));
    assert_eq!(envelope.code, AppErrorCode::EgressDenied, "{envelope:?}");

    assert_eq!(harness.transport.opens(), 0);
    assert!(harness.installed_ids().is_empty());
}

#[test]
fn issue_303_a_download_without_the_licence_or_the_footprint_acknowledgement_is_refused() {
    let harness = Harness::start();
    harness.local_only(false);

    let mut no_licence = download_request();
    no_licence.licence_accepted_spdx_id = String::new();
    let envelope = harness.reject(AppCommand::InstallModelDownload(no_licence));
    assert_eq!(envelope.code, AppErrorCode::InvalidArgument, "{envelope:?}");

    let mut no_footprint = download_request();
    no_footprint.footprint_acknowledged = false;
    let envelope = harness.reject(AppCommand::InstallModelDownload(no_footprint));
    assert_eq!(envelope.code, AppErrorCode::InvalidArgument, "{envelope:?}");

    assert_eq!(harness.transport.opens(), 0);
}

#[test]
fn issue_303_an_approved_download_installs_the_model_and_logs_a_grant_revoked_when_it_ends() {
    let harness = Harness::start();
    harness.local_only(false);

    let AppResponse::InstallModelDownload(response) =
        harness.call(AppCommand::InstallModelDownload(download_request()))
    else {
        panic!("wrong response");
    };

    assert_eq!(response.model.model_id, "whisper-test");
    assert_eq!(response.model.origin, ModelOriginDto::Download);
    assert_eq!(harness.transport.opens(), 1);
    assert_eq!(
        harness.transport.urls.lock().unwrap().as_slice(),
        [format!("https://{HOST}{FILE}")],
        "exactly the approved file was requested"
    );
    assert_eq!(harness.installed_ids(), ["whisper-test"]);

    let capabilities = harness.capabilities();
    let grant = capabilities
        .cloud_consents
        .iter()
        .find(|grant| grant.purpose == "model_download")
        .expect("the download is in the consent log");
    assert_eq!(grant.model_id, FILE, "the grant names the one file");
    assert_eq!(
        grant.content_type, "metadata_only",
        "a download sends no user content"
    );
    assert!(
        grant.revoked_at.is_some(),
        "the grant must be revoked as soon as the download ends"
    );
    assert!(
        !capabilities.preferences.local_only_mode,
        "the user's Local-only choice is untouched"
    );

    // The whisper engine scans <models>/whisper.cpp/<model>/*.bin, so a
    // downloaded model is an installed one as far as the inventory is concerned.
    let whisper = whisper_row(&capabilities);
    assert!(
        whisper
            .installed_models
            .iter()
            .any(|model| model.model_id == "whisper-test" && model.installed),
        "{whisper:?}"
    );
}

#[test]
fn issue_303_a_download_whose_bytes_do_not_match_the_checksum_is_refused_and_removed() {
    let tampered: Vec<u8> = WEIGHTS.iter().map(|byte| byte ^ 0x20).collect();
    let harness = Harness::with(CountingTransport::serving(&tampered), Vec::new());
    harness.local_only(false);

    let envelope = harness.reject(AppCommand::InstallModelDownload(download_request()));

    assert_eq!(envelope.code, AppErrorCode::IntegrityFailed, "{envelope:?}");
    assert!(harness.installed_ids().is_empty());
    let AppResponse::ListLocalModels(list) = harness.call(AppCommand::ListLocalModels) else {
        panic!("wrong response");
    };
    assert_eq!(
        list.disk.staging_bytes, 0,
        "the refused file was left behind"
    );
    let grant = harness
        .capabilities()
        .cloud_consents
        .into_iter()
        .find(|grant| grant.purpose == "model_download")
        .expect("the attempt is still in the consent log");
    assert!(
        grant.revoked_at.is_some(),
        "a failed download still ends its grant"
    );
}

#[test]
fn issue_303_cancelling_when_nothing_is_downloading_reports_that_nothing_was_cancelled() {
    let harness = Harness::start();
    let AppResponse::CancelModelInstall(response) = harness.call(AppCommand::CancelModelInstall(
        plaudon_app::dto::CancelModelInstallRequest {
            model_id: "whisper-test".to_owned(),
        },
    )) else {
        panic!("wrong response");
    };
    assert!(!response.cancelled);
}

/* -------------------------------------------------------------- import */

#[test]
fn issue_303_importing_a_model_file_works_with_local_only_mode_on() {
    let harness = Harness::start();
    harness.local_only(true);
    let downloads = tempfile::tempdir().unwrap();
    let file = downloads.path().join("my-copy-of-the-model.bin");
    std::fs::write(&file, WEIGHTS).unwrap();

    let AppResponse::ImportModelFile(response) =
        harness.call(AppCommand::ImportModelFile(ImportModelFileRequest {
            path: file.to_string_lossy().into_owned(),
            model_id: None,
        }))
    else {
        panic!("wrong response");
    };

    assert_eq!(response.model.model_id, "whisper-test");
    assert_eq!(response.model.origin, ModelOriginDto::ImportedFile);
    assert_eq!(harness.transport.opens(), 0, "an import reaches no network");
    assert!(harness.capabilities().preferences.local_only_mode);
    assert_eq!(harness.installed_ids(), ["whisper-test"]);
}

#[test]
fn issue_303_importing_a_file_with_the_wrong_checksum_is_refused() {
    let harness = Harness::start();
    let downloads = tempfile::tempdir().unwrap();
    let file = downloads.path().join("not-the-model.bin");
    let tampered: Vec<u8> = WEIGHTS.iter().map(|byte| byte ^ 0x01).collect();
    std::fs::write(&file, &tampered).unwrap();

    let envelope = harness.reject(AppCommand::ImportModelFile(ImportModelFileRequest {
        path: file.to_string_lossy().into_owned(),
        model_id: None,
    }));
    assert_eq!(envelope.code, AppErrorCode::IntegrityFailed, "{envelope:?}");
    assert!(harness.installed_ids().is_empty());
}

/* ---------------------------------------------------------- PLAUD CLI */

/// Lay out a fake npm global prefix: `<prefix>/bin/{node,npm,plaud}` and the
/// installed package's manifest where npm puts it.
fn fake_prefix(root: &Path, programs: &[&str], plaud_version: Option<&str>) -> PathBuf {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for program in programs {
        let path = bin.join(program);
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    if let Some(version) = plaud_version {
        let package = root.join("lib/node_modules/@plaud-ai/cli");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("package.json"),
            format!(r#"{{"name":"@plaud-ai/cli","version":"{version}"}}"#),
        )
        .unwrap();
    }
    bin
}

fn detect(harness: &Harness) -> plaudon_app::dto::PlaudToolchainStatus {
    match harness.call(AppCommand::DetectPlaudToolchain) {
        AppResponse::DetectPlaudToolchain(status) => status,
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn issue_303_detection_reports_exactly_what_is_missing_and_the_copyable_commands() {
    let empty = tempfile::tempdir().unwrap();
    let harness = Harness::with(
        CountingTransport::serving(WEIGHTS),
        vec![empty.path().to_path_buf()],
    );
    harness.local_only(true);

    let status = detect(&harness);

    assert_eq!(status.missing, ["node", "npm", "plaud"]);
    assert_eq!(status.node.path, None);
    assert_eq!(status.plaud_version, None);
    assert_eq!(status.release_exercised, None);
    assert_eq!(status.exercised_release, "0.3.15");
    assert_eq!(
        status.install_command,
        "npm install --global --registry=https://registry.npmjs.org/ @plaud-ai/cli@0.3.15"
    );
    assert_eq!(status.login_command, "plaud login");
    assert!(!status.guided_install_available, "there is no npm to run");
    assert!(
        status.summary.contains("Node.js") && status.summary.contains("npm"),
        "{}",
        status.summary
    );
    // Detection reads the filesystem only, so Local-only Mode does not stop it.
    assert!(harness.capabilities().preferences.local_only_mode);
}

#[test]
fn issue_303_detection_finds_the_toolchain_and_says_whether_the_release_was_exercised() {
    let prefix = tempfile::tempdir().unwrap();
    let bin = fake_prefix(prefix.path(), &["node", "npm", "plaud"], Some("0.3.15"));
    let harness = Harness::with(CountingTransport::serving(WEIGHTS), vec![bin.clone()]);

    let status = detect(&harness);

    assert!(status.missing.is_empty(), "{:?}", status.missing);
    assert_eq!(
        status.plaud.path.as_deref(),
        Some(bin.join("plaud").to_string_lossy().as_ref())
    );
    assert_eq!(status.plaud_version.as_deref(), Some("0.3.15"));
    assert_eq!(status.release_exercised, Some(true));
    #[cfg(unix)]
    assert!(status.guided_install_available);
}

#[test]
fn issue_303_detection_flags_a_plaud_release_this_build_has_not_exercised() {
    let prefix = tempfile::tempdir().unwrap();
    let bin = fake_prefix(prefix.path(), &["node", "npm", "plaud"], Some("0.4.0"));
    let harness = Harness::with(CountingTransport::serving(WEIGHTS), vec![bin]);

    let status = detect(&harness);

    assert_eq!(status.plaud_version.as_deref(), Some("0.4.0"));
    assert_eq!(status.release_exercised, Some(false));
    assert!(status.summary.contains("0.3.15"), "{}", status.summary);
}

#[test]
fn issue_303_detection_reports_plaud_alone_missing_when_node_and_npm_are_present() {
    let prefix = tempfile::tempdir().unwrap();
    let bin = fake_prefix(prefix.path(), &["node", "npm"], None);
    let harness = Harness::with(CountingTransport::serving(WEIGHTS), vec![bin]);

    let status = detect(&harness);

    assert_eq!(status.missing, ["plaud"]);
}

#[test]
fn issue_303_an_install_approval_for_another_package_is_refused() {
    let prefix = tempfile::tempdir().unwrap();
    let bin = fake_prefix(prefix.path(), &["node", "npm"], None);
    let harness = Harness::with(CountingTransport::serving(WEIGHTS), vec![bin]);
    harness.local_only(false);

    let envelope = harness.reject(AppCommand::InstallPlaudCli(InstallPlaudCliRequest {
        approved_host: "registry.npmjs.org".to_owned(),
        approved_package: "@plaud-ai/cli@latest".to_owned(),
    }));
    assert_eq!(envelope.code, AppErrorCode::EgressDenied, "{envelope:?}");
    assert!(harness.capabilities().cloud_consents.is_empty());
}

/* -------------------------------------------- the inventory, issue #305 */

#[test]
fn issue_305_the_engine_inventory_reports_the_catalogue_download_size_and_offers() {
    // The committed catalogue, not the test one.
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = AppEngine::start(
        EngineConfig::new(dir.path().to_path_buf())
            .with_secrets(Arc::new(InMemorySecretStore::new()))
            .with_notifier(Arc::new(UnsupportedNotifier))
            .with_worker_threads(2),
    )
    .expect("engine starts");
    let AppResponse::GetCapabilities(capabilities) = engine
        .dispatcher()
        .call_blocking(AppCommand::GetCapabilities)
        .expect("capabilities")
    else {
        panic!("wrong response");
    };

    let whisper = whisper_row(&capabilities);
    assert_eq!(
        whisper.download_bytes_required,
        Some(77_691_713),
        "the smallest catalogue download, not 'unknown'"
    );
    assert!(whisper.catalogue.len() >= 3, "{:?}", whisper.catalogue);
    for offer in &whisper.catalogue {
        assert_eq!(offer.host, "huggingface.co");
        assert!(offer.file.ends_with(".bin"), "{}", offer.file);
        assert_eq!(offer.licence_spdx_id, "MIT");
        assert!(offer.size_bytes > 0);
    }
    // An engine with nothing to download offers nothing.
    for engine in capabilities
        .engines
        .iter()
        .filter(|engine| engine.engine_id != "whisper.cpp")
    {
        assert!(
            engine.catalogue.is_empty(),
            "{} offers a download",
            engine.engine_id
        );
    }
}
