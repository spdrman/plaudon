//! Engine setup helpers behind the models port (issue #303).
//!
//! Four things live here so `backend::models` stays about the registry:
//!
//! * the HTTPS transport for an approved catalogue download, which asks the
//!   per-action grant before it connects and before it follows each redirect;
//! * the adapters that let `plaudon-app-core`'s installer consult that grant
//!   as its fetch policy and byte source, so the installer's own "policy
//!   first, zero requests on refusal" rule is the one enforced;
//! * detection of node, npm and the PLAUD CLI, which reads the filesystem and
//!   launches nothing, so it works the same with Local-only Mode on;
//! * the catalogue offers the engine inventory reports.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use plaudon_app_core::models::catalogue;
use plaudon_app_core::models::{
    FetchRefused, ModelByteSource, ModelFetchPolicy, ModelId, ModelManifest, ModelSource,
    TransferError,
};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_security::outbound::action::ActionEgressGrant;
use plaudon_security::outbound::subprocess::{cli_install_command_text, GatedCommand, NPM_PROGRAM};
use plaudon_security::outbound::{DenyReason, EgressDenied};

use crate::dto::{CatalogueOfferDto, PlaudToolchainStatus, ToolPresence};

use super::models::ModelDownloadTransport;

/// The release of `@plaud-ai/cli` this build has exercised end to end.
pub const PLAUD_CLI_EXERCISED_RELEASE: &str = "0.3.15";

/// The sign-in command, as text the user can copy.
pub const PLAUD_LOGIN_COMMAND: &str = "plaud login";

/// The most redirects a download follows before it gives up.
const MAX_REDIRECTS: usize = 5;

/* ------------------------------------------------------------- errors */

/// The envelope for a refusal from the per-action grant or the gate.
///
/// Local-only Mode keeps its own code, so the view can say "turn Local-only
/// Mode off" rather than something vaguer; every other refusal is an egress
/// denial.
pub(crate) fn egress_error(denied: &EgressDenied) -> AppError {
    let code = if denied.reason == DenyReason::LocalOnlyMode {
        AppErrorCode::LocalOnlyMode
    } else {
        AppErrorCode::EgressDenied
    };
    AppError::new(code, format!("egress refused: {}", denied.reason.label()))
}

/* -------------------------------------------------- installer adapters */

/// The installer's fetch policy, answered by the per-action grant.
pub(crate) struct GrantFetchPolicy(pub(crate) Arc<ActionEgressGrant>);

impl ModelFetchPolicy for GrantFetchPolicy {
    fn authorize(&self, id: &ModelId, source: &ModelSource) -> Result<(), FetchRefused> {
        match source {
            ModelSource::Remote { url } => self
                .0
                .authorize_url(url)
                .map(|_| ())
                .map_err(|_| FetchRefused::NotApproved { id: id.to_string() }),
            ModelSource::Pack { .. } => Ok(()),
        }
    }
}

/// The installer's byte source: the transport, handed the grant.
pub(crate) struct GrantByteSource {
    pub(crate) grant: Arc<ActionEgressGrant>,
    pub(crate) transport: Arc<dyn ModelDownloadTransport>,
}

impl ModelByteSource for GrantByteSource {
    fn open(
        &self,
        source: &ModelSource,
        resume_from: u64,
    ) -> Result<Box<dyn Read + Send>, TransferError> {
        match source {
            ModelSource::Remote { url } => self.transport.open(&self.grant, url, resume_from),
            ModelSource::Pack { .. } => Err(TransferError::Unavailable(
                "a pack entry is not a download".to_owned(),
            )),
        }
    }

    fn supports_resume(&self, _source: &ModelSource) -> bool {
        // A fresh grant per attempt, and a partial file is never kept across
        // attempts, so there is nothing to resume.
        false
    }
}

/* ------------------------------------------------------- the transport */

/// HTTPS for approved catalogue downloads.
///
/// Redirects are never followed by the client: each hop is put to the grant
/// first, so a server that sends the transfer somewhere the disclosure did not
/// name is refused before a byte is requested from it.
#[derive(Debug, Default)]
pub struct HttpsModelTransport {
    client: std::sync::OnceLock<Result<reqwest::Client, String>>,
}

impl HttpsModelTransport {
    /// A transport that builds its client on first use.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn client(&self) -> Result<&reqwest::Client, TransferError> {
        self.client
            .get_or_init(|| {
                reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .https_only(true)
                    .connect_timeout(Duration::from_secs(30))
                    .build()
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| TransferError::Unavailable(error.clone()))
    }
}

impl ModelDownloadTransport for HttpsModelTransport {
    fn open(
        &self,
        grant: &ActionEgressGrant,
        url: &str,
        resume_from: u64,
    ) -> Result<Box<dyn Read + Send>, TransferError> {
        let refused = |denied: EgressDenied| TransferError::Unavailable(denied.to_string());
        grant.authorize_url(url).map_err(refused)?;
        let handle = tokio::runtime::Handle::try_current()
            .map_err(|_| TransferError::Unavailable("no async runtime".to_owned()))?;
        let client = self.client()?.clone();
        let mut current = reqwest::Url::parse(url)
            .map_err(|_| TransferError::Unavailable("the URL is not valid".to_owned()))?;
        for _ in 0..=MAX_REDIRECTS {
            let mut request = client.get(current.clone());
            if resume_from > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
            }
            let response = handle
                .block_on(request.send())
                // Without the URL: a signed redirect target carries a query
                // that must not reach a log line.
                .map_err(|error| TransferError::Unavailable(error.without_url().to_string()))?;
            let status = response.status();
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        TransferError::Unavailable("a redirect named no location".to_owned())
                    })?;
                let next = current.join(location).map_err(|_| {
                    TransferError::Unavailable("a redirect named an invalid location".to_owned())
                })?;
                grant.authorize_redirect(next.as_str()).map_err(refused)?;
                current = next;
                continue;
            }
            if !status.is_success() {
                return Err(TransferError::Unavailable(format!(
                    "the server answered {status}"
                )));
            }
            return Ok(Box::new(ResponseReader {
                handle,
                response,
                pending: Vec::new(),
                offset: 0,
                received: resume_from,
            }));
        }
        Err(TransferError::Unavailable(
            "the server redirected too many times".to_owned(),
        ))
    }
}

/// A response body read synchronously, one chunk at a time.
struct ResponseReader {
    handle: tokio::runtime::Handle,
    response: reqwest::Response,
    /// The unread rest of the last body chunk.
    pending: Vec<u8>,
    offset: usize,
    received: u64,
}

impl Read for ResponseReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.offset >= self.pending.len() {
            match self.handle.block_on(self.response.chunk()) {
                Ok(Some(chunk)) => {
                    self.pending = chunk.to_vec();
                    self.offset = 0;
                }
                Ok(None) => return Ok(0),
                Err(error) => {
                    return Err(std::io::Error::other(format!(
                        "interrupted after {} bytes: {}",
                        self.received,
                        error.without_url()
                    )))
                }
            }
        }
        let available = &self.pending[self.offset..];
        let count = available.len().min(buf.len());
        buf[..count].copy_from_slice(&available[..count]);
        self.offset += count;
        self.received += count as u64;
        Ok(count)
    }
}

/* ---------------------------------------------------- the catalogue */

/// What the catalogue offers for `engine`, smallest first, with each entry's
/// host and file spelled out for the disclosure.
pub(crate) fn offers_for(
    manifest: &ModelManifest,
    engine: &str,
    installed: &BTreeSet<String>,
) -> Vec<CatalogueOfferDto> {
    let mut entries: Vec<_> = manifest
        .models
        .iter()
        .filter(|entry| entry.engine == engine)
        .collect();
    entries.sort_by_key(|entry| (entry.size_bytes, entry.id.clone()));
    entries
        .into_iter()
        .filter_map(|entry| {
            let (host, file) = catalogue::host_and_file(entry)?;
            let delivery_host_suffix = (host == catalogue::CATALOGUE_HOST)
                .then(|| catalogue::DELIVERY_HOST_SUFFIX.to_owned());
            Some(CatalogueOfferDto {
                model_id: entry.id.to_string(),
                display_name: entry.display_name.clone(),
                size_bytes: entry.size_bytes,
                licence_spdx_id: entry.license.spdx_id.clone(),
                licence_name: entry.license.name.clone(),
                host,
                file,
                delivery_host_suffix,
                recommended_ram_bytes: entry.hardware.recommended_ram_bytes,
                installed: installed.contains(entry.id.as_str()),
            })
        })
        .collect()
}

/* ---------------------------------------------------- the toolchain */

/// Where node, npm and plaud are looked for when nobody said otherwise: the
/// PLAUD connector's own well-known directories, then `PATH`.
pub(crate) fn default_toolchain_dirs() -> Vec<PathBuf> {
    let discovery = plaudon_source_plaud_cli::discovery::CliDiscovery::new();
    let mut dirs = discovery.well_known_directories();
    for dir in discovery.path_directories() {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// The file names each program is accepted under on this platform.
fn leaves(program: &str) -> &'static [&'static str] {
    match program {
        "node" if cfg!(windows) => &["node.exe"],
        "node" => &["node"],
        "npm" if cfg!(windows) => &["npm.cmd", "npm.exe"],
        "npm" => &["npm"],
        _ if cfg!(windows) => &["plaud.exe"],
        _ => &["plaud"],
    }
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn find(program: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        leaves(program)
            .iter()
            .map(|leaf| dir.join(leaf))
            .find(|candidate| is_executable(candidate))
    })
}

/// The `@plaud-ai/cli` version installed beside `plaud`, read from the
/// package manifest npm wrote. Nothing is executed.
fn plaud_version(plaud: &Path) -> Option<String> {
    let resolved = std::fs::canonicalize(plaud).unwrap_or_else(|_| plaud.to_path_buf());
    let mut candidates: Vec<PathBuf> = resolved
        .ancestors()
        .skip(1)
        .take(6)
        .map(|dir| dir.join("package.json"))
        .collect();
    if let Some(bin) = plaud.parent() {
        // `<prefix>/bin/plaud` beside `<prefix>/lib/node_modules/...` on Unix,
        // `<prefix>/plaud.exe` beside `<prefix>/node_modules/...` on Windows.
        if let Some(prefix) = bin.parent() {
            candidates.push(prefix.join("lib/node_modules/@plaud-ai/cli/package.json"));
        }
        candidates.push(bin.join("node_modules/@plaud-ai/cli/package.json"));
    }
    candidates.into_iter().find_map(|path| {
        let text = std::fs::read_to_string(path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        (value.get("name")?.as_str()? == "@plaud-ai/cli")
            .then(|| value.get("version")?.as_str().map(str::to_owned))?
    })
}

fn presence(program: &str, path: Option<&Path>) -> ToolPresence {
    ToolPresence {
        program: program.to_owned(),
        path: path.map(|path| path.to_string_lossy().into_owned()),
    }
}

/// What this machine has of the PLAUD Cloud CLI toolchain.
pub(crate) fn detect_toolchain(dirs: &[PathBuf]) -> PlaudToolchainStatus {
    let node = find("node", dirs);
    let npm = find("npm", dirs);
    let plaud = find("plaud", dirs);
    let version = plaud.as_deref().and_then(plaud_version);
    let release_exercised = version.as_deref().map(|version| {
        plaudon_source_plaud_cli::compatibility::CliCompatibilityTable::committed()
            .usable_versions()
            .contains(&version)
    });

    let mut missing = Vec::new();
    for (name, found) in [("node", &node), ("npm", &npm), ("plaud", &plaud)] {
        if found.is_none() {
            missing.push(name.to_owned());
        }
    }
    let npm_runs_directly = npm.as_deref().is_some_and(|path| {
        path.file_name()
            .and_then(|leaf| leaf.to_str())
            .is_some_and(|leaf| leaf.strip_suffix(".exe").unwrap_or(leaf) == NPM_PROGRAM)
    });

    PlaudToolchainStatus {
        summary: summary(&missing, version.as_deref(), release_exercised),
        node: presence("node", node.as_deref()),
        npm: presence("npm", npm.as_deref()),
        plaud: presence("plaud", plaud.as_deref()),
        plaud_version: version,
        exercised_release: PLAUD_CLI_EXERCISED_RELEASE.to_owned(),
        release_exercised,
        missing,
        install_command: cli_install_command_text(),
        login_command: PLAUD_LOGIN_COMMAND.to_owned(),
        // A script shim (`npm.cmd`) would put a shell between us and the
        // arguments, so the guided install only runs a real executable.
        guided_install_available: cfg!(unix) && npm_runs_directly,
    }
}

fn human(name: &str) -> &'static str {
    match name {
        "node" => "Node.js",
        "npm" => "npm",
        _ => "the PLAUD CLI",
    }
}

fn join_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn summary(missing: &[String], version: Option<&str>, exercised: Option<bool>) -> String {
    let names: Vec<&str> = missing.iter().map(|name| human(name)).collect();
    let plaud_missing = missing.iter().any(|name| name == "plaud");
    if plaud_missing {
        return format!(
            "Not found on this machine: {}. PLAUD Cloud needs Node.js, npm and the PLAUD CLI.",
            join_names(&names)
        );
    }
    let mut sentence = match (version, exercised) {
        (Some(version), Some(true)) => {
            format!("The PLAUD CLI {version} is installed, the release this build has exercised.")
        }
        (Some(version), _) => format!(
            "The PLAUD CLI {version} is installed, but this build has only exercised \
             {PLAUD_CLI_EXERCISED_RELEASE}, so PLAUD Cloud may not work with it."
        ),
        (None, _) => format!(
            "The PLAUD CLI is installed, but its version could not be read. This build has \
             exercised {PLAUD_CLI_EXERCISED_RELEASE}."
        ),
    };
    if !names.is_empty() {
        let _ = write!(
            sentence,
            " Not found: {}, which the PLAUD CLI needs to run.",
            join_names(&names)
        );
    }
    sentence
}

/* ------------------------------------------------- running npm */

/// Run an approved install to completion, within its time budget.
///
/// Output is drained on separate threads so a chatty install cannot fill a
/// pipe and stall; only whether it succeeded is reported, never its text,
/// which can carry paths and registry URLs.
pub(crate) fn run_install(command: GatedCommand) -> Result<(), AppError> {
    let timeout = command.timeout();
    let limit = command.max_output_bytes();
    let mut child = command
        .into_command()
        .spawn()
        .map_err(|_| AppError::new(AppErrorCode::HelperFailed, "npm could not be started"))?;
    let drain = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            if let Some(stream) = stream {
                let _ = stream.take(limit as u64).read_to_end(&mut sink);
            }
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    );
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AppError::new(
                    AppErrorCode::Timeout,
                    "the PLAUD CLI install did not finish in time",
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(_) => {
                return Err(AppError::new(
                    AppErrorCode::HelperFailed,
                    "the PLAUD CLI install could not be watched",
                ))
            }
        }
    };
    let _ = stdout.join();
    let _ = stderr.join();
    if status.success() {
        Ok(())
    } else {
        Err(AppError::new(
            AppErrorCode::HelperFailed,
            format!("npm exited with {status}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_joined_the_way_a_sentence_lists_them() {
        assert_eq!(join_names(&["Node.js"]), "Node.js");
        assert_eq!(join_names(&["Node.js", "npm"]), "Node.js and npm");
        assert_eq!(
            join_names(&["Node.js", "npm", "the PLAUD CLI"]),
            "Node.js, npm and the PLAUD CLI"
        );
    }

    #[test]
    fn the_committed_catalogue_offers_whisper_models_with_their_host_and_file() {
        let offers = offers_for(catalogue::committed(), "whisper.cpp", &BTreeSet::new());
        assert!(offers.len() >= 3);
        assert!(offers
            .windows(2)
            .all(|pair| pair[0].size_bytes <= pair[1].size_bytes));
        for offer in &offers {
            assert_eq!(offer.host, "huggingface.co");
            assert_eq!(offer.delivery_host_suffix.as_deref(), Some("hf.co"));
            assert!(offer.file.starts_with("/ggerganov/whisper.cpp/resolve/"));
        }
        assert_eq!(
            offers_for(catalogue::committed(), "apple.speech", &BTreeSet::new()),
            Vec::new()
        );
    }
}
