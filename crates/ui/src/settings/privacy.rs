//! Local-only mode (matrix row C2) and the voice-profile block (VP-090).
//!
//! The toggle is the single egress switch. Two things are structural here:
//!
//! * The change is sent with `confirmed: true`, because spec §15.2 rejects an
//!   unconfirmed egress change outright; the toggle *is* the confirmation.
//! * The enforced policy — the description and the reachable hosts — is only
//!   rendered after the gate has answered. Before that the interface knows the
//!   stored preference and nothing about what is actually enforced, and
//!   composing a policy sentence from the preference would be inventing it.
//!
//! The second half of the section is the biometric tier of speaker voice
//! profiles (`docs/specs/VOICE_PROFILES_SPEC.md` §9 VP-090, §12.2, §12.2A).
//! Three obligations are structural there too:
//!
//! * **Off until the disclosure is accepted.** The master toggle cannot turn
//!   the feature on by itself: clicking it opens the versioned activation
//!   disclosure, and only the explicit acceptance control sends
//!   `set_voice_profiles_enabled` — carrying the version the backend named
//!   (VP-003, VP-120). Turning the feature *off* is immediate; nothing is
//!   gated on switching a thing off.
//! * **The disclosure is the backend's text.** Title, every body paragraph,
//!   the two-tier sentence and the responsibility sentence are rendered from
//!   [`plaudon_app::dto::ActivationDisclosureDto`] verbatim; this module
//!   composes none of them (VP-121, VP-128).
//! * **A locked key is not a fallback.** `key_available: false` disables the
//!   toggle and says why: the OS credential store did not answer, so the
//!   feature refuses to run rather than keep a voiceprint in plaintext
//!   (ADR-0014, VP-132).

use egui::{Align, Layout, RichText, Ui};
use plaudon_app::command::{AppCommand, AppResponse};
use plaudon_app::dto::{
    ActivationDisclosureDto, EmptyRequest, ForgetAllVoiceProfilesRequest,
    ForgetAllVoiceProfilesResponse, SetLocalOnlyModeRequest, SetVoiceProfilesEnabledRequest,
    VoiceProfileSettingsDto,
};
use plaudon_app::settings_presentation::{
    enforced_policy_lines, local_only_status, Fact, LOCAL_ONLY_PROSE, LOCAL_ONLY_TOGGLE,
};
use plaudon_domain::error::ErrorEnvelope;

use super::intent::SettingsDestination;
use super::section::Confirmation;
use super::{section, Section, SettingsData, SettingsIntent, SettingsOutput, SettingsTokens, Tone};

/// Label of the retry offered after a rejected change.
const RETRY: &str = "Try changing local-only mode again";

/// Glyph on the failure badge, so the failure is not colour alone.
const FAILURE_GLYPH: &str = "✕";

/// Glyph on a warning badge.
const WARNING_GLYPH: &str = "⚠";

/// Glyph on a neutral badge.
const NEUTRAL_GLYPH: &str = "–";

/// Heading of the voice-profile block.
pub const VOICE_HEADING: &str = "Voice profiles";

/// Standing prose of the voice-profile block.
const VOICE_PROSE: &str = "Voice matching compares a voiceprint computed on this device against \
the people you have enrolled. A voiceprint is biometric data, it never leaves this machine, and \
matching stays off until you turn it on. A match is only ever a suggestion you confirm.";

/// The VP-011 / VP-126 boundary, said before the toggle rather than after it.
const ROSTER_SENTENCE: &str = "The People list works whether or not matching is on: keeping names \
and assigning them by hand needs no special consent, and no voiceprint is computed for it.";

/// Accessible name of the master toggle.
pub const VOICE_TOGGLE: &str = "Match speakers against enrolled voice profiles";

/// Shown before `get_voice_profile_settings` has answered.
///
/// Not "matching is off": that is a claim about the stored setting, and the
/// setting has not been read yet. The toggle shows unticked because off is
/// what the feature is until something says otherwise (VP-003).
pub const VOICE_UNREAD: &str = "Voice-profile settings have not been read yet.";

/// Label of the control that opens the People screen.
pub const OPEN_PEOPLE: &str = "Open the People list";

/// Label of the control that re-opens the activation disclosure.
pub const REVIEW_DISCLOSURE: &str = "Review the voice-matching disclosure";

/// Label of the retry offered after a rejected toggle change.
pub const RETRY_VOICE: &str = "Try the voice-matching disclosure again";

/// Label of the acceptance control inside the disclosure.
pub const ACCEPT_DISCLOSURE: &str = "I understand — turn on voice matching";

/// The badge shown when the OS credential store did not answer.
const KEY_LOCKED: &str = "Locked — no credential store";

/// Why a locked credential store disables the feature instead of degrading it.
const KEY_LOCKED_REASON: &str = "The operating system's credential store did not answer, so voice \
features refuse to run rather than store a voiceprint in plaintext. Nothing is enrolled or \
matched until it does.";

/// The badge shown when this build has no speaker-embedding model.
const NO_EMBEDDER: &str = "No speaker-embedding model in this build";

/// Why that badge is neutral and leaves the toggle alone.
const NO_EMBEDDER_REASON: &str = "This build has no speaker-embedding model, so nothing can be \
enrolled or matched yet. The switch is your preference, not a claim that matching works.";

/// The badge shown when the disclosure has to be accepted again (VP-121).
const DISCLOSURE_REQUIRED: &str = "Disclosure not accepted";

/// Label of the control that starts the forget-everyone disclosure (VP-095).
pub const FORGET_BUTTON: &str = "Forget all voice profiles…";

/// Label of the typed-confirmation field.
pub const FORGET_FIELD: &str = "Type forget everyone to confirm";

/// Label of the control that performs the erasure.
pub const FORGET_CONFIRM: &str = "Forget everyone";

/// Label of the control that backs out of the erasure.
pub const FORGET_CANCEL: &str = "Keep voice profiles";

/// The words the user has to type, compared trimmed and case-insensitively.
pub const FORGET_PHRASE: &str = "forget everyone";

/// The sentence that keeps VP-095's promise about the roster explicit.
const FORGET_ROSTER_SURVIVES: &str = "Your People list and every speaker assignment survive: they \
are names, not voiceprints. Deleting a person is a separate, ordinary action.";

/// Local-only mode and the policy the gate reports (matrix row C2).
#[derive(Debug, Default)]
pub struct PrivacySection {
    /// The mode the last request asked for, so a retry asks for the same
    /// thing rather than for the opposite of whatever is on screen now.
    attempted: Option<bool>,
    /// Whether a change is in flight.
    pending: bool,
    /// The last rejection, kept until a change succeeds.
    error: Option<ErrorEnvelope>,
    /// The voice-profile block's own transient state.
    voice: VoiceProfiles,
}

impl PrivacySection {
    /// Raise the egress change, with the explicit confirmation §15.2 requires.
    fn request(&mut self, enabled: bool, out: &mut SettingsOutput) {
        self.attempted = Some(enabled);
        self.pending = true;
        out.command(AppCommand::SetLocalOnlyMode(SetLocalOnlyModeRequest {
            enabled,
            confirmed: true,
        }));
    }
}

impl Section for PrivacySection {
    fn title(&self) -> &'static str {
        "Privacy & network"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        section::prose(ui, tokens, LOCAL_ONLY_PROSE);

        // The enforced answer wins over the stored preference; before either
        // is known the safe reading is that nothing may leave the machine.
        let mut enabled = data
            .enforced_policy
            .as_ref()
            .map(|policy| policy.enabled)
            .or_else(|| data.preferences().map(|p| p.local_only_mode))
            .unwrap_or(true);

        ui.horizontal_wrapped(|ui| {
            let toggle = ui.add_enabled(
                !self.pending,
                egui::Checkbox::new(&mut enabled, LOCAL_ONLY_TOGGLE),
            );
            if toggle.changed() {
                self.request(enabled, out);
            }
            let status = local_only_status(enabled);
            let tone = if enabled {
                Tone::Success
            } else {
                Tone::Warning
            };
            section::status_badge(ui, tokens, tone, status.glyph, status.label);
        });

        // Only after the gate has answered. `enforced_policy_lines` returns an
        // empty vector until then, which is what keeps the claim honest.
        for line in enforced_policy_lines(data.enforced_policy.as_ref()) {
            section::note(ui, tokens, &line);
        }

        if let Some(envelope) = &self.error {
            section::status_badge(ui, tokens, Tone::Danger, FAILURE_GLYPH, &envelope.message);
            if section::primary_button(ui, tokens, RETRY).clicked() {
                let retry = self.attempted.unwrap_or(!enabled);
                self.request(retry, out);
            }
        }

        ui.separator();
        self.voice.ui(ui, tokens, data, out);
    }

    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
        match command {
            "set_local_only_mode" => {
                self.pending = false;
                match result {
                    Ok(_) => {
                        self.error = None;
                        // Which engines are usable depends on the egress
                        // policy, so the capability snapshot is stale the
                        // moment it changes.
                        out.push(SettingsIntent::RefreshCapabilities);
                    }
                    Err(envelope) => self.error = Some(envelope.clone()),
                }
            }
            "set_voice_profiles_enabled" | "forget_all_voice_profiles" => {
                self.voice.on_response(command, result, out);
            }
            _ => {}
        }
    }
}

/// The transient state of the voice-profile block (VP-090).
///
/// Everything durable lives in the DTO the application answers with; what is
/// kept here is what the user is in the middle of doing: an open disclosure,
/// an in-flight toggle, the forget-everyone disclosure and the phrase typed
/// into its confirmation field.
#[derive(Debug, Default)]
struct VoiceProfiles {
    /// Whether the activation disclosure is on screen.
    disclosure_open: bool,
    /// Whether a toggle change is in flight.
    pending: bool,
    /// The last rejected toggle change.
    error: Option<ErrorEnvelope>,
    /// The `apply: false` answer: what an erasure *would* destroy.
    forget_disclosure: Option<ForgetAllVoiceProfilesResponse>,
    /// The `apply: true` answer: what it did destroy.
    forget_receipt: Option<ForgetAllVoiceProfilesResponse>,
    /// What the user has typed into the confirmation field.
    forget_phrase: String,
    /// Whether a forget-everyone call is in flight.
    forget_pending: bool,
    /// The last rejected forget-everyone call.
    forget_error: Option<ErrorEnvelope>,
}

impl VoiceProfiles {
    /// Raise the toggle change.
    ///
    /// `version` is `Some` only when the user has just accepted the
    /// disclosure; turning the feature off never carries one.
    fn set_enabled(&mut self, enabled: bool, version: Option<String>, out: &mut SettingsOutput) {
        self.pending = true;
        self.disclosure_open = false;
        out.command(AppCommand::SetVoiceProfilesEnabled(
            SetVoiceProfilesEnabledRequest {
                enabled,
                accept_disclosure_version: version,
            },
        ));
    }

    /// Raise a forget-everyone call: `apply: false` discloses, `true` erases.
    fn forget(&mut self, apply: bool, out: &mut SettingsOutput) {
        self.forget_pending = true;
        out.command(AppCommand::ForgetAllVoiceProfiles(
            ForgetAllVoiceProfilesRequest { apply },
        ));
    }

    /// Draw the block.
    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        ui.label(RichText::new(VOICE_HEADING).strong().color(tokens.text()));
        section::prose(ui, tokens, VOICE_PROSE);
        section::note(ui, tokens, ROSTER_SENTENCE);

        let Some(dto) = data.voice_profiles.as_ref() else {
            // Unticked, and inoperable: a toggle that cannot know what it
            // would be changing must not pretend to change it.
            let mut off = false;
            ui.add_enabled(false, egui::Checkbox::new(&mut off, VOICE_TOGGLE));
            section::note(ui, tokens, VOICE_UNREAD);
            people_link(ui, tokens, out);
            return;
        };

        capability_badges(ui, tokens, dto);
        self.toggle(ui, tokens, dto, out);
        self.consent_state(ui, tokens, dto);
        section::facts(ui, tokens, &consent_facts(dto));
        people_link(ui, tokens, out);
        self.forget_everyone(ui, tokens, out);

        if let Some(envelope) = &self.error {
            section::status_badge(ui, tokens, Tone::Danger, FAILURE_GLYPH, &envelope.message);
            if section::primary_button(ui, tokens, RETRY_VOICE).clicked() {
                self.disclosure_open = true;
            }
        }

        if self.disclosure_open {
            match disclosure_modal(ui, tokens, &dto.disclosure) {
                Confirmation::Confirmed => {
                    self.set_enabled(true, Some(dto.disclosure.version.clone()), out);
                }
                Confirmation::Cancelled => self.disclosure_open = false,
                Confirmation::Pending => {}
            }
        }
    }

    /// The master toggle and, when it is inoperable, why.
    fn toggle(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        dto: &VoiceProfileSettingsDto,
        out: &mut SettingsOutput,
    ) {
        // A copy: the checkbox is driven by the DTO every frame, so a click
        // that opens the disclosure leaves nothing ticked behind it.
        let mut enabled = dto.enabled;
        let operable = dto.key_available && !self.pending;
        let toggle = ui.add_enabled(operable, egui::Checkbox::new(&mut enabled, VOICE_TOGGLE));
        if toggle.changed() {
            if enabled {
                self.disclosure_open = true;
            } else {
                self.set_enabled(false, None, out);
            }
        }
        if !dto.key_available {
            section::note(ui, tokens, KEY_LOCKED_REASON);
        }
    }

    /// The re-prompt VP-121 requires when the disclosure has to be accepted
    /// again, with the backend's reason code said in words.
    fn consent_state(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        dto: &VoiceProfileSettingsDto,
    ) {
        if dto.consent.state != "disclosure_required" {
            return;
        }
        section::status_badge(
            ui,
            tokens,
            Tone::Warning,
            WARNING_GLYPH,
            DISCLOSURE_REQUIRED,
        );
        section::note(ui, tokens, reason_words(dto.consent.reason.as_deref()));
        let review = ui
            .add_enabled_ui(dto.key_available, |ui| {
                section::primary_button(ui, tokens, REVIEW_DISCLOSURE)
            })
            .inner;
        if review.clicked() {
            self.disclosure_open = true;
        }
    }

    /// The forget-everyone action: disclosure, typed confirmation, receipt.
    fn forget_everyone(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        out: &mut SettingsOutput,
    ) {
        if self.forget_disclosure.is_none() {
            let start = ui
                .add_enabled_ui(!self.forget_pending, |ui| {
                    section::danger_button(ui, tokens, FORGET_BUTTON)
                })
                .inner;
            if start.clicked() {
                self.forget_receipt = None;
                self.forget_error = None;
                self.forget_phrase.clear();
                self.forget(false, out);
            }
        }

        if let Some(disclosure) = self.forget_disclosure.clone() {
            section::facts(ui, tokens, &forget_facts(&disclosure));
            section::note(ui, tokens, FORGET_ROSTER_SURVIVES);
            for limit in &disclosure.limits {
                section::note(ui, tokens, limit);
            }

            let label = ui.label(FORGET_FIELD);
            ui.add(
                egui::TextEdit::singleline(&mut self.forget_phrase)
                    .hint_text(FORGET_PHRASE)
                    .desired_width(220.0),
            )
            .labelled_by(label.id);

            let typed = self
                .forget_phrase
                .trim()
                .eq_ignore_ascii_case(FORGET_PHRASE);
            ui.horizontal_wrapped(|ui| {
                let confirm = ui
                    .add_enabled_ui(typed && !self.forget_pending, |ui| {
                        section::danger_button(ui, tokens, FORGET_CONFIRM)
                    })
                    .inner;
                if confirm.clicked() {
                    self.forget(true, out);
                }
                if ui.button(FORGET_CANCEL).clicked() {
                    self.forget_disclosure = None;
                    self.forget_phrase.clear();
                }
            });
        }

        if let Some(receipt) = self.forget_receipt.clone() {
            section::facts(ui, tokens, &forget_facts(&receipt));
            section::note(ui, tokens, key_outcome(&receipt));
            if receipt.feature_disabled {
                section::note(ui, tokens, "Voice matching is now off.");
            }
            section::note(ui, tokens, FORGET_ROSTER_SURVIVES);
            for limit in &receipt.limits {
                section::note(ui, tokens, limit);
            }
        }

        if let Some(envelope) = &self.forget_error {
            section::status_badge(ui, tokens, Tone::Danger, FAILURE_GLYPH, &envelope.message);
            if section::danger_button(ui, tokens, FORGET_BUTTON).clicked() {
                self.forget(false, out);
            }
        }
    }

    /// Fold an answer to one of the block's own commands in.
    fn on_response(
        &mut self,
        command: &'static str,
        result: &Result<AppResponse, ErrorEnvelope>,
        out: &mut SettingsOutput,
    ) {
        match command {
            "set_voice_profiles_enabled" => {
                self.pending = false;
                match result {
                    Ok(_) => self.error = None,
                    Err(envelope) => self.error = Some(envelope.clone()),
                }
            }
            "forget_all_voice_profiles" => {
                self.forget_pending = false;
                match result {
                    Ok(AppResponse::ForgetAllVoiceProfiles(response)) => {
                        self.forget_error = None;
                        if response.applied {
                            self.forget_disclosure = None;
                            self.forget_phrase.clear();
                            self.forget_receipt = Some(response.clone());
                            // The erasure turns the feature off and empties
                            // every count, so the cached settings are stale.
                            out.command(AppCommand::GetVoiceProfileSettings(EmptyRequest {}));
                        } else {
                            self.forget_disclosure = Some(response.clone());
                        }
                    }
                    Ok(_) => {}
                    Err(envelope) => self.forget_error = Some(envelope.clone()),
                }
            }
            _ => {}
        }
    }
}

/// The two capability statements, each a glyph and a word.
fn capability_badges(ui: &mut Ui, tokens: &dyn SettingsTokens, dto: &VoiceProfileSettingsDto) {
    if !dto.key_available {
        section::status_badge(ui, tokens, Tone::Warning, WARNING_GLYPH, KEY_LOCKED);
    }
    if !dto.embedder_available {
        section::status_badge(ui, tokens, Tone::Neutral, NEUTRAL_GLYPH, NO_EMBEDDER);
        section::note(ui, tokens, NO_EMBEDDER_REASON);
    }
}

/// The way to the People screen, which works either way (VP-011).
fn people_link(ui: &mut Ui, tokens: &dyn SettingsTokens, out: &mut SettingsOutput) {
    if section::primary_button(ui, tokens, OPEN_PEOPLE).clicked() {
        out.push(SettingsIntent::Navigate(SettingsDestination::People));
    }
}

/// The consent-gate reason code, said in words.
///
/// The code is an identifier, not a sentence: the application names *why* the
/// gate is closed and the interface is what says it out loud.
fn reason_words(reason: Option<&str>) -> &'static str {
    match reason {
        Some("never_accepted") => {
            "You have not accepted the activation disclosure yet, so matching cannot be turned on."
        }
        Some("version_changed") => {
            "The disclosure has changed since you accepted it, so it needs accepting again before \
matching can run."
        }
        Some("restored_library") => {
            "This library was restored from a backup, so the disclosure needs accepting again on \
this device."
        }
        _ => "The activation disclosure needs accepting before matching can run.",
    }
}

/// The consent summary, the counts and the retention default.
fn consent_facts(dto: &VoiceProfileSettingsDto) -> Vec<Fact> {
    let accepted_version = dto
        .consent
        .accepted_version
        .clone()
        .unwrap_or_else(|| "none accepted".to_owned());
    let accepted_at = dto
        .consent
        .accepted_at
        .map_or_else(|| "never".to_owned(), |at| at.to_string());
    vec![
        Fact {
            label: "People with a voiceprint",
            value: dto.enrolled_people.to_string(),
        },
        Fact {
            label: "Stored exemplars",
            value: dto.exemplars.to_string(),
        },
        Fact {
            label: "Pending suggestions",
            value: dto.pending_suggestions.to_string(),
        },
        Fact {
            label: "People in the list",
            value: dto.roster_people.to_string(),
        },
        Fact {
            label: "Retention default",
            value: format!("{} months", dto.disclosure.retention_default_months),
        },
        Fact {
            label: "Disclosure accepted",
            value: accepted_version,
        },
        Fact {
            label: "Accepted at",
            value: accepted_at,
        },
    ]
}

/// The counts an erasure would destroy, or did.
fn forget_facts(response: &ForgetAllVoiceProfilesResponse) -> Vec<Fact> {
    vec![
        Fact {
            label: "Enrolments",
            value: response.enrollments.to_string(),
        },
        Fact {
            label: "Exemplars",
            value: response.exemplars.to_string(),
        },
        Fact {
            label: "Suggestions",
            value: response.suggestions.to_string(),
        },
        Fact {
            label: "People kept",
            value: response.people_kept.to_string(),
        },
        Fact {
            label: "Encryption key",
            value: if response.key_present {
                "present".to_owned()
            } else {
                "none stored".to_owned()
            },
        },
    ]
}

/// What became of the data key, said from the response rather than assumed.
fn key_outcome(response: &ForgetAllVoiceProfilesResponse) -> &'static str {
    if response.key_destroyed {
        "The voice-profile encryption key is gone from the operating system's credential store, so \
every stored voiceprint — including any inside an encrypted backup — is unreadable."
    } else if response.key_present {
        "The encryption key is still in the credential store; it was not destroyed."
    } else {
        "There was no encryption key to destroy."
    }
}

/// The activation disclosure, rendered from the DTO and nothing else.
///
/// Only [`Confirmation::Confirmed`] may turn the feature on; Escape and the
/// cancel control both leave without sending anything (VP-120, VP-121).
fn disclosure_modal(
    ui: &mut Ui,
    tokens: &dyn SettingsTokens,
    disclosure: &ActivationDisclosureDto,
) -> Confirmation {
    let mut outcome = Confirmation::Pending;
    let modal = egui::Modal::new(egui::Id::new("voice-profile-activation-disclosure")).show(
        ui.ctx(),
        |ui| {
            ui.set_min_width(420.0);
            ui.label(
                RichText::new(&disclosure.title)
                    .heading()
                    .strong()
                    .color(tokens.text()),
            );
            ui.add_space(8.0);
            for paragraph in &disclosure.body {
                section::prose(ui, tokens, paragraph);
                ui.add_space(4.0);
            }
            section::prose(ui, tokens, &disclosure.two_tier_sentence);
            ui.add_space(4.0);
            section::prose(ui, tokens, &disclosure.responsibility_sentence);
            ui.add_space(8.0);
            section::fact_row(
                ui,
                tokens,
                "Retention default",
                &format!("{} months", disclosure.retention_default_months),
            );
            ui.add_space(12.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if section::primary_button(ui, tokens, ACCEPT_DISCLOSURE).clicked() {
                    outcome = Confirmation::Confirmed;
                }
                if ui.button("Cancel").clicked() {
                    outcome = Confirmation::Cancelled;
                }
            });
        },
    );
    // Escape, the backdrop and the cancel control all leave without consent.
    if modal.should_close() {
        outcome = Confirmation::Cancelled;
    }
    outcome
}
