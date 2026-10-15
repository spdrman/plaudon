//! "Reveal in Finder", "Open folder" and "Open link", without a shell.
//!
//! Three rules:
//!
//! 1. **Never a shell.** Every launch is a [`Command`] with an argument
//!    vector; `sh -c`, `cmd /c` and string interpolation do not appear in this
//!    module. A file called `; rm -rf ~` is one argument and nothing else.
//! 2. **Only three URL schemes leave the app.** `http`, `https` and `mailto`
//!    ([`ALLOWED_URL_SCHEMES`]). Anything else — `file:`, `javascript:`,
//!    `vbscript:`, `smb:`, a nested `plaudon:` — is refused with
//!    [`OpenError::UnsupportedScheme`], which the caller shows to the user
//!    instead of silently doing nothing.
//! 3. **The argument vector is pure data.** [`reveal_command`],
//!    [`open_folder_command`] and [`open_url_command`] are pure functions of
//!    (platform, path), compiled on every host and tested for all three
//!    platforms everywhere, so the Windows form is covered by a macOS test run.
//!
//! | Platform | Reveal | Open folder | Open URL |
//! | --- | --- | --- | --- |
//! | macOS | `open -R <path>` | `open <dir>` | `open <url>` |
//! | Windows | `explorer /select,<path>` | `explorer <dir>` | `rundll32 url.dll,FileProtocolHandler <url>` |
//! | Linux | `xdg-open <parent dir>` | `xdg-open <dir>` | `xdg-open <url>` |
//!
//! Windows has no shell-free "select this file" other than `explorer`'s
//! `/select,` form, which takes the whole thing as **one** argument; that is
//! why [`CommandSpec::args`] holds `OsString`s rather than a formatted line.
//! `rundll32 url.dll,FileProtocolHandler` is used for links because it hands
//! the URL to the shell's protocol handler without going through `cmd`'s
//! `start`, whose quoting rules are a known injection surface.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use plaudon_app::permissions::{SettingsHost, SystemSettingsPane};
use url::Url;

use crate::{lock, Platform};

/// URL schemes the app is willing to hand to the OS.
pub const ALLOWED_URL_SCHEMES: &[&str] = &["http", "https", "mailto"];

/// Why an open request did not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    /// This host has no known command for the request.
    #[error("this platform cannot open that: {reason}")]
    UnsupportedPlatform {
        /// Honest reason, safe to log and to show.
        reason: String,
    },
    /// The URL's scheme is not one Plaudon will open.
    #[error("refused to open a {scheme} URL; only http, https and mailto are allowed")]
    UnsupportedScheme {
        /// The refused scheme, lowercase. Never the rest of the URL.
        scheme: String,
    },
    /// The string is not a URL at all.
    #[error("that is not a valid URL")]
    InvalidUrl,
    /// The path has no parent directory to reveal it in.
    #[error("that path has no parent directory")]
    NoParentDirectory,
    /// The helper process could not be started.
    #[error("could not start the system handler: {reason}")]
    Spawn {
        /// Operating-system reason; never the path or URL.
        reason: String,
    },
}

/// A program and its argument vector, ready for [`Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Program name, resolved through `PATH` by the OS.
    pub program: &'static str,
    /// Arguments, one vector entry per argument. Never a shell line.
    pub args: Vec<OsString>,
}

impl CommandSpec {
    fn new(program: &'static str, args: impl IntoIterator<Item = OsString>) -> Self {
        Self {
            program,
            args: args.into_iter().collect(),
        }
    }

    /// Spawn the command and detach from it.
    ///
    /// The child is reaped by a short-lived thread, so a user who reveals
    /// twenty files does not leave twenty zombies behind, and the caller —
    /// which may be the UI thread — never waits for a file manager to start.
    fn spawn(self) -> Result<(), OpenError> {
        let mut child = Command::new(self.program)
            .args(&self.args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| OpenError::Spawn {
                reason: err.to_string(),
            })?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// Program names, as constants so the tests and the adapters cannot drift.
const MACOS_OPEN: &str = "open";
/// Windows file manager.
const WINDOWS_EXPLORER: &str = "explorer";
/// Windows protocol handler shim.
const WINDOWS_RUNDLL: &str = "rundll32";
/// First argument of [`WINDOWS_RUNDLL`] when opening a URL.
const WINDOWS_URL_HANDLER: &str = "url.dll,FileProtocolHandler";
/// freedesktop.org opener.
const LINUX_XDG_OPEN: &str = "xdg-open";

/// The command that shows `path` selected in the platform's file manager.
///
/// # Errors
///
/// [`OpenError::NoParentDirectory`] on Linux when `path` has no parent, since
/// the Linux form opens the containing directory.
pub fn reveal_command(platform: Platform, path: &Path) -> Result<CommandSpec, OpenError> {
    match platform {
        Platform::MacOs => Ok(CommandSpec::new(
            MACOS_OPEN,
            [OsString::from("-R"), path.as_os_str().to_owned()],
        )),
        Platform::Windows => {
            // One argument: `/select,` and the path are a single token.
            let mut arg = OsString::from("/select,");
            arg.push(path.as_os_str());
            Ok(CommandSpec::new(WINDOWS_EXPLORER, [arg]))
        }
        Platform::Linux => {
            let parent = path.parent().ok_or(OpenError::NoParentDirectory)?;
            Ok(CommandSpec::new(
                LINUX_XDG_OPEN,
                [parent.as_os_str().to_owned()],
            ))
        }
    }
}

/// The command that opens `directory` in the platform's file manager.
#[must_use]
pub fn open_folder_command(platform: Platform, directory: &Path) -> CommandSpec {
    let argument = directory.as_os_str().to_owned();
    match platform {
        Platform::MacOs => CommandSpec::new(MACOS_OPEN, [argument]),
        Platform::Windows => CommandSpec::new(WINDOWS_EXPLORER, [argument]),
        Platform::Linux => CommandSpec::new(LINUX_XDG_OPEN, [argument]),
    }
}

/// The command that opens an already-validated `url` in the default browser.
#[must_use]
pub fn open_url_command(platform: Platform, url: &Url) -> CommandSpec {
    let argument = OsString::from(url.as_str());
    match platform {
        Platform::MacOs => CommandSpec::new(MACOS_OPEN, [argument]),
        Platform::Windows => CommandSpec::new(
            WINDOWS_RUNDLL,
            [OsString::from(WINDOWS_URL_HANDLER), argument],
        ),
        Platform::Linux => CommandSpec::new(LINUX_XDG_OPEN, [argument]),
    }
}

/// The command that opens a system settings pane (#301).
///
/// Settings deep links (`x-apple.systempreferences:`, `ms-settings:`) are not
/// web links, so they do not go through [`open_url_command`] and its
/// [`ALLOWED_URL_SCHEMES`] check. They do not need it: each pane is a
/// compiled-in constant from [`plaudon_app::permissions`], and the only input
/// is which pane.
///
/// # Errors
///
/// [`OpenError::UnsupportedPlatform`] on Linux, which has no settings link
/// every desktop environment honours.
pub fn system_settings_command(
    platform: Platform,
    pane: SystemSettingsPane,
) -> Result<CommandSpec, OpenError> {
    let host = match platform {
        Platform::MacOs => SettingsHost::MacOs,
        Platform::Windows => SettingsHost::Windows,
        Platform::Linux => SettingsHost::Other,
    };
    let link = pane
        .deep_link(host)
        .ok_or_else(|| OpenError::UnsupportedPlatform {
            reason: "this desktop has no system settings link for Bluetooth".to_owned(),
        })?;
    let argument = OsString::from(link);
    Ok(match platform {
        Platform::MacOs => CommandSpec::new(MACOS_OPEN, [argument]),
        Platform::Windows => CommandSpec::new(
            WINDOWS_RUNDLL,
            [OsString::from(WINDOWS_URL_HANDLER), argument],
        ),
        Platform::Linux => CommandSpec::new(LINUX_XDG_OPEN, [argument]),
    })
}

/// Open a system settings pane on the host this binary was built for.
///
/// # Errors
///
/// [`OpenError::UnsupportedPlatform`] when the host has no link for `pane`,
/// and [`OpenError::Spawn`] when the handler could not be started.
pub fn open_system_settings(pane: SystemSettingsPane) -> Result<(), OpenError> {
    let platform = Platform::host().ok_or_else(|| OpenError::UnsupportedPlatform {
        reason: "no system settings application is known for this operating system".to_owned(),
    })?;
    system_settings_command(platform, pane)?.spawn()
}

/// Parse and authorise an outbound URL.
///
/// # Errors
///
/// [`OpenError::InvalidUrl`] when the string is not a URL, and
/// [`OpenError::UnsupportedScheme`] for anything outside
/// [`ALLOWED_URL_SCHEMES`]. Only the scheme is ever reported back or logged:
/// the rest of the URL may be user data.
pub fn validate_external_url(raw: &str) -> Result<Url, OpenError> {
    let url = Url::parse(raw).map_err(|_| {
        tracing::debug!("refused to open a string that is not a URL");
        OpenError::InvalidUrl
    })?;
    let scheme = url.scheme().to_ascii_lowercase();
    if !ALLOWED_URL_SCHEMES.contains(&scheme.as_str()) {
        tracing::debug!(scheme = %scheme, "refused an outbound URL scheme");
        return Err(OpenError::UnsupportedScheme { scheme });
    }
    Ok(url)
}

/// Reveal paths, open folders and open links in the user's own applications.
pub trait Opener: Send + Sync + fmt::Debug {
    /// Show `path` selected in the file manager.
    fn reveal_in_folder(&self, path: &Path) -> Result<(), OpenError>;

    /// Open `directory` in the file manager.
    fn open_folder(&self, directory: &Path) -> Result<(), OpenError>;

    /// Open `url` in the default browser or mail client.
    fn open_url(&self, url: &str) -> Result<(), OpenError>;
}

/// The real opener: spawns the platform's handler.
#[derive(Debug, Clone, Copy)]
pub struct NativeOpener {
    platform: Option<Platform>,
}

impl Default for NativeOpener {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeOpener {
    /// An opener for the host this binary was built for.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            platform: Platform::host(),
        }
    }

    /// An opener for a named platform, used by tests of the wiring.
    #[must_use]
    pub const fn for_platform(platform: Platform) -> Self {
        Self {
            platform: Some(platform),
        }
    }

    fn platform(self) -> Result<Platform, OpenError> {
        self.platform.ok_or_else(|| OpenError::UnsupportedPlatform {
            reason: "no file manager or URL handler is known for this operating system".to_owned(),
        })
    }
}

impl Opener for NativeOpener {
    fn reveal_in_folder(&self, path: &Path) -> Result<(), OpenError> {
        reveal_command(self.platform()?, path)?.spawn()
    }

    fn open_folder(&self, directory: &Path) -> Result<(), OpenError> {
        open_folder_command(self.platform()?, directory).spawn()
    }

    fn open_url(&self, url: &str) -> Result<(), OpenError> {
        let platform = self.platform()?;
        let parsed = validate_external_url(url)?;
        open_url_command(platform, &parsed).spawn()
    }
}

/// One thing the UI asked the opener to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenRequest {
    /// [`Opener::reveal_in_folder`].
    Reveal(PathBuf),
    /// [`Opener::open_folder`].
    Folder(PathBuf),
    /// [`Opener::open_url`], recorded only when the scheme was allowed.
    Url(String),
}

/// Recording opener for tests: spawns nothing, refuses exactly what the real
/// one refuses.
///
/// The scheme check is deliberately shared with [`NativeOpener`] rather than
/// stubbed out, so a test that asserts "the UI shows a refusal" is testing the
/// same rule production uses.
#[derive(Debug, Default)]
pub struct FakeOpener {
    requests: Mutex<Vec<OpenRequest>>,
}

impl FakeOpener {
    /// A fresh recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything that was asked for, in order.
    #[must_use]
    pub fn requests(&self) -> Vec<OpenRequest> {
        lock(&self.requests).clone()
    }
}

impl Opener for FakeOpener {
    fn reveal_in_folder(&self, path: &Path) -> Result<(), OpenError> {
        lock(&self.requests).push(OpenRequest::Reveal(path.to_path_buf()));
        Ok(())
    }

    fn open_folder(&self, directory: &Path) -> Result<(), OpenError> {
        lock(&self.requests).push(OpenRequest::Folder(directory.to_path_buf()));
        Ok(())
    }

    fn open_url(&self, url: &str) -> Result<(), OpenError> {
        let parsed = validate_external_url(url)?;
        lock(&self.requests).push(OpenRequest::Url(parsed.to_string()));
        Ok(())
    }
}

/// Convenience for tests and assertions: the args as `&OsStr`.
#[must_use]
pub fn arg_refs(spec: &CommandSpec) -> Vec<&OsStr> {
    spec.args.iter().map(OsString::as_os_str).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(spec: &CommandSpec) -> Vec<String> {
        spec.args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn macos_reveals_with_open_dash_r() {
        let spec = reveal_command(Platform::MacOs, Path::new("/Users/x/My Audio.wav"))
            .expect("macOS reveal");
        assert_eq!(spec.program, "open");
        assert_eq!(args(&spec), vec!["-R", "/Users/x/My Audio.wav"]);
    }

    #[test]
    fn windows_reveals_with_a_single_select_argument() {
        let spec = reveal_command(Platform::Windows, Path::new("C:\\Users\\x\\My Audio.wav"))
            .expect("windows reveal");
        assert_eq!(spec.program, "explorer");
        assert_eq!(args(&spec), vec!["/select,C:\\Users\\x\\My Audio.wav"]);
        assert_eq!(spec.args.len(), 1, "/select, and the path are one argument");
    }

    #[test]
    fn linux_reveals_by_opening_the_parent_directory() {
        let spec = reveal_command(Platform::Linux, Path::new("/home/x/audio/a.wav"))
            .expect("linux reveal");
        assert_eq!(spec.program, "xdg-open");
        assert_eq!(args(&spec), vec!["/home/x/audio"]);
    }

    #[test]
    fn linux_cannot_reveal_a_path_without_a_parent() {
        assert_eq!(
            reveal_command(Platform::Linux, Path::new("/")),
            Err(OpenError::NoParentDirectory)
        );
    }

    #[test]
    fn opening_a_folder_passes_the_directory_as_one_argument() {
        for (platform, program) in [
            (Platform::MacOs, "open"),
            (Platform::Windows, "explorer"),
            (Platform::Linux, "xdg-open"),
        ] {
            let spec = open_folder_command(platform, Path::new("/tmp/a b"));
            assert_eq!(spec.program, program);
            assert_eq!(args(&spec), vec!["/tmp/a b"]);
        }
    }

    #[test]
    fn opening_a_url_uses_the_platform_handler() {
        let url = validate_external_url("https://plaudon.app/docs?q=1").expect("valid");
        assert_eq!(
            args(&open_url_command(Platform::MacOs, &url)),
            vec!["https://plaudon.app/docs?q=1"]
        );
        let windows = open_url_command(Platform::Windows, &url);
        assert_eq!(windows.program, "rundll32");
        assert_eq!(
            args(&windows),
            vec![
                "url.dll,FileProtocolHandler",
                "https://plaudon.app/docs?q=1"
            ]
        );
        assert_eq!(open_url_command(Platform::Linux, &url).program, "xdg-open");
    }

    #[test]
    fn only_http_https_and_mailto_are_opened() {
        assert!(validate_external_url("http://example.com").is_ok());
        assert!(validate_external_url("https://example.com").is_ok());
        assert!(validate_external_url("mailto:support@plaudon.app").is_ok());
    }

    #[test]
    fn every_other_scheme_is_refused_with_its_name() {
        for (raw, scheme) in [
            ("file:///etc/passwd", "file"),
            ("javascript:alert(1)", "javascript"),
            ("vbscript:msgbox(1)", "vbscript"),
            ("smb://server/share", "smb"),
            ("plaudon://settings", "plaudon"),
            ("data:text/html,<script>", "data"),
            ("HTTPX://example.com", "httpx"),
        ] {
            assert_eq!(
                validate_external_url(raw),
                Err(OpenError::UnsupportedScheme {
                    scheme: scheme.to_owned()
                }),
                "{raw} must be refused"
            );
        }
    }

    #[test]
    fn a_string_that_is_not_a_url_is_refused() {
        for raw in ["", "not a url", "example.com", "   ", "://"] {
            assert_eq!(validate_external_url(raw), Err(OpenError::InvalidUrl));
        }
    }

    #[test]
    fn a_refusal_names_the_scheme_and_nothing_else() {
        let err =
            validate_external_url("file:///Users/secret/transcript.txt").expect_err("must refuse");
        let message = err.to_string();
        assert!(message.contains("file"));
        assert!(!message.contains("transcript"));
        assert!(!message.contains("secret"));
    }

    #[test]
    fn the_fake_records_what_was_asked_for() {
        let opener = FakeOpener::new();
        opener
            .reveal_in_folder(Path::new("/tmp/a.wav"))
            .expect("reveal");
        opener.open_folder(Path::new("/tmp")).expect("folder");
        opener.open_url("https://plaudon.app/").expect("url");
        assert_eq!(
            opener.requests(),
            vec![
                OpenRequest::Reveal(PathBuf::from("/tmp/a.wav")),
                OpenRequest::Folder(PathBuf::from("/tmp")),
                OpenRequest::Url("https://plaudon.app/".to_owned()),
            ]
        );
    }

    #[test]
    fn the_fake_refuses_the_same_schemes_as_the_real_opener() {
        let opener = FakeOpener::new();
        assert_eq!(
            opener.open_url("file:///etc/passwd"),
            Err(OpenError::UnsupportedScheme {
                scheme: "file".to_owned()
            })
        );
        assert!(
            opener.requests().is_empty(),
            "a refused URL must not be recorded as opened"
        );
    }

    #[test]
    fn the_real_opener_refuses_a_bad_scheme_before_spawning_anything() {
        // No process can be started for this call: the scheme check happens
        // first, so the test is safe on every host.
        let opener = NativeOpener::for_platform(Platform::Windows);
        assert_eq!(
            opener.open_url("javascript:alert(1)"),
            Err(OpenError::UnsupportedScheme {
                scheme: "javascript".to_owned()
            })
        );
    }

    #[test]
    fn a_host_with_no_known_handler_says_so() {
        let opener = NativeOpener { platform: None };
        let err = opener
            .open_folder(Path::new("/tmp"))
            .expect_err("unsupported");
        assert!(matches!(err, OpenError::UnsupportedPlatform { .. }));
        assert!(err.to_string().contains("operating system"));
    }

    #[test]
    fn a_hostile_file_name_stays_one_argument() {
        let hostile = Path::new("/tmp/; rm -rf ~/Library && echo .wav");
        let spec = reveal_command(Platform::MacOs, hostile).expect("reveal");
        assert_eq!(spec.args.len(), 2);
        assert_eq!(
            spec.args[1],
            OsString::from("/tmp/; rm -rf ~/Library && echo .wav")
        );
        assert_eq!(arg_refs(&spec).len(), 2);
    }

    #[test]
    fn the_port_is_usable_as_a_trait_object() {
        let opener: Box<dyn Opener> = Box::new(FakeOpener::new());
        opener.open_folder(Path::new("/tmp")).expect("folder");
        let _: &dyn Opener = &NativeOpener::new();
    }
}
