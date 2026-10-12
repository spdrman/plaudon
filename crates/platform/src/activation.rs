//! Deep links, opened documents and notification activations, in one value.
//!
//! Three things can ask a running Plaudon to show something:
//!
//! * a `plaudon://` URL — from the OS (`open plaudon://settings`), from a
//!   second launch whose argument vector carries the link, or from a browser,
//! * a notification click, which hands back the opaque activation token
//!   [`plaudon_notifications::NotificationTarget::activation_token`] wrote
//!   (migration plan §4.5 row J15), and
//! * a document the user opened, because the installer claimed the
//!   `.plaudon` extension (migration plan §4.2 row C10). Windows and Linux
//!   deliver it in the argument vector ([`launch_request`]); macOS sends a
//!   running application an Apple event instead, which arrives as a
//!   `file://` URL ([`open_request`]).
//!
//! The first two become an [`Activation`], and the shell reacts to them the
//! same way in both cases: raise and focus the window
//! (`ViewportCommand::Minimized(false)` then `ViewportCommand::Focus`) and
//! publish `ui.activation_requested` through
//! [`plaudon_app::notify::publish_activation`] with
//! [`Activation::notification_target`]. The third becomes a
//! [`LaunchRequest::Open`], carrying paths that have been through exactly
//! the validation a window drop goes through — the OS is not trusted to
//! hand us a path inside an approved root.
//!
//! # Only our own links navigate the app
//!
//! Anything else in the world can send us a URL, so parsing is strict and
//! total: an unknown scheme, an unknown host, a malformed id, a credential, a
//! port, a query string, a fragment or an extra path segment all yield `None`.
//! Rejections are logged at `debug` with a fixed reason code and **no part of
//! the input**, because the input is attacker-controlled and may carry user
//! data.
//!
//! Token parsing is not reimplemented here: it is
//! [`plaudon_app::notify::activation_target`], which is the same function the
//! notification backends round-trip through.

use std::path::PathBuf;

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_notifications::NotificationTarget;
use url::Url;

use crate::associations::URL_SCHEME;
use crate::dragdrop::{self, DropClassification};

/// Host component of the recording deep link.
pub const HOST_RECORDING: &str = "recording";
/// Host component of the job deep link.
pub const HOST_JOB: &str = "job";
/// Host component of the settings deep link.
pub const HOST_SETTINGS: &str = "settings";

/// What the app was asked to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// Open a recording.
    Recording(RecordingId),
    /// Open a job in the activity view.
    Job(JobId),
    /// Open the settings screen.
    Settings,
}

impl Activation {
    /// The in-process target the shell publishes as `ui.activation_requested`.
    ///
    /// The mapping is total and lossless in both directions, which is why the
    /// shell never has to `match` on this enum to raise the event.
    #[must_use]
    pub const fn notification_target(self) -> NotificationTarget {
        match self {
            Self::Recording(id) => NotificationTarget::Recording(id),
            Self::Job(id) => NotificationTarget::Job(id),
            Self::Settings => NotificationTarget::Settings,
        }
    }

    /// The opaque token the OS carries for a notification of this target.
    #[must_use]
    pub fn activation_token(self) -> String {
        self.notification_target().activation_token()
    }

    /// The canonical `plaudon://` URL for this activation.
    ///
    /// Round-trips through [`from_url`]; used when the app has to hand a link
    /// to the OS (a toast action, a "copy link" affordance).
    #[must_use]
    pub fn url(self) -> String {
        match self {
            Self::Recording(id) => format!("{URL_SCHEME}://{HOST_RECORDING}/{id}"),
            Self::Job(id) => format!("{URL_SCHEME}://{HOST_JOB}/{id}"),
            Self::Settings => format!("{URL_SCHEME}://{HOST_SETTINGS}"),
        }
    }
}

impl From<NotificationTarget> for Activation {
    fn from(target: NotificationTarget) -> Self {
        match target {
            NotificationTarget::Recording(id) => Self::Recording(id),
            NotificationTarget::Job(id) => Self::Job(id),
            NotificationTarget::Settings => Self::Settings,
        }
    }
}

impl From<Activation> for NotificationTarget {
    fn from(activation: Activation) -> Self {
        activation.notification_target()
    }
}

/// Parse a `plaudon://` deep link.
///
/// Accepts exactly three shapes and nothing else:
///
/// * `plaudon://recording/<uuid>`
/// * `plaudon://job/<uuid>`
/// * `plaudon://settings`
#[must_use]
pub fn from_url(raw: &str) -> Option<Activation> {
    let url = Url::parse(raw).ok().or_else(|| {
        reject("not a url");
        None
    })?;

    if url.scheme() != URL_SCHEME {
        reject("foreign scheme");
        return None;
    }
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        reject("credentials or port in a deep link");
        return None;
    }
    if url.query().is_some() || url.fragment().is_some() {
        reject("query or fragment in a deep link");
        return None;
    }

    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let path = url.path().trim_matches('/');

    match host.as_str() {
        HOST_SETTINGS => {
            if path.is_empty() {
                Some(Activation::Settings)
            } else {
                reject("trailing path on a settings link");
                None
            }
        }
        HOST_RECORDING => parse_id(path).map(Activation::Recording),
        HOST_JOB => parse_id(path).map(Activation::Job),
        _ => {
            reject("unknown host");
            None
        }
    }
}

/// Parse a single-segment identifier, rejecting anything with more structure.
fn parse_id<T: std::str::FromStr>(path: &str) -> Option<T> {
    if path.is_empty() {
        reject("missing id");
        return None;
    }
    if path.contains('/') {
        reject("extra path segments");
        return None;
    }
    // Percent-encoding is not valid in a UUID; the raw segment is parsed as
    // given, so `%72ecording`-style tricks cannot smuggle a different id in.
    let Ok(id) = path.parse() else {
        reject("malformed id");
        return None;
    };
    Some(id)
}

/// Parse an activation token the OS handed back with a notification click.
///
/// Delegates to [`plaudon_app::notify::activation_target`] so there is exactly
/// one token grammar in the application.
#[must_use]
pub fn from_notification_token(token: &str) -> Option<Activation> {
    let Some(target) = plaudon_app::notify::activation_target(token) else {
        reject("unknown activation token");
        return None;
    };
    Some(Activation::from(target))
}

/// What the operating system asked the application to open.
///
/// A launch argument vector and a macOS `application:openURLs:` carry the
/// same two things, so they parse into the same value: either a deep link,
/// which navigates, or file-system paths, which are classified against the
/// approved roots exactly as a window drop is. There is deliberately no
/// third arm: an argument that is neither is not an instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRequest {
    /// A `plaudon://` deep link.
    Activate(Activation),
    /// Paths the OS handed over, already validated.
    ///
    /// The classification is returned whole, rejections included, because a
    /// path the application refuses has to be reported to the user rather
    /// than dropped silently.
    Open(DropClassification),
}

/// Everything a process argument vector asked for.
///
/// A deep link wins over a path: a link is an explicit instruction, a path
/// is a document, and nothing the installer registers produces both at
/// once. `argv[0]` is skipped, options are skipped (macOS adds `-psn_0_…`
/// to a Finder launch), and a URL in a scheme that is not ours is skipped
/// rather than mistaken for a relative path.
#[must_use]
pub fn launch_request<I, S>(arguments: I, approved_roots: &[PathBuf]) -> Option<LaunchRequest>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut paths: Vec<PathBuf> = Vec::new();
    for argument in arguments.into_iter().skip(1) {
        let argument = argument.as_ref();
        if has_our_scheme(argument) {
            // A malformed link in our own scheme is refused outright; it is
            // never reinterpreted as a file name.
            if let Some(activation) = from_url(argument) {
                return Some(LaunchRequest::Activate(activation));
            }
            continue;
        }
        if let Some(path) = path_argument(argument) {
            paths.push(path);
        }
    }
    if paths.is_empty() {
        return None;
    }
    Some(LaunchRequest::Open(dragdrop::classify(
        &paths,
        approved_roots,
    )))
}

/// Route one thing a **running** application was asked to open.
///
/// macOS delivers both halves of the claim to a running application as
/// Apple events rather than as arguments: `plaudon://` links, and — because
/// the archive's document type is claimed by UTI — a `file://` URL for a
/// double-clicked `.plaudon` archive. Both arrive here, and both end where
/// their argument-vector equivalents end.
#[must_use]
pub fn open_request(raw: &str, approved_roots: &[PathBuf]) -> Option<LaunchRequest> {
    if has_our_scheme(raw) {
        return from_url(raw).map(LaunchRequest::Activate);
    }
    let path = path_argument(raw)?;
    Some(LaunchRequest::Open(dragdrop::classify(
        std::slice::from_ref(&path),
        approved_roots,
    )))
}

/// Turn one argument that is not a deep link into a path, or refuse it.
fn path_argument(argument: &str) -> Option<PathBuf> {
    if argument.is_empty() || argument.starts_with('-') {
        return None;
    }
    match url_scheme(argument) {
        Some("file") => {
            let Ok(url) = Url::parse(argument) else {
                reject("malformed file url");
                return None;
            };
            let Ok(path) = url.to_file_path() else {
                reject("file url with a host or a relative path");
                return None;
            };
            Some(path)
        }
        Some(_) => {
            reject("foreign scheme in an open request");
            None
        }
        None => Some(PathBuf::from(argument)),
    }
}

/// The scheme of `argument` when it is a URL rather than a path.
///
/// A one-letter scheme is a Windows drive letter (`C:\Users\…`), not a URL,
/// and the installer's `"%EXEPATH%" "%1"` hands us exactly that shape.
fn url_scheme(argument: &str) -> Option<&str> {
    let (scheme, _) = argument.split_once(':')?;
    let mut characters = scheme.chars();
    if !characters.next()?.is_ascii_alphabetic() || scheme.len() < 2 {
        return None;
    }
    characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.'))
        .then_some(scheme)
}

/// Whether `raw` claims our own URL scheme, without parsing the rest.
fn has_our_scheme(raw: &str) -> bool {
    raw.get(..URL_SCHEME.len() + 3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&format!("{URL_SCHEME}://")))
}

/// Log a rejection without any part of the input.
fn reject(reason: &'static str) {
    tracing::debug!(reason, "rejected an activation request");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_link_parses() {
        let id = RecordingId::new();
        let activation = from_url(&format!("plaudon://recording/{id}")).expect("recording link");
        assert_eq!(activation, Activation::Recording(id));
        assert_eq!(
            activation.notification_target(),
            NotificationTarget::Recording(id)
        );
    }

    #[test]
    fn a_job_link_parses() {
        let id = JobId::new();
        let activation = from_url(&format!("plaudon://job/{id}")).expect("job link");
        assert_eq!(activation, Activation::Job(id));
        assert_eq!(
            activation.notification_target(),
            NotificationTarget::Job(id)
        );
    }

    #[test]
    fn the_settings_link_parses_with_and_without_a_trailing_slash() {
        assert_eq!(from_url("plaudon://settings"), Some(Activation::Settings));
        assert_eq!(from_url("plaudon://settings/"), Some(Activation::Settings));
        assert_eq!(from_url("PLAUDON://SETTINGS"), Some(Activation::Settings));
    }

    #[test]
    fn every_activation_round_trips_through_its_url() {
        for activation in [
            Activation::Recording(RecordingId::new()),
            Activation::Job(JobId::new()),
            Activation::Settings,
        ] {
            assert_eq!(from_url(&activation.url()), Some(activation));
        }
    }

    #[test]
    fn every_activation_round_trips_through_its_notification_token() {
        for activation in [
            Activation::Recording(RecordingId::new()),
            Activation::Job(JobId::new()),
            Activation::Settings,
        ] {
            let token = activation.activation_token();
            assert_eq!(from_notification_token(&token), Some(activation));
            assert_eq!(
                Activation::from(activation.notification_target()),
                activation
            );
        }
    }

    #[test]
    fn a_foreign_scheme_is_refused() {
        let id = RecordingId::new();
        for raw in [
            format!("https://recording/{id}"),
            format!("plaudonx://recording/{id}"),
            format!("file://recording/{id}"),
            format!("javascript://recording/{id}"),
        ] {
            assert_eq!(from_url(&raw), None, "{raw} must be refused");
        }
    }

    #[test]
    fn an_unknown_host_is_refused() {
        let id = RecordingId::new();
        for raw in [
            format!("plaudon://recordings/{id}"),
            format!("plaudon://transcript/{id}"),
            format!("plaudon://{id}"),
            "plaudon://".to_owned(),
            "plaudon:///settings".to_owned(),
        ] {
            assert_eq!(from_url(&raw), None, "{raw} must be refused");
        }
    }

    #[test]
    fn a_malformed_id_is_refused() {
        for raw in [
            "plaudon://recording/not-a-uuid",
            "plaudon://recording/",
            "plaudon://recording",
            "plaudon://job/0000",
            "plaudon://job/../../etc/passwd",
            "plaudon://recording/%2e%2e%2f",
        ] {
            assert_eq!(from_url(raw), None, "{raw} must be refused");
        }
    }

    #[test]
    fn extra_path_segments_credentials_queries_and_fragments_are_refused() {
        let id = RecordingId::new();
        for raw in [
            format!("plaudon://recording/{id}/export"),
            format!("plaudon://recording/{id}?then=delete"),
            format!("plaudon://recording/{id}#fragment"),
            format!("plaudon://user:pass@recording/{id}"),
            format!("plaudon://recording:8080/{id}"),
            format!("plaudon://settings/{id}"),
        ] {
            assert_eq!(from_url(&raw), None, "{raw} must be refused");
        }
    }

    #[test]
    fn garbage_is_refused() {
        for raw in ["", "   ", "plaudon", "://", "not a url at all", "\u{0}"] {
            assert_eq!(from_url(raw), None, "{raw:?} must be refused");
        }
    }

    #[test]
    fn an_unknown_notification_token_is_refused() {
        for token in [
            "",
            "settings:extra",
            "recording",
            "recording:not-a-uuid",
            "transcript:00000000-0000-0000-0000-000000000000",
            "job:",
        ] {
            assert_eq!(
                from_notification_token(token),
                None,
                "{token:?} must be refused"
            );
        }
    }

    #[test]
    fn a_launch_argument_vector_yields_at_most_one_activation() {
        let id = RecordingId::new();
        let link = format!("plaudon://recording/{id}");
        let no_roots: Vec<PathBuf> = Vec::new();
        assert_eq!(
            launch_request(["plaudon.exe".to_owned(), link.clone()], &no_roots),
            Some(LaunchRequest::Activate(Activation::Recording(id)))
        );
        assert_eq!(
            launch_request(
                [
                    "plaudon.exe".to_owned(),
                    "--flag".to_owned(),
                    "/Users/x/a.wav".to_owned(),
                    link,
                ],
                &no_roots
            ),
            Some(LaunchRequest::Activate(Activation::Recording(id))),
            "a link anywhere in the vector wins over a document"
        );
        assert_eq!(
            launch_request(["plaudon.exe".to_owned()], &no_roots),
            None,
            "a plain launch asks for nothing"
        );
        assert_eq!(
            launch_request(Vec::<String>::new(), &no_roots),
            None,
            "an empty argument vector asks for nothing"
        );
        assert!(
            !matches!(
                launch_request(
                    [
                        "plaudon.exe".to_owned(),
                        "https://example.com/".to_owned(),
                        "file:///etc/passwd".to_owned(),
                    ],
                    &no_roots
                ),
                Some(LaunchRequest::Activate(_))
            ),
            "only our own scheme may navigate the app"
        );
    }

    #[test]
    fn the_programs_own_name_is_never_treated_as_a_link() {
        let id = RecordingId::new();
        // argv[0] is the executable; a hostile launcher that sets argv[0] to a
        // link must not navigate the app.
        assert_eq!(
            launch_request([format!("plaudon://recording/{id}")], &[]),
            None
        );
    }

    #[test]
    fn the_url_form_matches_the_documented_shape() {
        let id = RecordingId::new();
        assert_eq!(
            Activation::Recording(id).url(),
            format!("plaudon://recording/{id}")
        );
        assert_eq!(Activation::Settings.url(), "plaudon://settings");
        assert_eq!(Activation::Settings.activation_token(), "settings");
    }

    /// `tempfile` hands back `/var/folders/…` on macOS, which is itself a
    /// symlink to `/private/var/…`; an approved root has to be canonical or
    /// every path "escapes" it.
    fn roots(dir: &std::path::Path) -> Vec<PathBuf> {
        vec![std::fs::canonicalize(dir).expect("canonical root")]
    }

    fn fixture(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"RIFF").expect("write fixture");
        path
    }

    #[test]
    fn a_double_clicked_archive_arrives_as_an_archive_to_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = fixture(dir.path(), "library.plaudon");
        let request = launch_request(
            ["plaudon-egui".to_owned(), archive.display().to_string()],
            &roots(dir.path()),
        );
        let Some(LaunchRequest::Open(classified)) = request else {
            panic!("a claimed document type opens");
        };
        assert_eq!(classified.archives.len(), 1);
        assert!(classified.audio.is_empty());
        assert!(classified.rejected.is_empty());
    }

    #[test]
    fn an_audio_argument_is_validated_the_way_a_drop_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let audio = fixture(dir.path(), "meeting.wav");
        let request = launch_request(
            ["plaudon-egui".to_owned(), audio.display().to_string()],
            &roots(dir.path()),
        );
        let Some(LaunchRequest::Open(classified)) = request else {
            panic!("audio opens");
        };
        assert_eq!(classified.audio.len(), 1);
    }

    #[test]
    fn a_path_outside_every_approved_root_is_refused_rather_than_opened() {
        let approved = tempfile::tempdir().expect("tempdir");
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let escaped = fixture(elsewhere.path(), "library.plaudon");
        let request = launch_request(
            ["plaudon-egui".to_owned(), escaped.display().to_string()],
            &roots(approved.path()),
        );
        let Some(LaunchRequest::Open(classified)) = request else {
            panic!("the refusal is reported, not dropped");
        };
        assert!(classified.is_empty(), "nothing outside a root is accepted");
        assert_eq!(
            classified.rejected.first().map(|drop| drop.reason),
            Some(crate::dragdrop::DropRejection::OutsideApprovedRoots)
        );
    }

    #[test]
    fn a_deep_link_wins_over_a_path_in_the_same_argument_vector() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = fixture(dir.path(), "library.plaudon");
        assert_eq!(
            launch_request(
                [
                    "plaudon-egui".to_owned(),
                    archive.display().to_string(),
                    "plaudon://settings".to_owned(),
                ],
                &roots(dir.path()),
            ),
            Some(LaunchRequest::Activate(Activation::Settings))
        );
    }

    #[test]
    fn junk_arguments_ask_for_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        for junk in [
            "-psn_0_1234567",
            "--flag",
            "",
            "https://example.invalid/recording/1",
            "mailto:someone@example.invalid",
            "plaudon://recording/not-a-uuid",
            "plaudon://moon",
        ] {
            assert_eq!(
                launch_request(["plaudon-egui", junk], &roots(dir.path())),
                None,
                "{junk} must not open anything"
            );
        }
        assert_eq!(
            launch_request(["plaudon-egui"], &roots(dir.path())),
            None,
            "a plain launch opens nothing"
        );
    }

    #[test]
    fn a_windows_drive_letter_is_a_path_and_not_a_url_scheme() {
        assert_eq!(url_scheme("C:\\Users\\sam\\library.plaudon"), None);
        assert_eq!(url_scheme("file:///tmp/library.plaudon"), Some("file"));
        assert_eq!(url_scheme("/tmp/library.plaudon"), None);
        assert_eq!(
            path_argument("C:\\Users\\sam\\library.plaudon"),
            Some(PathBuf::from("C:\\Users\\sam\\library.plaudon"))
        );
    }

    #[test]
    fn a_running_application_takes_both_halves_of_the_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = fixture(dir.path(), "library.plaudon");
        let url = Url::from_file_path(&archive).expect("an absolute path is a file url");
        let Some(LaunchRequest::Open(classified)) = open_request(url.as_str(), &roots(dir.path()))
        else {
            panic!("a file url opens the document");
        };
        assert_eq!(classified.archives.len(), 1);
        assert_eq!(
            open_request("plaudon://settings", &roots(dir.path())),
            Some(LaunchRequest::Activate(Activation::Settings))
        );
        assert_eq!(
            open_request("plaudon://recording/not-a-uuid", &roots(dir.path())),
            None,
            "a malformed link of our own scheme is never retried as a path"
        );
    }
}
