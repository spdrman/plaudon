//! Deep links and notification activations, routed into one value.
//!
//! Two things can ask a running Plaudon to show something:
//!
//! * a `plaudon://` URL — from the OS (`open plaudon://settings`), from a
//!   second launch whose argument vector carries the link, or from a browser,
//!   and
//! * a notification click, which hands back the opaque activation token
//!   [`plaudon_notifications::NotificationTarget::activation_token`] wrote
//!   (migration plan §4.5 row J15).
//!
//! Both become an [`Activation`], and the shell reacts to it the same way in
//! both cases: raise and focus the window
//! (`ViewportCommand::Minimized(false)` then `ViewportCommand::Focus`) and
//! publish `ui.activation_requested` through
//! [`plaudon_app::notify::publish_activation`] with
//! [`Activation::notification_target`].
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

use plaudon_domain::ids::{JobId, RecordingId};
use plaudon_notifications::NotificationTarget;
use url::Url;

use crate::associations::URL_SCHEME;

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

/// Find a deep link in a process argument vector.
///
/// Windows and most Linux desktops deliver a URL activation by launching the
/// application with the link as an argument; the running instance receives it
/// through a single-instance handshake owned by the shell. The first argument
/// that parses as a valid activation wins; everything else — including a file
/// path, which is handled by [`crate::dragdrop`] — is ignored.
#[must_use]
pub fn from_launch_arguments<I, S>(arguments: I) -> Option<Activation>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    arguments
        .into_iter()
        .skip(1)
        .find_map(|argument| from_url_quiet(argument.as_ref()))
}

/// [`from_url`] without the rejection log: scanning an argument vector hits
/// non-URL arguments by design, and logging each one would be noise.
fn from_url_quiet(raw: &str) -> Option<Activation> {
    let prefix = raw.get(..URL_SCHEME.len() + 3)?;
    if !prefix.eq_ignore_ascii_case(&format!("{URL_SCHEME}://")) {
        return None;
    }
    from_url(raw)
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
        assert_eq!(
            from_launch_arguments(["plaudon.exe".to_owned(), link.clone()]),
            Some(Activation::Recording(id))
        );
        assert_eq!(
            from_launch_arguments([
                "plaudon.exe".to_owned(),
                "--flag".to_owned(),
                "/Users/x/a.wav".to_owned(),
                link,
            ]),
            Some(Activation::Recording(id))
        );
        assert_eq!(
            from_launch_arguments(["plaudon.exe".to_owned()]),
            None,
            "a plain launch is not an activation"
        );
        assert_eq!(
            from_launch_arguments(Vec::<String>::new()),
            None,
            "an empty argument vector is not an activation"
        );
        assert_eq!(
            from_launch_arguments([
                "plaudon.exe".to_owned(),
                "https://example.com/".to_owned(),
                "file:///etc/passwd".to_owned(),
            ]),
            None,
            "only our own scheme may navigate the app"
        );
    }

    #[test]
    fn the_programs_own_name_is_never_treated_as_a_link() {
        let id = RecordingId::new();
        // argv[0] is the executable; a hostile launcher that sets argv[0] to a
        // link must not navigate the app.
        assert_eq!(
            from_launch_arguments([format!("plaudon://recording/{id}")]),
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
}
