//! The people roster: the ungated master speaker list
//! (`VOICE_PROFILES_SPEC` §8, §9A, VP-010, VP-011, VP-210..VP-214, §12.2A).
//!
//! These six commands are a separate module from
//! [`crate::commands::voice_profiles`] because they are a separate *tier*: a
//! roster entry is a typed name with optional plain contacts, which is
//! ordinary personal data, and none of the biometric definitions in §12.1
//! reaches it. So nothing here reads the consent gate, asks for the
//! voice-profile key or can call an embedder — the whole module works in a
//! library where biometric matching was never turned on, which is exactly
//! what VP-011's manual-only mode is.
//!
//! The split is also the reason the *commands* are divided this way rather
//! than by object: `delete_person` destroys a voiceprint as a consequence, but
//! it is a roster action a manual-only user must be able to take, so it lives
//! here and discloses what it reaches.

use plaudon_domain::error::AppError;
use plaudon_domain::ids::{PersonId, RecordingId, SpeakerId};
use plaudon_persistence::people::{PersonDraft, MAX_DISPLAY_NAME_CHARS};

use crate::dto::{
    AssignClusterPersonRequest, AssignClusterPersonResponse, CreatePersonRequest,
    DeletePersonRequest, DeletePersonResponse, ListPeopleRequest, ListPeopleResponse,
    MergePeopleRequest, MergePeopleResponse, PersonResponse, UpdatePersonRequest,
};
use crate::error::invalid_argument;
use crate::ports::ClusterAssignment;
use crate::validate::parse_id;
use crate::AppPorts;

/// Longest search string the roster filter accepts.
const MAX_QUERY_CHARS: usize = 200;

/// Largest number of aliases one entry may carry.
const MAX_ALIASES: usize = 32;

/// Longest alias, contact address and organisation the schema accepts.
const MAX_ALIAS_CHARS: usize = 120;
const MAX_EMAIL_CHARS: usize = 254;
const MAX_ORGANISATION_CHARS: usize = 200;

/// Validate the editable fields of a roster entry.
///
/// Trimmed, bounded and never empty-by-accident: an empty optional field
/// arrives as `None` rather than as `Some("")`, so "cleared the email" and
/// "typed one space" are the same stored state.
fn draft(
    display_name: &str,
    aliases: &[String],
    email: Option<&str>,
    organisation: Option<&str>,
) -> Result<PersonDraft, AppError> {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return Err(invalid_argument("display_name must not be empty"));
    }
    if display_name.chars().count() > MAX_DISPLAY_NAME_CHARS {
        return Err(invalid_argument(format!(
            "display_name is at most {MAX_DISPLAY_NAME_CHARS} characters"
        )));
    }
    if aliases.len() > MAX_ALIASES {
        return Err(invalid_argument(format!(
            "a person carries at most {MAX_ALIASES} aliases"
        )));
    }
    let mut cleaned = Vec::with_capacity(aliases.len());
    for alias in aliases {
        let alias = alias.trim();
        if alias.is_empty() {
            continue;
        }
        if alias.chars().count() > MAX_ALIAS_CHARS {
            return Err(invalid_argument(format!(
                "an alias is at most {MAX_ALIAS_CHARS} characters"
            )));
        }
        if !cleaned.iter().any(|existing: &String| existing == alias) {
            cleaned.push(alias.to_owned());
        }
    }

    let optional = |value: Option<&str>, limit: usize, field: &'static str| {
        let trimmed = value.map(str::trim).filter(|value| !value.is_empty());
        match trimmed {
            Some(value) if value.chars().count() > limit => Err(invalid_argument(format!(
                "{field} is at most {limit} characters"
            ))),
            other => Ok(other.map(str::to_owned)),
        }
    };

    Ok(PersonDraft {
        display_name: display_name.to_owned(),
        aliases: cleaned,
        email: optional(email, MAX_EMAIL_CHARS, "email")?,
        organisation: optional(organisation, MAX_ORGANISATION_CHARS, "organisation")?,
    })
}

/// The master speaker list, with the counts the People screen shows (VP-091).
pub async fn list_people(
    ports: &AppPorts,
    request: ListPeopleRequest,
) -> Result<ListPeopleResponse, AppError> {
    if let Some(query) = request.query.as_deref() {
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(invalid_argument(format!(
                "query is at most {MAX_QUERY_CHARS} characters"
            )));
        }
    }
    ports.people.list_people(request.query.as_deref()).await
}

/// Add a roster entry (VP-210's "Create new person…").
pub async fn create_person(
    ports: &AppPorts,
    request: CreatePersonRequest,
) -> Result<PersonResponse, AppError> {
    let draft = draft(
        &request.display_name,
        &request.aliases,
        request.email.as_deref(),
        request.organisation.as_deref(),
    )?;
    Ok(PersonResponse {
        person: ports.people.create_person(&draft).await?,
    })
}

/// Edit a roster entry's name, aliases or plain contacts.
///
/// A rename propagates by reference — every recording shows the new name
/// because display joins through the person — and changes no assignment
/// (VP-213a). `recordings` on the response is the blast radius the dialog
/// quotes.
pub async fn update_person(
    ports: &AppPorts,
    request: UpdatePersonRequest,
) -> Result<PersonResponse, AppError> {
    let person_id: PersonId = parse_id("person_id", &request.person_id)?;
    let draft = draft(
        &request.display_name,
        &request.aliases,
        request.email.as_deref(),
        request.organisation.as_deref(),
    )?;
    Ok(PersonResponse {
        person: ports.people.update_person(person_id, &draft).await?,
    })
}

/// Merge two entries, or disclose what that would move (VP-213c).
///
/// `apply: false` is the disclosure the user confirms: affected recordings,
/// exemplars and pending suggestions, counted from the same tables the merge
/// then moves.
pub async fn merge_people(
    ports: &AppPorts,
    request: MergePeopleRequest,
) -> Result<MergePeopleResponse, AppError> {
    let loser: PersonId = parse_id("loser_person_id", &request.loser_person_id)?;
    let survivor: PersonId = parse_id("survivor_person_id", &request.survivor_person_id)?;
    if loser == survivor {
        return Err(invalid_argument(
            "a person cannot be merged into themselves",
        ));
    }
    ports
        .people
        .merge_people(loser, survivor, request.apply)
        .await
}

/// Delete a roster entry, or disclose what that would reach (VP-080).
///
/// Links are severed and each recording keeps the name it already displayed;
/// transcript text is never touched.
pub async fn delete_person(
    ports: &AppPorts,
    request: DeletePersonRequest,
) -> Result<DeletePersonResponse, AppError> {
    let person_id: PersonId = parse_id("person_id", &request.person_id)?;
    ports.people.delete_person(person_id, request.apply).await
}

/// Assign one cluster of one recording to a person, or clear it (VP-211).
///
/// The scope is exactly what the control names: this cluster, this recording.
/// No other recording's assignments, suggestions or exemplars are touched
/// (VP-213b).
pub async fn assign_cluster_person(
    ports: &AppPorts,
    request: AssignClusterPersonRequest,
) -> Result<AssignClusterPersonResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let speaker_id: SpeakerId = parse_id("speaker_id", &request.speaker_id)?;
    let person_id: Option<PersonId> = request
        .person_id
        .as_deref()
        .map(|value| parse_id("person_id", value))
        .transpose()?;
    ports
        .people
        .assign_cluster_person(&ClusterAssignment {
            recording_id,
            speaker_id,
            person_id,
            overwrite_segment_exceptions: request.overwrite_segment_exceptions,
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::draft;

    #[test]
    fn a_draft_is_trimmed_bounded_and_deduplicated() {
        let built = draft(
            "  Priya  ",
            &[
                "  P. Raman ".to_owned(),
                "P. Raman".to_owned(),
                "   ".to_owned(),
            ],
            Some("  "),
            Some(" Acme "),
        )
        .expect("the draft is valid");
        assert_eq!(built.display_name, "Priya");
        assert_eq!(built.aliases, vec!["P. Raman".to_owned()]);
        assert_eq!(built.email, None, "a blank optional field clears it");
        assert_eq!(built.organisation.as_deref(), Some("Acme"));
    }

    #[test]
    fn an_empty_or_over_long_name_is_refused() {
        assert!(draft("   ", &[], None, None).is_err());
        assert!(draft(&"p".repeat(121), &[], None, None).is_err());
    }

    #[test]
    fn hostile_text_is_data_not_a_command() {
        // §15.1: a name is stored metadata and is never interpolated into a
        // command line, so the only rules here are length and emptiness.
        let built = draft("; rm -rf / --no-preserve-root", &[], None, None)
            .expect("a hostile name is ordinary data");
        assert_eq!(built.display_name, "; rm -rf / --no-preserve-root");
    }
}
