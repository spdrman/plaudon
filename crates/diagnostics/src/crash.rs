//! Crash reporting that cannot carry credentials or audio.
//!
//! Spec App. B.7: "prohibit raw credentials/audio in crash reports". The hook
//! installed here records exactly three things — the redacted panic payload,
//! the panic location and (only when `RUST_BACKTRACE` is set) the redacted
//! backtrace. It never reaches for request bodies, decoded audio buffers or
//! BLE frames, and it deliberately does **not** chain the default panic hook,
//! because the default hook prints the raw payload straight to stderr.

use std::any::Any;
use std::io::Write;
use std::panic::PanicHookInfo;

use crate::logging::LoggingHandle;
use crate::redact::Redactor;

/// Render a panic payload as a redacted string.
///
/// Handles the two payload types the standard library produces (`&'static str`
/// from `panic!("literal")` and `String` from a formatted panic) and reports an
/// opaque description for anything else, because an arbitrary `Any` can only be
/// rendered by code that knows its type — and guessing would risk printing a
/// struct full of user data.
///
/// Redaction here uses the shape patterns of the process-wide
/// [`Redactor::shared`]; the installed hook additionally re-redacts through the
/// handle's redactor, which also knows the registered live secrets.
#[must_use]
pub fn redact_panic_payload(payload: &dyn Any) -> String {
    let raw: &str = if let Some(message) = payload.downcast_ref::<&'static str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        return "<non-string panic payload>".to_owned();
    };
    Redactor::shared().redact(raw).into_owned()
}

/// Install the redacting panic hook.
///
/// Replaces any previously installed hook. The redacted crash line is written
/// directly to the log file through `handle` (so it survives even if the global
/// subscriber is unusable during unwinding) and mirrored to stderr.
pub fn install_crash_hook(handle: &LoggingHandle) {
    let handle = handle.clone();
    std::panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        let line = crash_line(&handle, info);
        let _ = handle.write_diagnostic_line(&line);
        let _ = handle.flush();
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{line}");
    }));
}

fn crash_line(handle: &LoggingHandle, info: &PanicHookInfo<'_>) -> String {
    let payload = redact_panic_payload(info.payload());
    let location = info.location().map_or_else(
        || "<unknown location>".to_owned(),
        |l| format!("{}:{}:{}", l.file(), l.line(), l.column()),
    );
    let backtrace = std::backtrace::Backtrace::capture();
    let backtrace_text = match backtrace.status() {
        std::backtrace::BacktraceStatus::Captured => {
            let rendered = backtrace.to_string();
            handle.redactor().redact(&rendered).replace('\n', " | ")
        }
        _ => "<backtrace disabled; set RUST_BACKTRACE=1>".to_owned(),
    };
    let payload = handle.redactor().redact(&payload).into_owned();
    format!("PANIC location={location} payload={payload} backtrace={backtrace_text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_payloads_are_redacted() {
        let payload: Box<dyn Any> = Box::new(String::from(
            "failed with key sk-or-v1-0123456789abcdefghij",
        ));
        let rendered = redact_panic_payload(payload.as_ref());
        assert!(!rendered.contains("sk-or-v1-"));
        assert!(rendered.contains("<redacted:openrouter_key>"));
    }

    #[test]
    fn static_str_payloads_are_handled() {
        let payload: Box<dyn Any> = Box::new("plain failure");
        assert_eq!(redact_panic_payload(payload.as_ref()), "plain failure");
    }

    #[test]
    fn non_string_payloads_are_opaque() {
        let payload: Box<dyn Any> = Box::new(vec![0u8, 1, 2]);
        assert_eq!(
            redact_panic_payload(payload.as_ref()),
            "<non-string panic payload>"
        );
    }
}
