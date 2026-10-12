//! The output-template store behind the Templates screen (spec §9.3, matrix
//! row A12).
//!
//! `plaudon_app_core::templates::TemplateRegistry` already owns what a
//! template *is*: the five built-ins compiled into the binary, the meta-schema
//! every document must satisfy, and the rule that a built-in is never edited
//! in place and never shadowed by a copy. This backend owns the only thing the
//! registry does not: where a user copy lives and what happens to the document
//! it replaces.
//!
//! * `<app_data_dir>/templates/<id>.json` is the current document of a user
//!   copy, in exactly the layout `TemplateRegistry::load_user_dir` expects, so
//!   the pipeline resolves a template without knowing this backend exists.
//! * `<app_data_dir>/templates/versions/<id>/<version>.json` is the history.
//!   An edit writes a *new* version and leaves its predecessor byte for byte
//!   where it was: a stored output records `(template_id, template_version)`
//!   and must still resolve to the document that produced it (spec §9.3).
//!
//! A missing directory means the user has made no copies, which is the normal
//! state of a fresh install and never an error. Every read and write is
//! blocking file I/O and runs on a blocking worker, and the two writing
//! commands hold a mutex: two concurrent saves would both read the same
//! current version and both decide on the same next one.
//!
//! Acceptance of a document is decided in one place —
//! `TemplateRegistry::parse_document`, the loader's own check. The problem
//! list `validate_template` returns is an *explanation* of a refusal, never a
//! second, weaker opinion about it: a document is called valid only when the
//! loader itself accepts it.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use async_trait::async_trait;
use plaudon_ai_contracts::template::{
    ExpectedSchema, ExtractionField, OutputTemplate, TemplateError, TemplateOrigin,
    TEMPLATE_SCHEMA_VERSION,
};
use plaudon_app_core::templates::{TemplateLoadError, TemplateRegistry};
use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::time::Timestamp;
use tokio::sync::Mutex;

use crate::dto::{
    ListTemplateVersionsResponse, ListTemplatesResponse, SaveTemplateResponse, TemplateDocumentDto,
    TemplateFieldDto, TemplateInstructionsDto, TemplateProblemDto, TemplateRenderingDto,
    TemplateSummaryDto, TemplateVersionDto, ValidateTemplateResponse,
};
use crate::ports::TemplatesPort;

/// Directory under the application data directory holding user copies.
pub const TEMPLATE_ROOT_DIR: &str = "templates";

/// Sub-directory of the template root holding the version history.
///
/// It has no `.json` extension, so the loader walking the template root skips
/// it and a superseded version is never mistaken for a current template.
const VERSION_DIR: &str = "versions";

/// Longest version label the template meta-schema accepts.
const MAX_VERSION_CHARS: usize = 32;
/// Longest identifier the template meta-schema accepts.
const MAX_IDENTIFIER_CHARS: usize = 64;
/// Longest display name the template meta-schema accepts.
const MAX_DISPLAY_NAME_CHARS: usize = 120;
/// Longest purpose text the template meta-schema accepts.
const MAX_PURPOSE_CHARS: usize = 2_000;
/// Longest stage instruction the template meta-schema accepts.
const MAX_INSTRUCTION_CHARS: usize = 8_000;
/// Longest field heading the template meta-schema accepts.
const MAX_HEADING_CHARS: usize = 120;
/// Longest empty-field placeholder the template meta-schema accepts.
const MAX_PLACEHOLDER_CHARS: usize = 200;
/// Most fields a template may declare.
const MAX_FIELDS: usize = 32;
/// Largest item ceiling a field may declare.
const MAX_FIELD_ITEMS: u32 = 1_000;
/// Largest text ceiling a field may declare.
const MAX_FIELD_TEXT_CHARS: u32 = 4_000;
/// Largest anchor count a field may require or permit.
const MAX_FIELD_ANCHORS: u32 = 8;
/// How many successors are tried before a version label is given up on.
const MAX_VERSION_PROBES: u64 = 1_000;

/* ------------------------------------------------------- problem codes */

/// The document is not JSON, or not shaped like a template.
const PROBLEM_MALFORMED: &str = "document_malformed";
/// The document declares a template format this build does not read.
const PROBLEM_SCHEMA_VERSION: &str = "unsupported_schema_version";
/// The identifier is not a lowercase identifier.
const PROBLEM_ID: &str = "id_invalid";
/// The version label is empty, too long, or not storable as a file name.
const PROBLEM_VERSION_LABEL: &str = "version_label_invalid";
/// The displayed name is empty or too long.
const PROBLEM_DISPLAY_NAME: &str = "display_name_invalid";
/// The purpose text is empty or too long.
const PROBLEM_PURPOSE: &str = "purpose_invalid";
/// A provenance label recorded on every output is empty or too long.
const PROBLEM_PROVENANCE_VERSION: &str = "provenance_version_invalid";
/// A stage instruction is missing or too long.
const PROBLEM_INSTRUCTIONS: &str = "instructions_invalid";
/// The document claims to be a built-in, which is never stored as a copy.
const PROBLEM_BUILT_IN: &str = "origin_built_in";
/// A user copy names its origin with an unusable identifier or version.
const PROBLEM_ORIGIN: &str = "origin_invalid";
/// The document declares no fields, or more than the format allows.
const PROBLEM_FIELD_COUNT: &str = "field_count_invalid";
/// A field key is not a lowercase identifier.
const PROBLEM_FIELD_KEY: &str = "field_key_invalid";
/// Two fields share one key.
const PROBLEM_FIELD_DUPLICATE: &str = "field_key_duplicate";
/// A field heading is empty or too long.
const PROBLEM_FIELD_HEADING: &str = "field_heading_invalid";
/// A field item or text ceiling is outside the permitted range.
const PROBLEM_FIELD_LIMITS: &str = "field_limits_invalid";
/// A field's anchor requirement is impossible to satisfy.
const PROBLEM_FIELD_ANCHORS: &str = "field_anchors_invalid";
/// The display order does not list every field exactly once.
const PROBLEM_RENDERING_ORDER: &str = "rendering_order_invalid";
/// The display order names a field the document does not declare.
const PROBLEM_RENDERING_UNKNOWN: &str = "rendering_order_unknown_field";
/// The empty-field placeholder is missing or too long.
const PROBLEM_PLACEHOLDER: &str = "empty_placeholder_invalid";
/// The document references an output schema this build does not ship.
const PROBLEM_SCHEMA_REFERENCE: &str = "output_schema_reference_unknown";
/// The inline output schema is not a JSON Schema object.
const PROBLEM_SCHEMA_INLINE: &str = "output_schema_invalid";
/// The loader refused the document for a reason nothing above explains.
const PROBLEM_REJECTED: &str = "document_rejected";

/// The user's editable copies of the shipped output templates.
#[derive(Debug)]
pub struct TemplatesBackend {
    root: PathBuf,
    /// One writer at a time. The next version of a template is derived from
    /// the stored one, so two concurrent saves must not read it at once.
    write_lock: Mutex<()>,
}

impl TemplatesBackend {
    /// Open the template store inside `app_data_dir`.
    ///
    /// Nothing is created here: a user with no copies has no directory, and a
    /// read of a store that does not exist yet is an empty store.
    #[must_use]
    pub fn new(app_data_dir: &Path) -> Self {
        Self {
            root: app_data_dir.join(TEMPLATE_ROOT_DIR),
            write_lock: Mutex::new(()),
        }
    }

    /// The directory holding the current document of every user copy.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[async_trait]
impl TemplatesPort for TemplatesBackend {
    async fn list_templates(&self) -> Result<ListTemplatesResponse, AppError> {
        let root = self.root.clone();
        blocking(move || list_all(&root)).await
    }

    async fn get_template(
        &self,
        template_id: String,
        version: Option<String>,
    ) -> Result<TemplateDocumentDto, AppError> {
        let root = self.root.clone();
        blocking(move || read_template(&root, &template_id, version.as_deref())).await
    }

    async fn create_template_copy(
        &self,
        base_id: String,
        new_id: String,
        display_name: Option<String>,
    ) -> Result<TemplateDocumentDto, AppError> {
        let _writing = self.write_lock.lock().await;
        let root = self.root.clone();
        blocking(move || create_copy(&root, &base_id, &new_id, display_name.as_deref())).await
    }

    async fn save_template(
        &self,
        template_id: String,
        document: String,
        expected_version: Option<String>,
    ) -> Result<SaveTemplateResponse, AppError> {
        let _writing = self.write_lock.lock().await;
        let root = self.root.clone();
        blocking(move || {
            store_next_version(&root, &template_id, &document, expected_version.as_deref())
        })
        .await
    }

    async fn validate_template(
        &self,
        document: String,
    ) -> Result<ValidateTemplateResponse, AppError> {
        blocking(move || {
            let problems = inspect(&document);
            Ok::<_, AppError>(ValidateTemplateResponse {
                valid: problems.is_empty(),
                problems,
            })
        })
        .await
    }

    async fn list_template_versions(
        &self,
        template_id: String,
    ) -> Result<ListTemplateVersionsResponse, AppError> {
        let root = self.root.clone();
        blocking(move || version_history(&root, &template_id)).await
    }
}

/// Run one blocking template-store operation off the async executor.
async fn blocking<T>(
    task: impl FnOnce() -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError>
where
    T: Send + 'static,
{
    tokio::task::spawn_blocking(task).await.map_err(|_| {
        AppError::new(
            AppErrorCode::InternalError,
            "the template store worker stopped",
        )
    })?
}

/* ------------------------------------------------------------ operations */

/// Built-ins first, then user copies, each in identity order.
fn list_all(root: &Path) -> Result<ListTemplatesResponse, AppError> {
    let registry = open_registry(root)?;
    let mut templates = Vec::new();
    for summary in registry.list() {
        let template = registry.get(&summary.id).ok_or_else(unresolved)?;
        let (base_id, base_version) = origin_of(template);
        let version_count = if summary.built_in {
            1
        } else {
            stored_versions(root, &template.id, &template.version)?.len() as u32
        };
        templates.push(TemplateSummaryDto {
            template_id: template.id.clone(),
            version: template.version.clone(),
            display_name: template.display_name.clone(),
            purpose: template.purpose.clone(),
            built_in: summary.built_in,
            base_id,
            base_version,
            field_count: template.fields.len() as u32,
            version_count,
        });
    }
    Ok(ListTemplatesResponse { templates })
}

/// One template at its current version, or at one stored version.
fn read_template(
    root: &Path,
    template_id: &str,
    version: Option<&str>,
) -> Result<TemplateDocumentDto, AppError> {
    if !is_identifier(template_id) {
        return Err(not_found());
    }
    let registry = open_registry(root)?;
    let current = registry.get(template_id).ok_or_else(not_found)?;
    let Some(version) = version else {
        return document_dto(current);
    };
    if version == current.version {
        return document_dto(current);
    }
    // A built-in is compiled into the binary and has exactly one version, so
    // any other label names a version that was never stored.
    if current.origin.is_built_in() || !is_version_label(version) {
        return Err(not_found());
    }
    let stored = read_file(&version_path(root, template_id, version))?.ok_or_else(not_found)?;
    let template = TemplateRegistry::parse_document(&stored).map_err(unreadable)?;
    document_dto(&template)
}

/// Create an editable copy of a built-in and store it as its first version.
fn create_copy(
    root: &Path,
    base_id: &str,
    new_id: &str,
    display_name: Option<&str>,
) -> Result<TemplateDocumentDto, AppError> {
    if !is_identifier(new_id) {
        return Err(invalid(
            "a template identifier must be a lowercase identifier",
        ));
    }
    if !is_identifier(base_id) {
        return Err(not_found());
    }
    let mut registry = open_registry(root)?;
    let base_is_built_in = registry
        .list()
        .into_iter()
        .any(|summary| summary.built_in && summary.id == base_id);
    if !base_is_built_in {
        return Err(not_found());
    }
    let mut copy = registry
        .copy_builtin(base_id, new_id)
        .map_err(|error| match error {
            TemplateLoadError::ReservedId { .. } | TemplateLoadError::DuplicateId { .. } => {
                AppError::new(
                    AppErrorCode::Conflict,
                    "a template already uses that identifier",
                )
            }
            other => unreadable(other),
        })?;
    copy.display_name = match display_name {
        Some(name) if name.trim().is_empty() => {
            return Err(invalid("a template name must not be empty"))
        }
        Some(name) => name.to_owned(),
        None => derived_name(&copy.display_name),
    };

    let document = canonical_document(&copy)?;
    // What is written has to be what the loader reads back on the next start.
    TemplateRegistry::parse_document(&document).map_err(|error| refused(&error))?;
    write_file(&version_path(root, new_id, &copy.version), &document)?;
    write_file(&document_path(root, new_id), &document)?;
    document_dto(&copy)
}

/// Store an edited user copy as a new version, keeping its predecessor.
fn store_next_version(
    root: &Path,
    template_id: &str,
    document: &str,
    expected_version: Option<&str>,
) -> Result<SaveTemplateResponse, AppError> {
    if !is_identifier(template_id) {
        return Err(invalid(
            "a template identifier must be a lowercase identifier",
        ));
    }
    let submitted = TemplateRegistry::parse_document(document).map_err(|error| refused(&error))?;
    if submitted.origin.is_built_in() {
        return Err(invalid(
            "a built-in template is never edited in place; edit a copy of it",
        ));
    }
    if submitted.id != template_id {
        return Err(invalid(
            "the document names a different template than the one being saved",
        ));
    }
    if !is_version_label(&submitted.version) {
        return Err(invalid("the document's version label cannot be stored"));
    }

    let registry = open_registry(root)?;
    if registry
        .list()
        .into_iter()
        .any(|summary| summary.built_in && summary.id == template_id)
    {
        return Err(AppError::new(
            AppErrorCode::Conflict,
            "that identifier belongs to a built-in template",
        ));
    }
    let stored_text = read_file(&document_path(root, template_id))?.ok_or_else(not_found)?;
    let stored = TemplateRegistry::parse_document(&stored_text).map_err(unreadable)?;
    if expected_version.is_some_and(|expected| expected != stored.version) {
        return Err(AppError::new(
            AppErrorCode::Conflict,
            "the stored template changed after the editor read it",
        ));
    }

    let taken = stored_versions(root, template_id, &stored.version)?;
    let next = if submitted.version == stored.version {
        successor(&stored.version, &taken)?
    } else if taken.contains(&submitted.version) {
        return Err(AppError::new(
            AppErrorCode::Conflict,
            "that version was already stored and is never rewritten",
        ));
    } else {
        submitted.version.clone()
    };

    let mut saved = submitted;
    saved.version = next;
    let saved_document = canonical_document(&saved)?;
    TemplateRegistry::parse_document(&saved_document).map_err(|error| refused(&error))?;

    // An output may already record the superseded version; make sure it is
    // readable even if the history file was never written (a hand-placed copy).
    let previous = version_path(root, template_id, &stored.version);
    if read_file(&previous)?.is_none() {
        write_file(&previous, &canonical_document(&stored)?)?;
    }
    let history = version_path(root, template_id, &saved.version);
    write_file(&history, &saved_document)?;
    if let Err(error) = write_file(&document_path(root, template_id), &saved_document) {
        // The new version is not current, so it must not be listed either.
        let _ = fs::remove_file(&history);
        return Err(error);
    }

    Ok(SaveTemplateResponse {
        template: document_dto(&saved)?,
        previous_version: Some(stored.version),
    })
}

/// The stored versions of one template, newest first.
fn version_history(
    root: &Path,
    template_id: &str,
) -> Result<ListTemplateVersionsResponse, AppError> {
    if !is_identifier(template_id) {
        return Err(not_found());
    }
    let registry = open_registry(root)?;
    let current = registry.get(template_id).ok_or_else(not_found)?;
    if current.origin.is_built_in() {
        // A built-in ships inside the binary: there is one version, and no
        // stored file whose time could be reported, so none is invented.
        return Ok(ListTemplateVersionsResponse {
            versions: vec![TemplateVersionDto {
                version: current.version.clone(),
                created_at: None,
                is_current: true,
            }],
        });
    }

    let mut found = history_files(root, template_id)?;
    if !found.iter().any(|(version, _)| version == &current.version) {
        found.push((
            current.version.clone(),
            modified_at(&document_path(root, template_id)),
        ));
    }
    // Newest first, with a label that has no readable time sorting last
    // rather than claiming to be the oldest.
    found.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));
    Ok(ListTemplateVersionsResponse {
        versions: found
            .into_iter()
            .map(|(version, at)| TemplateVersionDto {
                is_current: version == current.version,
                version,
                created_at: at.and_then(rfc3339),
            })
            .collect(),
    })
}

/* ------------------------------------------------------------ validation */

/// Every problem that stops `save_template` from accepting a document.
///
/// The authority is the loader: a document with no problem listed here is one
/// `TemplateRegistry::parse_document` accepts. The individual problems exist
/// to tell the editor *which* rule was broken; a refusal nothing explains is
/// still reported, never swallowed.
///
/// No message quotes the document or a path. A user copy carries the user's
/// own wording, and a field is named by its position instead.
fn inspect(document: &str) -> Vec<TemplateProblemDto> {
    let template: OutputTemplate = match serde_json::from_str(document) {
        Ok(template) => template,
        Err(error) => return vec![problem(PROBLEM_MALFORMED, malformed_message(&error))],
    };

    let mut problems = Vec::new();
    if template.template_schema_version != TEMPLATE_SCHEMA_VERSION {
        problems.push(problem(
            PROBLEM_SCHEMA_VERSION,
            format!(
                "This document is written for template format {} and this version of the application reads format {TEMPLATE_SCHEMA_VERSION}.",
                template.template_schema_version
            ),
        ));
    }
    if !is_identifier(&template.id) {
        problems.push(problem(
            PROBLEM_ID,
            "The identifier must be lowercase letters, digits and underscores, and at most 64 characters.".to_owned(),
        ));
    }
    if !is_version_label(&template.version) {
        problems.push(problem(
            PROBLEM_VERSION_LABEL,
            "The version must be at most 32 letters, digits, dots, dashes, underscores or plus signs.".to_owned(),
        ));
    }
    if !is_bounded_text(&template.display_name, MAX_DISPLAY_NAME_CHARS) {
        problems.push(problem(
            PROBLEM_DISPLAY_NAME,
            "The name must not be empty and must be at most 120 characters.".to_owned(),
        ));
    }
    if !is_bounded_text(&template.purpose, MAX_PURPOSE_CHARS) {
        problems.push(problem(
            PROBLEM_PURPOSE,
            "The purpose must not be empty and must be at most 2000 characters.".to_owned(),
        ));
    }
    if !is_bounded_text(&template.prompt_version, MAX_VERSION_CHARS)
        || !is_bounded_text(&template.output_schema_version, MAX_VERSION_CHARS)
    {
        problems.push(problem(
            PROBLEM_PROVENANCE_VERSION,
            "The prompt and output-schema versions are recorded on every result, so both must be present and at most 32 characters.".to_owned(),
        ));
    }
    let instructions = [
        &template.instructions.system,
        &template.instructions.section_summary,
        &template.instructions.synthesis,
        &template.instructions.extraction,
    ];
    if instructions
        .iter()
        .any(|text| !is_bounded_text(text, MAX_INSTRUCTION_CHARS))
    {
        problems.push(problem(
            PROBLEM_INSTRUCTIONS,
            "Every stage instruction must be present and at most 8000 characters.".to_owned(),
        ));
    }
    match &template.origin {
        TemplateOrigin::BuiltIn => problems.push(problem(
            PROBLEM_BUILT_IN,
            "This document is marked as a built-in template. Only a copy can be saved.".to_owned(),
        )),
        TemplateOrigin::UserCopy {
            base_id,
            base_version,
        } => {
            if !is_identifier(base_id) || !is_version_label(base_version) {
                problems.push(problem(
                    PROBLEM_ORIGIN,
                    "The template this copy was made from is named with an unusable identifier or version.".to_owned(),
                ));
            }
        }
    }
    inspect_fields(&template, &mut problems);
    inspect_rendering(&template, &mut problems);
    inspect_output_schema(&template, &mut problems);

    if problems.is_empty() && TemplateRegistry::parse_document(document).is_err() {
        problems.push(problem(
            PROBLEM_REJECTED,
            "This document does not match the template format this version of the application accepts.".to_owned(),
        ));
    }
    problems
}

/// Field declarations, named by position so no wording is echoed.
fn inspect_fields(template: &OutputTemplate, problems: &mut Vec<TemplateProblemDto>) {
    if template.fields.is_empty() || template.fields.len() > MAX_FIELDS {
        problems.push(problem(
            PROBLEM_FIELD_COUNT,
            "A template must declare between one and 32 fields.".to_owned(),
        ));
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (index, field) in template.fields.iter().enumerate() {
        let position = index + 1;
        if is_identifier(&field.key) {
            if !seen.insert(field.key.as_str()) {
                problems.push(problem(
                    PROBLEM_FIELD_DUPLICATE,
                    format!("Field {position} repeats the key of an earlier field."),
                ));
            }
        } else {
            problems.push(problem(
                PROBLEM_FIELD_KEY,
                format!("Field {position} has a key that is not lowercase letters, digits and underscores."),
            ));
        }
        if !is_bounded_text(&field.heading, MAX_HEADING_CHARS) {
            problems.push(problem(
                PROBLEM_FIELD_HEADING,
                format!("Field {position} needs a heading of at most 120 characters."),
            ));
        }
        if !(1..=MAX_FIELD_ITEMS).contains(&field.max_items)
            || !(1..=MAX_FIELD_TEXT_CHARS).contains(&field.max_text_chars)
        {
            problems.push(problem(
                PROBLEM_FIELD_LIMITS,
                format!("Field {position} must allow between 1 and 1000 items of at most 4000 characters each."),
            ));
        }
        if !anchors_satisfiable(field) {
            problems.push(problem(
                PROBLEM_FIELD_ANCHORS,
                format!("Field {position} must permit between one and eight anchors per item, and at least as many as it requires."),
            ));
        }
    }
}

/// Whether a field's anchor requirement can be met at all.
fn anchors_satisfiable(field: &ExtractionField) -> bool {
    let anchors = field.anchors;
    anchors.max_per_item >= 1
        && anchors.max_per_item <= MAX_FIELD_ANCHORS
        && anchors.min_per_item <= MAX_FIELD_ANCHORS
        && anchors.max_per_item >= anchors.min_per_item
}

/// Rendering rules: every field shown exactly once, with a placeholder.
fn inspect_rendering(template: &OutputTemplate, problems: &mut Vec<TemplateProblemDto>) {
    let declared: BTreeSet<&str> = template
        .fields
        .iter()
        .map(|field| field.key.as_str())
        .collect();
    let ordered: BTreeSet<&str> = template
        .rendering
        .order
        .iter()
        .map(String::as_str)
        .collect();
    if ordered.iter().any(|key| !declared.contains(key)) {
        problems.push(problem(
            PROBLEM_RENDERING_UNKNOWN,
            "The display order lists a field this template does not declare.".to_owned(),
        ));
    }
    if template.rendering.order.len() != template.fields.len() || ordered.len() != declared.len() {
        problems.push(problem(
            PROBLEM_RENDERING_ORDER,
            "The display order must list every field of the template exactly once.".to_owned(),
        ));
    }
    if !is_bounded_text(&template.rendering.empty_placeholder, MAX_PLACEHOLDER_CHARS) {
        problems.push(problem(
            PROBLEM_PLACEHOLDER,
            "The text shown for an empty field must not be empty and must be at most 200 characters.".to_owned(),
        ));
    }
}

/// The expected output schema, inline or shipped.
fn inspect_output_schema(template: &OutputTemplate, problems: &mut Vec<TemplateProblemDto>) {
    match &template.output_schema {
        ExpectedSchema::Reference { schema_ref } => {
            if schema_ref != plaudon_ai_contracts::template::MEETING_EXTRACTION_SCHEMA_ID {
                problems.push(problem(
                    PROBLEM_SCHEMA_REFERENCE,
                    "The output schema names a schema this version of the application does not ship.".to_owned(),
                ));
            }
        }
        ExpectedSchema::Inline(schema) => {
            if !schema.get("type").is_some_and(serde_json::Value::is_string) {
                problems.push(problem(
                    PROBLEM_SCHEMA_INLINE,
                    "An inline output schema must be a JSON Schema object declaring a type."
                        .to_owned(),
                ));
            }
        }
    }
}

/// Why a document could not even be decoded, without quoting it.
fn malformed_message(error: &serde_json::Error) -> String {
    let reason = match error.classify() {
        serde_json::error::Category::Syntax => "it is not valid JSON",
        serde_json::error::Category::Data => "a value is missing or has the wrong type",
        serde_json::error::Category::Eof => "it ends before the document is complete",
        serde_json::error::Category::Io => "it could not be read",
    };
    format!(
        "This document could not be read as a template: {reason} (line {}).",
        error.line()
    )
}

/// One reported problem.
fn problem(code: &str, message: String) -> TemplateProblemDto {
    TemplateProblemDto {
        code: code.to_owned(),
        message,
    }
}

/* ---------------------------------------------------------------- store */

/// The registry with the built-ins and every user copy on disk.
pub(super) fn open_registry(root: &Path) -> Result<TemplateRegistry, AppError> {
    let mut registry = TemplateRegistry::with_builtins();
    registry.load_user_dir(root).map_err(unreadable)?;
    Ok(registry)
}

/// Path of the current document of one user copy.
fn document_path(root: &Path, template_id: &str) -> PathBuf {
    root.join(format!("{template_id}.json"))
}

/// Path of one stored version of one template.
fn version_path(root: &Path, template_id: &str, version: &str) -> PathBuf {
    root.join(VERSION_DIR)
        .join(template_id)
        .join(format!("{version}.json"))
}

/// Every version label this template has on disk, including the current one.
fn stored_versions(
    root: &Path,
    template_id: &str,
    current: &str,
) -> Result<BTreeSet<String>, AppError> {
    let mut versions: BTreeSet<String> = history_files(root, template_id)?
        .into_iter()
        .map(|(version, _)| version)
        .collect();
    versions.insert(current.to_owned());
    Ok(versions)
}

/// The history files of one template, with the time each was written.
fn history_files(
    root: &Path,
    template_id: &str,
) -> Result<Vec<(String, Option<SystemTime>)>, AppError> {
    let dir = root.join(VERSION_DIR).join(template_id);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        // No history directory means nothing has been superseded yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(storage(&error)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| storage(&error))?;
        let path = entry.path();
        if path.extension() != Some(OsStr::new("json")) {
            continue;
        }
        let Some(version) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if !is_version_label(version) {
            continue;
        }
        let at = entry.metadata().ok().and_then(|data| data.modified().ok());
        found.push((version.to_owned(), at));
    }
    Ok(found)
}

/// Read a stored document, or `None` when there is none.
fn read_file(path: &Path) -> Result<Option<String>, AppError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(storage(&error)),
    }
}

/// Write a document, replacing any previous one in a single step.
///
/// The staging file carries a `.tmp` extension, so a half-written document is
/// never picked up by the loader even if the process stops between the two
/// calls.
fn write_file(path: &Path, document: &str) -> Result<(), AppError> {
    let parent = path.parent().ok_or_else(|| {
        AppError::new(
            AppErrorCode::InternalError,
            "a template path has no directory",
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| storage(&error))?;
    let staging = path.with_extension("json.tmp");
    fs::write(&staging, document).map_err(|error| storage(&error))?;
    if let Err(error) = fs::rename(&staging, path) {
        let _ = fs::remove_file(&staging);
        return Err(storage(&error));
    }
    Ok(())
}

/// The modification time of a stored file, when it has one.
fn modified_at(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

/// A file time as RFC 3339, or `None` when it cannot be represented.
fn rfc3339(at: SystemTime) -> Option<String> {
    let millis = match at.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(since) => i64::try_from(since.as_millis()).ok()?,
        Err(before) => i64::try_from(before.duration().as_millis())
            .ok()
            .and_then(i64::checked_neg)?,
    };
    Timestamp::from_millis(millis).to_rfc3339()
}

/// The next free version label after `current`.
///
/// The trailing numeric component is incremented (`1.0.0` → `1.0.1`); a label
/// that does not end in a number gets a numeric suffix (`draft` → `draft.1`).
/// Either way the result is a label nothing in the history already uses, so an
/// edit can never overwrite a version an output recorded.
fn successor(current: &str, taken: &BTreeSet<String>) -> Result<String, AppError> {
    let (prefix, first) = match current.rsplit_once('.') {
        Some((head, tail)) => match tail.parse::<u64>() {
            Ok(number) => (format!("{head}."), number.saturating_add(1)),
            Err(_) => (format!("{current}."), 1),
        },
        None => match current.parse::<u64>() {
            Ok(number) => (String::new(), number.saturating_add(1)),
            Err(_) => (format!("{current}."), 1),
        },
    };
    for step in 0..MAX_VERSION_PROBES {
        let candidate = format!("{prefix}{}", first.saturating_add(step));
        if is_version_label(&candidate) && !taken.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err(AppError::new(
        AppErrorCode::Conflict,
        "no unused version label follows the current one",
    ))
}

/// The name a copy gets when the user did not choose one.
fn derived_name(base: &str) -> String {
    let derived = format!("{base} (copy)");
    if derived.chars().count() <= MAX_DISPLAY_NAME_CHARS {
        derived
    } else {
        base.to_owned()
    }
}

/* ----------------------------------------------------------- conversion */

/// The canonical form of a document: pretty-printed, newline-terminated.
///
/// Everything the store writes and everything `get_template` hands the editor
/// goes through here, so the text in the editor and the text on disk are the
/// same text.
fn canonical_document(template: &OutputTemplate) -> Result<String, AppError> {
    let mut document = serde_json::to_string_pretty(template).map_err(|error| {
        AppError::new(
            AppErrorCode::InternalError,
            format!("a template could not be rendered: {error}"),
        )
    })?;
    document.push('\n');
    Ok(document)
}

/// One template as the editor and the viewer both see it.
fn document_dto(template: &OutputTemplate) -> Result<TemplateDocumentDto, AppError> {
    let (base_id, base_version) = origin_of(template);
    Ok(TemplateDocumentDto {
        template_id: template.id.clone(),
        version: template.version.clone(),
        display_name: template.display_name.clone(),
        purpose: template.purpose.clone(),
        built_in: template.origin.is_built_in(),
        base_id,
        base_version,
        prompt_version: template.prompt_version.clone(),
        output_schema_version: template.output_schema_version.clone(),
        instructions: TemplateInstructionsDto {
            system: template.instructions.system.clone(),
            section_summary: template.instructions.section_summary.clone(),
            synthesis: template.instructions.synthesis.clone(),
            extraction: template.instructions.extraction.clone(),
        },
        fields: template.fields.iter().map(field_dto).collect(),
        rendering: TemplateRenderingDto {
            order: template.rendering.order.clone(),
            empty_placeholder: template.rendering.empty_placeholder.clone(),
            show_anchors: template.rendering.show_anchors,
        },
        document: canonical_document(template)?,
    })
}

/// One extraction field as the editor lists it.
fn field_dto(field: &ExtractionField) -> TemplateFieldDto {
    TemplateFieldDto {
        key: field.key.clone(),
        kind: field.kind.as_str().to_owned(),
        heading: field.heading.clone(),
        min_anchors: field.anchors.min_per_item,
        max_anchors: field.anchors.max_per_item,
        max_items: field.max_items,
        max_text_chars: field.max_text_chars,
    }
}

/// The built-in a user copy derives from, if any.
fn origin_of(template: &OutputTemplate) -> (Option<String>, Option<String>) {
    match &template.origin {
        TemplateOrigin::BuiltIn => (None, None),
        TemplateOrigin::UserCopy {
            base_id,
            base_version,
        } => (Some(base_id.clone()), Some(base_version.clone())),
    }
}

/* ---------------------------------------------------------------- rules */

/// Whether a value is a lowercase identifier the store can use as a name.
fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_CHARS
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Whether a version label can be stored and named back.
fn is_version_label(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_VERSION_CHARS
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+'))
}

/// Whether a text is present and within its ceiling.
fn is_bounded_text(value: &str, max_chars: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= max_chars
}

/* --------------------------------------------------------------- errors */

/// A template or version the store does not hold.
fn not_found() -> AppError {
    AppError::new(
        AppErrorCode::RecordNotFound,
        "no such template or template version",
    )
}

/// A request the store cannot act on.
fn invalid(detail: &str) -> AppError {
    AppError::new(AppErrorCode::InvalidArgument, detail)
}

/// A submitted document the loader refused.
fn refused(error: &TemplateError) -> AppError {
    AppError::new(
        AppErrorCode::InvalidArgument,
        format!("the submitted template was refused: {error}"),
    )
}

/// A stored document that can no longer be read.
fn unreadable(error: impl std::fmt::Display) -> AppError {
    AppError::new(
        AppErrorCode::StorageUnavailable,
        format!("the template store could not be read: {error}"),
    )
}

/// A file the store could not read or write.
fn storage(error: &std::io::Error) -> AppError {
    AppError::new(
        AppErrorCode::StorageUnavailable,
        format!("the template store failed: {error}"),
    )
}

/// A listed template that did not resolve, which the registry rules out.
fn unresolved() -> AppError {
    AppError::new(
        AppErrorCode::InternalError,
        "a listed template did not resolve",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::{json, Value};

    fn backend() -> (TemplatesBackend, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (TemplatesBackend::new(dir.path()), dir)
    }

    async fn a_copy(backend: &TemplatesBackend) -> TemplateDocumentDto {
        backend
            .create_template_copy("general_meeting".into(), "my_meeting".into(), None)
            .await
            .expect("a copy of a built-in is created")
    }

    /// Edit one value of a document and hand the result back as text.
    fn edited(document: &str, mutate: impl FnOnce(&mut Value)) -> String {
        let mut value: Value = serde_json::from_str(document).expect("the document is JSON");
        mutate(&mut value);
        serde_json::to_string_pretty(&value).expect("re-serializes")
    }

    #[tokio::test]
    async fn a_fresh_install_lists_the_five_built_ins_and_no_copies() {
        let (backend, _dir) = backend();
        let listed = backend.list_templates().await.expect("the store lists");
        let ids: Vec<&str> = listed
            .templates
            .iter()
            .map(|template| template.template_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "client_call",
                "general_meeting",
                "interview",
                "lecture_notes",
                "project_meeting"
            ]
        );
        for template in &listed.templates {
            assert!(template.built_in);
            assert_eq!(template.version_count, 1);
            assert_eq!(template.base_id, None);
            assert!(template.field_count >= 6);
        }
        assert!(!backend.root().exists(), "nothing is created by a read");
    }

    #[tokio::test]
    async fn a_copy_is_listed_after_the_built_ins_and_records_its_base() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        assert!(!copy.built_in);
        assert_eq!(copy.base_id.as_deref(), Some("general_meeting"));
        assert_eq!(copy.display_name, "General Meeting (copy)");

        let listed = backend.list_templates().await.expect("the store lists");
        let last = listed.templates.last().expect("a copy is listed");
        assert_eq!(last.template_id, "my_meeting");
        assert!(!last.built_in);
        assert_eq!(last.version_count, 1);
        assert_eq!(last.base_version.as_deref(), Some(copy.version.as_str()));
    }

    #[tokio::test]
    async fn a_second_copy_under_the_same_identifier_is_a_conflict() {
        let (backend, _dir) = backend();
        a_copy(&backend).await;
        let again = backend
            .create_template_copy("general_meeting".into(), "my_meeting".into(), None)
            .await
            .expect_err("the identifier is taken");
        assert_eq!(again.code(), AppErrorCode::Conflict);

        let reserved = backend
            .create_template_copy("general_meeting".into(), "interview".into(), None)
            .await
            .expect_err("a built-in identifier is reserved");
        assert_eq!(reserved.code(), AppErrorCode::Conflict);
    }

    #[tokio::test]
    async fn copying_a_template_that_does_not_exist_is_not_found() {
        let (backend, _dir) = backend();
        let missing = backend
            .create_template_copy("no_such_template".into(), "my_copy".into(), None)
            .await
            .expect_err("an unknown base is refused");
        assert_eq!(missing.code(), AppErrorCode::RecordNotFound);
    }

    #[tokio::test]
    async fn two_edits_keep_both_predecessors_and_only_the_last_is_current() {
        let (backend, dir) = backend();
        let copy = a_copy(&backend).await;
        assert_eq!(copy.version, "1.0.0");

        let first = backend
            .save_template(
                "my_meeting".into(),
                edited(&copy.document, |value| {
                    value["purpose"] = json!("A first revision of the purpose.");
                }),
                Some(copy.version.clone()),
            )
            .await
            .expect("the first edit is stored");
        assert_eq!(first.template.version, "1.0.1");
        assert_eq!(first.previous_version.as_deref(), Some("1.0.0"));

        let second = backend
            .save_template(
                "my_meeting".into(),
                edited(&first.template.document, |value| {
                    value["purpose"] = json!("A second revision of the purpose.");
                }),
                Some(first.template.version.clone()),
            )
            .await
            .expect("the second edit is stored");
        assert_eq!(second.template.version, "1.0.2");
        assert_eq!(second.previous_version.as_deref(), Some("1.0.1"));

        let history = backend
            .list_template_versions("my_meeting".into())
            .await
            .expect("the history is listed");
        let labels: Vec<&str> = history
            .versions
            .iter()
            .map(|version| version.version.as_str())
            .collect();
        assert_eq!(labels.len(), 3, "{labels:?}");
        assert!(labels.contains(&"1.0.0") && labels.contains(&"1.0.1"));
        assert_eq!(
            history
                .versions
                .iter()
                .filter(|version| version.is_current)
                .map(|version| version.version.as_str())
                .collect::<Vec<_>>(),
            vec!["1.0.2"]
        );
        for version in &history.versions {
            let at = version
                .created_at
                .as_deref()
                .expect("a stored version reports when it was written");
            assert!(at.contains('T'), "{at} is not an RFC 3339 time");
        }

        // The superseded document is unchanged and still readable.
        let original = backend
            .get_template("my_meeting".into(), Some("1.0.0".into()))
            .await
            .expect("the first version still resolves");
        assert_eq!(original.purpose, copy.purpose);
        assert_eq!(original.document, copy.document);

        let stored = dir.path().join("templates/versions/my_meeting/1.0.0.json");
        assert_eq!(
            fs::read_to_string(stored).expect("the history file exists"),
            copy.document
        );

        // And the current document is what the pipeline loader resolves.
        let mut registry = TemplateRegistry::with_builtins();
        let loaded = registry
            .load_user_dir(&dir.path().join("templates"))
            .expect("the store round-trips through the loader");
        assert_eq!(loaded, 1);
        let resolved = registry.get("my_meeting").expect("the copy resolves");
        assert_eq!(resolved.version, "1.0.2");
        assert_eq!(resolved.purpose, "A second revision of the purpose.");
    }

    #[tokio::test]
    async fn a_version_label_without_a_number_gets_a_numeric_suffix() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        let labelled = backend
            .save_template(
                "my_meeting".into(),
                edited(&copy.document, |value| value["version"] = json!("draft")),
                Some(copy.version.clone()),
            )
            .await
            .expect("an explicit label is accepted");
        assert_eq!(labelled.template.version, "draft");

        let next = backend
            .save_template(
                "my_meeting".into(),
                labelled.template.document.clone(),
                Some("draft".into()),
            )
            .await
            .expect("the successor is derived");
        assert_eq!(next.template.version, "draft.1");
    }

    #[tokio::test]
    async fn a_version_already_in_the_history_is_a_conflict() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        let saved = backend
            .save_template(
                "my_meeting".into(),
                copy.document.clone(),
                Some(copy.version.clone()),
            )
            .await
            .expect("the first edit is stored");
        assert_eq!(saved.template.version, "1.0.1");

        let reused = backend
            .save_template(
                "my_meeting".into(),
                edited(&saved.template.document, |value| {
                    value["version"] = json!("1.0.0");
                }),
                Some(saved.template.version.clone()),
            )
            .await
            .expect_err("a stored version is never rewritten");
        assert_eq!(reused.code(), AppErrorCode::Conflict);
    }

    #[tokio::test]
    async fn a_stale_expected_version_is_a_conflict_and_stores_nothing() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        backend
            .save_template(
                "my_meeting".into(),
                copy.document.clone(),
                Some(copy.version.clone()),
            )
            .await
            .expect("the first editor saves");

        let stale = backend
            .save_template(
                "my_meeting".into(),
                edited(&copy.document, |value| {
                    value["purpose"] = json!("An edit from an editor opened before the save.");
                }),
                Some(copy.version.clone()),
            )
            .await
            .expect_err("the second editor started from a replaced document");
        assert_eq!(stale.code(), AppErrorCode::Conflict);

        let current = backend
            .get_template("my_meeting".into(), None)
            .await
            .expect("the template still resolves");
        assert_eq!(current.version, "1.0.1");
        assert_eq!(current.purpose, copy.purpose);
    }

    #[tokio::test]
    async fn a_built_in_is_never_edited_in_place() {
        let (backend, dir) = backend();
        let built_in = backend
            .get_template("general_meeting".into(), None)
            .await
            .expect("a built-in resolves");
        assert!(built_in.built_in);

        let refusal = backend
            .save_template(
                "general_meeting".into(),
                edited(&built_in.document, |value| {
                    value["purpose"] = json!("An attempt to edit a shipped template.");
                }),
                None,
            )
            .await
            .expect_err("a built-in document is refused");
        assert_eq!(refusal.code(), AppErrorCode::InvalidArgument);

        // Even a document that calls itself a copy cannot take the identifier.
        let shadow = backend
            .save_template(
                "general_meeting".into(),
                edited(&built_in.document, |value| {
                    value["origin"] = json!("user_copy");
                    value["base_id"] = json!("general_meeting");
                    value["base_version"] = json!("1.0.0");
                }),
                None,
            )
            .await
            .expect_err("a built-in identifier is reserved");
        assert_eq!(shadow.code(), AppErrorCode::Conflict);

        assert!(!dir.path().join("templates").exists(), "nothing was stored");
        let after = backend
            .get_template("general_meeting".into(), None)
            .await
            .expect("the built-in is unchanged");
        assert_eq!(after.document, built_in.document);
    }

    #[tokio::test]
    async fn saving_a_template_that_was_never_copied_is_not_found() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        let orphan = backend
            .save_template(
                "other_copy".into(),
                edited(&copy.document, |value| value["id"] = json!("other_copy")),
                None,
            )
            .await
            .expect_err("a template that was never created cannot be edited");
        assert_eq!(orphan.code(), AppErrorCode::RecordNotFound);
    }

    #[tokio::test]
    async fn a_document_naming_another_template_is_refused() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        backend
            .create_template_copy("interview".into(), "my_interview".into(), None)
            .await
            .expect("a second copy");
        let mismatched = backend
            .save_template(
                "my_interview".into(),
                copy.document.clone(),
                Some(copy.version.clone()),
            )
            .await
            .expect_err("the document names a different template");
        assert_eq!(mismatched.code(), AppErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn an_unknown_template_or_version_is_not_found() {
        let (backend, _dir) = backend();
        a_copy(&backend).await;
        for (id, version) in [
            ("no_such_template", None),
            ("my_meeting", Some("9.9.9".to_owned())),
            ("general_meeting", Some("9.9.9".to_owned())),
            ("my_meeting", Some("../../etc/passwd".to_owned())),
        ] {
            let missing = backend
                .get_template(id.to_owned(), version.clone())
                .await
                .expect_err("an unknown template or version is refused");
            assert_eq!(
                missing.code(),
                AppErrorCode::RecordNotFound,
                "{id} {version:?}"
            );
        }
        let history = backend
            .list_template_versions("no_such_template".into())
            .await
            .expect_err("an unknown template has no history");
        assert_eq!(history.code(), AppErrorCode::RecordNotFound);
    }

    #[tokio::test]
    async fn a_built_in_has_exactly_one_version_and_no_invented_time() {
        let (backend, _dir) = backend();
        let history = backend
            .list_template_versions("general_meeting".into())
            .await
            .expect("a built-in has a history of one");
        assert_eq!(history.versions.len(), 1);
        assert!(history.versions[0].is_current);
        assert_eq!(history.versions[0].version, "1.0.0");
        assert!(
            history.versions[0].created_at.is_none(),
            "a built-in ships in the binary and has no stored time"
        );
    }

    #[tokio::test]
    async fn a_malformed_document_is_reported_as_problems_not_as_an_error() {
        let (backend, _dir) = backend();
        for document in ["{", "[]", "{\"id\": \"my_meeting\"}"] {
            let checked = backend
                .validate_template(document.to_owned())
                .await
                .expect("checking a document never fails");
            assert!(!checked.valid, "{document}");
            assert_eq!(
                checked
                    .problems
                    .first()
                    .map(|problem| problem.code.as_str()),
                Some(PROBLEM_MALFORMED),
                "{document}"
            );
        }
    }

    #[tokio::test]
    async fn a_field_missing_from_the_display_order_is_reported() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        let document = edited(&copy.document, |value| {
            let order = value["rendering"]["order"]
                .as_array()
                .expect("an order")
                .clone();
            value["rendering"]["order"] = Value::Array(order[1..].to_vec());
        });

        let checked = backend
            .validate_template(document.clone())
            .await
            .expect("checking a document never fails");
        assert!(!checked.valid);
        let codes: Vec<&str> = checked
            .problems
            .iter()
            .map(|problem| problem.code.as_str())
            .collect();
        assert_eq!(codes, vec![PROBLEM_RENDERING_ORDER], "{codes:?}");
        assert!(
            !checked.problems[0].message.contains("executive_summary"),
            "a problem never echoes the document"
        );

        let refused = backend
            .save_template("my_meeting".into(), document, None)
            .await
            .expect_err("the same document is refused by the store");
        assert_eq!(refused.code(), AppErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn each_structural_rule_has_its_own_problem_code() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        let cases: Vec<(&str, Box<dyn FnOnce(&mut Value)>)> = vec![
            (
                PROBLEM_SCHEMA_VERSION,
                Box::new(|value: &mut Value| value["template_schema_version"] = json!(99)),
            ),
            (
                PROBLEM_ID,
                Box::new(|value: &mut Value| value["id"] = json!("My Copy")),
            ),
            (
                PROBLEM_VERSION_LABEL,
                Box::new(|value: &mut Value| value["version"] = json!("1.0 0")),
            ),
            (
                PROBLEM_DISPLAY_NAME,
                Box::new(|value: &mut Value| value["display_name"] = json!("   ")),
            ),
            (
                PROBLEM_PURPOSE,
                Box::new(|value: &mut Value| value["purpose"] = json!("")),
            ),
            (
                PROBLEM_PROVENANCE_VERSION,
                Box::new(|value: &mut Value| value["prompt_version"] = json!("")),
            ),
            (
                PROBLEM_INSTRUCTIONS,
                Box::new(|value: &mut Value| value["instructions"]["synthesis"] = json!(" ")),
            ),
            (
                PROBLEM_BUILT_IN,
                Box::new(|value: &mut Value| {
                    value["origin"] = json!("built_in");
                    value
                        .as_object_mut()
                        .expect("an object")
                        .remove("base_id")
                        .expect("a base id");
                    value
                        .as_object_mut()
                        .expect("an object")
                        .remove("base_version")
                        .expect("a base version");
                }),
            ),
            (
                PROBLEM_FIELD_COUNT,
                Box::new(|value: &mut Value| {
                    value["fields"] = json!([]);
                    value["rendering"]["order"] = json!([]);
                }),
            ),
            (
                PROBLEM_FIELD_KEY,
                Box::new(|value: &mut Value| value["fields"][0]["key"] = json!("Executive")),
            ),
            (
                PROBLEM_FIELD_HEADING,
                Box::new(|value: &mut Value| value["fields"][1]["heading"] = json!("")),
            ),
            (
                PROBLEM_FIELD_LIMITS,
                Box::new(|value: &mut Value| value["fields"][2]["max_items"] = json!(0)),
            ),
            (
                PROBLEM_FIELD_ANCHORS,
                Box::new(|value: &mut Value| {
                    value["fields"][3]["anchors"] = json!({"min_per_item": 4, "max_per_item": 1});
                }),
            ),
            (
                PROBLEM_RENDERING_UNKNOWN,
                Box::new(|value: &mut Value| value["rendering"]["order"][0] = json!("nothing")),
            ),
            (
                PROBLEM_PLACEHOLDER,
                Box::new(|value: &mut Value| value["rendering"]["empty_placeholder"] = json!("")),
            ),
            (
                PROBLEM_SCHEMA_REFERENCE,
                Box::new(|value: &mut Value| {
                    value["output_schema"] = json!({"schema_ref": "something_else"});
                }),
            ),
            (
                PROBLEM_SCHEMA_INLINE,
                Box::new(|value: &mut Value| value["output_schema"] = json!({"items": []})),
            ),
        ];

        for (code, mutate) in cases {
            let checked = backend
                .validate_template(edited(&copy.document, mutate))
                .await
                .expect("checking a document never fails");
            assert!(!checked.valid, "{code}");
            let codes: Vec<&str> = checked
                .problems
                .iter()
                .map(|problem| problem.code.as_str())
                .collect();
            assert!(codes.contains(&code), "{code} not in {codes:?}");
        }
    }

    #[tokio::test]
    async fn a_duplicate_field_key_is_reported_once() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        let document = edited(&copy.document, |value| {
            let first = value["fields"][0]["key"].clone();
            value["fields"][1]["key"] = first.clone();
            value["rendering"]["order"][1] = first;
        });
        let checked = backend
            .validate_template(document)
            .await
            .expect("checking a document never fails");
        assert!(!checked.valid);
        let duplicates = checked
            .problems
            .iter()
            .filter(|problem| problem.code == PROBLEM_FIELD_DUPLICATE)
            .count();
        assert_eq!(duplicates, 1);
    }

    #[tokio::test]
    async fn a_document_the_store_would_accept_is_valid() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        let checked = backend
            .validate_template(copy.document.clone())
            .await
            .expect("checking a document never fails");
        assert!(checked.valid, "{:?}", checked.problems);
        assert!(checked.problems.is_empty());

        // A built-in document is a legitimate template but not a storable copy.
        let built_in = backend
            .get_template("interview".into(), None)
            .await
            .expect("a built-in resolves");
        let checked = backend
            .validate_template(built_in.document)
            .await
            .expect("checking a document never fails");
        assert!(!checked.valid);
        assert_eq!(
            checked
                .problems
                .iter()
                .map(|problem| problem.code.as_str())
                .collect::<Vec<_>>(),
            vec![PROBLEM_BUILT_IN]
        );
    }

    #[tokio::test]
    async fn the_editor_document_and_the_structured_fields_agree() {
        let (backend, _dir) = backend();
        let copy = a_copy(&backend).await;
        let decoded: OutputTemplate =
            serde_json::from_str(&copy.document).expect("the document decodes");
        assert_eq!(decoded.id, copy.template_id);
        assert_eq!(decoded.version, copy.version);
        assert_eq!(decoded.display_name, copy.display_name);
        assert_eq!(decoded.fields.len(), copy.fields.len());
        for (field, dto) in decoded.fields.iter().zip(&copy.fields) {
            assert_eq!(&field.key, &dto.key);
            assert_eq!(field.kind.as_str(), dto.kind);
            assert_eq!(field.anchors.min_per_item, dto.min_anchors);
            assert_eq!(field.max_text_chars, dto.max_text_chars);
        }
        assert_eq!(decoded.rendering.order, copy.rendering.order);
    }

    #[test]
    fn a_successor_never_reuses_a_stored_label() {
        let taken: BTreeSet<String> = ["1.0.0", "1.0.1", "1.0.2"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        assert_eq!(successor("1.0.0", &taken).expect("a successor"), "1.0.3");
        assert_eq!(
            successor("2", &BTreeSet::new()).expect("a successor"),
            "3".to_owned()
        );
        assert_eq!(
            successor("1.0.9", &BTreeSet::new()).expect("a successor"),
            "1.0.10"
        );
        assert_eq!(
            successor("draft", &BTreeSet::new()).expect("a successor"),
            "draft.1"
        );
        let long = "v".repeat(MAX_VERSION_CHARS);
        assert_eq!(
            successor(&long, &BTreeSet::new())
                .expect_err("no label fits")
                .code(),
            AppErrorCode::Conflict
        );
    }
}
