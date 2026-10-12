//! Validated logic behind the six template commands (spec §9.3, matrix row
//! A12).
//!
//! The template store is a directory of user-authored documents, so the
//! command boundary owes it three checks before anything is read or written:
//!
//! * `template_id` is a lowercase identifier, so it cannot name a path
//!   component outside the store;
//! * a document is non-empty and bounded, so an accidental paste of something
//!   enormous is refused before it is parsed; and
//! * a version label is a bounded, printable token, so a stored version can
//!   always be named back.
//!
//! Structural validation of the document itself belongs to
//! [`plaudon_ai_contracts::template::OutputTemplate::validate`], and its
//! findings are returned as data by `validate_template` rather than as an
//! error envelope: an editor needs the list of problems, not one message.

use plaudon_domain::error::AppError;

use crate::dto::{
    CreateTemplateCopyRequest, GetTemplateRequest, ListTemplateVersionsRequest,
    ListTemplateVersionsResponse, ListTemplatesResponse, SaveTemplateRequest, SaveTemplateResponse,
    TemplateDocumentDto, ValidateTemplateRequest, ValidateTemplateResponse,
};
use crate::validate;
use crate::AppPorts;

/// Every template this installation can run.
pub async fn list_templates(ports: &AppPorts) -> Result<ListTemplatesResponse, AppError> {
    ports.templates.list_templates().await
}

/// One template document, at the current version or at a stored one.
pub async fn get_template(
    ports: &AppPorts,
    request: GetTemplateRequest,
) -> Result<TemplateDocumentDto, AppError> {
    let template_id = validate::template_id(&request.template_id)?;
    let version = request
        .version
        .as_deref()
        .map(validate::template_version)
        .transpose()?;
    ports.templates.get_template(template_id, version).await
}

/// Create an editable copy of a built-in template.
pub async fn create_template_copy(
    ports: &AppPorts,
    request: CreateTemplateCopyRequest,
) -> Result<TemplateDocumentDto, AppError> {
    let base_id = validate::template_id(&request.base_id)?;
    let new_id = validate::template_id(&request.new_id)?;
    let display_name = request
        .display_name
        .as_deref()
        .map(|value| validate::label("display_name", value))
        .transpose()?;
    ports
        .templates
        .create_template_copy(base_id, new_id, display_name)
        .await
}

/// Store an edited user copy as a new version.
pub async fn save_template(
    ports: &AppPorts,
    request: SaveTemplateRequest,
) -> Result<SaveTemplateResponse, AppError> {
    let template_id = validate::template_id(&request.template_id)?;
    let document = validate::template_document(&request.document)?;
    let expected_version = request
        .expected_version
        .as_deref()
        .map(validate::template_version)
        .transpose()?;
    ports
        .templates
        .save_template(template_id, document, expected_version)
        .await
}

/// Check a document without storing anything.
pub async fn validate_template(
    ports: &AppPorts,
    request: ValidateTemplateRequest,
) -> Result<ValidateTemplateResponse, AppError> {
    let document = validate::template_document(&request.document)?;
    ports.templates.validate_template(document).await
}

/// Stored versions of one template, newest first.
pub async fn list_template_versions(
    ports: &AppPorts,
    request: ListTemplateVersionsRequest,
) -> Result<ListTemplateVersionsResponse, AppError> {
    let template_id = validate::template_id(&request.template_id)?;
    ports.templates.list_template_versions(template_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use plaudon_domain::error::AppErrorCode;
    use plaudon_notifications::testing::RecordingNotifier;
    use plaudon_notifications::{Dispatcher, NotificationSettings as CrateSettings, Notifier};
    use serde::Serialize;
    use serde_json::Value;

    use crate::dto::{
        TemplateFieldDto, TemplateInstructionsDto, TemplateProblemDto, TemplateRenderingDto,
        TemplateSummaryDto, TemplateVersionDto,
    };
    use crate::events::{EventSink, NullEventSink};
    use crate::ports::TemplatesPort;
    use crate::secrets::{InMemorySecretStore, SecretStore};
    use crate::validate::MAX_TEMPLATE_DOCUMENT_BYTES;

    /// A port that records what reached it and invents nothing.
    ///
    /// Every method returns the same canned document, so a test that asserts
    /// on the arguments cannot accidentally be asserting on store behaviour.
    #[derive(Debug, Default)]
    struct FakeTemplates {
        calls: Mutex<Vec<String>>,
    }

    impl FakeTemplates {
        fn record(&self, call: String) {
            self.calls
                .lock()
                .expect("the recorder is not poisoned")
                .push(call);
        }

        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .expect("the recorder is not poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl TemplatesPort for FakeTemplates {
        async fn list_templates(&self) -> Result<ListTemplatesResponse, AppError> {
            self.record("list_templates".to_owned());
            Ok(ListTemplatesResponse {
                templates: vec![summary()],
            })
        }

        async fn get_template(
            &self,
            template_id: String,
            version: Option<String>,
        ) -> Result<TemplateDocumentDto, AppError> {
            self.record(format!("get_template {template_id} {version:?}"));
            Ok(document())
        }

        async fn create_template_copy(
            &self,
            base_id: String,
            new_id: String,
            display_name: Option<String>,
        ) -> Result<TemplateDocumentDto, AppError> {
            self.record(format!(
                "create_template_copy {base_id} {new_id} {display_name:?}"
            ));
            Ok(document())
        }

        async fn save_template(
            &self,
            template_id: String,
            document: String,
            expected_version: Option<String>,
        ) -> Result<SaveTemplateResponse, AppError> {
            self.record(format!(
                "save_template {template_id} {} {expected_version:?}",
                document.len()
            ));
            Ok(SaveTemplateResponse {
                template: self::document(),
                previous_version: Some("1.0.0".to_owned()),
            })
        }

        async fn validate_template(
            &self,
            document: String,
        ) -> Result<ValidateTemplateResponse, AppError> {
            self.record(format!("validate_template {}", document.len()));
            Ok(ValidateTemplateResponse {
                valid: false,
                problems: vec![TemplateProblemDto {
                    code: "document_malformed".to_owned(),
                    message: "This document could not be read as a template.".to_owned(),
                }],
            })
        }

        async fn list_template_versions(
            &self,
            template_id: String,
        ) -> Result<ListTemplateVersionsResponse, AppError> {
            self.record(format!("list_template_versions {template_id}"));
            Ok(ListTemplateVersionsResponse {
                versions: vec![TemplateVersionDto {
                    version: "1.0.1".to_owned(),
                    created_at: Some("2026-01-02T03:04:05Z".to_owned()),
                    is_current: true,
                }],
            })
        }
    }

    fn summary() -> TemplateSummaryDto {
        TemplateSummaryDto {
            template_id: "my_meeting".to_owned(),
            version: "1.0.1".to_owned(),
            display_name: "General Meeting (copy)".to_owned(),
            purpose: "Summarize an ordinary working meeting.".to_owned(),
            built_in: false,
            base_id: Some("general_meeting".to_owned()),
            base_version: Some("1.0.0".to_owned()),
            field_count: 6,
            version_count: 2,
        }
    }

    fn document() -> TemplateDocumentDto {
        TemplateDocumentDto {
            template_id: "my_meeting".to_owned(),
            version: "1.0.1".to_owned(),
            display_name: "General Meeting (copy)".to_owned(),
            purpose: "Summarize an ordinary working meeting.".to_owned(),
            built_in: false,
            base_id: Some("general_meeting".to_owned()),
            base_version: Some("1.0.0".to_owned()),
            prompt_version: "1.0.0".to_owned(),
            output_schema_version: "1.0.0".to_owned(),
            instructions: TemplateInstructionsDto {
                system: "You are a meeting-notes assistant.".to_owned(),
                section_summary: "Summarize one section.".to_owned(),
                synthesis: "Consolidate the section summaries.".to_owned(),
                extraction: "Produce the structured record.".to_owned(),
            },
            fields: vec![TemplateFieldDto {
                key: "decisions".to_owned(),
                kind: "decision".to_owned(),
                heading: "Decisions".to_owned(),
                min_anchors: 1,
                max_anchors: 4,
                max_items: 60,
                max_text_chars: 1_200,
            }],
            rendering: TemplateRenderingDto {
                order: vec!["decisions".to_owned()],
                empty_placeholder: "None recorded in this recording".to_owned(),
                show_anchors: true,
            },
            document: "{\"id\":\"my_meeting\"}".to_owned(),
        }
    }

    /// Real ports over a temporary library, with the template port faked.
    async fn ports() -> (AppPorts, Arc<FakeTemplates>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let notifier = RecordingNotifier::granted();
        let dispatcher = Arc::new(Dispatcher::new(
            Arc::clone(&notifier) as Arc<dyn Notifier>,
            CrateSettings::uniform(false),
        ));
        let ports = crate::build_ports(
            dir.path(),
            Arc::new(NullEventSink) as Arc<dyn EventSink>,
            dispatcher,
            Arc::new(InMemorySecretStore::new()) as Arc<dyn SecretStore>,
            Vec::<PathBuf>::new(),
        )
        .await
        .expect("the library opens");
        let fake = Arc::new(FakeTemplates::default());
        let ports = AppPorts {
            templates: Arc::clone(&fake) as Arc<dyn TemplatesPort>,
            ..ports
        };
        (ports, fake, dir)
    }

    /// A document long enough to be refused before it is ever parsed.
    fn oversized_document() -> String {
        "x".repeat(MAX_TEMPLATE_DOCUMENT_BYTES + 1)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hostile_template_identifier_never_reaches_the_port() {
        let (ports, fake, _dir) = ports().await;
        for hostile in [
            "../etc/passwd",
            "../../templates/general_meeting",
            "General_Meeting",
            "",
            "   ",
            "my copy",
            "my/copy",
            "my.copy",
            &"a".repeat(65),
        ] {
            let read = get_template(
                &ports,
                GetTemplateRequest {
                    template_id: hostile.to_owned(),
                    version: None,
                },
            )
            .await
            .expect_err("a hostile identifier is refused");
            assert_eq!(read.code(), AppErrorCode::InvalidArgument, "{hostile:?}");

            let history = list_template_versions(
                &ports,
                ListTemplateVersionsRequest {
                    template_id: hostile.to_owned(),
                },
            )
            .await
            .expect_err("a hostile identifier is refused");
            assert_eq!(history.code(), AppErrorCode::InvalidArgument, "{hostile:?}");

            let saved = save_template(
                &ports,
                SaveTemplateRequest {
                    template_id: hostile.to_owned(),
                    document: "{}".to_owned(),
                    expected_version: None,
                },
            )
            .await
            .expect_err("a hostile identifier is refused");
            assert_eq!(saved.code(), AppErrorCode::InvalidArgument, "{hostile:?}");

            let copied = create_template_copy(
                &ports,
                CreateTemplateCopyRequest {
                    base_id: "general_meeting".to_owned(),
                    new_id: hostile.to_owned(),
                    display_name: None,
                },
            )
            .await
            .expect_err("a hostile identifier is refused");
            assert_eq!(copied.code(), AppErrorCode::InvalidArgument, "{hostile:?}");
        }
        assert!(
            fake.calls().is_empty(),
            "the store was reached: {:?}",
            fake.calls()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_empty_or_oversized_document_is_refused_before_it_is_parsed() {
        let (ports, fake, _dir) = ports().await;
        for document in ["", "   \n\t ", &oversized_document()] {
            let saved = save_template(
                &ports,
                SaveTemplateRequest {
                    template_id: "my_meeting".to_owned(),
                    document: document.to_owned(),
                    expected_version: None,
                },
            )
            .await
            .expect_err("an empty or oversized document is refused");
            assert_eq!(saved.code(), AppErrorCode::InvalidArgument);

            let checked = validate_template(
                &ports,
                ValidateTemplateRequest {
                    document: document.to_owned(),
                },
            )
            .await
            .expect_err("an empty or oversized document is refused");
            assert_eq!(checked.code(), AppErrorCode::InvalidArgument);
        }
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_version_label_that_cannot_name_a_stored_file_is_refused() {
        let (ports, fake, _dir) = ports().await;
        for label in ["", " ", "../1.0.0", "1.0.0/../..", "1.0 0", "1.0.0\n"] {
            let read = get_template(
                &ports,
                GetTemplateRequest {
                    template_id: "my_meeting".to_owned(),
                    version: Some(label.to_owned()),
                },
            )
            .await
            .expect_err("an unusable version label is refused");
            assert_eq!(read.code(), AppErrorCode::InvalidArgument, "{label:?}");

            let saved = save_template(
                &ports,
                SaveTemplateRequest {
                    template_id: "my_meeting".to_owned(),
                    document: "{\"id\":\"my_meeting\"}".to_owned(),
                    expected_version: Some(label.to_owned()),
                },
            )
            .await
            .expect_err("an unusable version label is refused");
            assert_eq!(saved.code(), AppErrorCode::InvalidArgument, "{label:?}");
        }
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_valid_request_reaches_the_port_unchanged() {
        let (ports, fake, _dir) = ports().await;

        list_templates(&ports).await.expect("listing is valid");
        get_template(
            &ports,
            GetTemplateRequest {
                template_id: "my_meeting".to_owned(),
                version: Some("1.0.0".to_owned()),
            },
        )
        .await
        .expect("reading one version is valid");
        create_template_copy(
            &ports,
            CreateTemplateCopyRequest {
                base_id: "general_meeting".to_owned(),
                new_id: "my_meeting".to_owned(),
                display_name: Some("My meeting".to_owned()),
            },
        )
        .await
        .expect("copying a built-in is valid");
        save_template(
            &ports,
            SaveTemplateRequest {
                template_id: "my_meeting".to_owned(),
                document: "{\"id\":\"my_meeting\"}".to_owned(),
                expected_version: Some("1.0.0".to_owned()),
            },
        )
        .await
        .expect("saving an edit is valid");
        let checked = validate_template(
            &ports,
            ValidateTemplateRequest {
                document: "{ not a template }".to_owned(),
            },
        )
        .await
        .expect("checking a document is valid");
        assert!(!checked.valid, "a refusal is data, not an error");
        list_template_versions(
            &ports,
            ListTemplateVersionsRequest {
                template_id: "my_meeting".to_owned(),
            },
        )
        .await
        .expect("listing the history is valid");

        assert_eq!(
            fake.calls(),
            vec![
                "list_templates".to_owned(),
                "get_template my_meeting Some(\"1.0.0\")".to_owned(),
                "create_template_copy general_meeting my_meeting Some(\"My meeting\")".to_owned(),
                "save_template my_meeting 19 Some(\"1.0.0\")".to_owned(),
                "validate_template 18".to_owned(),
                "list_template_versions my_meeting".to_owned(),
            ]
        );
    }

    /* ------------------------------------------------------ schema conformance */

    fn validate_against(name: &str, instance: &impl Serialize) {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas/commands/v1")
            .join(format!("{name}.schema.json"));
        let raw = std::fs::read_to_string(&path).expect("the schema exists");
        let schema: Value = serde_json::from_str(&raw).expect("valid json");
        let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
        let value = serde_json::to_value(instance).expect("serializes");
        let errors: Vec<String> = validator
            .iter_errors(&value)
            .map(|error| format!("{error} at {}", error.instance_path))
            .collect();
        assert!(errors.is_empty(), "{name}: {}\n{value}", errors.join("\n"));
    }

    #[test]
    fn every_template_request_and_response_matches_its_schema() {
        validate_against("list_templates.request", &serde_json::json!({}));
        validate_against(
            "list_templates.response",
            &ListTemplatesResponse {
                templates: vec![summary()],
            },
        );
        validate_against(
            "get_template.request",
            &GetTemplateRequest {
                template_id: "my_meeting".to_owned(),
                version: Some("1.0.0".to_owned()),
            },
        );
        validate_against("get_template.response", &document());
        validate_against(
            "create_template_copy.request",
            &CreateTemplateCopyRequest {
                base_id: "general_meeting".to_owned(),
                new_id: "my_meeting".to_owned(),
                display_name: None,
            },
        );
        validate_against("create_template_copy.response", &document());
        validate_against(
            "save_template.request",
            &SaveTemplateRequest {
                template_id: "my_meeting".to_owned(),
                document: "{}".to_owned(),
                expected_version: None,
            },
        );
        validate_against(
            "save_template.response",
            &SaveTemplateResponse {
                template: document(),
                previous_version: Some("1.0.0".to_owned()),
            },
        );
        validate_against(
            "validate_template.request",
            &ValidateTemplateRequest {
                document: "{}".to_owned(),
            },
        );
        validate_against(
            "validate_template.response",
            &ValidateTemplateResponse {
                valid: false,
                problems: vec![TemplateProblemDto {
                    code: "document_malformed".to_owned(),
                    message: "This document could not be read as a template.".to_owned(),
                }],
            },
        );
        validate_against(
            "list_template_versions.request",
            &ListTemplateVersionsRequest {
                template_id: "my_meeting".to_owned(),
            },
        );
        validate_against(
            "list_template_versions.response",
            &ListTemplateVersionsResponse {
                versions: vec![TemplateVersionDto {
                    version: "1.0.1".to_owned(),
                    created_at: Some("2026-01-02T03:04:05Z".to_owned()),
                    is_current: true,
                }],
            },
        );
    }
}
