//! Rendering of stored study artifacts into the three export formats the
//! study affordance offers (spec §9.3, App. A.5).
//!
//! This module is the DTO-side twin of [`plaudon_ai_tools::render`]. The tool
//! crate renders domain artifacts against a live transcript revision, which it
//! re-verifies as it goes; by the time a set reaches the interface the
//! verification has already happened and travels in the
//! [`AnchorDto`](crate::dto::AnchorDto). Rendering therefore works from the
//! DTO alone, which keeps two rules true that a view cannot be trusted with:
//!
//! * **A timestamp is printed only when `seek_ms` is `Some`.** A revision
//!   without real times (spec §8.6A) produces `[#3]`, never `[#3 @ 00:00:00]`.
//! * **An anchor that did not verify is not a citation.** It is named as a
//!   failed citation instead of being rendered as if it had resolved, because
//!   a grounding failure the reader cannot see is a grounding failure that
//!   looks like success (spec §9.6).
//!
//! Nothing here does I/O, so the interface can render an export preview
//! without asking the backend for anything.

use std::fmt::Write as _;

use crate::dto::{AnchorDto, StudyArtifactsDto};

/// The sentence every rendered study export carries.
///
/// Spec §9.1: generated material is an assistive draft. The disclaimer is part
/// of the artifact rather than part of the window, so it survives a copy, a
/// paste and a file on disk.
pub const STUDY_DISCLAIMER: &str =
    "These materials are generated from the recording and cite the transcript segments they \
     came from. They are an assistive draft, not an authoritative record.";

/// Shown in place of a citation when no anchor of an item still resolves.
///
/// The stored item verified when it was written, so this text means the
/// transcript it cited has since changed; saying so is the point.
pub const CITATION_DID_NOT_VERIFY: &str = "[citation did not verify]";

/// `hh:mm:ss` for a millisecond offset, matching
/// [`plaudon_ai_tools::render::format_timestamp`].
#[must_use]
pub fn format_timestamp(ms: u64) -> String {
    let total_seconds = ms / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

/// Format the verified anchors of one item as `[#3]` or `[#3 @ 00:01:30]`.
///
/// Only verified anchors are listed: an unverified anchor is not a citation,
/// and an item that has none left says so.
#[must_use]
pub fn render_citations(anchors: &[AnchorDto]) -> String {
    let parts: Vec<String> = anchors
        .iter()
        .filter(|anchor| anchor.verified)
        .map(|anchor| match anchor.seek_ms {
            Some(ms) => format!("#{} @ {}", anchor.seq_index, format_timestamp(ms)),
            None => format!("#{}", anchor.seq_index),
        })
        .collect();
    if parts.is_empty() {
        return CITATION_DID_NOT_VERIFY.to_owned();
    }
    format!("[{}]", parts.join(", "))
}

/// Render a study set as Markdown.
#[must_use]
pub fn study_artifacts_markdown(artifacts: &StudyArtifactsDto) -> String {
    let mut out = String::new();
    out.push_str("# Study materials\n\n");
    out.push_str(STUDY_DISCLAIMER);
    out.push_str("\n\n");

    if !artifacts.flashcards.is_empty() {
        out.push_str("## Flashcards\n\n");
        for (index, card) in artifacts.flashcards.iter().enumerate() {
            let _ = writeln!(
                out,
                "{}. **{}**\n   - {}\n   - Source: {}",
                index + 1,
                card.front,
                card.back,
                render_citations(&card.anchors)
            );
        }
        out.push('\n');
    }

    if !artifacts.quiz.is_empty() {
        out.push_str("## Quiz\n\n");
        for (index, item) in artifacts.quiz.iter().enumerate() {
            let _ = writeln!(out, "{}. {}", index + 1, item.question);
            for (position, option) in item.options.iter().enumerate() {
                let _ = writeln!(out, "   {}. {}", option_label(position), option);
            }
        }
        out.push_str("\n### Answer key\n\n");
        for (index, item) in artifacts.quiz.iter().enumerate() {
            let _ = writeln!(
                out,
                "{}. {} — Source: {}",
                index + 1,
                answer_text(&item.options, item.answer_index),
                render_citations(&item.anchors)
            );
        }
        out.push('\n');
    }

    if !artifacts.definitions.is_empty() {
        out.push_str("## Definitions\n\n");
        for definition in &artifacts.definitions {
            let _ = writeln!(
                out,
                "- **{}** — {} {}",
                definition.term,
                definition.definition,
                render_citations(&definition.anchors)
            );
        }
        out.push('\n');
    }

    if artifacts.flashcards.is_empty()
        && artifacts.quiz.is_empty()
        && artifacts.definitions.is_empty()
    {
        out.push_str("No study material met the citation requirement for this recording.\n");
    }
    out
}

/// Render a study set as plain text.
///
/// The Markdown rendering is the single source of the wording; this strips the
/// markup rather than maintaining a second copy of the document that could
/// drift from it.
#[must_use]
pub fn study_artifacts_text(artifacts: &StudyArtifactsDto) -> String {
    study_artifacts_markdown(artifacts)
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

/// Render a study set as a JSON document.
///
/// Pretty-printed, because the only consumer is a file the user reads or
/// re-imports elsewhere.
#[must_use]
pub fn study_artifacts_json(artifacts: &StudyArtifactsDto) -> String {
    serde_json::to_string_pretty(artifacts)
        .unwrap_or_else(|_| "{\"flashcards\":[],\"quiz\":[],\"definitions\":[]}".to_owned())
}

/// `a`, `b`, `c`, … for an option position, falling back to the number once
/// the alphabet runs out.
fn option_label(position: usize) -> String {
    u8::try_from(position)
        .ok()
        .filter(|position| *position < 26)
        .map_or_else(
            || (position + 1).to_string(),
            |position| char::from(b'a' + position).to_string(),
        )
}

/// The answer an item's key points at.
///
/// An index that does not address an option is reported as missing rather than
/// silently rendered as the first option: an answer key that quietly points at
/// the wrong answer is worse than one that admits it is unavailable.
fn answer_text(options: &[String], answer_index: u32) -> String {
    options
        .get(answer_index as usize)
        .cloned()
        .unwrap_or_else(|| "answer not available".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dto::{DefinitionDto, FlashcardDto, QuizItemDto};

    fn anchor(seq_index: u32, verified: bool, seek_ms: Option<u64>) -> AnchorDto {
        AnchorDto {
            seq_index,
            char_start: 0,
            char_end: 7,
            excerpt: "We will".to_owned(),
            verified,
            rejection_code: (!verified).then(|| "excerpt_mismatch".to_owned()),
            matched_text: verified.then(|| "We will".to_owned()),
            alignment_origin: if seek_ms.is_some() { "native" } else { "none" }.to_owned(),
            seek_ms,
        }
    }

    fn artifacts() -> StudyArtifactsDto {
        StudyArtifactsDto {
            flashcards: vec![FlashcardDto {
                front: "What is idempotency?".to_owned(),
                back: "Retrying does not duplicate work.".to_owned(),
                anchors: vec![anchor(3, true, Some(90_000))],
            }],
            quiz: vec![QuizItemDto {
                question: "When does the migration ship?".to_owned(),
                options: vec!["Monday".to_owned(), "Friday".to_owned()],
                answer_index: 1,
                anchors: vec![anchor(0, true, None)],
            }],
            definitions: vec![DefinitionDto {
                term: "Idempotency".to_owned(),
                definition: "A retry that duplicates nothing.".to_owned(),
                anchors: vec![anchor(1, true, Some(0))],
            }],
        }
    }

    #[test]
    fn a_citation_prints_a_time_only_when_the_anchor_carries_one() {
        assert_eq!(
            render_citations(&[anchor(3, true, Some(90_000))]),
            "[#3 @ 00:01:30]"
        );
        assert_eq!(render_citations(&[anchor(3, true, None)]), "[#3]");
    }

    #[test]
    fn an_anchor_that_did_not_verify_is_never_rendered_as_a_citation() {
        let rendered = render_citations(&[anchor(4, false, Some(1_000))]);
        assert_eq!(rendered, CITATION_DID_NOT_VERIFY);
        assert!(!rendered.contains("#4"));
    }

    #[test]
    fn markdown_carries_every_section_the_answer_key_and_the_disclaimer() {
        let markdown = study_artifacts_markdown(&artifacts());
        assert!(markdown.starts_with("# Study materials"));
        assert!(markdown.contains(STUDY_DISCLAIMER));
        assert!(markdown.contains("## Flashcards"));
        assert!(markdown.contains("## Quiz"));
        assert!(markdown.contains("### Answer key"));
        assert!(markdown.contains("## Definitions"));
        assert!(markdown.contains("[#3 @ 00:01:30]"));
        // The key names the option the index addresses, not the first one.
        assert!(markdown.contains("1. Friday — Source: [#0]"));
    }

    #[test]
    fn an_answer_key_outside_the_options_says_so_instead_of_naming_one() {
        let mut set = artifacts();
        set.quiz[0].answer_index = 7;
        let markdown = study_artifacts_markdown(&set);
        assert!(markdown.contains("answer not available"));
        assert!(!markdown.contains("1. Monday — Source"));
    }

    #[test]
    fn an_empty_set_says_nothing_met_the_citation_requirement() {
        let markdown = study_artifacts_markdown(&StudyArtifactsDto::default());
        assert!(markdown.contains("No study material met the citation requirement"));
    }

    #[test]
    fn plain_text_keeps_the_words_and_drops_the_markup() {
        let text = study_artifacts_text(&artifacts());
        assert!(text.starts_with("Study materials"));
        assert!(!text.contains("**"));
        assert!(text.contains("Answer key"));
        assert!(text.contains("What is idempotency?"));
    }

    #[test]
    fn json_round_trips_through_the_dto() {
        let rendered = study_artifacts_json(&artifacts());
        let parsed: StudyArtifactsDto = serde_json::from_str(&rendered).expect("valid json");
        assert_eq!(parsed, artifacts());
    }
}
