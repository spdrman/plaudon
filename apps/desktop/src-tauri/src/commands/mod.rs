//! The entire IPC surface: narrow commands and nothing else.
//!
//! Spec §15.1: no generic shell execution, no HTTP proxy and no generic
//! filesystem command is exposed to JavaScript. Every command takes one
//! `request` struct, validates it in Rust and returns either a typed response
//! or the §15.4 error envelope. The allowlist test at the bottom of this file
//! fails if a handler outside the published list is ever registered.

pub mod cloud;
pub mod devices;
pub mod import;
pub mod jobs;
pub mod library;
pub mod outputs;
pub mod search;
pub mod settings;
pub mod sources;
pub mod speakers;
pub mod transcript;

use tauri::State;

use crate::dto::{
    ActivityPage, CancelJobRequest, Capabilities, CloudBudget, CompleteOnboardingRequest,
    ConfirmSpeakerRequest, DeviceAuthStatusDto, DeviceAuthorizeRequest, DeviceDisconnectDto,
    DeviceDisconnectRequest, DeviceIdRequest, DeviceImportDto, DeviceImportRequest,
    DeviceListingDto, DeviceProbeDto, DeviceProbeRequest, DeviceScanDto, DeviceSyncPolicyDto,
    DeviceSyncPolicyRequest, EmptyRequest, ExportRecordingRequest, ExportRecordingResponse,
    GetRecordingRequest, GetTranscriptRequest, GetUsageRequest, GetUsageResponse,
    GrantCloudConsentRequest, GrantCloudConsentResponse, ImportFilesRequest, ImportFilesResponse,
    JobPage, JobSummary, ListActivityRequest, ListJobsRequest, ListModelsRequest,
    ListModelsResponse, ListNearbyDevicesRequest, ListOutputsRequest, ListOutputsResponse,
    ListRecordingsRequest, ListSourceArtifactsRequest, ListSourceArtifactsResponse,
    ListSpeakersRequest, ListSpeakersResponse, LocalOnlyModeResponse, NotificationSettingsResponse,
    PlaudConnectRequest, PlaudConnectorSnapshot, PlaudDisconnectResponse, PlaudImportRequest,
    PlaudImportResponse, PlaudListRequest, PlaudListResponse, Preferences, RecordingDetail,
    RecordingPage, RegenerateOutputRequest, RegenerateOutputResponse, RenameSpeakerRequest,
    ResetSpeakerRequest, RetryJobRequest, ReviewClaimRequest, ReviewClaimResponse,
    RevokeCloudConsentRequest, SaveTranscriptRevisionRequest, SaveTranscriptRevisionResponse,
    SearchLibraryRequest, SearchLibraryResponse, SetCloudBudgetRequest, SetLocalOnlyModeRequest,
    SetModelPreferenceRequest, SetModelPreferenceResponse, SetNotificationSettingsRequest,
    SetOpenRouterKeyRequest, SourceAddFolderRequest, SourceAddFolderResponse,
    SourceReconcileRequest, SourceReconcileResponse, SourceSyncRequest, SourceSyncResponse,
    SpeakerOperationResponse, StartProcessingRequest, StartProcessingResponse, StatusResponse,
    TranscriptPage, VerifyModelRequest, VerifyModelResponse,
};
use crate::error::{to_command_result, CommandResult};
use crate::AppState;

/// The command names this build exposes, in the order of the §15.2 table
/// followed by the two additions recorded in ADR-0020 and the EPIC K source
/// commands, which §15.2 lists verbatim.
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
    "plaud_probe",
    "plaud_connect",
    "plaud_disconnect",
    "plaud_list",
    "plaud_import",
    "source_add_folder",
    "source_sync",
    "source_reconcile",
    "list_source_artifacts",
    "list_models",
    "start_processing",
    "grant_cloud_consent",
    "revoke_cloud_consent",
    "get_usage",
    "set_cloud_budget",
    "set_model_preference",
    "verify_model",
    "list_nearby_devices",
    "device_probe",
    "device_authorize",
    "device_list_recordings",
    "device_import",
    "device_disconnect",
    "device_sync_policy",
    "list_outputs",
    "review_claim",
    "regenerate_output",
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
        complete_onboarding,
        plaud_probe,
        plaud_connect,
        plaud_disconnect,
        plaud_list,
        plaud_import,
        source_add_folder,
        source_sync,
        source_reconcile,
        list_source_artifacts,
        search_library,
        list_speakers,
        rename_speaker,
        confirm_speaker,
        reset_speaker,
        list_models,
        start_processing,
        grant_cloud_consent,
        revoke_cloud_consent,
        get_usage,
        set_cloud_budget,
        set_model_preference,
        verify_model,
        list_nearby_devices,
        device_probe,
        device_authorize,
        device_list_recordings,
        device_import,
        device_disconnect,
        device_sync_policy,
        list_outputs,
        review_claim,
        regenerate_output
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
pub async fn plaud_probe(
    state: State<'_, AppState>,
    #[allow(unused_variables)] request: Option<EmptyRequest>,
) -> CommandResult<PlaudConnectorSnapshot> {
    to_command_result(sources::plaud_probe(&state.ports).await)
}

#[tauri::command]
pub async fn plaud_connect(
    state: State<'_, AppState>,
    request: PlaudConnectRequest,
) -> CommandResult<PlaudConnectorSnapshot> {
    to_command_result(sources::plaud_connect(&state.ports, request).await)
}

#[tauri::command]
pub async fn plaud_disconnect(
    state: State<'_, AppState>,
    #[allow(unused_variables)] request: Option<EmptyRequest>,
) -> CommandResult<PlaudDisconnectResponse> {
    to_command_result(sources::plaud_disconnect(&state.ports).await)
}

#[tauri::command]
pub async fn plaud_list(
    state: State<'_, AppState>,
    request: PlaudListRequest,
) -> CommandResult<PlaudListResponse> {
    to_command_result(sources::plaud_list(&state.ports, request).await)
}

#[tauri::command]
pub async fn plaud_import(
    state: State<'_, AppState>,
    request: PlaudImportRequest,
) -> CommandResult<PlaudImportResponse> {
    to_command_result(sources::plaud_import(&state.ports, request).await)
}

#[tauri::command]
pub async fn source_add_folder(
    state: State<'_, AppState>,
    request: SourceAddFolderRequest,
) -> CommandResult<SourceAddFolderResponse> {
    to_command_result(sources::source_add_folder(&state.ports, request).await)
}

#[tauri::command]
pub async fn source_sync(
    state: State<'_, AppState>,
    request: SourceSyncRequest,
) -> CommandResult<SourceSyncResponse> {
    to_command_result(sources::source_sync(&state.ports, request).await)
}

#[tauri::command]
pub async fn source_reconcile(
    state: State<'_, AppState>,
    request: SourceReconcileRequest,
) -> CommandResult<SourceReconcileResponse> {
    to_command_result(sources::source_reconcile(&state.ports, request).await)
}

#[tauri::command]
pub async fn list_source_artifacts(
    state: State<'_, AppState>,
    request: ListSourceArtifactsRequest,
) -> CommandResult<ListSourceArtifactsResponse> {
    to_command_result(sources::list_source_artifacts(&state.ports, request).await)
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

#[tauri::command]
pub async fn list_models(
    state: State<'_, AppState>,
    request: ListModelsRequest,
) -> CommandResult<ListModelsResponse> {
    to_command_result(cloud::list_models(&state.ports, request).await)
}

#[tauri::command]
pub async fn start_processing(
    state: State<'_, AppState>,
    request: StartProcessingRequest,
) -> CommandResult<StartProcessingResponse> {
    to_command_result(cloud::start_processing(&state.ports, request).await)
}

#[tauri::command]
pub async fn grant_cloud_consent(
    state: State<'_, AppState>,
    request: GrantCloudConsentRequest,
) -> CommandResult<GrantCloudConsentResponse> {
    to_command_result(cloud::grant_cloud_consent(&state.ports, request).await)
}

#[tauri::command]
pub async fn revoke_cloud_consent(
    state: State<'_, AppState>,
    request: RevokeCloudConsentRequest,
) -> CommandResult<StatusResponse> {
    to_command_result(cloud::revoke_cloud_consent(&state.ports, request).await)
}

#[tauri::command]
pub async fn get_usage(
    state: State<'_, AppState>,
    request: GetUsageRequest,
) -> CommandResult<GetUsageResponse> {
    to_command_result(cloud::get_usage(&state.ports, request).await)
}

#[tauri::command]
pub async fn set_cloud_budget(
    state: State<'_, AppState>,
    request: SetCloudBudgetRequest,
) -> CommandResult<CloudBudget> {
    to_command_result(cloud::set_cloud_budget(&state.ports, request).await)
}

#[tauri::command]
pub async fn set_model_preference(
    state: State<'_, AppState>,
    request: SetModelPreferenceRequest,
) -> CommandResult<SetModelPreferenceResponse> {
    to_command_result(cloud::set_model_preference(&state.ports, request).await)
}

#[tauri::command]
pub async fn verify_model(
    state: State<'_, AppState>,
    request: VerifyModelRequest,
) -> CommandResult<VerifyModelResponse> {
    to_command_result(cloud::verify_model(&state.ports, request).await)
}

#[tauri::command]
pub async fn list_nearby_devices(
    state: State<'_, AppState>,
    request: ListNearbyDevicesRequest,
) -> CommandResult<DeviceScanDto> {
    to_command_result(devices::list_nearby_devices(&state.ports, request).await)
}

#[tauri::command]
pub async fn device_probe(
    state: State<'_, AppState>,
    request: DeviceProbeRequest,
) -> CommandResult<DeviceProbeDto> {
    to_command_result(devices::device_probe(&state.ports, request).await)
}

#[tauri::command]
pub async fn device_authorize(
    state: State<'_, AppState>,
    request: DeviceAuthorizeRequest,
) -> CommandResult<DeviceAuthStatusDto> {
    to_command_result(devices::device_authorize(&state.ports, request).await)
}

#[tauri::command]
pub async fn device_list_recordings(
    state: State<'_, AppState>,
    request: DeviceIdRequest,
) -> CommandResult<DeviceListingDto> {
    to_command_result(devices::device_list_recordings(&state.ports, request).await)
}

#[tauri::command]
pub async fn device_import(
    state: State<'_, AppState>,
    request: DeviceImportRequest,
) -> CommandResult<DeviceImportDto> {
    to_command_result(devices::device_import(&state.ports, request).await)
}

#[tauri::command]
pub async fn device_disconnect(
    state: State<'_, AppState>,
    request: DeviceDisconnectRequest,
) -> CommandResult<DeviceDisconnectDto> {
    to_command_result(devices::device_disconnect(&state.ports, request).await)
}

#[tauri::command]
pub async fn device_sync_policy(
    state: State<'_, AppState>,
    request: DeviceSyncPolicyRequest,
) -> CommandResult<DeviceSyncPolicyDto> {
    to_command_result(devices::device_sync_policy(&state.ports, request).await)
}

#[tauri::command]
pub async fn list_outputs(
    state: State<'_, AppState>,
    request: ListOutputsRequest,
) -> CommandResult<ListOutputsResponse> {
    to_command_result(outputs::list_outputs(&state.ports, request).await)
}

#[tauri::command]
pub async fn review_claim(
    state: State<'_, AppState>,
    request: ReviewClaimRequest,
) -> CommandResult<ReviewClaimResponse> {
    to_command_result(outputs::review_claim(&state.ports, request).await)
}

#[tauri::command]
pub async fn regenerate_output(
    state: State<'_, AppState>,
    request: RegenerateOutputRequest,
) -> CommandResult<RegenerateOutputResponse> {
    to_command_result(outputs::regenerate_output(&state.ports, request).await)
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

    /// The IPC surface before EPIC K, as published in §15.2 and ADR-0020.
    const BASELINE: [&str; 16] = [
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
        "remove_openrouter_key",
        "list_activity",
        "complete_onboarding",
    ];

    /// What EPIC K adds, quoted from the §15.2 table.
    const EPIC_K_ADDITIONS: [&str; 9] = [
        "plaud_probe",
        "plaud_connect",
        "plaud_disconnect",
        "plaud_list",
        "plaud_import",
        "source_add_folder",
        "source_sync",
        "source_reconcile",
        "list_source_artifacts",
    ];

    #[test]
    fn the_source_commands_are_the_only_additions_for_epic_k() {
        // Nothing the baseline published went away.
        for name in BASELINE {
            assert!(COMMAND_ALLOWLIST.contains(&name), "{name} was removed");
        }
        // Every EPIC K command is published and registered exactly once.
        let registered = registered_handlers();
        for name in EPIC_K_ADDITIONS {
            assert_eq!(
                COMMAND_ALLOWLIST.iter().filter(|n| **n == name).count(),
                1,
                "{name} must be allowlisted exactly once"
            );
            assert_eq!(
                registered.iter().filter(|n| n.as_str() == name).count(),
                1,
                "{name} must be registered exactly once"
            );
        }
        // Anything else touching a source is a widening of the surface.
        for name in COMMAND_ALLOWLIST {
            let is_source_command = name.starts_with("plaud_")
                || name.starts_with("source_")
                || name.contains("source_artifacts");
            if is_source_command {
                assert!(
                    EPIC_K_ADDITIONS.contains(name),
                    "{name} is a source command that EPIC K did not publish"
                );
            }
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
