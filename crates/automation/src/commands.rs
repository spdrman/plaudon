//! Execution of the scripting commands (spec 14.5, App. A.5).
//!
//! Every command goes through the same two gates before it does anything:
//! the bridge must be enabled and owned by the signed-in user
//! ([`crate::bridge`]), and cost-bearing work must be covered by consent and
//! permitted by Local-only Mode ([`crate::policy`]). Both read the same state
//! the GUI writes, so revoking consent in the app revokes it for scripts in
//! the same instant.
//!
//! The commands themselves delegate to [`plaudon_app_core`] and
//! [`plaudon_jobs`]. There is no SQL here that duplicates an application
//! service, and no second code path for anything the GUI can do.

use std::path::Path;

use plaudon_app_core::library::{LibraryOrder, LibraryPage, LibraryService};
use plaudon_domain::ids::RecordingId;
use plaudon_jobs::{
    EnqueueRequest, JobHistory, JobHistoryQuery, JobKey, JobStore, ProcessingJobKey,
    ProcessingStage, ResourceClass,
};
use plaudon_persistence::{Database, PersistenceError};

use crate::bridge::{BridgeCommandOutcome, BridgeRefusal, BridgeStateStore};
use crate::cli::{
    BridgeCommand, Command, ExportArgs, ExportFormat, JobsCommand, ListArgs, ProcessArgs,
    SortOrder, Stage, Where,
};
use crate::output::Envelope;
use crate::policy::{Destination, PolicyReader, PolicyRefusal};

/// Run one parsed command against a library.
///
/// Returns the envelope to print. Failures are envelopes, not panics: a script
/// must always receive parseable output.
pub async fn run(command: &Command, library_path: &Path, now_ms: i64) -> Envelope {
    let name = command.name();
    let db = match Database::open(library_path).await {
        Ok(db) => db,
        Err(error) => {
            return Envelope::error(
                name,
                error.code().as_str(),
                "the local library could not be opened",
                true,
            )
        }
    };

    let envelope = dispatch(command, &db, now_ms).await;
    db.close().await;
    envelope
}

async fn dispatch(command: &Command, db: &Database, now_ms: i64) -> Envelope {
    let name = command.name();
    let bridge = BridgeStateStore::new(db.clone());

    // `bridge status`/`enable`/`disable` are the switch itself, so they are
    // the only commands that run while the bridge is off.
    if let Command::Bridge { command } = command {
        return bridge_command(&bridge, command, now_ms).await;
    }

    match bridge.authorize().await {
        Err(error) => {
            return Envelope::error(
                name,
                error.code().as_str(),
                "the local library could not be read",
                true,
            )
        }
        Ok(Err(refusal)) => {
            return Envelope::error(name, refusal.code(), &refusal.to_string(), false)
        }
        Ok(Ok(_)) => {}
    }

    match command {
        Command::List(args) => list(db, args).await,
        Command::Export(args) => export(db, args).await,
        Command::Process(args) => process(db, args, now_ms).await,
        Command::Jobs { command } => jobs(db, command).await,
        Command::Bridge { .. } => unreachable!("handled above"),
    }
}

async fn bridge_command(
    store: &BridgeStateStore,
    command: &BridgeCommand,
    now_ms: i64,
) -> Envelope {
    let outcome = match command {
        BridgeCommand::Status => store.load().await.map(BridgeCommandOutcome::Status),
        BridgeCommand::Enable { confirm } => {
            if !confirm {
                return Envelope::error(
                    "bridge",
                    "CONFIRMATION_REQUIRED",
                    "pass --confirm to enable the local scripting bridge",
                    false,
                );
            }
            store
                .enable(now_ms)
                .await
                .map(BridgeCommandOutcome::Enabled)
        }
        BridgeCommand::Disable => store
            .disable(now_ms)
            .await
            .map(BridgeCommandOutcome::Disabled),
    };
    match outcome {
        Ok(outcome) => Envelope::ok("bridge", serde_json::to_value(outcome).unwrap_or_default()),
        Err(error) => Envelope::error(
            "bridge",
            error.code().as_str(),
            "the bridge state could not be updated",
            true,
        ),
    }
}

async fn list(db: &Database, args: &ListArgs) -> Envelope {
    let service = LibraryService::new(db.clone());
    let order = match args.sort {
        SortOrder::Newest => LibraryOrder::RecordedAtDesc,
        SortOrder::Oldest => LibraryOrder::RecordedAtAsc,
        SortOrder::Title => LibraryOrder::TitleAsc,
    };
    match service
        .list_recordings(
            LibraryPage {
                limit: args.limit,
                offset: args.offset,
            },
            order,
        )
        .await
    {
        Ok(recordings) => Envelope::ok(
            "list",
            serde_json::json!({
                "count": recordings.len(),
                "offset": args.offset,
                "recordings": recordings,
            }),
        ),
        Err(error) => persistence_error("list", &error),
    }
}

async fn export(db: &Database, args: &ExportArgs) -> Envelope {
    let Ok(recording_id) = args.recording.parse::<RecordingId>() else {
        return Envelope::error(
            "export",
            "INVALID_ARGUMENT",
            "--recording must be a recording identifier",
            false,
        );
    };

    let service = LibraryService::new(db.clone());
    let export = match service.export_transcript(recording_id).await {
        Ok(export) => export,
        Err(error) => return persistence_error("export", &error),
    };

    let rendered = match args.format {
        ExportFormat::Json => serde_json::to_string_pretty(&export).unwrap_or_default(),
        ExportFormat::Text => export.to_plain_text(),
    };

    if let Some(path) = &args.out {
        if let Err(error) = std::fs::write(path, rendered.as_bytes()) {
            return Envelope::error(
                "export",
                "STORAGE_UNAVAILABLE",
                &format!("the export could not be written: {}", error.kind()),
                true,
            );
        }
        return Envelope::ok(
            "export",
            serde_json::json!({
                "recording_id": export.recording_id,
                "transcript_revision_id": export.transcript_revision_id,
                "segment_count": export.segments.len(),
                "format": format_name(args.format),
                "written_to": path.display().to_string(),
            }),
        );
    }

    match args.format {
        ExportFormat::Json => Envelope::ok(
            "export",
            serde_json::json!({
                "format": "json",
                "segment_count": export.segments.len(),
                "transcript": export,
            }),
        ),
        ExportFormat::Text => Envelope::ok(
            "export",
            serde_json::json!({
                "format": "text",
                "recording_id": export.recording_id,
                "transcript_revision_id": export.transcript_revision_id,
                "segment_count": export.segments.len(),
                "text": rendered,
            }),
        ),
    }
}

const fn format_name(format: ExportFormat) -> &'static str {
    match format {
        ExportFormat::Json => "json",
        ExportFormat::Text => "text",
    }
}

async fn process(db: &Database, args: &ProcessArgs, now_ms: i64) -> Envelope {
    let Ok(recording_id) = args.recording.parse::<RecordingId>() else {
        return Envelope::error(
            "process",
            "INVALID_ARGUMENT",
            "--recording must be a recording identifier",
            false,
        );
    };

    let service = LibraryService::new(db.clone());
    match service.is_live(recording_id).await {
        Ok(false) => {
            return Envelope::error(
                "process",
                "RECORD_NOT_FOUND",
                "that recording is not in your library",
                false,
            )
        }
        Err(error) => return persistence_error("process", &error),
        Ok(true) => {}
    }

    let destination = Destination::from(args.run);
    let policy = match PolicyReader::new(db.clone()).load(now_ms).await {
        Ok(policy) => policy,
        Err(error) => return persistence_error("process", &error),
    };
    let consent_id = match policy.authorize(destination) {
        Ok(consent_id) => consent_id.map(ToString::to_string),
        Err(refusal) => return policy_refusal("process", &refusal),
    };

    let engine = match args.run {
        Where::Local => "local",
        Where::Cloud => "cloud",
    };
    let model = args.model.clone().unwrap_or_else(|| "default".to_string());
    let key = ProcessingJobKey::new(
        // The audio digest dimension of the key is the recording identity for
        // a stage queued by identifier; the engine resolves the exact asset
        // when it runs, and the job key still deduplicates repeat invocations.
        recording_id.to_string(),
        processing_stage(args.stage),
        engine,
        model.clone(),
    );

    let store = match JobStore::open_at(db.path()).await {
        Ok(store) => store,
        Err(_) => {
            return Envelope::error(
                "process",
                "STORAGE_UNAVAILABLE",
                "the job store could not be opened",
                true,
            )
        }
    };

    let mut request = EnqueueRequest::new(
        args.stage.job_type(),
        JobKey::Processing(key),
        resource_class(destination),
    )
    .with_recording(recording_id)
    .with_config_snapshot(serde_json::json!({
        "source": "automation_cli",
        "stage": args.stage.job_type(),
        "destination": destination,
        "model": model,
    }));
    if let Some(consent_id) = consent_id {
        request = request.with_consent(consent_id);
    }

    match store.enqueue(request).await {
        Ok(outcome) => Envelope::ok(
            "process",
            serde_json::json!({
                "job_id": outcome.job_id(),
                "created": outcome.is_created(),
                "stage": args.stage.job_type(),
                "destination": destination,
            }),
        ),
        Err(_) => Envelope::error(
            "process",
            "INTERNAL_ERROR",
            "the job could not be queued",
            true,
        ),
    }
}

const fn processing_stage(stage: Stage) -> ProcessingStage {
    match stage {
        Stage::Transcribe => ProcessingStage::Transcribe,
        Stage::Summarize => ProcessingStage::ConsolidatedSummary,
        Stage::Diarize => ProcessingStage::Diarize,
    }
}

const fn resource_class(destination: Destination) -> ResourceClass {
    match destination {
        Destination::Local => ResourceClass::Cpu,
        Destination::Cloud => ResourceClass::CloudDispatch,
    }
}

async fn jobs(db: &Database, command: &JobsCommand) -> Envelope {
    let JobsCommand::Status { recording, limit } = command;

    let store = match JobStore::open_at(db.path()).await {
        Ok(store) => store,
        Err(_) => {
            return Envelope::error(
                "jobs",
                "STORAGE_UNAVAILABLE",
                "the job store could not be opened",
                true,
            )
        }
    };
    let history = JobHistory::new(store.read_pool().clone());

    let mut query = JobHistoryQuery::all().with_limit(i64::from(*limit));
    if let Some(recording) = recording {
        let Ok(recording_id) = recording.parse::<RecordingId>() else {
            return Envelope::error(
                "jobs",
                "INVALID_ARGUMENT",
                "--recording must be a recording identifier",
                false,
            );
        };
        query = query.for_recording(recording_id);
    }

    match history.list_jobs(&query).await {
        Ok(page) => Envelope::ok(
            "jobs",
            serde_json::json!({
                "total": page.total,
                "watermark_seq": page.watermark_seq,
                "jobs": page.jobs,
            }),
        ),
        Err(_) => Envelope::error(
            "jobs",
            "STORAGE_UNAVAILABLE",
            "the job history could not be read",
            true,
        ),
    }
}

fn persistence_error(command: &str, error: &PersistenceError) -> Envelope {
    let code = error.code();
    Envelope::ok_or_error(command, code)
}

fn policy_refusal(command: &str, refusal: &PolicyRefusal) -> Envelope {
    Envelope::error(command, refusal.code(), &refusal.to_string(), false)
}

impl Envelope {
    /// Build a failure envelope from a stable application code, using the
    /// code's own canonical message.
    ///
    /// The canonical message is vendor-neutral by construction (spec 15.4),
    /// which is what keeps provider text and transcript excerpts out of CLI
    /// output.
    #[must_use]
    fn ok_or_error(command: &str, code: plaudon_domain::error::AppErrorCode) -> Self {
        Self::error(
            command,
            code.as_str(),
            code.canonical_message(),
            code.is_retryable(),
        )
    }
}

/// Convenience for `BridgeRefusal`, kept next to the other code mappings.
impl BridgeRefusal {
    /// Stable error code for the output envelope.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "COMMAND_NOT_AUTHORIZED",
            Self::WrongUser => "COMMAND_NOT_AUTHORIZED",
        }
    }
}
