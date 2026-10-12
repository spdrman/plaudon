//! The entire IPC surface: sixteen narrow commands and nothing else.
//!
//! Spec §15.1: no generic shell execution, no HTTP proxy and no generic
//! filesystem command is exposed to JavaScript. Every command takes one
//! `request` struct, validates it in Rust and returns either a typed response
//! or the §15.4 error envelope. The allowlist test at the bottom of this file
//! fails if a handler outside the published list is ever registered.

pub mod import;
pub mod jobs;
pub mod library;
pub mod settings;
pub mod transcript;
pub mod search;
pub mod speakers;

use tauri::State;

use crate::dto::{
    ActivityPage, CancelJobRequest, Capabilities, CompleteOnboardingRequest,
    ConfirmSpeakerRequest, EmptyRequest, ExportRecordingRequest, ExportRecordingResponse,
    GetRecordingRequest, GetTranscriptRequest, ImportFilesRequest, ImportFilesResponse, JobPage,
    JobSummary, ListActivityRequest, ListJobsRequest, ListRecordingsRequest,
    ListSpeakersRequest, ListSpeakersResponse, LocalOnlyModeResponse,
    NotificationSettingsResponse, Preferences, RecordingDetail, RecordingPage,
    RenameSpeakerRequest, ResetSpeakerRequest, RetryJobRequest, SaveTranscriptRevisionRequest,
    SaveTranscriptRevisionResponse, SearchLibraryRequest, SearchLibraryResponse,
    SetLocalOnlyModeRequest, SetNotificationSettingsRequest, SetOpenRouterKeyRequest,
    SpeakerOperationResponse, StatusResponse, TranscriptPage,
};
use crate::error::{to_command_result, CommandResult};
use crate::AppState;

/// The command names this build exposes, in the order of the §15.2 table
/// followed by the two additions recorded in ADR-0020.
pub const COMMAND_ALLOWLIST: &[&str] = &[
    "get_capabilities",
    "list_recordings",
    "import_files",
    "get_recording",
    "get_transcript",
    "save_transcript_revision",
    "list_jobs",
    "cancel_job",
    "retry_job",
    "export_recording",
    "set_notification_settings",
    "set_local_only_mode",
    "set_openrouter_key",
    "search_library",
    "list_speakers",
    "rename_speaker",
    "confirm_speaker",
    "reset_speaker",
    "remove_openrouter_key",
    "list_activity",
    "complete_onboarding",
];

/// Every command of the IPC surface, for [`tauri::Builder::invoke_handler`].
// Not `#[must_use]`: the returned closure is already a must-use type.
pub fn handler<R: tauri::Runtime>() -> impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static
{
    tauri::generate_handler![
        get_capabilities,
        list_recordings,
        import_files,
        get_recording,
        get_transcript,
        save_transcript_revision,
        list_jobs,
        cancel_job,
        retry_job,
        export_recording,
        set_notification_settings,
        set_local_only_mode,
        set_openrouter_key,
        remove_openrouter_key,
        list_activity,
        search_library,
        list_speakers,
        rename_speaker,
        confirm_speaker,
        reset_speaker,
        complete_onboarding
    ]
}

#[tauri::command]
pub async fn get_capabilities(
    state: State<'_, AppState>,
    #[allow(unused_variables)] request: Option<EmptyRequest>,
) -> CommandResult<Capabilities> {
    to_command_result(settings::get_capabilities(&state.ports).await)
}

#[tauri::command]
pub async fn list_recordings(
    state: State<'_, AppState>,
    request: ListRecordingsRequest,
) -> CommandResult<RecordingPage> {
    to_command_result(library::list_recordings(&state.ports, request).await)
}

#[tauri::command]
pub async fn import_files(
    state: State<'_, AppState>,
    request: ImportFilesRequest,
) -> CommandResult<ImportFilesResponse> {
    to_command_result(import::import_files(&state.ports, request).await)
}

#[tauri::command]
pub async fn get_recording(
    state: State<'_, AppState>,
    request: GetRecordingRequest,
) -> CommandResult<RecordingDetail> {
    to_command_result(library::get_recording(&state.ports, request).await)
}

#[tauri::command]
pub async fn get_transcript(
    state: State<'_, AppState>,
    request: GetTranscriptRequest,
) -> CommandResult<TranscriptPage> {
    to_command_result(transcript::get_transcript(&state.ports, request).await)
}

#[tauri::command]
pub async fn save_transcript_revision(
    state: State<'_, AppState>,
    request: SaveTranscriptRevisionRequest,
) -> CommandResult<SaveTranscriptRevisionResponse> {
    to_command_result(transcript::save_transcript_revision(&state.ports, request).await)
}

#[tauri::command]
pub async fn list_jobs(
    state: State<'_, AppState>,
    request: ListJobsRequest,
) -> CommandResult<JobPage> {
    to_command_result(jobs::list_jobs(&state.ports, request).await)
}

#[tauri::command]
pub async fn cancel_job(
    state: State<'_, AppState>,
    request: CancelJobRequest,
) -> CommandResult<JobSummary> {
    to_command_result(jobs::cancel_job(&state.ports, request).await)
}

#[tauri::command]
pub async fn retry_job(
    state: State<'_, AppState>,
    request: RetryJobRequest,
) -> CommandResult<JobSummary> {
    to_command_result(jobs::retry_job(&state.ports, request).await)
}

#[tauri::command]
pub async fn export_recording(
    state: State<'_, AppState>,
    request: ExportRecordingRequest,
) -> CommandResult<ExportRecordingResponse> {
    to_command_result(jobs::export_recording(&state.ports, request).await)
}

#[tauri::command]
pub async fn set_notification_settings(
    state: State<'_, AppState>,
    request: SetNotificationSettingsRequest,
) -> CommandResult<NotificationSettingsResponse> {
    to_command_result(settings::set_notification_settings(&state.ports, request).await)
}

#[tauri::command]
pub async fn set_local_only_mode(
    state: State<'_, AppState>,
    request: SetLocalOnlyModeRequest,
) -> CommandResult<LocalOnlyModeResponse> {
    to_command_result(settings::set_local_only_mode(&state.ports, request).await)
}

#[tauri::command]
pub async fn set_openrouter_key(
    state: State<'_, AppState>,
    request: SetOpenRouterKeyRequest,
) -> CommandResult<StatusResponse> {
    to_command_result(settings::set_openrouter_key(&state.ports, request).await)
}

#[tauri::command]
pub async fn remove_openrouter_key(
    state: State<'_, AppState>,
    #[allow(unused_variables)] request: Option<EmptyRequest>,
) -> CommandResult<StatusResponse> {
    to_command_result(settings::remove_openrouter_key(&state.ports).await)
}

#[tauri::command]
pub async fn list_activity(
    state: State<'_, AppState>,
    request: ListActivityRequest,
) -> CommandResult<ActivityPage> {
    to_command_result(settings::list_activity(&state.ports, request).await)
}

#[tauri::command]
pub async fn complete_onboarding(
    state: State<'_, AppState>,
    request: CompleteOnboardingRequest,
) -> CommandResult<Preferences> {
    to_command_result(settings::complete_onboarding(&state.ports, request).await)
}


#[tauri::command]
pub async fn search_library(
    state: State<'_, AppState>,
    request: SearchLibraryRequest,
) -> CommandResult<SearchLibraryResponse> {
    to_command_result(search::search_library(&state.ports, request).await)
}

#[tauri::command]
pub async fn list_speakers(
    state: State<'_, AppState>,
    request: ListSpeakersRequest,
) -> CommandResult<ListSpeakersResponse> {
    to_command_result(speakers::list_speakers(&state.ports, request).await)
}

#[tauri::command]
pub async fn rename_speaker(
    state: State<'_, AppState>,
    request: RenameSpeakerRequest,
) -> CommandResult<SpeakerOperationResponse> {
    to_command_result(speakers::rename_speaker(&state.ports, request).await)
}

#[tauri::command]
pub async fn confirm_speaker(
    state: State<'_, AppState>,
    request: ConfirmSpeakerRequest,
) -> CommandResult<SpeakerOperationResponse> {
    to_command_result(speakers::confirm_speaker(&state.ports, request).await)
}

#[tauri::command]
pub async fn reset_speaker(
    state: State<'_, AppState>,
    request: ResetSpeakerRequest,
) -> CommandResult<SpeakerOperationResponse> {
    to_command_result(speakers::reset_speaker(&state.ports, request).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This file's own source, parsed by the allowlist test below.
    const SOURCE: &str = include_str!("mod.rs");

    /// The marker is assembled at compile time so that this test module's own
    /// source does not count as a second handler list.
    const MARKER: &str = concat!("tauri::generate", "_handler![");

    /// Names registered in the single `tauri::generate_handler!` invocation.
    fn registered_handlers() -> Vec<String> {
        let start = SOURCE
            .find(MARKER)
            .expect("the crate registers exactly one handler list");
        let rest = &SOURCE[start + MARKER.len()..];
        let end = rest.find(']').expect("the handler list is closed");
        rest[..end]
            .split(',')
            .map(|name| name.trim().trim_end_matches(',').to_owned())
            .filter(|name| !name.is_empty())
            .collect()
    }

    #[test]
    fn exactly_one_handler_list_exists() {
        assert_eq!(
            SOURCE.matches(MARKER).count(),
            1,
            "a second handler list would widen the IPC surface invisibly"
        );
    }

    #[test]
    fn every_registered_handler_is_on_the_allowlist() {
        let registered = registered_handlers();
        assert_eq!(registered.len(), COMMAND_ALLOWLIST.len());
        for name in &registered {
            assert!(
                COMMAND_ALLOWLIST.contains(&name.as_str()),
                "{name} is not a published command"
            );
        }
        for name in COMMAND_ALLOWLIST {
            assert!(
                registered.iter().any(|registered| registered == name),
                "{name} is published but not registered"
            );
        }
    }

    #[test]
    fn no_generic_shell_http_or_filesystem_command_is_exposed() {
        let forbidden = ["exec", "run_", "shell", "http_", "read_file", "write_file"];
        for name in registered_handlers() {
            for pattern in forbidden {
                assert!(
                    !name.starts_with(pattern),
                    "{name} matches the forbidden prefix {pattern}"
                );
            }
        }
    }

    #[test]
    fn the_allowlist_matches_the_published_typescript_contract() {
        // `apps/desktop/src/ipc/contract.ts` is the wire contract the UI
        // compiles against; drift in either direction is a bug.
        let contract = include_str!("../../../src/ipc/contract.ts");
        for name in COMMAND_ALLOWLIST {
            assert!(
                contract.contains(&format!("\"{name}\"")),
                "{name} is not listed in contract.ts"
            );
        }
    }
}
