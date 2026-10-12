//! What the operating system says about light/dark appearance.
//!
//! The settings screen offers *Light / Dark / Follow the system*; the last
//! option needs an answer from the host, and the host does not always have
//! one. [`OsThemeSource::os_theme`] therefore returns `Option<OsTheme>`, where
//! `None` means "this host does not say" — the shell then keeps whatever it
//! was using instead of guessing.
//!
//! # Why a child process, and why it is cached
//!
//! Plaudon has no AppKit/WinRT/GTK dependency in this crate, so
//! [`NativeOsTheme`] asks the platform the way a shell script would, with a
//! short-lived child process and an argument vector (never `sh -c`):
//!
//! | Platform | Command |
//! | --- | --- |
//! | macOS | `defaults read -g AppleInterfaceStyle` |
//! | Windows | `reg query HKCU\…\Themes\Personalize /v AppsUseLightTheme` |
//! | Linux | `gsettings get org.gnome.desktop.interface color-scheme`, then `gtk-theme` |
//!
//! Spawning a process costs milliseconds, which is fine once and catastrophic
//! sixty times a second. **[`OsThemeSource::os_theme`] never spawns anything**:
//! it returns a cached value. The process runs in [`NativeOsTheme::new`] and
//! again only when the shell calls [`NativeOsTheme::refresh`] — on window
//! focus, or when the user opens the appearance settings. eframe also delivers
//! the system theme through winit when the platform supports it; this module
//! is the fallback for hosts and builds where it does not, and the shell is
//! free to prefer the winit value and never call `refresh`.
//!
//! The parsers are pure functions of the command's output and are compiled on
//! every platform, so the Windows and Linux parsing rules are tested on macOS
//! and in CI rather than only on the platform that uses them.

use std::fmt;
use std::process::Command;
use std::sync::Mutex;

use crate::{lock, Platform};

/// The two appearances an OS can ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OsTheme {
    /// Light appearance.
    Light,
    /// Dark appearance.
    Dark,
}

impl OsTheme {
    /// Stable lowercase name, safe to log and to persist.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

/// Where the shell learns the OS appearance from.
pub trait OsThemeSource: Send + Sync + fmt::Debug {
    /// The OS appearance, or `None` when the host does not say.
    ///
    /// Implementations must be cheap: this is called while a frame is being
    /// built.
    fn os_theme(&self) -> Option<OsTheme>;

    /// Re-ask the platform and update whatever is cached.
    ///
    /// This is the expensive half of the port: a real implementation spawns a
    /// child process, so a caller runs it on window focus or when the
    /// appearance settings open, never per frame. A source with nothing to
    /// re-read answers with what it already has.
    fn refresh(&self) -> Option<OsTheme> {
        self.os_theme()
    }
}

/// The real source: one cached answer from the platform.
pub struct NativeOsTheme {
    platform: Option<Platform>,
    cached: Mutex<Option<OsTheme>>,
}

impl fmt::Debug for NativeOsTheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeOsTheme")
            .field("platform", &self.platform.map(Platform::as_str))
            .field("cached", &lock(&self.cached).map(OsTheme::as_str))
            .finish()
    }
}

impl Default for NativeOsTheme {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeOsTheme {
    /// Query the platform once and cache the answer.
    #[must_use]
    pub fn new() -> Self {
        let platform = Platform::host();
        let source = Self {
            platform,
            cached: Mutex::new(None),
        };
        source.refresh();
        source
    }

    /// Build a source for `platform` without querying anything.
    ///
    /// Used by tests and by a shell that already knows the answer (for
    /// example from winit) and only wants the caching port.
    #[must_use]
    pub fn with_cached(platform: Option<Platform>, theme: Option<OsTheme>) -> Self {
        Self {
            platform,
            cached: Mutex::new(theme),
        }
    }

    /// Re-run the platform query and update the cache.
    ///
    /// This spawns a child process. Call it on window focus or when the
    /// appearance settings are opened — never per frame.
    pub fn refresh(&self) -> Option<OsTheme> {
        let theme = self.platform.and_then(query_platform);
        *lock(&self.cached) = theme;
        theme
    }
}

impl OsThemeSource for NativeOsTheme {
    fn os_theme(&self) -> Option<OsTheme> {
        *lock(&self.cached)
    }

    fn refresh(&self) -> Option<OsTheme> {
        Self::refresh(self)
    }
}

/// Run the platform's query and parse its output.
///
/// Every failure mode — the tool is missing, it is killed, it prints something
/// we do not recognise — is `None`, which the caller reads as "the host does
/// not say".
fn query_platform(platform: Platform) -> Option<OsTheme> {
    match platform {
        Platform::MacOs => {
            let (success, stdout, stderr) =
                run(MACOS_PROGRAM, &["read", "-g", "AppleInterfaceStyle"])?;
            parse_macos_interface_style(success, &stdout, &stderr)
        }
        Platform::Windows => run(
            WINDOWS_PROGRAM,
            &[
                "query",
                WINDOWS_PERSONALIZE_KEY,
                "/v",
                WINDOWS_PERSONALIZE_VALUE,
            ],
        )
        .filter(|(success, _, _)| *success)
        .and_then(|(_, stdout, _)| parse_windows_apps_use_light_theme(&stdout)),
        Platform::Linux => gsettings("color-scheme")
            .as_deref()
            .and_then(parse_gsettings_color_scheme)
            .or_else(|| {
                gsettings("gtk-theme")
                    .as_deref()
                    .and_then(parse_gtk_theme_name)
            }),
    }
}

/// Read one `org.gnome.desktop.interface` key, or `None` if it is not there.
fn gsettings(key: &str) -> Option<String> {
    run(LINUX_PROGRAM, &["get", "org.gnome.desktop.interface", key])
        .filter(|(success, _, _)| *success)
        .map(|(_, stdout, _)| stdout)
}

/// macOS appearance query.
const MACOS_PROGRAM: &str = "defaults";
/// Windows appearance query.
const WINDOWS_PROGRAM: &str = "reg";
/// Registry key holding the Windows personalisation flags.
const WINDOWS_PERSONALIZE_KEY: &str =
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
/// Registry value naming the app appearance.
const WINDOWS_PERSONALIZE_VALUE: &str = "AppsUseLightTheme";
/// freedesktop appearance query.
const LINUX_PROGRAM: &str = "gsettings";

/// Run a command with an argument vector and capture its output.
///
/// `None` when the program could not be started at all. No shell is involved
/// and no argument is user-supplied: every argument here is a constant.
fn run(program: &str, args: &[&str]) -> Option<(bool, String, String)> {
    let output = Command::new(program).args(args).output().ok()?;
    Some((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Parse `defaults read -g AppleInterfaceStyle`.
///
/// macOS stores the key **only in dark mode**: in light mode the read fails
/// with "does not exist", which is the documented way to detect light mode.
/// Any other failure (no `defaults` binary, a sandbox denial) is `None` rather
/// than an assumption of light.
#[must_use]
pub fn parse_macos_interface_style(success: bool, stdout: &str, stderr: &str) -> Option<OsTheme> {
    if success {
        let value = stdout.trim();
        return value.eq_ignore_ascii_case("dark").then_some(OsTheme::Dark);
    }
    let reason = stderr.to_ascii_lowercase();
    (reason.contains("does not exist") || reason.contains("not exist")).then_some(OsTheme::Light)
}

/// Parse `reg query … /v AppsUseLightTheme`.
///
/// The output is a header line followed by `    AppsUseLightTheme    REG_DWORD
/// 0x0`. `0x0` means "apps use the dark theme", `0x1` light; anything else is
/// `None`.
#[must_use]
pub fn parse_windows_apps_use_light_theme(stdout: &str) -> Option<OsTheme> {
    let line = stdout
        .lines()
        .find(|line| line.contains(WINDOWS_PERSONALIZE_VALUE))?;
    let mut fields = line.split_whitespace();
    let name = fields.next()?;
    if name != WINDOWS_PERSONALIZE_VALUE {
        return None;
    }
    let kind = fields.next()?;
    if !kind.eq_ignore_ascii_case("REG_DWORD") {
        return None;
    }
    let raw = fields.next()?;
    let digits = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X"))?;
    match u32::from_str_radix(digits, 16).ok()? {
        0 => Some(OsTheme::Dark),
        1 => Some(OsTheme::Light),
        _ => None,
    }
}

/// Parse `gsettings get org.gnome.desktop.interface color-scheme`.
///
/// The value is a quoted GVariant string: `'prefer-dark'`, `'prefer-light'` or
/// `'default'`. `default` means the desktop expresses no preference, which is
/// `None` — not light.
#[must_use]
pub fn parse_gsettings_color_scheme(stdout: &str) -> Option<OsTheme> {
    let value = stdout.trim().trim_matches('\'').trim_matches('"');
    match value {
        "prefer-dark" => Some(OsTheme::Dark),
        "prefer-light" => Some(OsTheme::Light),
        _ => None,
    }
}

/// Parse `gsettings get org.gnome.desktop.interface gtk-theme`.
///
/// Only a theme whose name actually ends in `dark` (`Adwaita-dark`,
/// `Yaru-blue-dark`) is evidence of a dark appearance. A name that does not
/// say so is `None`: plenty of dark themes are not named for it, so claiming
/// "light" here would be a guess.
#[must_use]
pub fn parse_gtk_theme_name(stdout: &str) -> Option<OsTheme> {
    let value = stdout.trim().trim_matches('\'').trim_matches('"');
    if value.is_empty() {
        return None;
    }
    let lowered = value.to_ascii_lowercase();
    (lowered.ends_with("-dark") || lowered.ends_with(":dark")).then_some(OsTheme::Dark)
}

/// A source that always reports the same thing.
#[derive(Debug, Clone, Copy, Default)]
pub struct FixedOsTheme {
    theme: Option<OsTheme>,
}

impl FixedOsTheme {
    /// A source that reports `theme`.
    #[must_use]
    pub const fn new(theme: Option<OsTheme>) -> Self {
        Self { theme }
    }

    /// A source that reports a host which does not say.
    #[must_use]
    pub const fn silent() -> Self {
        Self { theme: None }
    }
}

impl OsThemeSource for FixedOsTheme {
    fn os_theme(&self) -> Option<OsTheme> {
        self.theme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_reports_dark_only_when_the_key_is_set() {
        assert_eq!(
            parse_macos_interface_style(true, "Dark\n", ""),
            Some(OsTheme::Dark)
        );
        assert_eq!(
            parse_macos_interface_style(true, "  dark  ", ""),
            Some(OsTheme::Dark)
        );
    }

    #[test]
    fn macos_reports_light_when_the_key_is_absent() {
        assert_eq!(
            parse_macos_interface_style(
                false,
                "",
                "2026-01-01 12:00:00.000 defaults[1:1] \
                 The domain/default pair of (kCFPreferencesAnyApplication, \
                 AppleInterfaceStyle) does not exist\n"
            ),
            Some(OsTheme::Light)
        );
    }

    #[test]
    fn macos_says_nothing_when_the_query_failed_for_another_reason() {
        assert_eq!(
            parse_macos_interface_style(false, "", "defaults: command not found"),
            None
        );
        assert_eq!(parse_macos_interface_style(true, "Graphite\n", ""), None);
        assert_eq!(parse_macos_interface_style(true, "", ""), None);
    }

    #[test]
    fn windows_reads_the_apps_use_light_theme_dword() {
        let dark = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\r\n    AppsUseLightTheme    REG_DWORD    0x0\r\n\r\n";
        let light = "\r\nHKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\r\n    AppsUseLightTheme    REG_DWORD    0x1\r\n\r\n";
        assert_eq!(
            parse_windows_apps_use_light_theme(dark),
            Some(OsTheme::Dark)
        );
        assert_eq!(
            parse_windows_apps_use_light_theme(light),
            Some(OsTheme::Light)
        );
    }

    #[test]
    fn windows_says_nothing_for_an_unexpected_shape() {
        assert_eq!(parse_windows_apps_use_light_theme(""), None);
        assert_eq!(
            parse_windows_apps_use_light_theme(
                "ERROR: The system was unable to find the specified registry key or value."
            ),
            None
        );
        assert_eq!(
            parse_windows_apps_use_light_theme("    AppsUseLightTheme    REG_SZ    0x1\r\n"),
            None
        );
        assert_eq!(
            parse_windows_apps_use_light_theme("    AppsUseLightTheme    REG_DWORD    0x7\r\n"),
            None
        );
        assert_eq!(
            parse_windows_apps_use_light_theme("    AppsUseLightTheme    REG_DWORD    banana\r\n"),
            None
        );
    }

    #[test]
    fn linux_reads_the_gnome_colour_scheme() {
        assert_eq!(
            parse_gsettings_color_scheme("'prefer-dark'\n"),
            Some(OsTheme::Dark)
        );
        assert_eq!(
            parse_gsettings_color_scheme("'prefer-light'\n"),
            Some(OsTheme::Light)
        );
        assert_eq!(parse_gsettings_color_scheme("'default'\n"), None);
        assert_eq!(parse_gsettings_color_scheme(""), None);
        assert_eq!(
            parse_gsettings_color_scheme("No such schema \"org.gnome.desktop.interface\"\n"),
            None
        );
    }

    #[test]
    fn linux_falls_back_to_a_theme_name_only_when_it_says_dark() {
        assert_eq!(
            parse_gtk_theme_name("'Adwaita-dark'\n"),
            Some(OsTheme::Dark)
        );
        assert_eq!(
            parse_gtk_theme_name("'Yaru-blue-dark'\n"),
            Some(OsTheme::Dark)
        );
        assert_eq!(parse_gtk_theme_name("'Adwaita'\n"), None);
        assert_eq!(parse_gtk_theme_name("'Breeze'\n"), None);
        assert_eq!(parse_gtk_theme_name(""), None);
    }

    #[test]
    fn the_fake_reports_what_it_was_given() {
        assert_eq!(
            FixedOsTheme::new(Some(OsTheme::Dark)).os_theme(),
            Some(OsTheme::Dark)
        );
        assert_eq!(FixedOsTheme::silent().os_theme(), None);
        let source: Box<dyn OsThemeSource> = Box::new(FixedOsTheme::new(Some(OsTheme::Light)));
        assert_eq!(source.os_theme(), Some(OsTheme::Light));
    }

    #[test]
    fn the_cache_is_what_os_theme_reads() {
        let source = NativeOsTheme::with_cached(Some(Platform::Linux), Some(OsTheme::Dark));
        assert_eq!(source.os_theme(), Some(OsTheme::Dark));
        assert!(format!("{source:?}").contains("linux"));
    }

    /// `refresh` is the only thing that may spawn a process, and whatever it
    /// finds on this machine, `os_theme` must afterwards agree with it without
    /// spawning anything itself.
    #[test]
    fn refresh_updates_the_cache_and_os_theme_is_pure() {
        let source = NativeOsTheme::new();
        let refreshed = source.refresh();
        assert_eq!(source.os_theme(), refreshed);
        assert_eq!(source.os_theme(), refreshed, "reading must not re-query");
    }

    #[test]
    fn theme_names_are_stable() {
        assert_eq!(OsTheme::Light.as_str(), "light");
        assert_eq!(OsTheme::Dark.as_str(), "dark");
    }
}
