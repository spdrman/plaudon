//! The one place an [`AiOutput`] becomes shell work.
//!
//! The AI panels, the jobs screen, the activity log and the recording detail
//! all return [`AiIntent`]s rather than touching the shell: that is what lets
//! a kittest drive them with no dispatcher. Every host of those panels then
//! needs the same translation — dispatch a command, navigate where a §5.4
//! corrective control points, open a file dialog — and writing it twice is
//! how two screens end up sending the user to two different places for the
//! same button.
//!
//! So it is written once, here, and it is **total**: every [`AiIntent`] and
//! every [`ControlIntent`] has exactly one arm, which makes a new variant a
//! compile error rather than a silently dead control.

use plaudon_app::command::AppCommand;
use plaudon_app::dto::{ExportMediaRequest, ExportRecordingRequest};
use plaudon_app::job_presentation::ControlIntent;

use crate::ai::{AiIntent, AiOutput, SeekRequest};
use crate::widgets::toast::ToastTone;

use super::route::Route;
use super::screen::ScreenCtx;

/// The wall clock the elapsed labels are measured against.
///
/// `job_presentation` takes "now" as an argument rather than reading a
/// clock, so that the words it produces are testable. Somewhere the real
/// clock still has to be read, and this is that one place: a host that
/// wants a fixed clock passes its own value instead of calling this.
///
/// A clock before the Unix epoch is not a time this application can be
/// running at; it saturates to zero rather than producing a negative
/// elapsed figure.
#[must_use]
pub fn wall_clock_ms() -> i128 {
    time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000
}

/// Apply one frame of panel output to the shell.
///
/// Returns the seek requests, which only the host that owns a player can
/// carry out; a host with no player drops them, and dropping them is visible
/// here rather than hidden inside this function.
pub fn apply_ai_output(out: AiOutput, cx: &mut ScreenCtx<'_>) -> Vec<SeekRequest> {
    let mut seeks = Vec::new();
    for intent in out.intents {
        match intent {
            AiIntent::Command(command) => {
                cx.send(command);
            }
            AiIntent::Seek(request) => seeks.push(request),
            AiIntent::Navigate(control) => navigate(control, cx),
            AiIntent::PickTranscriptExportPath {
                recording_id,
                revision_id,
                format,
                suggested_file_name,
            } => {
                // The dialog blocks on the operating system, so it is opened
                // after the frame the control was clicked in, never inside
                // it.
                if let Some(path) = (cx.hooks().pick_save_path)(&suggested_file_name) {
                    cx.send(AppCommand::ExportRecording(ExportRecordingRequest {
                        recording_id,
                        revision_id,
                        format,
                        destination: path.to_string_lossy().into_owned(),
                    }));
                }
            }
            AiIntent::PickMediaExportDirectory {
                recording_id,
                include_sanitized_derivative,
            } => {
                if let Some(directory) = (cx.hooks().pick_folder)() {
                    cx.send(AppCommand::ExportMedia(ExportMediaRequest {
                        recording_id,
                        media_asset_id: None,
                        destination_directory: directory.to_string_lossy().into_owned(),
                        include_sanitized_derivative,
                    }));
                }
            }
        }
    }
    seeks
}

/// Where a §5.4 corrective control sends the user.
///
/// Four of the intents are not navigations at all: they are things this
/// build has no screen for. They raise a toast saying so rather than moving
/// the user somewhere that cannot help, because a control that silently does
/// nothing is worse than one that says what it cannot do.
fn navigate(control: ControlIntent, cx: &mut ScreenCtx<'_>) {
    match control {
        // The retry and queue decisions are carried out by the job card
        // itself, which turns them into `retry_job` / `cancel_job`
        // commands; by the time one reaches here it has already been
        // dispatched, so there is nothing left to navigate to.
        ControlIntent::Retry | ControlIntent::RetryWithCostAck | ControlIntent::KeepQueued => {}
        ControlIntent::ChooseLocalEngine
        | ControlIntent::OpenAiSettings
        | ControlIntent::ReviewConsent
        | ControlIntent::ReviewLocalOnlyMode
        | ControlIntent::OpenStorageSettings
        | ControlIntent::OpenPermissionSettings
        | ControlIntent::OpenSettings => cx.navigate(Route::Settings),
        ControlIntent::OpenPlaudConnect
        | ControlIntent::RefreshSourceLink
        | ControlIntent::OpenCliSetup => cx.navigate(Route::SettingsSources),
        ControlIntent::OpenDeviceDiagnostics | ControlIntent::ChargeDevice => {
            cx.navigate(Route::Devices);
        }
        ControlIntent::RetryImport => cx.navigate(Route::Library),
        ControlIntent::OpenDiagnostics => cx.toast(
            ToastTone::Info,
            "Diagnostics are written to the log file beside your library.",
            None,
        ),
        ControlIntent::FreeSpace => cx.navigate(Route::Settings),
    }
}
