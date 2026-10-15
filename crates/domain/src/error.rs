//! Stable error taxonomy and frontend error envelope (spec 7.6, 7.7, 10.3, 15.4).
//!
//! The taxonomy is a closed, versioned set. Vendor errors are mapped into it;
//! an unmapped vendor error degrades to [`AppErrorCode::InternalError`] and
//! never passes through as raw text. The envelope the frontend receives is
//! built entirely from the code's own canonical message plus an optional
//! allowlisted, redacted hint, so a provider payload, bearer token, signed URL
//! or transcript excerpt cannot reach the UI.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::DiagnosticId;
use crate::time::Timestamp;

/// Version of the error-envelope contract (`schemas/error-envelope.v1.schema.json`).
pub const ERROR_ENVELOPE_VERSION: u32 = 1;

macro_rules! error_codes {
    ($(
        $(#[$meta:meta])*
        $variant:ident => $code:literal, retryable: $retryable:literal, action: $action:expr, message: $message:literal;
    )*) => {
        /// The closed set of stable error codes.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        pub enum AppErrorCode {
            $(
                $(#[$meta])*
                #[serde(rename = $code)]
                $variant,
            )*
        }

        impl AppErrorCode {
            /// Every code, in declaration order. The order and membership of
            /// this slice are part of the published contract.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The stable wire string for this code.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $code,)*
                }
            }

            /// Whether retrying the same operation unchanged is safe.
            #[must_use]
            pub const fn is_retryable(self) -> bool {
                match self {
                    $(Self::$variant => $retryable,)*
                }
            }

            /// The UI affordance that resolves this error, if any.
            #[must_use]
            pub const fn action(self) -> Option<ErrorAction> {
                match self {
                    $(Self::$variant => $action,)*
                }
            }

            /// Vendor-neutral, user-actionable message for this code.
            #[must_use]
            pub const fn canonical_message(self) -> &'static str {
                match self {
                    $(Self::$variant => $message,)*
                }
            }
        }
    };
}

/// A UI affordance that can resolve an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorAction {
    /// Open the PLAUD connector setup screen.
    OpenPlaudConnect,
    /// Show the CLI installation instructions.
    OpenCliSetup,
    /// Open application settings.
    OpenSettings,
    /// Open the operating-system permission settings.
    OpenPermissionSettings,
    /// Offer the import again.
    RetryImport,
    /// Re-request the download link from the source.
    RefreshSourceLink,
    /// Open the consent review sheet.
    ReviewConsent,
    /// Prompt the user to free disk space.
    FreeDiskSpace,
    /// Open the local-only mode explanation.
    ReviewLocalOnlyMode,
    /// Open the device troubleshooting screen.
    OpenDeviceDiagnostics,
    /// Prompt the user to charge the device.
    ChargeDevice,
    /// Open the redacted diagnostics view.
    OpenDiagnostics,
    /// Offer to choose a different model.
    ChooseAnotherModel,
    /// Wait and try again later.
    WaitAndRetry,
    /// Offer to install a speech model, or import a model file the user
    /// already has (issue #305).
    InstallSpeechModel,
}

error_codes! {
    // ---- Source errors (spec 7.6) ----
    /// The official PLAUD Cloud CLI executable was not found.
    CliMissing => "CLI_MISSING", retryable: false, action: Some(ErrorAction::OpenCliSetup),
        message: "The PLAUD command line tool was not found. Manual file import still works.";
    /// The detected CLI version is outside the validated compatibility table.
    CliUnsupported => "CLI_UNSUPPORTED", retryable: false, action: Some(ErrorAction::OpenCliSetup),
        message: "This version of the PLAUD command line tool has not been validated. Manual file import still works.";
    /// The source requires an interactive login before it can be used.
    AuthenticationRequired => "AUTHENTICATION_REQUIRED", retryable: false, action: Some(ErrorAction::OpenPlaudConnect),
        message: "Sign in to the source before importing recordings.";
    /// The user dismissed or cancelled the login.
    AuthCanceled => "AUTH_CANCELED", retryable: false, action: Some(ErrorAction::OpenPlaudConnect),
        message: "Sign-in was cancelled. Nothing was imported.";
    /// The source could not be reached or reported itself unavailable.
    SourceUnavailable => "SOURCE_UNAVAILABLE", retryable: true, action: Some(ErrorAction::WaitAndRetry),
        message: "The source is currently unavailable. You can try again.";
    /// The remote side asked us to slow down.
    RemoteRateLimited => "REMOTE_RATE_LIMITED", retryable: true, action: Some(ErrorAction::WaitAndRetry),
        message: "The provider is rate limiting requests. We will wait before trying again.";
    /// The temporary download link is no longer valid.
    LinkExpired => "LINK_EXPIRED", retryable: true, action: Some(ErrorAction::RefreshSourceLink),
        message: "The download link expired. A fresh link is required.";
    /// The requested item no longer exists at the source.
    AssetNotFound => "ASSET_NOT_FOUND", retryable: false, action: None,
        message: "The recording is no longer available at the source.";
    /// Source output did not match the validated contract.
    MalformedOutput => "MALFORMED_OUTPUT", retryable: false, action: Some(ErrorAction::OpenDiagnostics),
        message: "The source returned output we could not safely interpret.";
    /// A transfer stopped before all bytes arrived.
    DownloadInterrupted => "DOWNLOAD_INTERRUPTED", retryable: true, action: Some(ErrorAction::RetryImport),
        message: "The download was interrupted. No partial file was added to your library.";
    /// Stored or received bytes failed their integrity check.
    IntegrityFailed => "INTEGRITY_FAILED", retryable: false, action: Some(ErrorAction::OpenDiagnostics),
        message: "The file failed its integrity check and was not added to your library.";

    // ---- Direct-device transfer errors (spec 7.7) ----
    /// The operating system denied radio access.
    RadioPermissionDenied => "RADIO_PERMISSION_DENIED", retryable: false, action: Some(ErrorAction::OpenPermissionSettings),
        message: "Bluetooth access is not permitted for this application.";
    /// The device is busy with another host or operation.
    DeviceBusy => "DEVICE_BUSY", retryable: true, action: Some(ErrorAction::WaitAndRetry),
        message: "The device is busy. Close other applications connected to it and try again.";
    /// The device link dropped.
    DeviceDisconnected => "DEVICE_DISCONNECTED", retryable: true, action: Some(ErrorAction::RetryImport),
        message: "The device disconnected before the transfer finished.";
    /// The observed firmware is outside every validated compatibility cell.
    UnsupportedFirmware => "UNSUPPORTED_FIRMWARE", retryable: false, action: Some(ErrorAction::OpenDeviceDiagnostics),
        message: "This device firmware is not in our validated compatibility list.";
    /// The device requires a provisioning step we do not implement.
    AuthBootstrapRequired => "AUTH_BOOTSTRAP_REQUIRED", retryable: false, action: Some(ErrorAction::OpenDeviceDiagnostics),
        message: "This device requires a provisioning step this application does not perform.";
    /// The device rejected our credentials.
    AuthRejected => "AUTH_REJECTED", retryable: false, action: Some(ErrorAction::OpenDeviceDiagnostics),
        message: "The device rejected the stored credentials for this application.";
    /// A device transfer ended without a verified terminal marker.
    TransferIncomplete => "TRANSFER_INCOMPLETE", retryable: true, action: Some(ErrorAction::RetryImport),
        message: "The transfer did not complete. Nothing was added to your library.";
    /// The acquired bytes use a codec we cannot decode.
    UnsupportedCodec => "UNSUPPORTED_CODEC", retryable: false, action: None,
        message: "This audio format is not supported by the installed decoders.";
    /// The requested item is not present on the device.
    NotPresentOnDevice => "NOT_PRESENT_ON_DEVICE", retryable: false, action: None,
        message: "The device does not list this recording. It may already have been synchronized elsewhere.";
    /// The device battery is too low to transfer safely.
    DevicePowerLow => "DEVICE_POWER_LOW", retryable: true, action: Some(ErrorAction::ChargeDevice),
        message: "The device battery is too low to transfer recordings.";

    // ---- Media validation errors (spec 8.1) ----
    /// The media could not be identified or the container is unsupported.
    UnsupportedMedia => "UNSUPPORTED_MEDIA", retryable: false, action: None,
        message: "This file type is not supported.";
    /// The media failed structural validation.
    MediaValidationFailed => "MEDIA_VALIDATION_FAILED", retryable: false, action: None,
        message: "The audio file is damaged or its contents do not match its name.";

    // ---- Persistence and archive errors (spec 13) ----
    /// The local database or archive could not be opened.
    StorageUnavailable => "STORAGE_UNAVAILABLE", retryable: true, action: Some(ErrorAction::OpenDiagnostics),
        message: "The local library could not be opened.";
    /// There is not enough free space to complete the operation.
    StorageFull => "STORAGE_FULL", retryable: true, action: Some(ErrorAction::FreeDiskSpace),
        message: "There is not enough free disk space to finish this operation.";
    /// A schema migration failed; the previous database remains usable.
    MigrationFailed => "MIGRATION_FAILED", retryable: false, action: Some(ErrorAction::OpenDiagnostics),
        message: "The library could not be upgraded. Your previous library was left unchanged.";
    /// The requested record does not exist.
    RecordNotFound => "RECORD_NOT_FOUND", retryable: false, action: None,
        message: "That item no longer exists in your library.";
    /// A concurrent change invalidated the operation's base version.
    Conflict => "CONFLICT", retryable: false, action: None,
        message: "Someone or something else changed this item first. Review the change before saving.";
    /// A path was rejected by the archive path policy.
    ArchivePathRejected => "ARCHIVE_PATH_REJECTED", retryable: false, action: None,
        message: "That location is not allowed for the media archive.";
    /// Staging and archive are on different volumes, so no atomic commit exists.
    CrossVolumeCommit => "CROSS_VOLUME_COMMIT", retryable: false, action: Some(ErrorAction::OpenSettings),
        message: "Imports must be staged on the same disk as the media archive.";

    // ---- Policy, consent and capability errors (spec 10.5, 16.3) ----
    /// The operation needs consent that has not been granted.
    ConsentRequired => "CONSENT_REQUIRED", retryable: false, action: Some(ErrorAction::ReviewConsent),
        message: "This step needs your explicit approval before it can run.";
    /// Local-only mode forbids the requested network operation.
    LocalOnlyMode => "LOCAL_ONLY_MODE", retryable: false, action: Some(ErrorAction::ReviewLocalOnlyMode),
        message: "Local-only Mode is enabled, so this network operation was refused.";
    /// A privacy or routing policy could not be resolved for the exact route.
    PolicyUnresolved => "POLICY_UNRESOLVED", retryable: false, action: Some(ErrorAction::ChooseAnotherModel),
        message: "We could not confirm the privacy route for this provider, so nothing was sent.";
    /// The requested capability is not verified for this configuration.
    CapabilityUnverified => "CAPABILITY_UNVERIFIED", retryable: false, action: Some(ErrorAction::ChooseAnotherModel),
        message: "This capability is not verified for your configuration and was not used.";
    /// The requested model or engine is unavailable right now.
    ModelUnavailable => "MODEL_UNAVAILABLE", retryable: false, action: Some(ErrorAction::ChooseAnotherModel),
        message: "The selected model is not available on this machine.";
    /// No speech-to-text model is installed, so nothing can be transcribed
    /// locally yet (issue #305).
    NoSpeechModel => "NO_SPEECH_MODEL", retryable: false, action: Some(ErrorAction::InstallSpeechModel),
        message: "No speech model is installed on this machine, so there is nothing to transcribe with yet. Install a model, or import a model file you already have.";

    // ---- Inference and dispatch errors (spec 10.6, 10.7) ----
    /// The request was transmitted but the outcome is unknown.
    OutcomeUnknown => "OUTCOME_UNKNOWN", retryable: false, action: Some(ErrorAction::OpenDiagnostics),
        message: "The request was sent but no result arrived. It may still have been billed, so it was not retried automatically.";
    /// The provider refused the request.
    ProviderRejected => "PROVIDER_REJECTED", retryable: false, action: Some(ErrorAction::ChooseAnotherModel),
        message: "The provider refused this request.";
    /// Structured output failed schema validation and bounded repair.
    SchemaValidationFailed => "SCHEMA_VALIDATION_FAILED", retryable: true, action: Some(ErrorAction::OpenDiagnostics),
        message: "The model returned output that did not match the required structure.";
    /// The input exceeds the model's context or payload limits.
    ContextTooLarge => "CONTEXT_TOO_LARGE", retryable: false, action: Some(ErrorAction::ChooseAnotherModel),
        message: "This recording is larger than the selected model accepts.";

    // ---- Job lifecycle (spec 14) ----
    /// The operation was cancelled by the user or by shutdown.
    Canceled => "CANCELED", retryable: true, action: Some(ErrorAction::RetryImport),
        message: "The operation was cancelled. No partial result was saved.";
    /// The operation exceeded its time budget before anything was dispatched.
    Timeout => "TIMEOUT", retryable: true, action: Some(ErrorAction::WaitAndRetry),
        message: "The operation took too long and was stopped.";

    // ---- Application boundary (spec 15.1, 15.2) ----
    /// A command argument failed Rust-side validation.
    InvalidArgument => "INVALID_ARGUMENT", retryable: false, action: None,
        message: "That request was not valid. Check the selection and try again.";
    /// A policy-changing command was invoked without its required confirmation.
    ConfirmationRequired => "CONFIRMATION_REQUIRED", retryable: false, action: Some(ErrorAction::OpenSettings),
        message: "Confirm this change before it can be applied.";
    /// Retrying would re-dispatch possibly-billed work without a cost acknowledgement.
    CostAcknowledgementRequired => "COST_ACKNOWLEDGEMENT_REQUIRED", retryable: false, action: Some(ErrorAction::ReviewConsent),
        message: "This retry may be billed again. Confirm the cost before retrying.";
    /// The configured spending limit or the provider account balance is exhausted.
    BudgetExceeded => "BUDGET_EXCEEDED", retryable: false, action: Some(ErrorAction::OpenSettings),
        message: "The spending limit for cloud processing has been reached.";

    // ---- Security and process boundary (spec 6.3A, 7.3, 16.2A, 16.4) ----
    /// The operating system credential store is locked.
    SecretStoreLocked => "SECRET_STORE_LOCKED", retryable: true, action: Some(ErrorAction::OpenPermissionSettings),
        message: "Your keychain is locked, so the saved credential could not be read. Unlock it and try again.";
    /// The egress gate refused an outbound connection.
    EgressDenied => "EGRESS_DENIED", retryable: false, action: Some(ErrorAction::ReviewLocalOnlyMode),
        message: "The app refused to start a network connection to an unapproved destination.";
    /// A command was invoked outside the allowlist or from the wrong window.
    CommandNotAuthorized => "COMMAND_NOT_AUTHORIZED", retryable: false, action: Some(ErrorAction::OpenDiagnostics),
        message: "That operation is not permitted from this window.";
    /// A sandboxed decoder or inference helper terminated abnormally.
    HelperFailed => "HELPER_FAILED", retryable: true, action: Some(ErrorAction::RetryImport),
        message: "A background helper stopped unexpectedly. The step was not completed and can be retried.";

    // ---- Export, portable archive and deletion (spec 4.6, 13.5, 13.6, 13.8) ----
    /// The chosen export destination is not a safe, writable location.
    ExportDestinationRejected => "EXPORT_DESTINATION_REJECTED", retryable: false, action: Some(ErrorAction::OpenSettings),
        message: "That export location cannot be used. Choose a different folder.";
    /// Captions were requested for a transcript that has no real timestamps.
    CaptionsUnavailable => "CAPTIONS_UNAVAILABLE", retryable: false, action: None,
        message: "This transcript has no real timestamps, so captions cannot be exported. We do not invent seek times.";
    /// A portable archive failed validation and was refused before any write.
    ArchiveRejected => "ARCHIVE_REJECTED", retryable: false, action: Some(ErrorAction::OpenDiagnostics),
        message: "This archive failed validation and was not restored. Your library was left unchanged.";
    /// An encrypted archive could not be decrypted with the supplied password.
    ArchiveDecryptionFailed => "ARCHIVE_DECRYPTION_FAILED", retryable: true, action: None,
        message: "The archive could not be decrypted. Check the password; it cannot be recovered if lost.";
    /// Archive creation found credential-shaped content and stopped.
    SecretExclusionFailed => "SECRET_EXCLUSION_FAILED", retryable: false, action: Some(ErrorAction::OpenDiagnostics),
        message: "Backup stopped because something credential-shaped would have been written into the archive. No archive file was created.";

    // ---- Fallback ----
    /// An unmapped internal or vendor failure.
    InternalError => "INTERNAL_ERROR", retryable: false, action: Some(ErrorAction::OpenDiagnostics),
        message: "Something went wrong. A redacted diagnostic entry was recorded.";
}

impl fmt::Display for AppErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An application error carrying a stable code and non-public diagnostic detail.
///
/// `detail` is for redacted diagnostics only; it is never serialized and never
/// reaches the frontend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}")]
pub struct AppError {
    code: AppErrorCode,
    detail: String,
    hint: Option<SafeHint>,
}

/// A short, allowlisted hint that may be shown to the user.
///
/// Hints are constructed from typed values, never from provider text, so there
/// is no path by which a vendor payload can be rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SafeHint {
    /// Retry is possible after the given number of seconds.
    RetryAfterSeconds(u32),
    /// A limit was exceeded by a known amount.
    LimitExceeded {
        /// Name of the limit, from a fixed vocabulary.
        limit: &'static str,
        /// The configured limit value.
        allowed: u64,
        /// The observed value.
        observed: u64,
    },
    /// A named, non-secret component is required.
    RequiredComponent(&'static str),
}

impl SafeHint {
    /// Render the hint as a short sentence.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::RetryAfterSeconds(seconds) => {
                format!("You can try again in about {seconds} seconds.")
            }
            Self::LimitExceeded {
                limit,
                allowed,
                observed,
            } => {
                format!("The {limit} limit is {allowed}; this request needed {observed}.")
            }
            Self::RequiredComponent(component) => format!("Required component: {component}."),
        }
    }
}

impl AppError {
    /// Construct an error with internal diagnostic detail.
    #[must_use]
    pub fn new(code: AppErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            hint: None,
        }
    }

    /// Attach a user-safe hint.
    #[must_use]
    pub fn with_hint(mut self, hint: SafeHint) -> Self {
        self.hint = Some(hint);
        self
    }

    /// The stable code.
    #[must_use]
    pub const fn code(&self) -> AppErrorCode {
        self.code
    }

    /// Internal diagnostic detail. Never serialize this directly.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Whether retrying the same operation unchanged is safe.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        self.code.is_retryable()
    }

    /// Degrade an unmapped vendor failure to the generic internal code.
    ///
    /// The vendor text is kept only as redacted diagnostic detail.
    #[must_use]
    pub fn from_unmapped_vendor(vendor_text: impl AsRef<str>) -> Self {
        Self::new(
            AppErrorCode::InternalError,
            Redactor::default().redact(vendor_text.as_ref()),
        )
    }

    /// Build the frontend envelope, recording a redacted diagnostics entry.
    #[must_use]
    pub fn to_envelope(&self, diagnostics: &mut DiagnosticLog, at: Timestamp) -> ErrorEnvelope {
        let diagnostic_id = diagnostics.record(self, at);
        let mut message = self.code.canonical_message().to_owned();
        if let Some(hint) = &self.hint {
            message.push(' ');
            message.push_str(&hint.render());
        }
        ErrorEnvelope {
            code: self.code,
            message,
            retryable: self.code.is_retryable(),
            action: self.code.action(),
            diagnostic_id,
        }
    }
}

/// The error payload delivered to the frontend (spec 15.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    /// Stable error code.
    pub code: AppErrorCode,
    /// Vendor-neutral, user-actionable message.
    pub message: String,
    /// Whether the same operation may be retried unchanged.
    pub retryable: bool,
    /// Suggested UI affordance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<ErrorAction>,
    /// Correlates to a redacted diagnostics entry.
    pub diagnostic_id: DiagnosticId,
}

/// Redacts secret-shaped substrings before anything is persisted or logged.
#[derive(Debug, Clone)]
pub struct Redactor {
    max_len: usize,
}

impl Default for Redactor {
    fn default() -> Self {
        Self { max_len: 512 }
    }
}

impl Redactor {
    /// Placeholder substituted for every removed value.
    pub const PLACEHOLDER: &'static str = "[redacted]";

    /// Construct a redactor with a custom length bound.
    #[must_use]
    pub const fn with_max_len(max_len: usize) -> Self {
        Self { max_len }
    }

    /// Replace secret-shaped tokens and truncate to the configured bound.
    ///
    /// The rules are deliberately broad: any URL, any `key=value` pair whose key
    /// looks sensitive, any bearer credential and any long opaque token is
    /// removed. Over-redaction is acceptable; leaking is not.
    #[must_use]
    pub fn redact(&self, input: &str) -> String {
        let mut out = String::with_capacity(input.len().min(self.max_len));
        for (index, token) in input.split_whitespace().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            out.push_str(&Self::redact_token(token));
        }
        if out.chars().count() > self.max_len {
            out = out.chars().take(self.max_len).collect::<String>() + "…";
        }
        out
    }

    fn redact_token(token: &str) -> String {
        let lower = token.to_ascii_lowercase();
        // A URL is matched anywhere in the token, not only at its start:
        // surrounding punctuation such as `(`, `"`, `<` or a `key=` prefix
        // must not be able to smuggle a signed link past the redactor.
        let carries_url = ["http://", "https://", "file://"]
            .iter()
            .any(|scheme| lower.contains(scheme));
        if carries_url || lower.starts_with('/') || lower.contains(":\\") {
            return Self::PLACEHOLDER.to_owned();
        }
        // The bare word on its own is not a secret; it is usually a label in
        // a sentence such as "a token is required".
        if Self::SENSITIVE_KEYS.iter().any(|needle| lower == *needle) {
            return token.to_owned();
        }
        // A token may carry several `key=value` pairs, as a query fragment
        // does. Every pair is inspected, not only the first.
        for pair in token.split(['&', ';']) {
            if let Some((key, value)) = pair.split_once(['=', ':']) {
                let key_lower = key.to_ascii_lowercase();
                if Self::SENSITIVE_KEYS
                    .iter()
                    .any(|needle| key_lower.contains(needle))
                {
                    // `Authorization:` with its value in the next token still
                    // marks the label itself as redacted, so the pair never
                    // survives half-visible.
                    return if value == Self::PLACEHOLDER {
                        token.to_owned()
                    } else if value.is_empty() {
                        format!("{key}{}{}", &pair[key.len()..=key.len()], Self::PLACEHOLDER)
                    } else {
                        Self::PLACEHOLDER.to_owned()
                    };
                }
            }
        }
        if Self::looks_like_opaque_secret(token) {
            return Self::PLACEHOLDER.to_owned();
        }
        token.to_owned()
    }

    /// Substrings in a field name that mark its value as sensitive.
    const SENSITIVE_KEYS: &'static [&'static str] = &[
        "token",
        "secret",
        "key",
        "password",
        "passphrase",
        "signature",
        "authorization",
        "cookie",
        "sig",
        "credential",
        "session",
        "bearer",
    ];

    /// Whether a token has the shape of an opaque credential.
    ///
    /// A credential frequently carries internal separators: a JWT is
    /// `header.payload.signature`, base64 uses `+`, `/` and `=`, and a query
    /// fragment uses `&`. The token is therefore split on those separators
    /// and redacted when **any** segment looks opaque, which over-redacts a
    /// long dotted identifier and never under-redacts a credential.
    fn looks_like_opaque_secret(token: &str) -> bool {
        let trimmed = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        trimmed
            .split(['.', '&', '?', ',', ';'])
            .any(Self::segment_looks_opaque)
    }

    fn segment_looks_opaque(segment: &str) -> bool {
        segment.len() >= 20
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '='))
            && segment.chars().any(|c| c.is_ascii_digit())
            && segment.chars().any(|c| c.is_ascii_alphabetic())
    }
}

/// One redacted diagnostics entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticRecord {
    /// Identity referenced by the error envelope.
    pub id: DiagnosticId,
    /// Stable code of the originating error.
    pub code: AppErrorCode,
    /// Redacted internal detail.
    pub redacted_detail: String,
    /// When the entry was recorded.
    pub recorded_at: Timestamp,
}

/// An in-memory, redaction-enforcing diagnostics log.
///
/// Every entry passes through [`Redactor`] on the way in, so the log itself
/// cannot hold a credential even if a caller passes one.
#[derive(Debug, Default)]
pub struct DiagnosticLog {
    redactor: Redactor,
    entries: BTreeMap<DiagnosticId, DiagnosticRecord>,
}

impl DiagnosticLog {
    /// Create an empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an error and return the identity the envelope will carry.
    pub fn record(&mut self, error: &AppError, at: Timestamp) -> DiagnosticId {
        let id = DiagnosticId::new();
        self.entries.insert(
            id,
            DiagnosticRecord {
                id,
                code: error.code,
                redacted_detail: self.redactor.redact(error.detail()),
                recorded_at: at,
            },
        );
        id
    }

    /// Resolve a diagnostic identity from an envelope.
    #[must_use]
    pub fn get(&self, id: DiagnosticId) -> Option<&DiagnosticRecord> {
        self.entries.get(&id)
    }

    /// Number of recorded entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the log is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published code set, in order, as `schemas/error-envelope.v1.schema.json`
    /// lists it. The schema is the one place the contract is written down:
    /// this test and every other test that cares how many codes there are
    /// read it from there, so adding a code means editing the schema (where a
    /// reviewer sees it and where two branches adding codes conflict in git)
    /// rather than bumping a count in several suites.
    fn published_codes() -> Vec<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/error-envelope.v1.schema.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let schema: serde_json::Value = serde_json::from_str(&text).expect("schema parses");
        schema["properties"]["code"]["enum"]
            .as_array()
            .expect("the schema lists the codes")
            .iter()
            .map(|v| v.as_str().expect("every code is a string").to_owned())
            .collect()
    }

    #[test]
    fn the_code_set_is_closed_and_stable() {
        let codes: Vec<&str> = AppErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        let published = published_codes();
        assert_eq!(
            codes, published,
            "AppErrorCode::ALL must match the published schema exactly, in order: \
             a code is never renamed, dropped or reordered without changing the contract"
        );
        // The codes every release so far has shipped. They may be joined by
        // new ones, but never lose one: an envelope already stored or sent
        // has to keep meaning the same thing.
        for shipped in [
            "CLI_MISSING",
            "CLI_UNSUPPORTED",
            "AUTHENTICATION_REQUIRED",
            "AUTH_CANCELED",
            "SOURCE_UNAVAILABLE",
            "REMOTE_RATE_LIMITED",
            "LINK_EXPIRED",
            "ASSET_NOT_FOUND",
            "MALFORMED_OUTPUT",
            "DOWNLOAD_INTERRUPTED",
            "INTEGRITY_FAILED",
            "RADIO_PERMISSION_DENIED",
            "DEVICE_BUSY",
            "DEVICE_DISCONNECTED",
            "UNSUPPORTED_FIRMWARE",
            "AUTH_BOOTSTRAP_REQUIRED",
            "AUTH_REJECTED",
            "TRANSFER_INCOMPLETE",
            "UNSUPPORTED_CODEC",
            "NOT_PRESENT_ON_DEVICE",
            "DEVICE_POWER_LOW",
            "UNSUPPORTED_MEDIA",
            "MEDIA_VALIDATION_FAILED",
            "STORAGE_UNAVAILABLE",
            "STORAGE_FULL",
            "MIGRATION_FAILED",
            "RECORD_NOT_FOUND",
            "CONFLICT",
            "ARCHIVE_PATH_REJECTED",
            "CROSS_VOLUME_COMMIT",
            "CONSENT_REQUIRED",
            "LOCAL_ONLY_MODE",
            "POLICY_UNRESOLVED",
            "CAPABILITY_UNVERIFIED",
            "MODEL_UNAVAILABLE",
            "NO_SPEECH_MODEL",
            "OUTCOME_UNKNOWN",
            "PROVIDER_REJECTED",
            "SCHEMA_VALIDATION_FAILED",
            "CONTEXT_TOO_LARGE",
            "CANCELED",
            "TIMEOUT",
            "INVALID_ARGUMENT",
            "CONFIRMATION_REQUIRED",
            "COST_ACKNOWLEDGEMENT_REQUIRED",
            "BUDGET_EXCEEDED",
            "SECRET_STORE_LOCKED",
            "EGRESS_DENIED",
            "COMMAND_NOT_AUTHORIZED",
            "HELPER_FAILED",
            "EXPORT_DESTINATION_REJECTED",
            "CAPTIONS_UNAVAILABLE",
            "ARCHIVE_REJECTED",
            "ARCHIVE_DECRYPTION_FAILED",
            "SECRET_EXCLUSION_FAILED",
            "INTERNAL_ERROR",
        ] {
            assert!(
                codes.contains(&shipped),
                "{shipped} was dropped from the set"
            );
        }
    }

    #[test]
    fn every_code_maps_to_exactly_one_string_and_one_message() {
        let mut seen = std::collections::BTreeSet::new();
        for code in AppErrorCode::ALL {
            assert!(
                seen.insert(code.as_str()),
                "duplicate code string {}",
                code.as_str()
            );
            assert_ne!(code.canonical_message(), "");
            assert_eq!(
                code.as_str(),
                serde_json::to_value(code).unwrap().as_str().unwrap(),
                "serde rename must match as_str"
            );
        }
    }

    #[test]
    fn terminal_codes_are_not_retryable() {
        for code in [
            AppErrorCode::AuthCanceled,
            AppErrorCode::CliUnsupported,
            AppErrorCode::IntegrityFailed,
            AppErrorCode::OutcomeUnknown,
            AppErrorCode::ConsentRequired,
            AppErrorCode::LocalOnlyMode,
        ] {
            assert!(!code.is_retryable(), "{code} must not be retryable");
        }
        assert!(AppErrorCode::DownloadInterrupted.is_retryable());
        assert!(AppErrorCode::RemoteRateLimited.is_retryable());
    }

    #[test]
    fn an_unmapped_vendor_error_degrades_to_a_generic_code() {
        let error = AppError::from_unmapped_vendor("provider exploded: trace 0xdeadbeef");
        assert_eq!(error.code(), AppErrorCode::InternalError);
    }

    #[test]
    fn an_envelope_never_carries_vendor_text_secrets_or_links() {
        let leaky = concat!(
            "PLAUD CLI said: Authorization: Bearer sk-or-v1-0123456789abcdefghijklmnop ",
            "download https://cdn.example.com/audio.wav?X-Amz-Signature=deadbeef0123456789 ",
            "transcript: the quarterly numbers are confidential ",
            "/Users/someone/Library/Application Support/plaudon/app.sqlite"
        );
        let error = AppError::new(AppErrorCode::LinkExpired, leaky);
        let mut log = DiagnosticLog::new();
        let envelope = error.to_envelope(&mut log, Timestamp::EPOCH);
        let serialized = serde_json::to_string(&envelope).unwrap();

        for forbidden in [
            "Bearer",
            "sk-or-v1",
            "https://",
            "X-Amz-Signature",
            "quarterly numbers",
            "/Users/someone",
            "PLAUD CLI said",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "envelope leaked {forbidden}: {serialized}"
            );
        }
        assert_eq!(envelope.code, AppErrorCode::LinkExpired);
        assert_eq!(
            envelope.message,
            AppErrorCode::LinkExpired.canonical_message()
        );
    }

    #[test]
    fn the_diagnostic_id_resolves_to_a_redacted_entry() {
        let error = AppError::new(
            AppErrorCode::SourceUnavailable,
            "connect https://cdn.example.com/x?sig=abc123 failed token=sk-live-abcdef0123456789xyz",
        );
        let mut log = DiagnosticLog::new();
        let envelope = error.to_envelope(&mut log, Timestamp::from_millis(42));
        let record = log
            .get(envelope.diagnostic_id)
            .expect("diagnostic entry must resolve");

        assert_eq!(record.code, AppErrorCode::SourceUnavailable);
        assert_eq!(record.recorded_at, Timestamp::from_millis(42));
        assert!(!record.redacted_detail.contains("https://"));
        assert!(!record.redacted_detail.contains("sk-live"));
        assert!(record.redacted_detail.contains(Redactor::PLACEHOLDER));
    }

    #[test]
    fn hints_are_typed_and_rendered_from_our_own_vocabulary() {
        let error = AppError::new(AppErrorCode::RemoteRateLimited, "429")
            .with_hint(SafeHint::RetryAfterSeconds(30));
        let mut log = DiagnosticLog::new();
        let envelope = error.to_envelope(&mut log, Timestamp::EPOCH);
        assert!(envelope
            .message
            .ends_with("You can try again in about 30 seconds."));
        assert!(envelope.retryable);
    }

    #[test]
    fn a_url_is_redacted_wherever_it_appears_in_a_token() {
        // Regression: a leading `(`, `"`, `<` or `key=` used to defeat the
        // scheme check, which is the signed-URL leak row of spec 16.2.
        let redactor = Redactor::default();
        for leaky in [
            "see (https://cdn.example/x?sig=abc) now",
            "\"https://cdn.example/x?X-Amz-Signature=deadbeefcafebabe0123\"",
            "<https://cdn.example/signed>",
            "url=https://cdn.example/signed",
            "[link](http://cdn.example/a)",
            "fetch:file:///Users/example/secret.wav",
        ] {
            let redacted = redactor.redact(leaky);
            for scheme in ["http://", "https://", "file://"] {
                assert!(
                    !redacted.contains(scheme),
                    "{leaky:?} leaked {scheme} as {redacted:?}"
                );
            }
            assert!(
                redacted.contains(Redactor::PLACEHOLDER),
                "{leaky:?} -> {redacted:?}"
            );
        }
    }

    #[test]
    fn a_dotted_credential_is_redacted_segment_by_segment() {
        // Regression: a JWT used to survive because its `.` separators made
        // the whole token fail the opaque-token shape test.
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r";
        let redacted = Redactor::default().redact(&format!("vendor said {jwt} is invalid"));
        assert!(!redacted.contains("eyJhbGciOiJIUzI1NiJ9"), "{redacted}");
        assert!(redacted.contains(Redactor::PLACEHOLDER));
        assert!(redacted.starts_with("vendor said "));
    }

    #[test]
    fn a_dotted_or_base64_credential_is_redacted_segment_by_segment() {
        // Regression: credentials with internal separators used to survive,
        // because one `.`, `+` or `/` disqualified the whole token.
        let redactor = Redactor::default();
        for credential in [
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
            "sk-or-v1-abc1234567890abcdef.def1234567890abcdef",
            "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY3ODkw+/==",
            "a=1&token=0123456789abcdefghijklmnop",
        ] {
            let redacted = redactor.redact(&format!("vendor said {credential} is invalid"));
            assert!(
                redacted.contains(Redactor::PLACEHOLDER),
                "{credential:?} survived as {redacted:?}"
            );
            assert!(redacted.starts_with("vendor said "));
        }
    }

    #[test]
    fn a_sensitive_key_is_caught_in_any_position_of_a_query_fragment() {
        let redacted = Redactor::default().redact("sv=2021-06-08&sig=abcdef&sp=r");
        assert_eq!(redacted, Redactor::PLACEHOLDER);
    }

    #[test]
    fn the_redactor_keeps_ordinary_words() {
        let redacted = Redactor::default().redact("the import stopped after 3 chunks");
        assert_eq!(redacted, "the import stopped after 3 chunks");
    }

    #[test]
    fn the_redactor_bounds_output_length() {
        let long = "word ".repeat(400);
        let redacted = Redactor::with_max_len(64).redact(&long);
        assert!(redacted.chars().count() <= 65);
    }
}
