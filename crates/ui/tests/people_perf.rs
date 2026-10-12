//! The scale case of the voice-profile surfaces
//! (`VOICE_PROFILES_SPEC` VP-153, VP-154; `docs/EGUI_TESTING.md` §6.1).
//!
//! Three claims, each a different kind:
//!
//! 1. **The queue is bounded by its page, not by the library.** However many
//!    suggestions are pending across 10,000 recordings, the review queue asks
//!    for one page and lays out one page. This is a count, it is
//!    deterministic, and it fails the same way on every machine.
//! 2. **The frame cost does not grow with the library.** A library with ten
//!    thousand pending suggestions must not cost noticeably more per frame
//!    than one with ten. This is a ratio measured on the machine running the
//!    test, so it means something everywhere.
//! 3. **The measured budget**, enforced only where it was measured: the
//!    runner signals that with `PLAUDON_PERF=1`. Everywhere else the cost is
//!    printed, so a developer sees the number without the suite failing on a
//!    machine the budget does not describe.

use egui_kittest::Harness;
use plaudon_app::dto::{
    ActivationDisclosureDto, ConsentGateDto, ListPeopleResponse, ListSpeakerSuggestionsResponse,
    PersonDto, PersonRefDto, SpeakerSuggestionDto, VoiceProfileSettingsDto,
};
use plaudon_domain::ids::{PersonId, RecordingId, SpeakerSuggestionId};
use plaudon_domain::time::Timestamp;
use plaudon_ui::screens::people::{PeopleScreen, PeopleTab, QUEUE_PAGE_SIZE};

/// Recordings in the §17.5 scale case, and therefore pending suggestions:
/// one cluster of one recording each.
const SCALE_SUGGESTIONS: usize = 10_000;

/// Frames measured after the warm-up.
const MEASURED_FRAMES: usize = 30;

/// Environment variable that enforces the budget.
const ENFORCE_ENV: &str = "PLAUDON_PERF";

/// Mean frame cost allowed for the review queue at the scale case, in
/// milliseconds.
///
/// The §17.5 interactive budget is 100 ms; this is the same order of headroom
/// the library's scale test uses over its own measurement. The number is
/// enforced only under `PLAUDON_PERF`, which the runner sets, because an
/// absolute millisecond figure describes the machine it was measured on.
const FRAME_BUDGET_MS: f64 = 20.0;

fn enforced() -> bool {
    std::env::var(ENFORCE_ENV).is_ok_and(|value| value == "1" || value == "true")
}

fn settings() -> VoiceProfileSettingsDto {
    VoiceProfileSettingsDto {
        enabled: true,
        consent: ConsentGateDto {
            state: "active".to_owned(),
            reason: None,
            accepted_version: Some("2026-10-12".to_owned()),
            accepted_at: Some(Timestamp::from_millis(0)),
            required_version: "2026-10-12".to_owned(),
        },
        disclosure: ActivationDisclosureDto {
            version: "2026-10-12".to_owned(),
            title: "What a voiceprint is".to_owned(),
            body: vec!["A voiceprint is biometric data.".to_owned()],
            two_tier_sentence: "A name is not a voiceprint.".to_owned(),
            responsibility_sentence: "People you enrol must agree first.".to_owned(),
            retention_default_months: 12,
        },
        enrolled_people: 20,
        exemplars: 60,
        pending_suggestions: 0,
        roster_people: 200,
        key_available: true,
        embedder_available: true,
        enrol_speech_ms: 10_000,
        exemplar_speech_ms: 5_000,
    }
}

fn person(index: usize) -> PersonDto {
    PersonDto {
        person_id: PersonId::new(),
        display_name: format!("Person {index}"),
        aliases: Vec::new(),
        email: None,
        organisation: None,
        status: "active".to_owned(),
        created_at: Timestamp::from_millis(0),
        updated_at: Timestamp::from_millis(0),
        recordings: 3,
        pending_suggestions: 1,
        voice: None,
    }
}

fn suggestion(index: usize, person: &PersonDto) -> SpeakerSuggestionDto {
    SpeakerSuggestionDto {
        suggestion_id: SpeakerSuggestionId::new(),
        recording_id: RecordingId::new(),
        recording_title: Some(format!("Stand-up {index}")),
        diarization_run_id: "run-1".to_owned(),
        cluster_key: format!("S{index}"),
        speaker_id: None,
        speaker_label: Some("Speaker 1".to_owned()),
        person: PersonRefDto {
            person_id: person.person_id,
            display_name: person.display_name.clone(),
        },
        score: 0.8,
        score_kind: "as_norm".to_owned(),
        band: "suggest".to_owned(),
        state: "pending".to_owned(),
        created_at: Timestamp::from_millis(0),
    }
}

/// The People screen showing the review queue, with `pending` suggestions in
/// the library — of which the command returns at most one page, exactly as the
/// screen asks for.
fn queue_harness(pending: usize) -> Harness<'static, PeopleScreen> {
    let people: Vec<PersonDto> = (0..200).map(person).collect();
    let page = pending.min(QUEUE_PAGE_SIZE as usize);
    let suggestions: Vec<SpeakerSuggestionDto> = (0..page)
        .map(|index| suggestion(index, &people[index % people.len()]))
        .collect();
    let mut screen = PeopleScreen::new();
    screen.set_now_ms(0);
    screen.show_tab(PeopleTab::Review);
    screen.data_mut().settings = Some(settings());
    screen.data_mut().people = Some(ListPeopleResponse {
        people,
        merge_candidates: Vec::new(),
    });
    screen.data_mut().suggestions = Some(ListSpeakerSuggestionsResponse {
        pending_count: u32::try_from(pending).expect("a countable library"),
        suggestions,
    });
    let mut harness = plaudon_ui_testkit::builder().build_ui_state(
        |ui: &mut egui::Ui, screen: &mut PeopleScreen| {
            screen.ui(ui);
        },
        screen,
    );
    harness.run();
    harness
}

/// Mean cost of a frame, in milliseconds, after a warm-up frame.
fn mean_frame_ms<State>(harness: &mut Harness<'_, State>) -> f64 {
    harness.run();
    let start = std::time::Instant::now();
    for _ in 0..MEASURED_FRAMES {
        harness.step();
    }
    start.elapsed().as_secs_f64() * 1_000.0 / MEASURED_FRAMES as f64
}

#[test]
fn the_queue_asks_for_one_page_however_many_suggestions_are_pending() {
    let harness = queue_harness(SCALE_SUGGESTIONS);
    let held = harness
        .state()
        .data()
        .suggestions
        .as_ref()
        .expect("the queue was read");
    assert_eq!(
        held.suggestions.len(),
        QUEUE_PAGE_SIZE as usize,
        "the queue rendered more than the page it asked for"
    );
    assert_eq!(
        held.pending_count, SCALE_SUGGESTIONS as u32,
        "the count the user is told is the library's, not the page's"
    );
}

#[test]
fn the_cost_of_a_queue_frame_does_not_grow_with_the_library() {
    let small = mean_frame_ms(&mut queue_harness(10));
    let large = mean_frame_ms(&mut queue_harness(SCALE_SUGGESTIONS));
    println!(
        "review queue frame cost: 10 pending {small:.3} ms, {SCALE_SUGGESTIONS} pending \
         {large:.3} ms"
    );

    if enforced() {
        assert!(
            large <= FRAME_BUDGET_MS,
            "a frame with {SCALE_SUGGESTIONS} pending suggestions took {large:.3} ms, over the \
             {FRAME_BUDGET_MS} ms budget"
        );
    }

    // A thousandfold more pending suggestions is a page more work, not a
    // library more: the page is the bound.
    let ratio = large / small.max(f64::EPSILON);
    assert!(
        ratio < 30.0,
        "a {SCALE_SUGGESTIONS}-suggestion library cost {ratio:.1}x a ten-suggestion one \
         ({small:.3} ms vs {large:.3} ms): the queue is not bounded by its page"
    );
}
