//! The biometric tier: the consent gate, enrolment, suggestions and erasure
//! (`VOICE_PROFILES_SPEC` §8, §12.2, VP-003, VP-085, VP-094, VP-095,
//! VP-120..VP-123, VP-216; ADR-0080, ADR-0082).
//!
//! Thirteen commands, deliberately separate from
//! [`crate::commands::people`]. The split is the §12.2A boundary made
//! structural: a roster command cannot reach biometric code because it never
//! obtains a
//! [`ConsentProof`](plaudon_app_core::voice_profiles::ConsentProof), and the
//! three commands that lead to an embedding — `enroll_person_voice`,
//! `add_voice_exemplar`, `start_speaker_matching` — all pass through the gate
//! before any audio is read (VP-011, VP-215).
//!
//! Four commands in this module are *not* gated, and that is also deliberate.
//! `delete_voice_enrollment` and `forget_all_voice_profiles` destroy biometric
//! data: a user whose disclosure went stale, or who already turned the feature
//! off, must still be able to delete what was collected (VP-123, VP-095), and
//! gating deletion behind consent would be the opposite of the right answer.
//! `list_voice_exemplars` is how they see what there is to destroy or split,
//! and `get_cluster_speech` is the meter read *while* the enrolment gate is
//! still open — it measures stored diarization rows, so there is no biometric
//! work on its path to authorise.

use plaudon_app_core::voice_profiles::{EnrolmentAttestation, ACTIVATION_DISCLOSURE};
use plaudon_domain::error::AppError;
use plaudon_domain::ids::{PersonId, RecordingId, SpeakerSuggestionId, VoiceExemplarId};

use crate::backend::voice_profiles::{SUGGESTION_PAGE_DEFAULT, SUGGESTION_PAGE_MAX};
use crate::dto::{
    AddVoiceExemplarRequest, ClusterSpeechDto, ConfirmSpeakerSuggestionRequest,
    DeleteVoiceEnrollmentRequest, DeleteVoiceEnrollmentResponse, EmptyRequest,
    EnrollPersonVoiceRequest, EnrolmentConsentDto, ForgetAllVoiceProfilesRequest,
    ForgetAllVoiceProfilesResponse, GetClusterSpeechRequest, ListSpeakerSuggestionsRequest,
    ListSpeakerSuggestionsResponse, ListVoiceExemplarsRequest, ListVoiceExemplarsResponse,
    RejectSpeakerSuggestionRequest, SetVoiceProfilesEnabledRequest, SpeakerSuggestionDecisionDto,
    SplitVoiceEnrollmentRequest, SplitVoiceEnrollmentResponse, StartSpeakerMatchingRequest,
    StartSpeakerMatchingResponse, VoiceEnrollmentDto, VoiceProfileSettingsDto,
};
use crate::error::invalid_argument;
use crate::ports::{EnrolmentRequest, SplitRequest, SplitTarget};
use crate::validate::parse_id;
use crate::AppPorts;

/// Longest cluster key the command layer accepts.
///
/// A cluster key is provider-controlled text that arrives back from the UI;
/// it is matched against stored rows and never interpolated anywhere, so the
/// only rule is a bound.
const MAX_CLUSTER_KEY_CHARS: usize = 200;

/// Largest selection one `start_speaker_matching` may queue.
///
/// Bulk review is an explicit, scoped action (VP-007); a bound keeps one
/// click from queueing a library.
pub const MAX_MATCH_SELECTION: usize = 500;

/// Largest number of exemplars one split may move.
const MAX_SPLIT_EXEMPLARS: usize = 256;

/// Validate a cluster key.
fn cluster_key(value: &str) -> Result<String, AppError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(invalid_argument("cluster_key must not be empty"));
    }
    if trimmed.chars().count() > MAX_CLUSTER_KEY_CHARS {
        return Err(invalid_argument(format!(
            "cluster_key is at most {MAX_CLUSTER_KEY_CHARS} characters"
        )));
    }
    Ok(trimmed.to_owned())
}

/// Turn the wire attestation into the validated one (VP-120 level 3).
///
/// The retention term and the note are checked by
/// [`EnrolmentAttestation::validate`] against the gate at the moment of use;
/// what happens here is the shape check the wire needs, so a malformed
/// request is an `INVALID_ARGUMENT` rather than a consent refusal.
///
/// Crate-visible because the processing suite drives the same conversion the
/// command does rather than hand-building an attestation the wire could not
/// produce.
pub(crate) fn attestation(consent: &EnrolmentConsentDto) -> Result<EnrolmentAttestation, AppError> {
    if !consent.person_attested {
        return Err(invalid_argument(
            "an enrolment needs the attestation that this person has agreed",
        ));
    }
    Ok(EnrolmentAttestation {
        disclosure_version: consent.disclosure_version.clone(),
        person_attested: consent.person_attested,
        retention_months: consent
            .retention_months
            .unwrap_or(ACTIVATION_DISCLOSURE.retention_default_months),
        consent_note: consent
            .consent_note
            .as_deref()
            .map(str::trim)
            .filter(|note| !note.is_empty())
            .map(str::to_owned),
    })
}

/// The feature's state: the toggle, the gate, the disclosure and the counts
/// the Settings → Privacy block shows (VP-090).
pub async fn get_voice_profile_settings(
    ports: &AppPorts,
    _request: EmptyRequest,
) -> Result<VoiceProfileSettingsDto, AppError> {
    ports.voice_profiles.voice_profile_settings().await
}

/// Turn biometric matching on by accepting the current disclosure, or off
/// (VP-003, VP-120, VP-121).
///
/// Turning it on *is* passing the activation gate: an enable without the
/// current disclosure version is refused, so a stale or absent acceptance can
/// never arm the feature.
pub async fn set_voice_profiles_enabled(
    ports: &AppPorts,
    request: SetVoiceProfilesEnabledRequest,
) -> Result<VoiceProfileSettingsDto, AppError> {
    ports
        .voice_profiles
        .set_voice_profiles_enabled(
            request.enabled,
            request.accept_disclosure_version.as_deref(),
        )
        .await
}

/// Attach an enrolment to a roster person from one recording's cluster
/// (VP-094).
pub async fn enroll_person_voice(
    ports: &AppPorts,
    request: EnrollPersonVoiceRequest,
) -> Result<VoiceEnrollmentDto, AppError> {
    let person_id: PersonId = parse_id("person_id", &request.person_id)?;
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let cluster = cluster_key(&request.cluster_key)?;
    let attestation = attestation(&request.consent)?;
    ports
        .voice_profiles
        .enroll_person_voice(&EnrolmentRequest {
            person_id,
            recording_id,
            cluster_key: cluster,
            attestation: Some(attestation),
        })
        .await
}

/// Extend an enrolment from another recording's cluster, which is what
/// improves cross-device robustness (§6.5, VP-064).
pub async fn add_voice_exemplar(
    ports: &AppPorts,
    request: AddVoiceExemplarRequest,
) -> Result<VoiceEnrollmentDto, AppError> {
    let person_id: PersonId = parse_id("person_id", &request.person_id)?;
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let cluster = cluster_key(&request.cluster_key)?;
    ports
        .voice_profiles
        .add_voice_exemplar(&EnrolmentRequest {
            person_id,
            recording_id,
            cluster_key: cluster,
            attestation: None,
        })
        .await
}

/// Split an enrolment's exemplars by provenance onto another person (§3.5).
///
/// The target's attestation is required, not inherited: the speech turned out
/// to be somebody else's, so somebody else has to have agreed.
pub async fn split_voice_enrollment(
    ports: &AppPorts,
    request: SplitVoiceEnrollmentRequest,
) -> Result<SplitVoiceEnrollmentResponse, AppError> {
    let person_id: PersonId = parse_id("person_id", &request.person_id)?;
    if request.exemplar_ids.is_empty() {
        return Err(invalid_argument("a split moves at least one exemplar"));
    }
    if request.exemplar_ids.len() > MAX_SPLIT_EXEMPLARS {
        return Err(invalid_argument(format!(
            "a split moves at most {MAX_SPLIT_EXEMPLARS} exemplars"
        )));
    }
    let mut exemplar_ids = Vec::with_capacity(request.exemplar_ids.len());
    for raw in &request.exemplar_ids {
        let id: VoiceExemplarId = parse_id("exemplar_ids", raw)?;
        if !exemplar_ids.contains(&id) {
            exemplar_ids.push(id);
        }
    }

    let target = match (
        request.target_person_id.as_deref(),
        request
            .new_person_display_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty()),
    ) {
        (Some(_), Some(_)) | (None, None) => {
            return Err(invalid_argument(
                "a split names either an existing person or a new display name",
            ))
        }
        (Some(raw), None) => SplitTarget::Existing(parse_id("target_person_id", raw)?),
        (None, Some(name)) => SplitTarget::New(name.to_owned()),
    };
    if target == SplitTarget::Existing(person_id) {
        return Err(invalid_argument(
            "a split moves exemplars to another person",
        ));
    }
    let attestation = attestation(&request.consent)?;

    ports
        .voice_profiles
        .split_voice_enrollment(&SplitRequest {
            person_id,
            exemplar_ids,
            target,
            attestation,
        })
        .await
}

/// What one enrolment is made of: exemplar ids, provenance and durations
/// (VP-091, §3.5).
///
/// Ungated for the same reason deletion is: a user whose disclosure went
/// stale, or who turned the feature off, must still be able to see what was
/// collected and split or destroy it. The response carries no vector — the
/// embedding stays sealed in the store (VP-085).
pub async fn list_voice_exemplars(
    ports: &AppPorts,
    request: ListVoiceExemplarsRequest,
) -> Result<ListVoiceExemplarsResponse, AppError> {
    let person_id: PersonId = parse_id("person_id", &request.person_id)?;
    ports.voice_profiles.list_voice_exemplars(person_id).await
}

/// How much clean speech one cluster offers, for the enrolment meter
/// (VP-094).
///
/// Ungated, because the meter is read *while* the user is deciding whether to
/// enrol: it answers from the stored reconciliation, opens no audio and calls
/// no model, so nothing biometric exists on this path to gate.
pub async fn get_cluster_speech(
    ports: &AppPorts,
    request: GetClusterSpeechRequest,
) -> Result<ClusterSpeechDto, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let cluster_key = cluster_key(&request.cluster_key)?;
    ports
        .voice_profiles
        .cluster_speech(recording_id, &cluster_key)
        .await
}

/// Delete one person's voiceprint, or disclose what that destroys (§12.4,
/// VP-123).
///
/// The roster entry stays as a name-only person, and every recording keeps the
/// label it already showed.
pub async fn delete_voice_enrollment(
    ports: &AppPorts,
    request: DeleteVoiceEnrollmentRequest,
) -> Result<DeleteVoiceEnrollmentResponse, AppError> {
    let person_id: PersonId = parse_id("person_id", &request.person_id)?;
    ports
        .voice_profiles
        .delete_voice_enrollment(person_id, request.apply, request.reason.as_deref())
        .await
}

/// "Forget everyone": destroy every enrolment, exemplar, suggestion and the
/// key, or disclose what that would reach (VP-095).
///
/// The roster and every assignment survive — they are names, not voiceprints.
pub async fn forget_all_voice_profiles(
    ports: &AppPorts,
    request: ForgetAllVoiceProfilesRequest,
) -> Result<ForgetAllVoiceProfilesResponse, AppError> {
    ports
        .voice_profiles
        .forget_all_voice_profiles(request.apply)
        .await
}

/// Pending suggestions for one recording, or the global review queue
/// (VP-092, VP-093).
pub async fn list_speaker_suggestions(
    ports: &AppPorts,
    request: ListSpeakerSuggestionsRequest,
) -> Result<ListSpeakerSuggestionsResponse, AppError> {
    let recording_id: Option<RecordingId> = request
        .recording_id
        .as_deref()
        .map(|value| parse_id("recording_id", value))
        .transpose()?;
    let limit = match request.limit {
        None => SUGGESTION_PAGE_DEFAULT,
        Some(value) if (1..=SUGGESTION_PAGE_MAX).contains(&value) => value,
        Some(_) => {
            return Err(invalid_argument(format!(
                "limit is between 1 and {SUGGESTION_PAGE_MAX}"
            )))
        }
    };
    ports
        .voice_profiles
        .list_speaker_suggestions(recording_id, limit)
        .await
}

/// Confirm a suggestion: the person is assigned through the ordinary
/// assignment path, and the cluster's segments with them (VP-032).
pub async fn confirm_speaker_suggestion(
    ports: &AppPorts,
    request: ConfirmSpeakerSuggestionRequest,
) -> Result<SpeakerSuggestionDecisionDto, AppError> {
    let suggestion_id: SpeakerSuggestionId = parse_id("suggestion_id", &request.suggestion_id)?;
    ports
        .voice_profiles
        .confirm_speaker_suggestion(suggestion_id, request.overwrite_segment_exceptions)
        .await
}

/// Reject a suggestion, optionally naming who it actually was (VP-216).
///
/// Without a corrected person nothing is renamed and the score is kept as
/// negative evidence for this enrolment's calibration. With one, that person
/// is assigned and the suggestion is marked `corrected` — and the negative
/// evidence is still a *score record*, never the rejected cluster's
/// embedding, which would be a voiceprint of whoever actually spoke.
pub async fn reject_speaker_suggestion(
    ports: &AppPorts,
    request: RejectSpeakerSuggestionRequest,
) -> Result<SpeakerSuggestionDecisionDto, AppError> {
    let suggestion_id: SpeakerSuggestionId = parse_id("suggestion_id", &request.suggestion_id)?;
    let corrected: Option<PersonId> = request
        .corrected_person_id
        .as_deref()
        .map(|value| parse_id("corrected_person_id", value))
        .transpose()?;
    ports
        .voice_profiles
        .reject_speaker_suggestion(
            suggestion_id,
            corrected,
            request.overwrite_segment_exceptions,
        )
        .await
}

/// Queue the §10 match job for an explicit selection of recordings (VP-007).
pub async fn start_speaker_matching(
    ports: &AppPorts,
    request: StartSpeakerMatchingRequest,
) -> Result<StartSpeakerMatchingResponse, AppError> {
    if request.recording_ids.is_empty() {
        return Err(invalid_argument("name at least one recording to match"));
    }
    if request.recording_ids.len() > MAX_MATCH_SELECTION {
        return Err(invalid_argument(format!(
            "at most {MAX_MATCH_SELECTION} recordings may be queued at once"
        )));
    }
    let mut recording_ids = Vec::with_capacity(request.recording_ids.len());
    for raw in &request.recording_ids {
        let id: RecordingId = parse_id("recording_ids", raw)?;
        if !recording_ids.contains(&id) {
            recording_ids.push(id);
        }
    }
    ports
        .voice_profiles
        .start_speaker_matching(&recording_ids)
        .await
}

#[cfg(test)]
mod tests {
    use super::{attestation, cluster_key, MAX_CLUSTER_KEY_CHARS};
    use crate::dto::EnrolmentConsentDto;
    use plaudon_app_core::voice_profiles::ACTIVATION_DISCLOSURE;

    fn consent() -> EnrolmentConsentDto {
        EnrolmentConsentDto {
            disclosure_version: ACTIVATION_DISCLOSURE.version.to_owned(),
            person_attested: true,
            retention_months: None,
            consent_note: None,
        }
    }

    #[test]
    fn an_unattested_consent_block_is_an_invalid_argument() {
        let refused = attestation(&EnrolmentConsentDto {
            person_attested: false,
            ..consent()
        })
        .expect_err("an unattested enrolment is refused at the boundary");
        assert_eq!(refused.code().as_str(), "INVALID_ARGUMENT");
    }

    #[test]
    fn an_absent_retention_term_becomes_the_disclosed_default() {
        let built = attestation(&consent()).expect("valid");
        assert_eq!(
            built.retention_months,
            ACTIVATION_DISCLOSURE.retention_default_months
        );
        assert_eq!(built.consent_note, None);
    }

    #[test]
    fn a_blank_consent_note_is_no_note() {
        let built = attestation(&EnrolmentConsentDto {
            consent_note: Some("   ".to_owned()),
            ..consent()
        })
        .expect("valid");
        assert_eq!(built.consent_note, None);
    }

    #[test]
    fn a_cluster_key_is_trimmed_and_bounded() {
        assert_eq!(cluster_key("  S1 ").expect("valid"), "S1");
        assert!(cluster_key("   ").is_err());
        assert!(cluster_key(&"c".repeat(MAX_CLUSTER_KEY_CHARS + 1)).is_err());
    }
}
