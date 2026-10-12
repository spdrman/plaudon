//! Rendering of study artifacts into the working-export formats (App. A.5).
//!
//! Rendering is pure and does no I/O, so the export crate can call it without
//! taking a dependency on model dispatch. Citations are rendered with the
//! segment index they point at; a playback time is only ever printed when the
//! revision's alignment origin permits one (spec 8.6A).

use plaudon_domain::transcript::TranscriptRevision;

use crate::anchor::{verify_anchor, AnchorVerification, SourceAnchor};
use crate::study::StudyArtifacts;

/// Format a citation list as `[#3]` or `[#3 @ 00:01:30]`.
#[must_use]
pub fn render_citations(anchors: &[SourceAnchor], revision: &TranscriptRevision) -> String {
    let parts: Vec<String> = anchors
        .iter()
        .map(|anchor| match verify_anchor(anchor, revision) {
            AnchorVerification::Verified {
                seek_ms: Some(ms), ..
            } => format!("#{} @ {}", anchor.seq_index, format_timestamp(ms)),
            _ => format!("#{}", anchor.seq_index),
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

/// `hh:mm:ss` for a millisecond offset.
#[must_use]
pub fn format_timestamp(ms: u64) -> String {
    let total_seconds = ms / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

/// Render study artifacts as Markdown.
#[must_use]
pub fn render_study_markdown(artifacts: &StudyArtifacts, revision: &TranscriptRevision) -> String {
    let mut out = String::new();
    out.push_str("# Study materials\n\n");
    out.push_str(&format!(
        "Template: `{}` · Transcript revision: `{}`\n\n",
        artifacts.template_version, revision.id
    ));
    out.push_str(
        "These materials are generated from the recording and cite the transcript segments \
         they came from. They are an assistive draft, not an authoritative record.\n\n",
    );

    if !artifacts.flashcards.is_empty() {
        out.push_str("## Flashcards\n\n");
        for (index, card) in artifacts.flashcards.iter().enumerate() {
            out.push_str(&format!(
                "{}. **{}**\n   - {}\n   - Source: {}\n",
                index + 1,
                card.front,
                card.back,
                render_citations(&card.anchors, revision)
            ));
        }
        out.push('\n');
    }

    if !artifacts.quiz.is_empty() {
        out.push_str("## Quiz\n\n");
        for (index, item) in artifacts.quiz.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", index + 1, item.question));
            for distractor in &item.distractors {
                out.push_str(&format!("   - {distractor}\n"));
            }
        }
        out.push_str("\n### Answer key\n\n");
        for (index, item) in artifacts.quiz.iter().enumerate() {
            out.push_str(&format!(
                "{}. {} — Source: {}\n",
                index + 1,
                item.answer,
                render_citations(&item.anchors, revision)
            ));
        }
        out.push('\n');
    }

    if !artifacts.definitions.is_empty() {
        out.push_str("## Definitions\n\n");
        for definition in &artifacts.definitions {
            out.push_str(&format!(
                "- **{}** — {} {}\n",
                definition.term,
                definition.definition,
                render_citations(&definition.anchors, revision)
            ));
        }
        out.push('\n');
    }

    if artifacts.is_empty() {
        out.push_str("No study material met the citation requirement for this recording.\n");
    }
    out
}

/// Render study artifacts as plain text.
#[must_use]
pub fn render_study_text(artifacts: &StudyArtifacts, revision: &TranscriptRevision) -> String {
    let markdown = render_study_markdown(artifacts, revision);
    markdown
        .lines()
        .map(|line| {
            line.trim_start_matches("# ")
                .trim_start_matches("## ")
                .trim_start_matches("### ")
                .replace("**", "")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render study artifacts as a JSON document.
///
/// # Panics
///
/// Panics only if the artifacts cannot be serialized, which the type system
/// makes impossible for the fields involved.
#[must_use]
pub fn render_study_json(artifacts: &StudyArtifacts) -> serde_json::Value {
    serde_json::to_value(artifacts).expect("study artifacts are always serializable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::study::{Definition, Flashcard, QuizItem, STUDY_TEMPLATE_VERSION};
    use crate::testing::{fixture_revision, FIXTURE_SENTENCES};
    use plaudon_domain::transcript::AlignmentOrigin;

    fn artifacts(revision: &TranscriptRevision) -> StudyArtifacts {
        let excerpt: String = FIXTURE_SENTENCES[4].chars().take(11).collect();
        StudyArtifacts {
            template_version: STUDY_TEMPLATE_VERSION.to_string(),
            flashcards: vec![Flashcard {
                front: "What is idempotency?".to_string(),
                back: "Retry safety.".to_string(),
                anchors: vec![SourceAnchor::new(revision.id, 4, 0, 11, excerpt.clone())],
            }],
            quiz: vec![QuizItem {
                question: "When does the migration ship?".to_string(),
                answer: "Friday".to_string(),
                distractors: vec!["Monday".to_string()],
                anchors: vec![SourceAnchor::new(revision.id, 0, 0, 7, "We will")],
            }],
            definitions: vec![Definition {
                term: "Idempotency".to_string(),
                definition: "Retrying does not duplicate work.".to_string(),
                anchors: vec![SourceAnchor::new(revision.id, 4, 0, 11, excerpt)],
            }],
        }
    }

    #[test]
    fn markdown_renders_every_section_with_citations() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let md = render_study_markdown(&artifacts(&revision), &revision);
        assert!(md.contains("## Flashcards"));
        assert!(md.contains("## Quiz"));
        assert!(md.contains("### Answer key"));
        assert!(md.contains("## Definitions"));
        assert!(md.contains("[#4 @ 00:00:40]"));
    }

    #[test]
    fn a_text_only_revision_renders_citations_without_inventing_a_time() {
        let revision = fixture_revision(AlignmentOrigin::None);
        let md = render_study_markdown(&artifacts(&revision), &revision);
        assert!(md.contains("[#4]"));
        assert!(!md.contains('@'));
    }

    #[test]
    fn the_answer_key_is_separate_from_the_questions() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let md = render_study_markdown(&artifacts(&revision), &revision);
        let question_at = md.find("When does the migration ship?").unwrap();
        let key_at = md.find("### Answer key").unwrap();
        let answer_at = md.rfind("Friday").unwrap();
        assert!(question_at < key_at);
        assert!(key_at < answer_at);
    }

    #[test]
    fn empty_artifacts_render_an_honest_message_rather_than_blank_sections() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let md = render_study_markdown(
            &StudyArtifacts {
                template_version: STUDY_TEMPLATE_VERSION.to_string(),
                flashcards: Vec::new(),
                quiz: Vec::new(),
                definitions: Vec::new(),
            },
            &revision,
        );
        assert!(md.contains("No study material met the citation requirement"));
    }

    #[test]
    fn text_rendering_drops_markdown_syntax_but_keeps_content() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let text = render_study_text(&artifacts(&revision), &revision);
        assert!(!text.contains("**"));
        assert!(text.contains("Study materials"));
        assert!(text.contains("Idempotency"));
    }

    #[test]
    fn json_rendering_round_trips_through_serde() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let value = render_study_json(&artifacts(&revision));
        let back: StudyArtifacts = serde_json::from_value(value).unwrap();
        assert_eq!(back.item_count(), 3);
    }

    #[test]
    fn timestamps_render_as_hours_minutes_seconds() {
        assert_eq!(format_timestamp(0), "00:00:00");
        assert_eq!(format_timestamp(3_661_000), "01:01:01");
    }

    #[test]
    fn the_rendered_header_names_the_exact_revision() {
        let revision = fixture_revision(AlignmentOrigin::Native);
        let md = render_study_markdown(&artifacts(&revision), &revision);
        assert!(md.contains(&revision.id.to_string()));
    }
}
