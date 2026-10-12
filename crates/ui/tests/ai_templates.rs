//! Interaction tests for the Templates destination — the manager, the editor
//! and the version history that replace the placeholder screen of matrix row
//! A12.
//!
//! The screen is driven without a dispatcher: `TemplatesScreen::render` paints
//! a frame and reports the commands it asked for, and `TemplatesScreen::absorb`
//! feeds a response back the way the shell would. Every assertion is on the
//! AccessKit tree or on the commands, never on pixels.

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    ListTemplateVersionsResponse, ListTemplatesResponse, SaveTemplateResponse, TemplateDocumentDto,
    TemplateFieldDto, TemplateInstructionsDto, TemplateProblemDto, TemplateRenderingDto,
    TemplateSummaryDto, TemplateVersionDto, ValidateTemplateResponse,
};
use plaudon_domain::error::{AppErrorCode, ErrorEnvelope};
use plaudon_domain::ids::DiagnosticId;
use plaudon_ui::screens::templates::TemplatesScreen;

/// A window tall enough that no control is clipped; the panel is hosted in a
/// scroll area by the shell, and a test that scrolled first would be asserting
/// on scrolling. No pixel snapshot is taken.
const TALL: egui::Vec2 = egui::vec2(1280.0, 4000.0);

/// The canonical document the fixtures hand back, short enough to assert on.
const DOCUMENT: &str = "{\"template_id\":\"meeting_mine\",\"fields\":[\"decisions\"]}";

struct App {
    screen: TemplatesScreen,
    sent: Vec<AppCommand>,
}

fn harness() -> Harness<'static, App> {
    plaudon_ui_testkit::builder()
        .with_size(TALL)
        .build_ui_state(
            |ui, app: &mut App| {
                egui::Frame::NONE.show(ui, |ui| {
                    let commands = app.screen.render(ui);
                    app.sent.extend(commands);
                });
            },
            App {
                screen: TemplatesScreen::new(),
                sent: Vec::new(),
            },
        )
}

fn announced(harness: &Harness<'_, App>) -> String {
    plaudon_ui_testkit::a11y::nodes(harness)
        .into_iter()
        .filter_map(|node| node.announced().map(str::to_owned))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Hand a response to the screen the way the shell does, dispatching the
/// follow-up reads it reports.
fn respond(
    harness: &mut Harness<'_, App>,
    command: &'static str,
    result: &Result<AppResponse, ErrorEnvelope>,
) {
    let follow_ups = harness.state_mut().screen.absorb(command, result);
    harness.state_mut().sent.extend(follow_ups);
    harness.run();
}

fn names(harness: &Harness<'_, App>) -> Vec<&'static str> {
    harness.state().sent.iter().map(AppCommand::name).collect()
}

fn clear(harness: &mut Harness<'_, App>) {
    harness.state_mut().sent.clear();
}

/* ------------------------------------------------------------- fixtures */

fn built_in_summary() -> TemplateSummaryDto {
    TemplateSummaryDto {
        template_id: "meeting".to_owned(),
        version: "3".to_owned(),
        display_name: "Meeting notes".to_owned(),
        purpose: "Decisions, actions and open questions of a meeting".to_owned(),
        built_in: true,
        base_id: None,
        base_version: None,
        field_count: 2,
        version_count: 1,
    }
}

fn copy_summary() -> TemplateSummaryDto {
    TemplateSummaryDto {
        template_id: "meeting_mine".to_owned(),
        version: "2".to_owned(),
        display_name: "My meeting notes".to_owned(),
        purpose: "The built-in, with our own risk register".to_owned(),
        built_in: false,
        base_id: Some("meeting".to_owned()),
        base_version: Some("3".to_owned()),
        field_count: 2,
        version_count: 2,
    }
}

fn catalogue() -> Result<AppResponse, ErrorEnvelope> {
    Ok(AppResponse::ListTemplates(ListTemplatesResponse {
        templates: vec![built_in_summary(), copy_summary()],
    }))
}

fn document(template_id: &str, version: &str, built_in: bool) -> TemplateDocumentDto {
    TemplateDocumentDto {
        template_id: template_id.to_owned(),
        version: version.to_owned(),
        display_name: if built_in {
            "Meeting notes".to_owned()
        } else {
            "My meeting notes".to_owned()
        },
        purpose: "Decisions, actions and open questions of a meeting".to_owned(),
        built_in,
        base_id: (!built_in).then(|| "meeting".to_owned()),
        base_version: (!built_in).then(|| "3".to_owned()),
        prompt_version: "prompt-7".to_owned(),
        output_schema_version: "schema-2".to_owned(),
        instructions: TemplateInstructionsDto {
            system: "Answer only from the transcript.".to_owned(),
            section_summary: "Summarize one section.".to_owned(),
            synthesis: "Consolidate the section summaries.".to_owned(),
            extraction: "Extract the declared fields.".to_owned(),
        },
        fields: vec![
            TemplateFieldDto {
                key: "decisions".to_owned(),
                kind: "decision".to_owned(),
                heading: "Key decisions".to_owned(),
                min_anchors: 1,
                max_anchors: 3,
                max_items: 12,
                max_text_chars: 400,
            },
            TemplateFieldDto {
                key: "questions".to_owned(),
                kind: "open_question".to_owned(),
                heading: "Open questions".to_owned(),
                min_anchors: 0,
                max_anchors: 2,
                max_items: 8,
                max_text_chars: 200,
            },
        ],
        rendering: TemplateRenderingDto {
            order: vec!["decisions".to_owned(), "questions".to_owned()],
            empty_placeholder: "Nothing was recorded for this field.".to_owned(),
            show_anchors: true,
        },
        document: DOCUMENT.to_owned(),
    }
}

/// A user copy's history: two stored versions, newest first, each with the
/// time it was written.
fn history() -> Result<AppResponse, ErrorEnvelope> {
    Ok(AppResponse::ListTemplateVersions(
        ListTemplateVersionsResponse {
            versions: vec![
                TemplateVersionDto {
                    version: "2".to_owned(),
                    created_at: Some("2026-03-04T10:00:00Z".to_owned()),
                    is_current: true,
                },
                TemplateVersionDto {
                    version: "1".to_owned(),
                    created_at: Some("2026-03-01T09:00:00Z".to_owned()),
                    is_current: false,
                },
            ],
        },
    ))
}

/// A built-in's history: one version, compiled in, with no stored time.
fn built_in_history() -> Result<AppResponse, ErrorEnvelope> {
    Ok(AppResponse::ListTemplateVersions(
        ListTemplateVersionsResponse {
            versions: vec![TemplateVersionDto {
                version: "3".to_owned(),
                created_at: None,
                is_current: true,
            }],
        },
    ))
}

fn envelope(code: AppErrorCode, message: &str) -> ErrorEnvelope {
    ErrorEnvelope {
        code,
        message: message.to_owned(),
        retryable: false,
        action: None,
        diagnostic_id: DiagnosticId::new(),
    }
}

/// Open the catalogue and select one template, leaving the dispatch log empty.
fn opened(template: &str, built_in: bool, version: &str) -> Harness<'static, App> {
    let mut harness = harness();
    respond(&mut harness, "list_templates", &catalogue());
    harness
        .get_by_label(if built_in {
            "Meeting notes"
        } else {
            "My meeting notes"
        })
        .click();
    harness.run();
    respond(
        &mut harness,
        "get_template",
        &Ok(AppResponse::GetTemplate(document(
            template, version, built_in,
        ))),
    );
    let versions = if built_in {
        built_in_history()
    } else {
        history()
    };
    respond(&mut harness, "list_template_versions", &versions);
    clear(&mut harness);
    harness
}

/* ----------------------------------------------------------- the catalogue */

#[test]
fn the_catalogue_distinguishes_built_ins_from_copies_by_label_not_colour() {
    let mut harness = harness();
    respond(&mut harness, "list_templates", &catalogue());
    let text = announced(&harness);

    assert!(text.contains("◆ Built-in"), "{text}");
    assert!(text.contains("✎ Your copy"), "{text}");
    assert!(text.contains("Meeting notes"), "{text}");
    assert!(text.contains("My meeting notes"), "{text}");
    assert!(text.contains("Version: 3"), "{text}");
    assert!(text.contains("Version: 2"), "{text}");
    assert!(text.contains("Extraction fields: 2"), "{text}");
    assert!(text.contains("Stored versions: 2"), "{text}");
    assert!(
        text.contains("Copied from: meeting at version 3"),
        "a user copy must say what it derives from: {text}"
    );
}

#[test]
fn selecting_a_template_reads_it_and_its_history_exactly_once() {
    let mut harness = harness();
    respond(&mut harness, "list_templates", &catalogue());
    clear(&mut harness);

    harness.get_by_label("My meeting notes").click();
    harness.run();
    assert_eq!(
        names(&harness),
        vec!["get_template", "list_template_versions"]
    );

    match &harness.state().sent[0] {
        AppCommand::GetTemplate(request) => {
            assert_eq!(request.template_id, "meeting_mine");
            assert_eq!(request.version, None, "a selection reads the current one");
        }
        other => panic!("{other:?} is not get_template"),
    }

    // Painting again asks for nothing: a read happens on an edge, never on a
    // frame.
    harness.run();
    harness.run();
    assert_eq!(names(&harness).len(), 2);
}

#[test]
fn the_selected_template_shows_its_versions_fields_and_rendering_order() {
    let harness = opened("meeting_mine", false, "2");
    let text = announced(&harness);

    assert!(text.contains("My meeting notes — meeting_mine"), "{text}");
    assert!(text.contains("Prompt version: prompt-7"), "{text}");
    assert!(text.contains("Output schema version: schema-2"), "{text}");
    assert!(text.contains("Answer only from the transcript."), "{text}");
    assert!(
        text.contains("Consolidate the section summaries."),
        "{text}"
    );
    assert!(text.contains("Key decisions — decisions"), "{text}");
    assert!(text.contains("Claim kind: decision"), "{text}");
    assert!(
        text.contains("Anchors: at least 1, at most 3"),
        "an anchor minimum is a constraint and is stated as one: {text}"
    );
    assert!(
        text.contains("Anchors: optional, at most 2"),
        "a minimum of zero is optional, not \"at least 0\": {text}"
    );
    assert!(
        text.contains("Ceilings: at most 12 items, 400 characters per string"),
        "{text}"
    );
    assert!(text.contains("Order: decisions → questions"), "{text}");
}

/* ------------------------------------------------------------- copying */

#[test]
fn a_built_in_offers_no_editor_and_says_why() {
    let harness = opened("meeting", true, "3");
    let text = announced(&harness);

    assert!(
        harness
            .query_all_by_label("Template document")
            .next()
            .is_none(),
        "a built-in must not offer an in-place editor: {text}"
    );
    assert!(
        harness.query_all_by_label("Save").next().is_none(),
        "{text}"
    );
    assert!(
        text.contains("never edited in place"),
        "the absence of an editor must be explained: {text}"
    );
    assert!(text.contains("Create a copy"), "{text}");
}

#[test]
fn creating_a_copy_sends_the_identifier_that_was_typed() {
    let mut harness = opened("meeting", true, "3");

    harness.get_by_label("New template id").focus();
    harness.run();
    harness
        .get_by_label("New template id")
        .type_text("meeting_mine");
    harness.run();
    harness.get_by_label("Create a copy").click();
    harness.run();

    assert_eq!(names(&harness), vec!["create_template_copy"]);
    match &harness.state().sent[0] {
        AppCommand::CreateTemplateCopy(request) => {
            assert_eq!(request.base_id, "meeting");
            assert_eq!(request.new_id, "meeting_mine");
            assert_eq!(
                request.display_name, None,
                "an empty name is absent, not an empty string"
            );
        }
        other => panic!("{other:?} is not create_template_copy"),
    }
}

#[test]
fn a_created_copy_becomes_the_open_template_and_refreshes_what_it_invalidated() {
    let mut harness = opened("meeting", true, "3");
    respond(
        &mut harness,
        "create_template_copy",
        &Ok(AppResponse::CreateTemplateCopy(document(
            "meeting_mine",
            "1",
            false,
        ))),
    );

    assert_eq!(
        names(&harness),
        vec!["list_templates", "list_template_versions"],
        "a new template changes the catalogue and has a history of its own"
    );
    assert_eq!(
        harness.state().screen.panel().selected(),
        Some("meeting_mine")
    );
    assert!(harness
        .query_all_by_label("Template document")
        .next()
        .is_some());
}

/* ------------------------------------------------------------ validation */

#[test]
fn validating_the_document_lists_every_problem_with_its_code() {
    let mut harness = opened("meeting_mine", false, "2");

    harness.get_by_label("Validate").click();
    harness.run();
    assert_eq!(names(&harness), vec!["validate_template"]);
    match &harness.state().sent[0] {
        AppCommand::ValidateTemplate(request) => assert_eq!(request.document, DOCUMENT),
        other => panic!("{other:?} is not validate_template"),
    }

    respond(
        &mut harness,
        "validate_template",
        &Ok(AppResponse::ValidateTemplate(ValidateTemplateResponse {
            valid: false,
            problems: vec![
                TemplateProblemDto {
                    code: "FIELD_KEY_DUPLICATED".to_owned(),
                    message: "Two fields share the key \"decisions\".".to_owned(),
                },
                TemplateProblemDto {
                    code: "ANCHOR_RANGE_INVALID".to_owned(),
                    message: "A field asks for more anchors than it allows.".to_owned(),
                },
            ],
        })),
    );

    let text = announced(&harness);
    assert!(text.contains("! Not valid"), "{text}");
    assert!(
        text.contains("FIELD_KEY_DUPLICATED: Two fields share the key"),
        "{text}"
    );
    assert!(
        text.contains("ANCHOR_RANGE_INVALID: A field asks for more anchors"),
        "{text}"
    );
}

#[test]
fn a_valid_document_is_reported_as_acceptable_without_saving_anything() {
    let mut harness = opened("meeting_mine", false, "2");
    harness.get_by_label("Validate").click();
    harness.run();
    clear(&mut harness);
    respond(
        &mut harness,
        "validate_template",
        &Ok(AppResponse::ValidateTemplate(ValidateTemplateResponse {
            valid: true,
            problems: Vec::new(),
        })),
    );

    assert!(announced(&harness).contains("✓ Valid"));
    assert!(names(&harness).is_empty(), "checking stores nothing");
}

/* ---------------------------------------------------------------- saving */

#[test]
fn saving_carries_the_version_the_editor_started_from_and_reports_both_versions() {
    let mut harness = opened("meeting_mine", false, "2");

    harness.get_by_label("Template document").focus();
    harness.run();
    harness
        .get_by_label("Template document")
        .type_text("and my own edit");
    harness.run();
    harness.get_by_label("Save").click();
    harness.run();

    assert_eq!(names(&harness), vec!["save_template"]);
    match &harness.state().sent[0] {
        AppCommand::SaveTemplate(request) => {
            assert_eq!(request.template_id, "meeting_mine");
            assert_eq!(
                request.expected_version.as_deref(),
                Some("2"),
                "a save carries the version the editor opened"
            );
            assert!(
                request.document.contains("and my own edit"),
                "the edited text is what is saved: {}",
                request.document
            );
        }
        other => panic!("{other:?} is not save_template"),
    }

    clear(&mut harness);
    respond(
        &mut harness,
        "save_template",
        &Ok(AppResponse::SaveTemplate(SaveTemplateResponse {
            template: document("meeting_mine", "3", false),
            previous_version: Some("2".to_owned()),
        })),
    );

    let text = announced(&harness);
    assert!(text.contains("✓ Saved"), "{text}");
    assert!(
        text.contains("Stored as version 3, superseding version 2."),
        "{text}"
    );
    assert_eq!(
        harness.state().screen.panel().expected_version(),
        Some("3"),
        "the next save starts from the version that was just stored"
    );
    assert_eq!(
        names(&harness),
        vec!["list_templates", "list_template_versions"],
        "a new version changes the catalogue row and the history"
    );
}

#[test]
fn a_conflict_is_a_refusal_the_user_can_act_on_and_keeps_their_text() {
    let mut harness = opened("meeting_mine", false, "2");

    harness.get_by_label("Template document").focus();
    harness.run();
    harness
        .get_by_label("Template document")
        .type_text("my unsaved edit");
    harness.run();
    harness.get_by_label("Save").click();
    harness.run();
    clear(&mut harness);

    respond(
        &mut harness,
        "save_template",
        &Err(envelope(
            AppErrorCode::Conflict,
            "Someone or something else changed this item first. Review the change before saving.",
        )),
    );

    let text = announced(&harness);
    assert!(text.contains("✕ Save refused"), "{text}");
    assert!(text.contains("Code: CONFLICT"), "{text}");
    assert!(
        text.contains("Someone or something else changed this item first"),
        "the envelope's own message is rendered verbatim: {text}"
    );
    assert!(
        harness
            .state()
            .screen
            .panel()
            .document_text()
            .contains("my unsaved edit"),
        "a refused save must never discard the user's text"
    );
    assert!(harness
        .query_all_by_label("Copy my changes")
        .next()
        .is_some());

    harness.get_by_label("Reload the current version").click();
    harness.run();
    assert_eq!(names(&harness), vec!["get_template"]);
}

/* -------------------------------------------------------- version history */

#[test]
fn the_history_lists_versions_newest_first_and_marks_the_current_one() {
    let harness = opened("meeting_mine", false, "2");
    let text = announced(&harness);

    let newest = text
        .find("Version 2 · stored 2026-03-04T10:00:00Z")
        .expect("the current version is listed");
    let older = text
        .find("Version 1 · stored 2026-03-01T09:00:00Z")
        .expect("the older version is listed");
    assert!(newest < older, "versions are listed newest first: {text}");
    assert!(text.contains("● Current version"), "{text}");
}

#[test]
fn a_built_in_version_shows_no_stored_time_because_it_has_none() {
    let harness = opened("meeting", true, "3");
    let text = announced(&harness);

    assert!(
        text.contains("Version 3 · ships with the application"),
        "a compiled-in version has no stored time and none is invented: {text}"
    );
    assert!(
        !text.contains("stored 20"),
        "no date may be substituted for a version that has none: {text}"
    );
    assert!(text.contains("● Current version"), "{text}");
}

#[test]
fn loading_an_older_version_reads_it_and_offers_no_editor() {
    let mut harness = opened("meeting_mine", false, "2");

    harness.get_by_label("Load version 1").click();
    harness.run();
    assert_eq!(names(&harness), vec!["get_template"]);
    match &harness.state().sent[0] {
        AppCommand::GetTemplate(request) => {
            assert_eq!(request.template_id, "meeting_mine");
            assert_eq!(request.version.as_deref(), Some("1"));
        }
        other => panic!("{other:?} is not get_template"),
    }

    clear(&mut harness);
    respond(
        &mut harness,
        "get_template",
        &Ok(AppResponse::GetTemplate(document(
            "meeting_mine",
            "1",
            false,
        ))),
    );

    let text = announced(&harness);
    assert!(text.contains("Version 1, read only"), "{text}");
    assert!(
        harness.state().screen.panel().expected_version().is_none(),
        "nothing may be saved against a version the editor is not editing"
    );
    assert!(
        harness.query_all_by_label("Save").next().is_none(),
        "{text}"
    );

    harness.get_by_label("Show the current version").click();
    harness.run();
    assert_eq!(names(&harness), vec!["get_template"]);
    match &harness.state().sent[0] {
        AppCommand::GetTemplate(request) => assert_eq!(request.version, None),
        other => panic!("{other:?} is not get_template"),
    }
}

/* ------------------------------------------------------------- keyboard */

#[test]
fn the_editor_and_the_history_are_operable_from_the_keyboard() {
    let mut harness = opened("meeting_mine", false, "2");
    let order = plaudon_ui_testkit::a11y::focus_order(&mut harness, 40);
    for expected in [
        "My meeting notes",
        "Template document",
        "Validate",
        "Save",
        "Load version 1",
    ] {
        assert!(
            order.iter().any(|label| label.contains(expected)),
            "{expected} is unreachable by Tab: {order:?}"
        );
    }
}

/* --------------------------------------------------------------- opening */

#[test]
fn the_screen_opens_by_reading_the_catalogue_and_nothing_else() {
    let names: Vec<&str> = TemplatesScreen::initial_commands()
        .iter()
        .map(AppCommand::name)
        .collect();
    assert_eq!(names, vec!["list_templates"]);
}

#[test]
fn an_unread_catalogue_says_so_rather_than_claiming_there_are_none() {
    let mut harness = harness();
    harness.run();
    assert!(announced(&harness).contains("Templates have not been read yet."));

    respond(
        &mut harness,
        "list_templates",
        &Ok(AppResponse::ListTemplates(ListTemplatesResponse {
            templates: Vec::new(),
        })),
    );
    assert!(announced(&harness).contains("No template is installed in this library."));
}
