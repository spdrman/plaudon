//! Template loading, validation and user customization (spec 9.3, task I1.2).
//!
//! Built-in templates ship as data and are compiled into the binary so a fresh
//! install has them without a download. A user copy lives in the user data
//! directory, carries its own id and records the built-in it came from; a
//! built-in is never edited in place and never shadowed by a copy.
//!
//! Loading always validates twice: against the versioned JSON Schema in
//! `schemas/meeting-template.v1.schema.json`, and against the structural
//! invariants of [`OutputTemplate`]. An invalid template is rejected with a
//! typed error. There is no "fall back to the default template" path, because
//! silently running a different template than the user chose would corrupt the
//! provenance of the output it produces.

use std::collections::BTreeMap;
use std::path::Path;

use plaudon_ai_contracts::template::{
    ExpectedSchema, OutputTemplate, TemplateError, MEETING_EXTRACTION_SCHEMA_ID,
};

/// Identifier of the section/consolidated summary schema.
pub const SECTION_SUMMARY_SCHEMA_ID: &str = "section_summary";

const TEMPLATE_META_SCHEMA: &str = include_str!("../../../schemas/meeting-template.v1.schema.json");
const MEETING_EXTRACTION_SCHEMA: &str =
    include_str!("../../../schemas/outputs/meeting-extraction.v1.schema.json");
const SECTION_SUMMARY_SCHEMA: &str =
    include_str!("../../../schemas/outputs/section-summary.v1.schema.json");

const BUILT_INS: &[(&str, &str)] = &[
    (
        "general_meeting",
        include_str!("../templates/general_meeting.json"),
    ),
    (
        "project_meeting",
        include_str!("../templates/project_meeting.json"),
    ),
    ("client_call", include_str!("../templates/client_call.json")),
    ("interview", include_str!("../templates/interview.json")),
    (
        "lecture_notes",
        include_str!("../templates/lecture_notes.json"),
    ),
];

/// Why a template could not be loaded from disk.
#[derive(Debug, thiserror::Error)]
pub enum TemplateLoadError {
    /// The template document is invalid.
    #[error(transparent)]
    Template(#[from] TemplateError),
    /// The file could not be read.
    #[error("a template file could not be read")]
    Io(#[from] std::io::Error),
    /// A user template reused the id of a built-in template.
    #[error("template id {id} is reserved by a built-in template")]
    ReservedId {
        /// Offending identifier.
        id: String,
    },
    /// Two user templates declare the same id.
    #[error("template id {id} is declared twice")]
    DuplicateId {
        /// Offending identifier.
        id: String,
    },
}

/// A short description of a template, for the Templates screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSummary {
    /// Template identifier.
    pub id: String,
    /// Template version.
    pub version: String,
    /// Display name.
    pub display_name: String,
    /// Purpose text.
    pub purpose: String,
    /// Whether the template ships with the application.
    pub built_in: bool,
}

/// Every template this installation can run.
#[derive(Debug, Clone)]
pub struct TemplateRegistry {
    built_in: BTreeMap<String, OutputTemplate>,
    user: BTreeMap<String, OutputTemplate>,
    schemas: BTreeMap<String, serde_json::Value>,
}

impl TemplateRegistry {
    /// Load the shipped templates.
    ///
    /// # Panics
    ///
    /// Never in a shipped build: the built-in documents are compiled in and
    /// are covered by a test that parses every one of them. A panic here means
    /// the binary was built from an inconsistent tree.
    #[must_use]
    pub fn with_builtins() -> Self {
        let validator = meta_validator();
        let mut built_in = BTreeMap::new();
        for (id, document) in BUILT_INS {
            let template = parse_validated(document, &validator)
                .unwrap_or_else(|error| panic!("built-in template {id} is invalid: {error}"));
            assert_eq!(&template.id, id, "built-in template id must match its file");
            assert!(
                template.origin.is_built_in(),
                "built-in template {id} must declare origin built_in"
            );
            built_in.insert(template.id.clone(), template);
        }

        let mut schemas = BTreeMap::new();
        schemas.insert(
            MEETING_EXTRACTION_SCHEMA_ID.to_owned(),
            serde_json::from_str(MEETING_EXTRACTION_SCHEMA).expect("shipped schema is valid JSON"),
        );
        schemas.insert(
            SECTION_SUMMARY_SCHEMA_ID.to_owned(),
            serde_json::from_str(SECTION_SUMMARY_SCHEMA).expect("shipped schema is valid JSON"),
        );

        Self {
            built_in,
            user: BTreeMap::new(),
            schemas,
        }
    }

    /// Load every `*.json` in `dir` as a user template.
    ///
    /// Returns the number loaded. The first invalid document fails the whole
    /// load: a half-loaded registry would make template resolution depend on
    /// directory order.
    pub fn load_user_dir(&mut self, dir: &Path) -> Result<usize, TemplateLoadError> {
        if !dir.is_dir() {
            return Ok(0);
        }
        let validator = meta_validator();
        let mut entries: Vec<_> = std::fs::read_dir(dir)?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        entries.sort();

        let mut loaded = 0;
        for path in entries {
            let document = std::fs::read_to_string(&path)?;
            let template = parse_validated(&document, &validator)?;
            self.insert_user(template)?;
            loaded += 1;
        }
        Ok(loaded)
    }

    /// Register one user template.
    pub fn insert_user(&mut self, template: OutputTemplate) -> Result<(), TemplateLoadError> {
        if self.built_in.contains_key(&template.id) {
            return Err(TemplateLoadError::ReservedId { id: template.id });
        }
        if self.user.contains_key(&template.id) {
            return Err(TemplateLoadError::DuplicateId { id: template.id });
        }
        self.user.insert(template.id.clone(), template);
        Ok(())
    }

    /// Persist a user template into `dir` and register it.
    pub fn save_user_copy(
        &mut self,
        dir: &Path,
        template: OutputTemplate,
    ) -> Result<(), TemplateLoadError> {
        template.validate()?;
        if template.origin.is_built_in() {
            return Err(TemplateLoadError::ReservedId {
                id: template.id.clone(),
            });
        }
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{}.json", template.id));
        let document =
            serde_json::to_string_pretty(&template).map_err(|error| TemplateError::Malformed {
                detail: error.classify_label(),
            })?;
        std::fs::write(path, document + "\n")?;
        self.insert_user(template)
    }

    /// Resolve a template by id, preferring built-ins.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&OutputTemplate> {
        self.built_in.get(id).or_else(|| self.user.get(id))
    }

    /// Every template, built-ins first, each sorted by id.
    #[must_use]
    pub fn list(&self) -> Vec<TemplateSummary> {
        self.built_in
            .values()
            .map(|template| summary(template, true))
            .chain(self.user.values().map(|template| summary(template, false)))
            .collect()
    }

    /// The JSON Schema a template's extraction stage must satisfy.
    pub fn extraction_schema(
        &self,
        template: &OutputTemplate,
    ) -> Result<serde_json::Value, TemplateError> {
        match &template.output_schema {
            ExpectedSchema::Inline(schema) => Ok(schema.clone()),
            ExpectedSchema::Reference { schema_ref } => {
                self.schemas.get(schema_ref).cloned().ok_or_else(|| {
                    TemplateError::UnknownSchemaReference {
                        id: template.id.clone(),
                        schema_ref: schema_ref.clone(),
                    }
                })
            }
        }
    }

    /// The JSON Schema section and consolidated summaries must satisfy.
    #[must_use]
    pub fn summary_schema(&self) -> serde_json::Value {
        self.schemas
            .get(SECTION_SUMMARY_SCHEMA_ID)
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    }

    /// Produce and register an editable copy of a built-in template.
    pub fn copy_builtin(
        &mut self,
        base_id: &str,
        new_id: impl Into<String>,
    ) -> Result<OutputTemplate, TemplateLoadError> {
        let new_id = new_id.into();
        let base =
            self.built_in
                .get(base_id)
                .ok_or_else(|| TemplateError::UnknownSchemaReference {
                    id: base_id.to_owned(),
                    schema_ref: "template".to_owned(),
                })?;
        let copy = base.to_user_copy(new_id);
        copy.validate()?;
        self.insert_user(copy.clone())?;
        Ok(copy)
    }
}

impl Default for TemplateRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

fn summary(template: &OutputTemplate, built_in: bool) -> TemplateSummary {
    TemplateSummary {
        id: template.id.clone(),
        version: template.version.clone(),
        display_name: template.display_name.clone(),
        purpose: template.purpose.clone(),
        built_in,
    }
}

fn meta_validator() -> jsonschema::Validator {
    let schema: serde_json::Value =
        serde_json::from_str(TEMPLATE_META_SCHEMA).expect("shipped meta-schema is valid JSON");
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("shipped meta-schema compiles")
}

fn parse_validated(
    document: &str,
    validator: &jsonschema::Validator,
) -> Result<OutputTemplate, TemplateError> {
    let value: serde_json::Value =
        serde_json::from_str(document).map_err(|error| TemplateError::Malformed {
            detail: error.classify_label(),
        })?;
    let errors: Vec<String> = validator
        .iter_errors(&value)
        .map(|error| format!("{} {}", error.instance_path, error.schema_path))
        .collect();
    if !errors.is_empty() {
        let id = value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("<unknown>")
            .to_owned();
        return Err(TemplateError::Invalid {
            id,
            detail: errors.join("; "),
        });
    }
    let template: OutputTemplate =
        serde_json::from_value(value).map_err(|error| TemplateError::Malformed {
            detail: error.classify_label(),
        })?;
    template.validate()?;
    Ok(template)
}

trait ClassifyLabel {
    fn classify_label(&self) -> String;
}

impl ClassifyLabel for serde_json::Error {
    fn classify_label(&self) -> String {
        match self.classify() {
            serde_json::error::Category::Data => "a field has the wrong type or is missing".into(),
            serde_json::error::Category::Syntax => "the document is not valid JSON".into(),
            serde_json::error::Category::Eof => "the document ended unexpectedly".into(),
            serde_json::error::Category::Io => "the document could not be read".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use plaudon_domain::claim::ClaimKind;

    #[test]
    fn every_shipped_template_loads_and_validates() {
        let registry = TemplateRegistry::with_builtins();
        let ids: Vec<String> = registry.list().into_iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            vec![
                "client_call",
                "general_meeting",
                "interview",
                "lecture_notes",
                "project_meeting"
            ],
            "spec 9.3 names exactly these five built-in templates"
        );
        assert!(registry.list().iter().all(|t| t.built_in));
    }

    #[test]
    fn every_shipped_template_declares_the_nine_two_output_types() {
        let registry = TemplateRegistry::with_builtins();
        for summary in registry.list() {
            let template = registry.get(&summary.id).expect("listed template resolves");
            let kinds: Vec<ClaimKind> = template.fields.iter().map(|f| f.kind).collect();
            for required in [
                ClaimKind::SummaryStatement,
                ClaimKind::Decision,
                ClaimKind::ActionItem,
                ClaimKind::Risk,
                ClaimKind::Question,
                ClaimKind::Topic,
            ] {
                assert!(
                    kinds.contains(&required),
                    "{} is missing the {required:?} output type",
                    summary.id
                );
            }
            assert!(!template.purpose.trim().is_empty());
            assert!(!template.instructions.extraction.trim().is_empty());
            assert!(template.rendering.show_anchors);
        }
    }

    #[test]
    fn every_shipped_template_requires_an_anchor_on_every_field() {
        let registry = TemplateRegistry::with_builtins();
        for summary in registry.list() {
            let template = registry.get(&summary.id).unwrap();
            for field in &template.fields {
                assert!(
                    field.anchors.min_per_item >= 1,
                    "{}/{} must require a source anchor",
                    summary.id,
                    field.key
                );
            }
        }
    }

    #[test]
    fn every_shipped_template_resolves_its_output_schema() {
        let registry = TemplateRegistry::with_builtins();
        for summary in registry.list() {
            let template = registry.get(&summary.id).unwrap();
            let schema = registry
                .extraction_schema(template)
                .expect("schema resolves");
            assert!(schema.get("$defs").is_some());
        }
        assert!(registry.summary_schema().get("properties").is_some());
    }

    #[test]
    fn every_shipped_system_instruction_refuses_embedded_directives() {
        let registry = TemplateRegistry::with_builtins();
        for summary in registry.list() {
            let template = registry.get(&summary.id).unwrap();
            let system = template.instructions.system.to_lowercase();
            for required in [
                "untrusted data",
                "never a set of instructions",
                "no tools",
                "anchor",
                "unassigned",
                "not_specified",
            ] {
                assert!(
                    system.contains(required),
                    "{}'s system prompt is missing the {required:?} guard",
                    summary.id
                );
            }
        }
    }

    #[test]
    fn an_unknown_schema_reference_is_rejected_rather_than_defaulted() {
        let registry = TemplateRegistry::with_builtins();
        let mut template = registry.get("general_meeting").unwrap().clone();
        template.output_schema = ExpectedSchema::Reference {
            schema_ref: "nope".into(),
        };
        assert!(matches!(
            registry.extraction_schema(&template),
            Err(TemplateError::UnknownSchemaReference { .. })
        ));
    }

    #[test]
    fn a_document_failing_the_meta_schema_is_rejected() {
        let validator = meta_validator();
        let mut document: serde_json::Value =
            serde_json::from_str(BUILT_INS[0].1).expect("built-in parses");
        document["fields"][0]["anchors"]["max_per_item"] = serde_json::json!(99);
        let error = parse_validated(&document.to_string(), &validator).unwrap_err();
        assert!(matches!(error, TemplateError::Invalid { .. }));
    }

    #[test]
    fn a_user_copy_cannot_take_a_builtin_id() {
        let mut registry = TemplateRegistry::with_builtins();
        let copy = registry
            .get("general_meeting")
            .unwrap()
            .to_user_copy("general_meeting");
        assert!(matches!(
            registry.insert_user(copy),
            Err(TemplateLoadError::ReservedId { .. })
        ));
    }

    #[test]
    fn a_user_copy_is_editable_and_leaves_the_builtin_untouched() {
        let mut registry = TemplateRegistry::with_builtins();
        let mut copy = registry
            .copy_builtin("general_meeting", "my_meeting")
            .unwrap();
        copy.instructions.extraction = "only extract decisions".into();
        copy.fields.truncate(1);
        copy.rendering.order = vec![copy.fields[0].key.clone()];
        copy.validate().expect("the edited copy is still valid");

        let built_in = registry.get("general_meeting").unwrap();
        assert_eq!(built_in.fields.len(), 6);
        assert_ne!(
            built_in.instructions.extraction,
            copy.instructions.extraction
        );
        assert_eq!(
            registry.get("my_meeting").map(|t| t.fields.len()),
            Some(6),
            "the registry holds the copy as registered, not a later local edit"
        );
    }

    #[test]
    fn user_templates_round_trip_through_a_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut registry = TemplateRegistry::with_builtins();
        let copy = registry
            .get("client_call")
            .unwrap()
            .to_user_copy("my_client_call");
        registry
            .save_user_copy(dir.path(), copy.clone())
            .expect("saved");

        let mut reloaded = TemplateRegistry::with_builtins();
        assert_eq!(reloaded.load_user_dir(dir.path()).unwrap(), 1);
        assert_eq!(reloaded.get("my_client_call"), Some(&copy));
        assert_eq!(reloaded.list().len(), 6);
    }

    #[test]
    fn loading_an_absent_user_directory_is_not_an_error() {
        let mut registry = TemplateRegistry::with_builtins();
        assert_eq!(
            registry
                .load_user_dir(Path::new("/nonexistent/plaudon"))
                .unwrap(),
            0
        );
    }

    #[test]
    fn an_invalid_user_template_fails_the_whole_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("broken.json"), "{ \"id\": ").unwrap();
        let mut registry = TemplateRegistry::with_builtins();
        assert!(registry.load_user_dir(dir.path()).is_err());
        assert_eq!(registry.list().len(), 5, "nothing partial was registered");
    }

    #[test]
    fn upgrading_a_template_does_not_touch_a_version_recorded_elsewhere() {
        // A stored output keeps the version string it ran with; the registry
        // moving on is invisible to it.
        let registry = TemplateRegistry::with_builtins();
        let recorded = registry.get("general_meeting").unwrap().qualified_version();
        let mut upgraded = registry.get("general_meeting").unwrap().clone();
        upgraded.version = "2.0.0".into();
        assert_eq!(recorded, "general_meeting@1.0.0");
        assert_eq!(upgraded.qualified_version(), "general_meeting@2.0.0");
        assert_ne!(recorded, upgraded.qualified_version());
    }
}
