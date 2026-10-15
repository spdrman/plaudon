//! The first-run step machine (matrix rows C8 and S15).
//!
//! A direct port of `features/onboarding/onboardingMachine.ts`, which the React
//! suite covered with its own unit tests. The reducer owns ordering,
//! skippability and the choices later sent to `complete_onboarding`; it
//! performs no IO, so the view can be a thin renderer and the ordering rules
//! stay testable without a window.
//!
//! Two product rules live here rather than in the view:
//!
//! * **Local-only is the default posture** (spec §16.2A). The machine starts
//!   with it on, and records separately whether the user has actually answered
//!   the question, so "never asked" is not stored as "chose cloud".
//! * **Only the account steps are skippable** (spec §5.2). Skipping is
//!   recorded, so the finish step can say what was skipped instead of implying
//!   a choice was made.

use serde::{Deserialize, Serialize};

use crate::dto::Platform;

/// One step of first-run setup, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingStep {
    /// What stays on this machine.
    Privacy,
    /// Local-only, or allow cloud AI later.
    Mode,
    /// Transcription engines present on this machine.
    Engines,
    /// The optional OpenRouter key.
    Key,
    /// Where audio comes from.
    Sources,
    /// The macOS Privacy & Security permissions the features need (#307).
    /// Only on macOS; skipped everywhere else.
    Permissions,
    /// Review and finish.
    Finish,
}

impl OnboardingStep {
    /// Every step, in order, on the platform that has the most of them.
    pub const ALL: &'static [Self] = &[
        Self::Privacy,
        Self::Mode,
        Self::Engines,
        Self::Key,
        Self::Sources,
        Self::Permissions,
        Self::Finish,
    ];

    /// The steps without the macOS permissions step, for every other
    /// platform.
    const WITHOUT_PERMISSIONS: &'static [Self] = &[
        Self::Privacy,
        Self::Mode,
        Self::Engines,
        Self::Key,
        Self::Sources,
        Self::Finish,
    ];

    /// The steps first-run setup shows on `platform`, in order.
    ///
    /// The permissions step exists only where there are permissions to set
    /// up, which today means macOS ([`crate::os_permissions`]).
    #[must_use]
    pub const fn steps_for(platform: Platform) -> &'static [Self] {
        match platform {
            Platform::Macos => Self::ALL,
            Platform::Windows | Platform::Linux => Self::WITHOUT_PERMISSIONS,
        }
    }

    /// Stable wire identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Privacy => "privacy",
            Self::Mode => "mode",
            Self::Engines => "engines",
            Self::Key => "key",
            Self::Sources => "sources",
            Self::Permissions => "permissions",
            Self::Finish => "finish",
        }
    }

    /// Heading of the step.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Privacy => "Your recordings stay on this computer",
            Self::Mode => "Local only, or allow cloud AI later",
            Self::Engines => "Transcription engines on this machine",
            Self::Key => "Optional: add an OpenRouter key",
            Self::Sources => "Where your audio comes from",
            Self::Permissions => "macOS Privacy & Security",
            Self::Finish => "Finish setup",
        }
    }

    /// Whether the user may leave the step without making any choice.
    #[must_use]
    pub const fn skippable(self) -> bool {
        matches!(self, Self::Key | Self::Sources | Self::Permissions)
    }
}

/// What the user did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnboardingAction {
    /// Advance one step.
    Next,
    /// Go back one step.
    Back,
    /// Leave a skippable step without choosing.
    Skip,
    /// Answer the local/cloud question.
    ChooseMode {
        /// Whether Local-only Mode should be enforced.
        local_only: bool,
    },
    /// Choose, or clear, the default speech-to-text engine.
    ChooseSttEngine(Option<&'static str>),
    /// Record that the key was handed over. The key itself never enters this
    /// state (spec §16.4).
    KeySaved,
}

/// Everything the flow remembers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingState {
    steps: &'static [OnboardingStep],
    step_index: usize,
    local_only: bool,
    mode_chosen: bool,
    default_stt_engine_id: Option<String>,
    key_saved: bool,
    skipped: Vec<OnboardingStep>,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self {
            steps: OnboardingStep::steps_for(Platform::host()),
            step_index: 0,
            // Spec §16.2A: the default posture is the private one.
            local_only: true,
            mode_chosen: false,
            default_stt_engine_id: None,
            key_saved: false,
            skipped: Vec::new(),
        }
    }
}

impl OnboardingState {
    /// A fresh flow for the platform this binary runs on.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A fresh flow with the steps `platform` shows.
    #[must_use]
    pub fn for_platform(platform: Platform) -> Self {
        Self {
            steps: OnboardingStep::steps_for(platform),
            ..Self::default()
        }
    }

    /// The steps this flow walks through, in order.
    #[must_use]
    pub const fn steps(&self) -> &'static [OnboardingStep] {
        self.steps
    }

    /// Index of the last step of this flow.
    #[must_use]
    pub const fn last_index(&self) -> usize {
        self.steps.len().saturating_sub(1)
    }

    /// The step being shown.
    #[must_use]
    pub fn step(&self) -> OnboardingStep {
        self.steps
            .get(self.step_index)
            .copied()
            .unwrap_or(OnboardingStep::Privacy)
    }

    /// Zero-based index of the step being shown.
    #[must_use]
    pub const fn step_index(&self) -> usize {
        self.step_index
    }

    /// Whether the flow is on its last step.
    #[must_use]
    pub const fn is_last_step(&self) -> bool {
        self.step_index >= self.last_index()
    }

    /// Whether Local-only Mode will be enforced.
    #[must_use]
    pub const fn local_only(&self) -> bool {
        self.local_only
    }

    /// Whether the user has actually answered the local/cloud question.
    #[must_use]
    pub const fn mode_chosen(&self) -> bool {
        self.mode_chosen
    }

    /// The chosen default speech-to-text engine.
    #[must_use]
    pub fn default_stt_engine_id(&self) -> Option<&str> {
        self.default_stt_engine_id.as_deref()
    }

    /// Whether a key has been handed to the keychain during this flow.
    #[must_use]
    pub const fn key_saved(&self) -> bool {
        self.key_saved
    }

    /// Steps the user skipped, in the order they were skipped.
    #[must_use]
    pub fn skipped(&self) -> &[OnboardingStep] {
        &self.skipped
    }

    /// Text progress indicator; status is never a bar or colour alone
    /// (spec §5.3).
    #[must_use]
    pub fn progress_label(&self) -> String {
        format!("Step {} of {}", self.step_index + 1, self.steps.len())
    }

    /// Apply one action. Anything the current step does not allow is a no-op,
    /// which is what keeps a double click from skipping two steps.
    pub fn apply(&mut self, action: OnboardingAction) {
        match action {
            OnboardingAction::Next => {
                if !self.is_last_step() {
                    self.step_index += 1;
                }
            }
            OnboardingAction::Back => {
                self.step_index = self.step_index.saturating_sub(1);
            }
            OnboardingAction::Skip => {
                let step = self.step();
                if !step.skippable() {
                    return;
                }
                self.step_index = (self.step_index + 1).min(self.last_index());
                if !self.skipped.contains(&step) {
                    self.skipped.push(step);
                }
            }
            OnboardingAction::ChooseMode { local_only } => {
                self.local_only = local_only;
                self.mode_chosen = true;
            }
            OnboardingAction::ChooseSttEngine(engine_id) => {
                self.default_stt_engine_id = engine_id.map(str::to_owned);
            }
            OnboardingAction::KeySaved => self.key_saved = true,
        }
    }

    /// Choose an engine whose identifier is not `'static`.
    pub fn choose_stt_engine(&mut self, engine_id: Option<String>) {
        self.default_stt_engine_id = engine_id;
    }

    /// The request `complete_onboarding` is called with.
    ///
    /// Notification preferences are passed through from the stored ones rather
    /// than invented: setup never turns a notification category on.
    #[must_use]
    pub fn completion_request(
        &self,
        notifications: crate::dto::NotificationSettings,
    ) -> crate::dto::CompleteOnboardingRequest {
        crate::dto::CompleteOnboardingRequest {
            default_stt_engine_id: self.default_stt_engine_id.clone(),
            default_text_engine_id: None,
            local_only_mode: self.local_only,
            notifications,
        }
    }
}

/// The sentence the engines step shows about downloading.
///
/// Setup downloads nothing by itself in either mode. With Local-only Mode on
/// it downloads nothing at all, and says the offline import still works; with
/// it off, nothing is fetched until the user approves one named download
/// (issue #303).
#[must_use]
pub const fn engines_download_note(local_only: bool) -> &'static str {
    if local_only {
        "Local-only mode is on, so Plaudon downloads nothing, not even a model. You can still \
         import a model file you already have; that never touches the network."
    } else {
        "Plaudon downloads nothing until you approve one specific download below. Each \
         approval names the one host and file it covers, and ends when that download does."
    }
}

/// The summary line for the chosen network policy.
#[must_use]
pub const fn mode_summary(local_only: bool) -> &'static str {
    if local_only {
        "Local only"
    } else {
        "Cloud AI allowed later, nothing granted yet"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flow_starts_private_and_unanswered() {
        let state = OnboardingState::for_platform(Platform::Macos);
        assert_eq!(state.step(), OnboardingStep::Privacy);
        assert!(state.local_only(), "the default posture is local only");
        assert!(
            !state.mode_chosen(),
            "never asked must not be stored as answered"
        );
        assert_eq!(state.progress_label(), "Step 1 of 7");
    }

    #[test]
    fn next_walks_every_step_and_stops_at_the_last() {
        let mut state = OnboardingState::for_platform(Platform::Macos);
        for expected in state.steps().iter().skip(1) {
            state.apply(OnboardingAction::Next);
            assert_eq!(state.step(), *expected);
        }
        assert!(state.is_last_step());
        state.apply(OnboardingAction::Next);
        assert_eq!(state.step(), OnboardingStep::Finish, "the last step holds");
    }

    #[test]
    fn back_stops_at_the_first_step() {
        let mut state = OnboardingState::new();
        state.apply(OnboardingAction::Back);
        assert_eq!(state.step_index(), 0);
        state.apply(OnboardingAction::Next);
        state.apply(OnboardingAction::Back);
        assert_eq!(state.step(), OnboardingStep::Privacy);
    }

    #[test]
    fn only_the_account_steps_are_skippable() {
        assert!(!OnboardingStep::Privacy.skippable());
        assert!(!OnboardingStep::Mode.skippable());
        assert!(!OnboardingStep::Engines.skippable());
        assert!(OnboardingStep::Key.skippable());
        assert!(OnboardingStep::Sources.skippable());
        assert!(OnboardingStep::Permissions.skippable());
        assert!(!OnboardingStep::Finish.skippable());
    }

    #[test]
    fn skipping_a_step_that_cannot_be_skipped_does_nothing() {
        let mut state = OnboardingState::new();
        state.apply(OnboardingAction::Skip);
        assert_eq!(state.step(), OnboardingStep::Privacy);
        assert!(
            state.skipped().is_empty(),
            "expected state.skipped() to be empty, got {:?}",
            state.skipped()
        );
    }

    #[test]
    fn skipping_records_the_step_once_and_advances() {
        let mut state = OnboardingState::new();
        for _ in 0..3 {
            state.apply(OnboardingAction::Next);
        }
        assert_eq!(state.step(), OnboardingStep::Key);
        state.apply(OnboardingAction::Skip);
        assert_eq!(state.step(), OnboardingStep::Sources);
        assert_eq!(state.skipped(), &[OnboardingStep::Key]);
        state.apply(OnboardingAction::Back);
        state.apply(OnboardingAction::Skip);
        assert_eq!(
            state.skipped(),
            &[OnboardingStep::Key],
            "skipping twice records once"
        );
    }

    #[test]
    fn choosing_cloud_records_both_the_choice_and_that_it_was_made() {
        let mut state = OnboardingState::new();
        state.apply(OnboardingAction::ChooseMode { local_only: false });
        assert!(!state.local_only());
        assert!(state.mode_chosen());
    }

    #[test]
    fn an_engine_choice_can_be_made_and_cleared() {
        let mut state = OnboardingState::new();
        state.apply(OnboardingAction::ChooseSttEngine(Some("whisper.cpp")));
        assert_eq!(state.default_stt_engine_id(), Some("whisper.cpp"));
        state.apply(OnboardingAction::ChooseSttEngine(None));
        assert_eq!(state.default_stt_engine_id(), None);
        state.choose_stt_engine(Some("apple.speech".to_owned()));
        assert_eq!(state.default_stt_engine_id(), Some("apple.speech"));
    }

    #[test]
    fn the_key_is_never_part_of_the_state_only_the_fact_it_was_saved() {
        let mut state = OnboardingState::new();
        assert!(!state.key_saved());
        state.apply(OnboardingAction::KeySaved);
        assert!(state.key_saved());
        let rendered = format!("{state:?}");
        assert!(!rendered.contains("sk-or"), "{rendered}");
    }

    #[test]
    fn the_completion_request_carries_the_choices_and_invents_no_notification() {
        let mut state = OnboardingState::new();
        state.apply(OnboardingAction::ChooseMode { local_only: false });
        state.apply(OnboardingAction::ChooseSttEngine(Some("whisper.cpp")));
        let stored = crate::backend::default_preferences().notifications;
        let request = state.completion_request(stored.clone());
        assert_eq!(
            request.default_stt_engine_id.as_deref(),
            Some("whisper.cpp")
        );
        assert_eq!(request.default_text_engine_id, None);
        assert!(!request.local_only_mode);
        assert_eq!(request.notifications, stored);
        assert!(
            request.notifications.categories.values().all(|on| !on),
            "setup turns no category on"
        );
    }

    #[test]
    fn the_download_note_states_the_guarantee_in_local_only_mode() {
        assert!(engines_download_note(true).contains("downloads nothing"));
        assert!(engines_download_note(false).contains("downloads nothing"));
        assert!(engines_download_note(true).contains("Local-only mode is on"));
    }

    #[test]
    fn the_mode_summary_never_claims_a_grant() {
        assert_eq!(mode_summary(true), "Local only");
        assert!(mode_summary(false).contains("nothing granted yet"));
    }

    #[test]
    fn every_step_has_a_title_and_a_stable_identifier() {
        for &step in OnboardingStep::ALL {
            assert!(
                !step.title().is_empty(),
                "expected step.title() to be non-empty"
            );
            assert!(
                !step.as_str().is_empty(),
                "expected step.as_str() to be non-empty"
            );
        }
        assert_eq!(OnboardingStep::ALL.len(), 7);
    }

    #[test]
    fn issue_307_the_macos_flow_has_a_permissions_step_just_before_finish() {
        let state = OnboardingState::for_platform(Platform::Macos);
        assert_eq!(
            state.steps(),
            &[
                OnboardingStep::Privacy,
                OnboardingStep::Mode,
                OnboardingStep::Engines,
                OnboardingStep::Key,
                OnboardingStep::Sources,
                OnboardingStep::Permissions,
                OnboardingStep::Finish,
            ]
        );
        assert_eq!(state.progress_label(), "Step 1 of 7");
        assert_eq!(state.last_index(), 6);
    }

    #[test]
    fn issue_307_the_permissions_step_is_skipped_off_macos() {
        for platform in [Platform::Windows, Platform::Linux] {
            let mut state = OnboardingState::for_platform(platform);
            assert!(
                !state.steps().contains(&OnboardingStep::Permissions),
                "{platform:?} has no macOS permissions to set up"
            );
            assert_eq!(state.progress_label(), "Step 1 of 6");
            for _ in 0..4 {
                state.apply(OnboardingAction::Next);
            }
            assert_eq!(state.step(), OnboardingStep::Sources);
            state.apply(OnboardingAction::Next);
            assert_eq!(state.step(), OnboardingStep::Finish, "{platform:?}");
            assert_eq!(state.progress_label(), "Step 6 of 6");
            assert!(state.is_last_step());
        }
    }

    #[test]
    fn issue_307_the_permissions_step_can_be_left_for_later_and_that_is_recorded() {
        assert!(OnboardingStep::Permissions.skippable());
        let mut state = OnboardingState::for_platform(Platform::Macos);
        for _ in 0..5 {
            state.apply(OnboardingAction::Next);
        }
        assert_eq!(state.step(), OnboardingStep::Permissions);
        state.apply(OnboardingAction::Skip);
        assert_eq!(state.step(), OnboardingStep::Finish);
        assert_eq!(state.skipped(), &[OnboardingStep::Permissions]);
    }

    #[test]
    fn issue_307_next_never_waits_on_a_permission() {
        // Nothing about a permission's status can hold the flow: Next on the
        // permissions step always advances.
        let mut state = OnboardingState::for_platform(Platform::Macos);
        for _ in 0..5 {
            state.apply(OnboardingAction::Next);
        }
        assert_eq!(state.step(), OnboardingStep::Permissions);
        state.apply(OnboardingAction::Next);
        assert_eq!(state.step(), OnboardingStep::Finish);
        assert_eq!(state.skipped(), &[] as &[OnboardingStep]);
    }

    #[test]
    fn issue_307_the_host_flow_matches_the_host_platform() {
        assert_eq!(
            OnboardingState::new().steps(),
            OnboardingStep::steps_for(Platform::host())
        );
    }
}
