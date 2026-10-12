//! The OS clipboard behind a port, with an honest failure value.
//!
//! "Copy transcript", "copy summary" and "copy diagnostics id" all go through
//! [`Clipboard`]. The real adapter is [`NativeClipboard`], built on `arboard`.
//!
//! A clipboard is not always there: a headless CI runner, an SSH session with
//! no display or a locked-down Wayland compositor can all refuse to open one.
//! That case is modelled, not hidden:
//!
//! * [`NativeClipboard::new`] returns [`ClipboardError::Unavailable`] instead
//!   of panicking, and
//! * [`UnavailableClipboard`] is the adapter the shell falls back to, so that
//!   "Copy" reports a real error in the UI rather than appearing to work.
//!
//! Nothing in this module logs clipboard content, and no error value carries
//! any of it: the reason strings come from the platform backend and describe
//! the *connection*, never the data.

use std::fmt;
use std::sync::Mutex;

use crate::lock;

/// Why a clipboard operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    /// No clipboard could be opened at all (headless session, no display).
    #[error("the clipboard is unavailable: {reason}")]
    Unavailable {
        /// Platform reason, free of user content.
        reason: String,
    },
    /// The clipboard is there but holds no text.
    #[error("the clipboard holds no text")]
    Empty,
    /// The clipboard is there and the operation failed anyway.
    #[error("the clipboard operation failed: {reason}")]
    Failed {
        /// Platform reason, free of user content.
        reason: String,
    },
}

/// Read and write the system clipboard's plain-text content.
pub trait Clipboard: Send + Sync + fmt::Debug {
    /// Replace the clipboard's text content.
    fn set_text(&self, text: &str) -> Result<(), ClipboardError>;

    /// Read the clipboard's text content.
    fn text(&self) -> Result<String, ClipboardError>;
}

/// The real clipboard, via `arboard`.
///
/// The handle is opened once and kept: on X11 the clipboard content belongs to
/// the process that owns the selection, so dropping the handle after every
/// copy would drop the data the user just copied.
pub struct NativeClipboard {
    inner: Mutex<arboard::Clipboard>,
}

impl fmt::Debug for NativeClipboard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The handle holds whatever the user last copied; it is never
        // formatted into a log line.
        f.debug_struct("NativeClipboard").finish_non_exhaustive()
    }
}

impl NativeClipboard {
    /// Open the system clipboard.
    ///
    /// # Errors
    ///
    /// [`ClipboardError::Unavailable`] when the session has no clipboard the
    /// platform backend can attach to.
    pub fn new() -> Result<Self, ClipboardError> {
        match arboard::Clipboard::new() {
            Ok(inner) => Ok(Self {
                inner: Mutex::new(inner),
            }),
            Err(err) => Err(ClipboardError::Unavailable {
                reason: err.to_string(),
            }),
        }
    }
}

impl Clipboard for NativeClipboard {
    fn set_text(&self, text: &str) -> Result<(), ClipboardError> {
        lock(&self.inner)
            .set_text(text.to_owned())
            .map_err(|err| map_error(&err))
    }

    fn text(&self) -> Result<String, ClipboardError> {
        lock(&self.inner).get_text().map_err(|err| map_error(&err))
    }
}

/// Translate a backend error without ever copying clipboard content into it.
fn map_error(err: &arboard::Error) -> ClipboardError {
    match err {
        arboard::Error::ContentNotAvailable => ClipboardError::Empty,
        arboard::Error::ClipboardNotSupported => ClipboardError::Unavailable {
            reason: err.to_string(),
        },
        other => ClipboardError::Failed {
            reason: other.to_string(),
        },
    }
}

/// The adapter for a session with no clipboard.
///
/// Every operation fails with the reason the platform gave when the clipboard
/// could not be opened. This is deliberately **not** a silent no-op: a user
/// who presses "Copy" in a headless session is told it did not happen.
#[derive(Debug, Clone)]
pub struct UnavailableClipboard {
    reason: String,
}

impl UnavailableClipboard {
    /// Build the fallback adapter with the reason the clipboard is missing.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// The reason this host has no clipboard.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }

    fn error(&self) -> ClipboardError {
        ClipboardError::Unavailable {
            reason: self.reason.clone(),
        }
    }
}

impl Clipboard for UnavailableClipboard {
    fn set_text(&self, _text: &str) -> Result<(), ClipboardError> {
        Err(self.error())
    }

    fn text(&self) -> Result<String, ClipboardError> {
        Err(self.error())
    }
}

/// The best clipboard this session can offer.
///
/// Returns [`NativeClipboard`] when one can be opened and
/// [`UnavailableClipboard`] carrying the platform's reason when it cannot, so
/// the shell has one line of wiring and no `unwrap`.
#[must_use]
pub fn host_clipboard() -> Box<dyn Clipboard> {
    match NativeClipboard::new() {
        Ok(clipboard) => Box::new(clipboard),
        Err(ClipboardError::Unavailable { reason }) => Box::new(UnavailableClipboard::new(reason)),
        Err(other) => Box::new(UnavailableClipboard::new(other.to_string())),
    }
}

/// In-memory clipboard for tests.
///
/// Holds exactly one text value, or fails every operation with a scripted
/// error so a caller's error path can be exercised.
#[derive(Debug, Default)]
pub struct FakeClipboard {
    text: Mutex<Option<String>>,
    failure: Option<ClipboardError>,
}

impl FakeClipboard {
    /// An empty, working fake.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A fake pre-loaded with clipboard text.
    #[must_use]
    pub fn with_text(text: impl Into<String>) -> Self {
        Self {
            text: Mutex::new(Some(text.into())),
            failure: None,
        }
    }

    /// A fake whose every operation fails with `failure`.
    #[must_use]
    pub fn failing(failure: ClipboardError) -> Self {
        Self {
            text: Mutex::new(None),
            failure: Some(failure),
        }
    }

    /// What the fake currently holds, without going through the port.
    #[must_use]
    pub fn peek(&self) -> Option<String> {
        lock(&self.text).clone()
    }
}

impl Clipboard for FakeClipboard {
    fn set_text(&self, text: &str) -> Result<(), ClipboardError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        *lock(&self.text) = Some(text.to_owned());
        Ok(())
    }

    fn text(&self) -> Result<String, ClipboardError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        lock(&self.text).clone().ok_or(ClipboardError::Empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_round_trips_text() {
        let clipboard = FakeClipboard::new();
        assert_eq!(clipboard.text(), Err(ClipboardError::Empty));
        clipboard.set_text("ok").expect("set");
        assert_eq!(clipboard.text().expect("get"), "ok");
        assert_eq!(clipboard.peek().as_deref(), Some("ok"));
        clipboard.set_text("replaced").expect("set");
        assert_eq!(clipboard.text().expect("get"), "replaced");
    }

    #[test]
    fn a_preloaded_fake_reports_its_text() {
        let clipboard = FakeClipboard::with_text("seeded");
        assert_eq!(clipboard.text().expect("get"), "seeded");
    }

    #[test]
    fn an_unopenable_clipboard_reports_an_error_on_every_operation() {
        let clipboard = UnavailableClipboard::new("no display server");
        let expected = ClipboardError::Unavailable {
            reason: "no display server".to_owned(),
        };
        assert_eq!(clipboard.set_text("anything"), Err(expected.clone()));
        assert_eq!(clipboard.text(), Err(expected));
        assert_eq!(clipboard.reason(), "no display server");
        assert!(clipboard
            .set_text("anything")
            .expect_err("must fail")
            .to_string()
            .contains("no display server"));
    }

    #[test]
    fn a_scripted_failure_reaches_the_caller() {
        let failure = ClipboardError::Failed {
            reason: "occupied".to_owned(),
        };
        let clipboard = FakeClipboard::failing(failure.clone());
        assert_eq!(clipboard.set_text("x"), Err(failure.clone()));
        assert_eq!(clipboard.text(), Err(failure));
        assert_eq!(clipboard.peek(), None, "a failing set must store nothing");
    }

    #[test]
    fn the_port_is_usable_as_a_trait_object() {
        let clipboard: Box<dyn Clipboard> = Box::new(FakeClipboard::new());
        clipboard.set_text("via dyn").expect("set");
        assert_eq!(clipboard.text().expect("get"), "via dyn");

        let unavailable: Box<dyn Clipboard> = Box::new(UnavailableClipboard::new("headless"));
        assert!(unavailable.text().is_err());
    }

    #[test]
    fn backend_errors_map_onto_the_port_vocabulary() {
        assert_eq!(
            map_error(&arboard::Error::ContentNotAvailable),
            ClipboardError::Empty
        );
        assert!(matches!(
            map_error(&arboard::Error::ClipboardNotSupported),
            ClipboardError::Unavailable { .. }
        ));
        assert!(matches!(
            map_error(&arboard::Error::ClipboardOccupied),
            ClipboardError::Failed { .. }
        ));
    }

    /// Opening the host clipboard is allowed to fail (headless CI) and allowed
    /// to succeed (a developer's desktop); what it may never do is panic or
    /// report success without a usable handle. The test deliberately neither
    /// reads nor writes the real pasteboard: a test must not clobber the
    /// user's clipboard, and must not pull their data into a test process.
    #[test]
    fn opening_the_native_clipboard_either_works_or_reports_why() {
        match NativeClipboard::new() {
            Ok(clipboard) => {
                assert!(format!("{clipboard:?}").starts_with("NativeClipboard"));
            }
            Err(err) => {
                assert!(
                    matches!(err, ClipboardError::Unavailable { .. }),
                    "a clipboard that cannot be opened must report Unavailable, got {err}"
                );
                assert!(!err.to_string().is_empty(), "the reason must be stated");
            }
        }

        // The fallback wiring is total: it always yields a usable port.
        let fallback = host_clipboard();
        assert!(format!("{fallback:?}").contains("Clipboard"));
    }
}
