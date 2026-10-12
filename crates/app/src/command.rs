//! The typed command surface and the one dispatcher every caller uses.
//!
//! Spec §15.1/§15.2 publish a closed list of narrow commands. The Tauri shell
//! expressed that list as 54 `#[tauri::command]` wrappers; this module
//! expresses the same list as [`AppCommand`] and [`AppResponse`], so a view, a
//! keyboard shortcut, a command palette entry and a test all reach the
//! application through exactly one path: [`Dispatcher`].
//!
//! Three properties are deliberate:
//!
//! * **Closed surface.** [`COMMAND_NAMES`] is generated from the same table
//!   that generates the enums and the dispatch arms, so a command cannot be
//!   added to one and forgotten in the others. Each name appears exactly once —
//!   the Tauri allowlist listed the six archive commands twice, and that
//!   duplication is not carried forward.
//! * **Validation stays in Rust.** Every arm calls the same
//!   `commands::<module>::<function>` the Tauri shell called, and those
//!   functions validate their arguments before touching a port. The view layer
//!   is untrusted input.
//! * **The caller never awaits.** [`Dispatcher::send`] hands the work to the
//!   Tokio runtime and returns a [`CommandId`] immediately; the outcome arrives
//!   as an [`AppEvent::Response`](crate::event::AppEvent::Response) on the event
//!   channel, which wakes the interface.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use plaudon_domain::error::ErrorEnvelope;
use tokio::runtime::Handle;

use crate::dto::{
    ActivityPage, CancelJobRequest, Capabilities, CloudBudget, CompleteOnboardingRequest,
    ConfirmSpeakerRequest, CreateBackupRequest, CreateBackupResponse, DeleteRecordingRequest,
    DeleteRecordingResponse, DeletionDisclosureDto, DeletionDisclosureRequest, DeviceAuthStatusDto,
    DeviceAuthorizeRequest, DeviceDisconnectDto, DeviceDisconnectRequest, DeviceIdRequest,
    DeviceImportDto, DeviceImportRequest, DeviceListingDto, DeviceProbeDto, DeviceProbeRequest,
    DeviceScanDto, DeviceSyncPolicyDto, DeviceSyncPolicyRequest, ExportMediaRequest,
    ExportMediaResponse, ExportRecordingRequest, ExportRecordingResponse, GetRecordingRequest,
    GetTranscriptRequest, GetUsageRequest, GetUsageResponse, GrantCloudConsentRequest,
    GrantCloudConsentResponse, ImportFilesRequest, ImportFilesResponse, InspectArchiveRequest,
    InspectArchiveResponse, JobPage, JobSummary, ListActivityRequest, ListJobsRequest,
    ListModelsRequest, ListModelsResponse, ListNearbyDevicesRequest, ListOutputsRequest,
    ListOutputsResponse, ListRecordingsRequest, ListSourceArtifactsRequest,
    ListSourceArtifactsResponse, ListSpeakersRequest, ListSpeakersResponse, LocalOnlyModeResponse,
    MediaLocation, NotificationSettingsResponse, PlaudConnectRequest, PlaudConnectorSnapshot,
    PlaudDisconnectResponse, PlaudImportRequest, PlaudImportResponse, PlaudListRequest,
    PlaudListResponse, Preferences, RecordingDetail, RecordingPage, RegenerateOutputRequest,
    RegenerateOutputResponse, RenameSpeakerRequest, ResetSpeakerRequest, ResolveMediaRequest,
    RestoreArchiveRequest, RestoreArchiveResponse, RetryJobRequest, ReviewClaimRequest,
    ReviewClaimResponse, RevokeCloudConsentRequest, SaveTranscriptRevisionRequest,
    SaveTranscriptRevisionResponse, SearchLibraryRequest, SearchLibraryResponse,
    SetAppearanceRequest, SetCloudBudgetRequest, SetEnginePreferencesRequest,
    SetExportDefaultsRequest, SetLocalOnlyModeRequest, SetModelPreferenceRequest,
    SetModelPreferenceResponse, SetNotificationSettingsRequest, SetOpenRouterKeyRequest,
    SetShortcutsRequest, SourceAddFolderRequest, SourceAddFolderResponse, SourceReconcileRequest,
    SourceReconcileResponse, SourceSyncRequest, SourceSyncResponse, SpeakerOperationResponse,
    StartProcessingRequest, StartProcessingResponse, StatusResponse, TranscriptPage,
    VerifyModelRequest, VerifyModelResponse,
};
use crate::dto::{
    AddVoiceExemplarRequest, AssignClusterPersonRequest, AssignClusterPersonResponse,
    ClusterSpeechDto, ConfirmSpeakerSuggestionRequest, CreatePersonRequest, DeletePersonRequest,
    DeletePersonResponse, DeleteVoiceEnrollmentRequest, DeleteVoiceEnrollmentResponse,
    EmptyRequest, EnrollPersonVoiceRequest, ForgetAllVoiceProfilesRequest,
    ForgetAllVoiceProfilesResponse, GetClusterSpeechRequest, ListPeopleRequest, ListPeopleResponse,
    ListSpeakerSuggestionsRequest, ListSpeakerSuggestionsResponse, ListVoiceExemplarsRequest,
    ListVoiceExemplarsResponse, MergePeopleRequest, MergePeopleResponse, PersonResponse,
    RejectSpeakerSuggestionRequest, SetVoiceProfilesEnabledRequest, SpeakerSuggestionDecisionDto,
    SplitVoiceEnrollmentRequest, SplitVoiceEnrollmentResponse, StartSpeakerMatchingRequest,
    StartSpeakerMatchingResponse, UpdatePersonRequest, VoiceEnrollmentDto, VoiceProfileSettingsDto,
};
use crate::dto::{
    AskRecordingRequest, AskRecordingResponse, CreateTemplateCopyRequest,
    GenerateStudyArtifactsRequest, GenerateStudyArtifactsResponse, GetTemplateRequest,
    ListConversationsRequest, ListConversationsResponse, ListStudyArtifactsRequest,
    ListStudyArtifactsResponse, ListTemplateVersionsRequest, ListTemplateVersionsResponse,
    ListTemplatesResponse, ListToolRunsRequest, ListToolRunsResponse, ListTranscriptToolsRequest,
    ListTranscriptToolsResponse, SaveTemplateRequest, SaveTemplateResponse, TemplateDocumentDto,
    ValidateTemplateRequest, ValidateTemplateResponse,
};
use crate::dto::{
    InspectModelPackRequest, InspectModelPackResponse, InstallModelFromPackRequest,
    InstallModelResponse, ListLocalModelsResponse, RemoveLocalModelRequest,
    RemoveLocalModelResponse, VerifyLocalModelRequest, VerifyLocalModelResponse,
};
use crate::dto::{SetSegmentSpeakerRequest, SetSegmentSpeakerResponse};
use crate::event::{AppEvent, EventSender};
use crate::settings_presentation::UpdateStatus;
use crate::shortcuts::ShortcutBindings;
use crate::AppPorts;

/// Generates the command surface from one table.
///
/// Two sections, because four of the published commands take no arguments at
/// all (the Tauri shell declared them as `Option<EmptyRequest>` and ignored
/// it):
///
/// * `with_request { "name" => Variant: module::function(Request) -> Response }`
/// * `without_request { "name" => Variant: module::function() -> Response }`
///
/// The enums, the name table and the dispatch arms all come from these two
/// lists, so a command cannot exist in one and be missing from another.
macro_rules! command_surface {
    (
        with_request {
            $(
                $name:literal => $variant:ident : $module:ident :: $function:ident ( $request:ty ) -> $response:ty
            ),* $(,)?
        }
        without_request {
            $(
                $bare_name:literal => $bare_variant:ident : $bare_module:ident :: $bare_function:ident () -> $bare_response:ty
            ),* $(,)?
        }
    ) => {
        /// One published command and its validated arguments.
        pub enum AppCommand {
            $( $variant($request), )*
            $( $bare_variant, )*
        }

        /// The typed answer to an [`AppCommand`].
        pub enum AppResponse {
            $( $variant($response), )*
            $( $bare_variant($bare_response), )*
        }

        /// Every published command name, each exactly once.
        pub const COMMAND_NAMES: &[&str] = &[$($name,)* $($bare_name,)*];

        impl AppCommand {
            /// The published name, as it appears in `schemas/commands/v1`.
            #[must_use]
            pub const fn name(&self) -> &'static str {
                match self {
                    $( Self::$variant { .. } => $name, )*
                    $( Self::$bare_variant => $bare_name, )*
                }
            }
        }

        impl AppResponse {
            /// The command this answers.
            #[must_use]
            pub const fn command(&self) -> &'static str {
                match self {
                    $( Self::$variant { .. } => $name, )*
                    $( Self::$bare_variant { .. } => $bare_name, )*
                }
            }
        }

        /// Run one command to completion on the current task.
        ///
        /// Async, so it must be called from the Tokio runtime
        /// [`Dispatcher`] owns — never from a render thread.
        pub async fn execute(
            ports: &AppPorts,
            command: AppCommand,
        ) -> Result<AppResponse, ErrorEnvelope> {
            match command {
                $(
                    AppCommand::$variant(request) => {
                        crate::commands::$module::$function(ports, request)
                            .await
                            .map(AppResponse::$variant)
                            .map_err(|err| crate::error::envelope(&err))
                    }
                )*
                $(
                    AppCommand::$bare_variant => {
                        crate::commands::$bare_module::$bare_function(ports)
                            .await
                            .map(AppResponse::$bare_variant)
                            .map_err(|err| crate::error::envelope(&err))
                    }
                )*
            }
        }
    };
}

command_surface! {
    with_request {
        "list_recordings" => ListRecordings: library::list_recordings(ListRecordingsRequest) -> RecordingPage,
        "import_files" => ImportFiles: import::import_files(ImportFilesRequest) -> ImportFilesResponse,
        "get_recording" => GetRecording: library::get_recording(GetRecordingRequest) -> RecordingDetail,
        "resolve_media" => ResolveMedia: media::resolve_media(ResolveMediaRequest) -> MediaLocation,
        "get_transcript" => GetTranscript: transcript::get_transcript(GetTranscriptRequest) -> TranscriptPage,
        "save_transcript_revision" => SaveTranscriptRevision: transcript::save_transcript_revision(SaveTranscriptRevisionRequest) -> SaveTranscriptRevisionResponse,
        "list_jobs" => ListJobs: jobs::list_jobs(ListJobsRequest) -> JobPage,
        "cancel_job" => CancelJob: jobs::cancel_job(CancelJobRequest) -> JobSummary,
        "retry_job" => RetryJob: jobs::retry_job(RetryJobRequest) -> JobSummary,
        "export_recording" => ExportRecording: jobs::export_recording(ExportRecordingRequest) -> ExportRecordingResponse,
        "export_media" => ExportMedia: archive::export_media(ExportMediaRequest) -> ExportMediaResponse,
        "get_deletion_disclosure" => GetDeletionDisclosure: archive::get_deletion_disclosure(DeletionDisclosureRequest) -> DeletionDisclosureDto,
        "delete_recording" => DeleteRecording: archive::delete_recording(DeleteRecordingRequest) -> DeleteRecordingResponse,
        "create_portable_backup" => CreatePortableBackup: archive::create_portable_backup(CreateBackupRequest) -> CreateBackupResponse,
        "inspect_restore_archive" => InspectRestoreArchive: archive::inspect_restore_archive(InspectArchiveRequest) -> InspectArchiveResponse,
        "restore_portable_archive" => RestorePortableArchive: archive::restore_portable_archive(RestoreArchiveRequest) -> RestoreArchiveResponse,
        "set_notification_settings" => SetNotificationSettings: settings::set_notification_settings(SetNotificationSettingsRequest) -> NotificationSettingsResponse,
        "set_local_only_mode" => SetLocalOnlyMode: settings::set_local_only_mode(SetLocalOnlyModeRequest) -> LocalOnlyModeResponse,
        "set_openrouter_key" => SetOpenRouterKey: settings::set_openrouter_key(SetOpenRouterKeyRequest) -> StatusResponse,
        "search_library" => SearchLibrary: search::search_library(SearchLibraryRequest) -> SearchLibraryResponse,
        "list_speakers" => ListSpeakers: speakers::list_speakers(ListSpeakersRequest) -> ListSpeakersResponse,
        "rename_speaker" => RenameSpeaker: speakers::rename_speaker(RenameSpeakerRequest) -> SpeakerOperationResponse,
        "confirm_speaker" => ConfirmSpeaker: speakers::confirm_speaker(ConfirmSpeakerRequest) -> SpeakerOperationResponse,
        "reset_speaker" => ResetSpeaker: speakers::reset_speaker(ResetSpeakerRequest) -> SpeakerOperationResponse,
        "set_segment_speaker" => SetSegmentSpeaker: speakers::set_segment_speaker(SetSegmentSpeakerRequest) -> SetSegmentSpeakerResponse,
        "list_activity" => ListActivity: settings::list_activity(ListActivityRequest) -> ActivityPage,
        "complete_onboarding" => CompleteOnboarding: settings::complete_onboarding(CompleteOnboardingRequest) -> Preferences,
        "plaud_connect" => PlaudConnect: sources::plaud_connect(PlaudConnectRequest) -> PlaudConnectorSnapshot,
        "plaud_list" => PlaudList: sources::plaud_list(PlaudListRequest) -> PlaudListResponse,
        "plaud_import" => PlaudImport: sources::plaud_import(PlaudImportRequest) -> PlaudImportResponse,
        "source_add_folder" => SourceAddFolder: sources::source_add_folder(SourceAddFolderRequest) -> SourceAddFolderResponse,
        "source_sync" => SourceSync: sources::source_sync(SourceSyncRequest) -> SourceSyncResponse,
        "source_reconcile" => SourceReconcile: sources::source_reconcile(SourceReconcileRequest) -> SourceReconcileResponse,
        "list_source_artifacts" => ListSourceArtifacts: sources::list_source_artifacts(ListSourceArtifactsRequest) -> ListSourceArtifactsResponse,
        "list_models" => ListModels: cloud::list_models(ListModelsRequest) -> ListModelsResponse,
        "start_processing" => StartProcessing: cloud::start_processing(StartProcessingRequest) -> StartProcessingResponse,
        "grant_cloud_consent" => GrantCloudConsent: cloud::grant_cloud_consent(GrantCloudConsentRequest) -> GrantCloudConsentResponse,
        "revoke_cloud_consent" => RevokeCloudConsent: cloud::revoke_cloud_consent(RevokeCloudConsentRequest) -> StatusResponse,
        "get_usage" => GetUsage: cloud::get_usage(GetUsageRequest) -> GetUsageResponse,
        "set_cloud_budget" => SetCloudBudget: cloud::set_cloud_budget(SetCloudBudgetRequest) -> CloudBudget,
        "set_model_preference" => SetModelPreference: cloud::set_model_preference(SetModelPreferenceRequest) -> SetModelPreferenceResponse,
        "verify_model" => VerifyModel: cloud::verify_model(VerifyModelRequest) -> VerifyModelResponse,
        "list_nearby_devices" => ListNearbyDevices: devices::list_nearby_devices(ListNearbyDevicesRequest) -> DeviceScanDto,
        "device_probe" => DeviceProbe: devices::device_probe(DeviceProbeRequest) -> DeviceProbeDto,
        "device_authorize" => DeviceAuthorize: devices::device_authorize(DeviceAuthorizeRequest) -> DeviceAuthStatusDto,
        "device_list_recordings" => DeviceListRecordings: devices::device_list_recordings(DeviceIdRequest) -> DeviceListingDto,
        "device_import" => DeviceImport: devices::device_import(DeviceImportRequest) -> DeviceImportDto,
        "device_disconnect" => DeviceDisconnect: devices::device_disconnect(DeviceDisconnectRequest) -> DeviceDisconnectDto,
        "device_sync_policy" => DeviceSyncPolicy: devices::device_sync_policy(DeviceSyncPolicyRequest) -> DeviceSyncPolicyDto,
        "list_outputs" => ListOutputs: outputs::list_outputs(ListOutputsRequest) -> ListOutputsResponse,
        "review_claim" => ReviewClaim: outputs::review_claim(ReviewClaimRequest) -> ReviewClaimResponse,
        "regenerate_output" => RegenerateOutput: outputs::regenerate_output(RegenerateOutputRequest) -> RegenerateOutputResponse,
        "set_appearance" => SetAppearance: settings::set_appearance(SetAppearanceRequest) -> Preferences,
        "set_engine_preferences" => SetEnginePreferences: settings::set_engine_preferences(SetEnginePreferencesRequest) -> Preferences,
        "set_export_defaults" => SetExportDefaults: settings::set_export_defaults(SetExportDefaultsRequest) -> Preferences,
        "set_shortcuts" => SetShortcuts: settings::set_shortcuts(SetShortcutsRequest) -> ShortcutBindings,
        "inspect_model_pack" => InspectModelPack: models::inspect_model_pack(InspectModelPackRequest) -> InspectModelPackResponse,
        "install_model_from_pack" => InstallModelFromPack: models::install_model_from_pack(InstallModelFromPackRequest) -> InstallModelResponse,
        "remove_local_model" => RemoveLocalModel: models::remove_local_model(RemoveLocalModelRequest) -> RemoveLocalModelResponse,
        "verify_local_model" => VerifyLocalModel: models::verify_local_model(VerifyLocalModelRequest) -> VerifyLocalModelResponse,
        "list_transcript_tools" => ListTranscriptTools: tools::list_transcript_tools(ListTranscriptToolsRequest) -> ListTranscriptToolsResponse,
        "ask_recording" => AskRecording: tools::ask_recording(AskRecordingRequest) -> AskRecordingResponse,
        "list_conversations" => ListConversations: tools::list_conversations(ListConversationsRequest) -> ListConversationsResponse,
        "generate_study_artifacts" => GenerateStudyArtifacts: tools::generate_study_artifacts(GenerateStudyArtifactsRequest) -> GenerateStudyArtifactsResponse,
        "list_study_artifacts" => ListStudyArtifacts: tools::list_study_artifacts(ListStudyArtifactsRequest) -> ListStudyArtifactsResponse,
        "list_tool_runs" => ListToolRuns: tools::list_tool_runs(ListToolRunsRequest) -> ListToolRunsResponse,
        "get_template" => GetTemplate: templates::get_template(GetTemplateRequest) -> TemplateDocumentDto,
        "create_template_copy" => CreateTemplateCopy: templates::create_template_copy(CreateTemplateCopyRequest) -> TemplateDocumentDto,
        "save_template" => SaveTemplate: templates::save_template(SaveTemplateRequest) -> SaveTemplateResponse,
        "validate_template" => ValidateTemplate: templates::validate_template(ValidateTemplateRequest) -> ValidateTemplateResponse,
        "list_template_versions" => ListTemplateVersions: templates::list_template_versions(ListTemplateVersionsRequest) -> ListTemplateVersionsResponse,
        // EPIC P task P1.5: the people roster (ungated, VP-011) and the
        // consent-gated biometric tier (`VOICE_PROFILES_SPEC` §8).
        "list_people" => ListPeople: people::list_people(ListPeopleRequest) -> ListPeopleResponse,
        "create_person" => CreatePerson: people::create_person(CreatePersonRequest) -> PersonResponse,
        "update_person" => UpdatePerson: people::update_person(UpdatePersonRequest) -> PersonResponse,
        "merge_people" => MergePeople: people::merge_people(MergePeopleRequest) -> MergePeopleResponse,
        "delete_person" => DeletePerson: people::delete_person(DeletePersonRequest) -> DeletePersonResponse,
        "assign_cluster_person" => AssignClusterPerson: people::assign_cluster_person(AssignClusterPersonRequest) -> AssignClusterPersonResponse,
        "get_voice_profile_settings" => GetVoiceProfileSettings: voice_profiles::get_voice_profile_settings(EmptyRequest) -> VoiceProfileSettingsDto,
        "set_voice_profiles_enabled" => SetVoiceProfilesEnabled: voice_profiles::set_voice_profiles_enabled(SetVoiceProfilesEnabledRequest) -> VoiceProfileSettingsDto,
        "enroll_person_voice" => EnrollPersonVoice: voice_profiles::enroll_person_voice(EnrollPersonVoiceRequest) -> VoiceEnrollmentDto,
        "add_voice_exemplar" => AddVoiceExemplar: voice_profiles::add_voice_exemplar(AddVoiceExemplarRequest) -> VoiceEnrollmentDto,
        "split_voice_enrollment" => SplitVoiceEnrollment: voice_profiles::split_voice_enrollment(SplitVoiceEnrollmentRequest) -> SplitVoiceEnrollmentResponse,
        "list_voice_exemplars" => ListVoiceExemplars: voice_profiles::list_voice_exemplars(ListVoiceExemplarsRequest) -> ListVoiceExemplarsResponse,
        "get_cluster_speech" => GetClusterSpeech: voice_profiles::get_cluster_speech(GetClusterSpeechRequest) -> ClusterSpeechDto,
        "delete_voice_enrollment" => DeleteVoiceEnrollment: voice_profiles::delete_voice_enrollment(DeleteVoiceEnrollmentRequest) -> DeleteVoiceEnrollmentResponse,
        "forget_all_voice_profiles" => ForgetAllVoiceProfiles: voice_profiles::forget_all_voice_profiles(ForgetAllVoiceProfilesRequest) -> ForgetAllVoiceProfilesResponse,
        "list_speaker_suggestions" => ListSpeakerSuggestions: voice_profiles::list_speaker_suggestions(ListSpeakerSuggestionsRequest) -> ListSpeakerSuggestionsResponse,
        "confirm_speaker_suggestion" => ConfirmSpeakerSuggestion: voice_profiles::confirm_speaker_suggestion(ConfirmSpeakerSuggestionRequest) -> SpeakerSuggestionDecisionDto,
        "reject_speaker_suggestion" => RejectSpeakerSuggestion: voice_profiles::reject_speaker_suggestion(RejectSpeakerSuggestionRequest) -> SpeakerSuggestionDecisionDto,
        "start_speaker_matching" => StartSpeakerMatching: voice_profiles::start_speaker_matching(StartSpeakerMatchingRequest) -> StartSpeakerMatchingResponse,
    }
    without_request {
        "get_capabilities" => GetCapabilities: settings::get_capabilities() -> Capabilities,
        "remove_openrouter_key" => RemoveOpenRouterKey: settings::remove_openrouter_key() -> StatusResponse,
        "get_shortcuts" => GetShortcuts: settings::get_shortcuts() -> ShortcutBindings,
        "list_local_models" => ListLocalModels: models::list_local_models() -> ListLocalModelsResponse,
        "get_update_status" => GetUpdateStatus: update::get_update_status() -> UpdateStatus,
        "plaud_probe" => PlaudProbe: sources::plaud_probe() -> PlaudConnectorSnapshot,
        "plaud_disconnect" => PlaudDisconnect: sources::plaud_disconnect() -> PlaudDisconnectResponse,
        "list_templates" => ListTemplates: templates::list_templates() -> ListTemplatesResponse,
    }
}

/// Prints the command name and nothing else.
///
/// Arguments carry keys, paths and transcript text; §15.4 keeps all three out
/// of logs, and a `Debug` that printed them would be the easiest way to break
/// that rule by accident.
impl fmt::Debug for AppCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AppCommand").field(&self.name()).finish()
    }
}

/// Prints the command name and nothing else, for the same reason as
/// [`AppCommand`]'s.
impl fmt::Debug for AppResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AppResponse").field(&self.command()).finish()
    }
}

/// Identifies one in-flight command so its outcome can be matched to the
/// caller that asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(u64);

impl CommandId {
    /// The raw counter value, for logs and tests.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Build an id from a raw value. Only tests need this.
    #[must_use]
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }
}

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

#[derive(Debug)]
struct DispatcherInner {
    ports: AppPorts,
    runtime: Handle,
    events: EventSender,
    next_id: AtomicU64,
}

/// The single path from an interface into the application.
///
/// Cheap to clone; every clone shares one runtime, one set of ports and one
/// event channel.
#[derive(Debug, Clone)]
pub struct Dispatcher {
    inner: Arc<DispatcherInner>,
}

impl Dispatcher {
    /// Dispatcher over `ports`, running work on `runtime`, reporting outcomes
    /// on `events`.
    #[must_use]
    pub fn new(ports: AppPorts, runtime: Handle, events: EventSender) -> Self {
        Self {
            inner: Arc::new(DispatcherInner {
                ports,
                runtime,
                events,
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// The ports, for the few callers that legitimately need a port directly
    /// (media resolution, for instance). Not a way around validation: the
    /// command functions are still the only validated entry points.
    #[must_use]
    pub fn ports(&self) -> &AppPorts {
        &self.inner.ports
    }

    /// The runtime commands run on.
    #[must_use]
    pub fn runtime(&self) -> &Handle {
        &self.inner.runtime
    }

    /// Hand a command to the runtime and return immediately.
    ///
    /// The outcome arrives later as
    /// [`AppEvent::Response`](crate::event::AppEvent::Response) carrying the
    /// returned id. This is the only call a render thread may make.
    pub fn send(&self, command: AppCommand) -> CommandId {
        let id = CommandId(self.inner.next_id.fetch_add(1, Ordering::SeqCst));
        let name = command.name();
        let ports = self.inner.ports.clone();
        let events = self.inner.events.clone();
        self.inner.runtime.spawn(async move {
            let result = execute(&ports, command).await;
            if let Err(envelope) = &result {
                tracing::debug!(
                    command = name,
                    code = envelope.code.as_str(),
                    "command rejected"
                );
            }
            events.send(AppEvent::Response {
                id,
                command: name,
                result: Box::new(result),
            });
        });
        id
    }

    /// Run a command to completion and return its outcome.
    ///
    /// Blocks the calling thread, so it is for tests, the headless harness and
    /// startup paths — never for a render thread.
    ///
    /// # Panics
    ///
    /// Panics if called from inside the Tokio runtime, because blocking a
    /// runtime worker on its own runtime deadlocks.
    pub fn call_blocking(&self, command: AppCommand) -> Result<AppResponse, ErrorEnvelope> {
        assert!(
            Handle::try_current().is_err(),
            "call_blocking must not be called from inside the runtime; use execute().await"
        );
        let ports = self.inner.ports.clone();
        self.inner
            .runtime
            .block_on(async move { execute(&ports, command).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_published_command_name_appears_exactly_once() {
        let unique: BTreeSet<&&str> = COMMAND_NAMES.iter().collect();
        assert_eq!(
            unique.len(),
            COMMAND_NAMES.len(),
            "the Tauri allowlist listed the archive commands twice; this surface must not"
        );
        // 54 published by the Tauri shell, plus the eleven EPIC O task O2.4
        // added for settings the React frontend never had, plus the twelve
        // task O2.6 added for transcript tools and templates, plus the one O2.7
        // added because a native player cannot be handed a URL, plus
        // `set_segment_speaker`, the T12 wire leg (spec §4.1, VP-031), plus
        // the seventeen EPIC P task P1.5 added for the people roster and the
        // consent-gated biometric tier (`VOICE_PROFILES_SPEC` §8), plus the
        // two task P2.6 added for the provenance list the split UI reads and
        // the clean-speech meter the enrolment flow shows (VP-091, VP-094).
        assert_eq!(COMMAND_NAMES.len(), 98);
    }

    /// Commands the egui screens added (EPIC O tasks O2.4, O2.6 and O2.7).
    ///
    /// They are deliberately absent from the Tauri allowlist: the React shell
    /// cannot call them, and widening its IPC surface for a frontend that is
    /// being deleted would be the wrong direction.
    const EGUI_ONLY_COMMANDS: &[&str] = &[
        "set_appearance",
        "set_engine_preferences",
        "set_export_defaults",
        "get_shortcuts",
        "set_shortcuts",
        "list_local_models",
        "inspect_model_pack",
        "install_model_from_pack",
        "remove_local_model",
        "verify_local_model",
        "list_transcript_tools",
        "ask_recording",
        "list_conversations",
        "generate_study_artifacts",
        "list_study_artifacts",
        "list_tool_runs",
        "list_templates",
        "get_template",
        "create_template_copy",
        "save_template",
        "validate_template",
        "list_template_versions",
        "get_update_status",
        // O2.7: the Tauri shell served media over a custom URI scheme; a
        // native player opens a file instead.
        "resolve_media",
        // P1.1 (VP-031): per-segment reassignment is an egui-era command; the
        // React shell never had a control that could ask for it.
        "set_segment_speaker",
        // P1.5 (`VOICE_PROFILES_SPEC` §8): the people roster is ungated and
        // works with matching off (VP-011); the biometric tier is
        // consent-gated. Neither existed when the React shell did.
        "list_people",
        "create_person",
        "update_person",
        "merge_people",
        "delete_person",
        "assign_cluster_person",
        "get_voice_profile_settings",
        "set_voice_profiles_enabled",
        "enroll_person_voice",
        "add_voice_exemplar",
        "split_voice_enrollment",
        "delete_voice_enrollment",
        "forget_all_voice_profiles",
        "list_speaker_suggestions",
        "confirm_speaker_suggestion",
        "reject_speaker_suggestion",
        "start_speaker_matching",
        // P2.6 (VP-091, VP-094): the exemplar provenance list behind the
        // split-by-provenance UI, and the cluster's clean speech behind the
        // enrolment meter.
        "list_voice_exemplars",
        "get_cluster_speech",
    ];

    #[test]
    fn the_surface_matches_the_tauri_allowlist_exactly() {
        // The shell's allowlist is the §15.2 table with the six archive
        // commands duplicated; compared as sets, the two must agree once the
        // egui-only additions are set aside.
        const TAURI_ALLOWLIST: &[&str] = &[
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
            "export_media",
            "get_deletion_disclosure",
            "delete_recording",
            "create_portable_backup",
            "inspect_restore_archive",
            "restore_portable_archive",
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
            "export_media",
            "get_deletion_disclosure",
            "delete_recording",
            "create_portable_backup",
            "inspect_restore_archive",
            "restore_portable_archive",
        ];
        let published: BTreeSet<&str> = COMMAND_NAMES.iter().copied().collect();
        let allowlisted: BTreeSet<&str> = TAURI_ALLOWLIST.iter().copied().collect();
        let egui_only: BTreeSet<&str> = EGUI_ONLY_COMMANDS.iter().copied().collect();
        assert_eq!(
            &published - &egui_only,
            allowlisted,
            "a command was added or removed without reviewing the Tauri allowlist"
        );
        assert!(
            egui_only.is_subset(&published),
            "an egui-only command was listed here but never published"
        );
        assert!(
            allowlisted.is_disjoint(&egui_only),
            "an egui-only command must not be reachable over Tauri IPC"
        );
    }

    #[test]
    fn no_generic_shell_http_or_filesystem_command_is_published() {
        for name in COMMAND_NAMES {
            for forbidden in ["exec", "run_", "shell", "http_", "read_file", "write_file"] {
                assert!(
                    !name.starts_with(forbidden),
                    "{name} matches the forbidden prefix {forbidden}"
                );
            }
        }
    }

    #[test]
    fn every_published_name_has_a_request_schema() {
        let schemas =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/commands/v1");
        for name in COMMAND_NAMES {
            assert!(
                schemas
                    .join(format!("{name}.request.schema.json"))
                    .is_file(),
                "{name} has no request schema under schemas/commands/v1"
            );
        }
    }

    #[test]
    fn a_command_debug_line_never_contains_its_arguments() {
        let command = AppCommand::SetOpenRouterKey(SetOpenRouterKeyRequest {
            key: "sk-or-v1-9f3c2b7d8e1a4c6f0b2d".to_owned(),
        });
        let rendered = format!("{command:?}");
        assert!(!rendered.contains("sk-or-v1"), "{rendered}");
        assert_eq!(rendered, "AppCommand(\"set_openrouter_key\")");
    }

    #[test]
    fn the_name_of_a_command_and_of_its_response_agree() {
        let response = AppResponse::ListJobs(JobPage {
            items: Vec::new(),
            next_cursor: None,
            state_counts: std::collections::BTreeMap::new(),
        });
        assert_eq!(response.command(), "list_jobs");
        assert_eq!(AppCommand::GetCapabilities.name(), "get_capabilities");
    }
}
