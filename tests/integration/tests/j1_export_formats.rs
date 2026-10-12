//! J1.1-J1.3, J1.5: TXT, Markdown, JSON, SRT/VTT and audio export.
//!
//! Hermetic: a temporary library on disk, no network, no provider, no device.

use std::path::{Path, PathBuf};

use plaudon_domain::time::Timestamp;
use plaudon_export::bundle::BundleLoader;
use plaudon_export::service::{
    ExportFormat, ExportMediaRequest, ExportRecordingRequest, ExportService,
};
use plaudon_export::{captions, render, ExportError};
use plaudon_integration_tests::fakes::library::{self, SeededLibrary};

const NOW: Timestamp = Timestamp::from_millis(1_700_000_100_000);

fn schemas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("root")
        .join("schemas")
}

fn validator(name: &str) -> jsonschema::Validator {
    let text = std::fs::read_to_string(schemas_dir().join(name)).expect("read schema");
    let schema: serde_json::Value = serde_json::from_str(&text).expect("parse schema");
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .expect("compile schema")
}

fn assert_valid(validator: &jsonschema::Validator, instance: &serde_json::Value) {
    let errors: Vec<String> = validator
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "instance failed schema: {errors:?}");
}

async fn seeded(dir: &Path) -> (SeededLibrary, ExportService) {
    let library = library::seed(&dir.join("library"), false).await;
    let service = ExportService::new(
        library.db.clone(),
        library.archive.clone(),
        library.root.clone(),
    );
    (library, service)
}

#[tokio::test]
async fn text_markdown_and_json_carry_the_revision_text_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    let out = dir.path().join("exports");
    std::fs::create_dir_all(&out).unwrap();

    let response = service
        .export_recording(
            &ExportRecordingRequest {
                recording_id: library.recording_id,
                transcript_revision_id: Some(library.native_revision),
                formats: vec![ExportFormat::Txt, ExportFormat::Markdown, ExportFormat::Json],
                destination: out.clone(),
            },
            NOW,
        )
        .await
        .expect("export");
    assert_eq!(response.files.len(), 3);
    assert_eq!(response.refusals, Vec::new());

    let loader = BundleLoader::new(library.db.clone());
    let export = loader
        .load(library.recording_id, Some(library.native_revision), NOW)
        .await
        .unwrap();
    let transcript = export.selected_transcript().unwrap();

    for file in &response.files {
        let bytes = std::fs::read(&file.path).unwrap();
        assert_eq!(bytes.len() as u64, file.byte_length);
        let text = String::from_utf8(bytes).unwrap();
        if file.format == ExportFormat::Json {
                let value: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_valid(&validator("recording-export.v1.schema.json"), &value);
                let rendered: Vec<&str> = value["transcripts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|t| t["id"] == serde_json::json!(library.native_revision.to_string()))
                    .unwrap()["segments"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|segment| segment["text"].as_str().unwrap())
                    .collect();
                let expected: Vec<&str> = transcript
                    .segments
                    .iter()
                    .map(|segment| segment.text.as_str())
                    .collect();
            assert_eq!(rendered, expected);
        } else {
            // Every segment's text appears verbatim, in order.
            let mut cursor = 0usize;
            for segment in &transcript.segments {
                let found = text[cursor..]
                    .find(&segment.text)
                    .unwrap_or_else(|| panic!("missing segment text in {:?}", file.format));
                cursor += found + segment.text.len();
            }
        }
    }
}

#[tokio::test]
async fn a_markdown_export_preserves_anchors_and_never_fills_in_a_missing_value() {
    let dir = tempfile::tempdir().unwrap();
    let (library, _) = seeded(dir.path()).await;
    let loader = BundleLoader::new(library.db.clone());
    let export = loader
        .load(library.recording_id, Some(library.native_revision), NOW)
        .await
        .unwrap();
    let markdown = render::to_markdown(&export);

    assert!(markdown.contains("not_specified"), "a missing due date must stay missing");
    assert!(markdown.contains("unknown"));
    assert!(markdown.contains("/action_items/0/verb"));
    assert!(markdown.contains("source_linked"));
    assert!(markdown.contains("human_confirmed"));
    assert!(markdown.contains("unverified"));
    assert!(markdown.contains("Speaker 1 (inferred cluster)"));
    assert!(markdown.contains("not a backup"));
    // An honest fidelity label, never "original" for a compressed transfer.
    assert!(markdown.contains("not the original device WAV"));
}

#[tokio::test]
async fn captions_are_refused_for_a_text_only_revision_and_produced_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    let out = dir.path().join("exports");
    std::fs::create_dir_all(&out).unwrap();

    let refused = service
        .export_recording(
            &ExportRecordingRequest {
                recording_id: library.recording_id,
                transcript_revision_id: Some(library.text_only_revision),
                formats: vec![ExportFormat::Srt, ExportFormat::Vtt, ExportFormat::Txt],
                destination: out.clone(),
            },
            NOW,
        )
        .await
        .expect("export");
    assert_eq!(refused.files.len(), 1, "only the text export may land");
    assert_eq!(refused.refusals.len(), 2);
    for refusal in &refused.refusals {
        assert_eq!(refusal.code, "CAPTIONS_UNAVAILABLE");
        assert!(refusal.explanation.contains("no real timestamps"));
    }
    let names: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().all(|name| {
            !std::path::Path::new(name)
                .extension()
                .is_some_and(|ext| ext == "srt" || ext == "vtt")
        }),
        "{names:?}"
    );

    let accepted = service
        .export_recording(
            &ExportRecordingRequest {
                recording_id: library.recording_id,
                transcript_revision_id: Some(library.native_revision),
                formats: vec![ExportFormat::Srt, ExportFormat::Vtt],
                destination: out.clone(),
            },
            NOW,
        )
        .await
        .expect("export");
    assert_eq!(accepted.files.len(), 2);
    assert_eq!(accepted.refusals, Vec::new());

    let srt = std::fs::read_to_string(&accepted.files[0].path).unwrap();
    assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:02,500\n"));
    let vtt = std::fs::read_to_string(&accepted.files[1].path).unwrap();
    assert!(vtt.starts_with("WEBVTT\n"));

    // Cue times round-trip to the stored segment offsets exactly.
    let loader = BundleLoader::new(library.db.clone());
    let export = loader
        .load(library.recording_id, Some(library.native_revision), NOW)
        .await
        .unwrap();
    let cues = captions::build_cues(&export, export.selected_transcript().unwrap()).unwrap();
    let expected: Vec<u64> = export
        .selected_transcript()
        .unwrap()
        .segments
        .iter()
        .filter_map(|segment| segment.start_ms)
        .collect();
    assert_eq!(
        cues.iter().map(|cue| cue.start_ms).collect::<Vec<_>>(),
        expected
    );
}

#[tokio::test]
async fn a_derived_alignment_is_labelled_as_derived_in_captions() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    let out = dir.path().join("exports");
    std::fs::create_dir_all(&out).unwrap();

    let response = service
        .export_recording(
            &ExportRecordingRequest {
                recording_id: library.recording_id,
                transcript_revision_id: Some(library.derived_revision),
                formats: vec![ExportFormat::Vtt, ExportFormat::Markdown],
                destination: out,
            },
            NOW,
        )
        .await
        .expect("export");
    let vtt = std::fs::read_to_string(&response.files[0].path).unwrap();
    assert!(vtt.contains("derived (approximate)"));
    let markdown = std::fs::read_to_string(&response.files[1].path).unwrap();
    assert!(markdown.contains("derived (approximate)"));
}

#[tokio::test]
async fn repeating_an_export_produces_identical_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    let out = dir.path().join("exports");
    std::fs::create_dir_all(&out).unwrap();

    let request = ExportRecordingRequest {
        recording_id: library.recording_id,
        transcript_revision_id: Some(library.native_revision),
        formats: ExportFormat::ALL.to_vec(),
        destination: out,
    };
    let first = service.export_recording(&request, NOW).await.unwrap();
    let before: Vec<Vec<u8>> = first
        .files
        .iter()
        .map(|file| std::fs::read(&file.path).unwrap())
        .collect();
    let second = service.export_recording(&request, NOW).await.unwrap();
    let after: Vec<Vec<u8>> = second
        .files
        .iter()
        .map(|file| std::fs::read(&file.path).unwrap())
        .collect();
    assert_eq!(before, after, "export is not deterministic");
}

#[tokio::test]
async fn exported_audio_matches_the_stored_digest_and_the_source_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    let out = dir.path().join("exports");
    std::fs::create_dir_all(&out).unwrap();

    let blobs_before: Vec<(String, u64)> = library.archive.list_blobs().unwrap();
    let digests_before: Vec<String> = blobs_before
        .iter()
        .map(|(relative, _)| {
            library::digest_of(&library.archive.absolute(relative).unwrap()).to_hex()
        })
        .collect();

    let response = service
        .export_media(
            &ExportMediaRequest {
                recording_id: library.recording_id,
                media_asset_id: Some(library.original_asset),
                destination: out,
                include_sanitized_derivative: true,
            },
            NOW,
        )
        .await
        .expect("export media");

    assert!(response.original.is_original);
    assert_eq!(
        response.original.sha256,
        library.original_identity.sha256.to_hex()
    );
    assert_eq!(
        std::fs::read(&response.original.path).unwrap(),
        library.original_bytes
    );

    let sanitized = response.sanitized.expect("sanitized derivative");
    assert!(!sanitized.is_original);
    assert!(sanitized.label.contains("Sanitized derivative"));
    assert_eq!(sanitized.stripped_metadata, vec!["LIST"]);
    let sanitized_bytes = std::fs::read(&sanitized.path).unwrap();
    assert!(
        !sanitized_bytes
            .windows(19)
            .any(|window| window == b"PLAUD-SERIAL-A1B2C3"),
        "the serial survived the sanitized derivative"
    );
    assert!(
        library
            .original_bytes
            .windows(19)
            .any(|window| window == b"PLAUD-SERIAL-A1B2C3"),
        "the fixture must contain a serial for this test to mean anything"
    );
    assert_ne!(sanitized.sha256, response.original.sha256);

    // Managed media is byte-identical after both exports.
    let digests_after: Vec<String> = blobs_before
        .iter()
        .map(|(relative, _)| {
            library::digest_of(&library.archive.absolute(relative).unwrap()).to_hex()
        })
        .collect();
    assert_eq!(digests_before, digests_after);
    assert_eq!(library.archive.list_blobs().unwrap(), blobs_before);

    // The derivative is recorded as its own asset, not as the original.
    let derivatives = plaudon_persistence::backups::ExportRepository::new(library.db.clone())
        .derivatives_for(library.recording_id)
        .await
        .unwrap();
    assert_eq!(derivatives.len(), 1);
    assert_eq!(derivatives[0].source_media_asset_id, library.original_asset);
    assert_ne!(
        derivatives[0].identity.sha256.to_hex(),
        library.original_identity.sha256.to_hex()
    );
}

#[tokio::test]
async fn a_hostile_title_cannot_escape_the_export_folder() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    sqlx::query("UPDATE recordings SET title = ?2 WHERE id = ?1")
        .bind(library.recording_id.to_string())
        .bind("../../../../etc/passwd")
        .execute(library.db.pool())
        .await
        .unwrap();

    let out = dir.path().join("exports");
    std::fs::create_dir_all(&out).unwrap();
    let response = service
        .export_recording(
            &ExportRecordingRequest {
                recording_id: library.recording_id,
                transcript_revision_id: None,
                formats: vec![ExportFormat::Txt],
                destination: out.clone(),
            },
            NOW,
        )
        .await
        .expect("export");
    let written = &response.files[0].path;
    assert_eq!(written.parent().unwrap(), out.canonicalize().unwrap());
    assert!(!dir.path().join("etc").exists());
}

#[tokio::test]
async fn an_unwritable_or_missing_destination_is_refused_before_anything_is_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    let error = service
        .export_recording(
            &ExportRecordingRequest {
                recording_id: library.recording_id,
                transcript_revision_id: None,
                formats: vec![ExportFormat::Txt],
                destination: dir.path().join("does-not-exist"),
            },
            NOW,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ExportError::DestinationRejected(_)));
    assert_eq!(error.code().as_str(), "EXPORT_DESTINATION_REJECTED");
}

#[tokio::test]
async fn every_export_is_recorded_without_a_full_path() {
    let dir = tempfile::tempdir().unwrap();
    let (library, service) = seeded(dir.path()).await;
    let out = dir.path().join("exports");
    std::fs::create_dir_all(&out).unwrap();
    service
        .export_recording(
            &ExportRecordingRequest {
                recording_id: library.recording_id,
                transcript_revision_id: Some(library.native_revision),
                formats: vec![ExportFormat::Txt, ExportFormat::Json],
                destination: out,
            },
            NOW,
        )
        .await
        .unwrap();

    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT format, destination_hint FROM export_events ORDER BY format")
            .fetch_all(library.db.pool())
            .await
            .unwrap();
    assert_eq!(rows.len(), 2);
    for (_, hint) in &rows {
        let hint = hint.as_deref().unwrap();
        assert_eq!(hint, "exports");
        assert!(!hint.contains('/'), "a full path must not be stored");
    }
}
