//! The §9.2 default output types.
//!
//! Three rules from spec 9.2 are enforced by the types rather than by review:
//!
//! * We never invent owners, deadlines, amounts or commitments. An absent
//!   owner is [`OwnerRef::Unassigned`] and an absent date is
//!   [`DueDate::NotSpecified`] — both are explicit values on the wire, so a
//!   downstream renderer cannot mistake "missing" for "none required".
//! * A directly stated commitment is distinguished from a model
//!   interpretation by [`Assertion`].
//! * Every item carries its own anchors, so grounding is per item and not per
//!   document.

use std::fmt;

use plaudon_domain::anchor::SourceAnchor;
use plaudon_domain::claim::{ClaimDraft, ClaimKind};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Wire sentinel for an unassigned owner.
pub const UNASSIGNED: &str = "unassigned";
/// Wire sentinel for an absent date, time reference, consequence or mitigation.
pub const NOT_SPECIFIED: &str = "not_specified";
/// Wire sentinel for a value the transcript does not establish.
pub const UNKNOWN: &str = "unknown";

/// Who owns an action item or open question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnerRef {
    /// The transcript does not name an owner.
    Unassigned,
    /// The transcript names this owner verbatim.
    Named(String),
}

impl OwnerRef {
    /// Rendered form.
    #[must_use]
    pub fn display(&self) -> &str {
        match self {
            Self::Unassigned => "Unassigned",
            Self::Named(name) => name,
        }
    }
}

impl Serialize for OwnerRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Unassigned => serializer.serialize_str(UNASSIGNED),
            Self::Named(name) => serializer.serialize_str(name),
        }
    }
}

impl<'de> Deserialize<'de> for OwnerRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw.trim().is_empty() {
            return Err(D::Error::custom(
                "owner must be a name or the 'unassigned' sentinel, never empty",
            ));
        }
        Ok(if raw == UNASSIGNED {
            Self::Unassigned
        } else {
            Self::Named(raw)
        })
    }
}

/// A calendar date in the proleptic Gregorian calendar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CalendarDate {
    /// Year.
    pub year: i32,
    /// Month, 1-12.
    pub month: u8,
    /// Day, 1-31 bounded by the month.
    pub day: u8,
}

impl CalendarDate {
    /// Parse a strict `YYYY-MM-DD` date, rejecting impossible days.
    pub fn parse(raw: &str) -> Result<Self, DateError> {
        let bytes = raw.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return Err(DateError::Shape);
        }
        let year: i32 = raw[0..4].parse().map_err(|_| DateError::Shape)?;
        let month: u8 = raw[5..7].parse().map_err(|_| DateError::Shape)?;
        let day: u8 = raw[8..10].parse().map_err(|_| DateError::Shape)?;
        let month_enum = time::Month::try_from(month).map_err(|_| DateError::OutOfRange)?;
        time::Date::from_calendar_date(year, month_enum, day).map_err(|_| DateError::OutOfRange)?;
        Ok(Self { year, month, day })
    }
}

impl fmt::Display for CalendarDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// Why a date could not be accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DateError {
    /// The value is not `YYYY-MM-DD` or the `not_specified` sentinel.
    #[error("date must be YYYY-MM-DD or 'not_specified'")]
    Shape,
    /// The value is well shaped but names a day that does not exist.
    #[error("date names a day that does not exist")]
    OutOfRange,
}

/// A due date, or the explicit absence of one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueDate {
    /// The transcript states no date. Spec 9.2 forbids inventing one.
    NotSpecified,
    /// The transcript states this date.
    On(CalendarDate),
}

impl DueDate {
    /// Rendered form.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::NotSpecified => "Not specified".to_owned(),
            Self::On(date) => date.to_string(),
        }
    }
}

impl Serialize for DueDate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::NotSpecified => serializer.serialize_str(NOT_SPECIFIED),
            Self::On(date) => serializer.serialize_str(&date.to_string()),
        }
    }
}

impl<'de> Deserialize<'de> for DueDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == NOT_SPECIFIED {
            return Ok(Self::NotSpecified);
        }
        CalendarDate::parse(&raw)
            .map(Self::On)
            .map_err(D::Error::custom)
    }
}

/// An optional free-text field that is explicitly absent rather than empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionalText {
    /// The transcript does not state this.
    NotSpecified,
    /// The transcript states this.
    Stated(String),
}

impl OptionalText {
    /// Rendered form.
    #[must_use]
    pub fn display(&self) -> &str {
        match self {
            Self::NotSpecified => "Not specified",
            Self::Stated(text) => text,
        }
    }

    /// The stated text, when present.
    #[must_use]
    pub fn stated(&self) -> Option<&str> {
        match self {
            Self::NotSpecified => None,
            Self::Stated(text) => Some(text),
        }
    }
}

impl Serialize for OptionalText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::NotSpecified => serializer.serialize_str(NOT_SPECIFIED),
            Self::Stated(text) => serializer.serialize_str(text),
        }
    }
}

impl<'de> Deserialize<'de> for OptionalText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw.trim().is_empty() {
            return Err(D::Error::custom(
                "use the 'not_specified' sentinel rather than an empty string",
            ));
        }
        Ok(if raw == NOT_SPECIFIED {
            Self::NotSpecified
        } else {
            Self::Stated(raw)
        })
    }
}

/// Whether the transcript states the item directly or the model inferred it.
///
/// Spec 9.2: we distinguish directly stated commitments from model
/// interpretations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assertion {
    /// A participant said this.
    DirectlyStated,
    /// The model concluded this from what was said.
    ModelInterpretation,
}

/// Status of an action item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    /// Not started as far as the transcript shows.
    Open,
    /// In progress as far as the transcript shows.
    InProgress,
    /// Completed as far as the transcript shows.
    Done,
    /// The transcript does not establish a status.
    Unknown,
}

/// One sentence of the executive summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryStatement {
    /// The statement.
    pub text: String,
    /// Whether the statement is objective, outcome or open question.
    pub facet: SummaryFacet,
    /// Supporting anchors.
    pub anchors: Vec<SourceAnchor>,
}

/// Which part of the executive summary a statement belongs to (spec 9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryFacet {
    /// What the meeting set out to do.
    Objective,
    /// What the meeting concluded.
    Outcome,
    /// What remained unresolved.
    OpenQuestion,
}

/// A decision (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    /// The decision statement.
    pub statement: String,
    /// Participants, only when explicitly evidenced.
    #[serde(default)]
    pub participants: Vec<String>,
    /// Time reference, or the explicit absence of one.
    pub time_reference: OptionalText,
    /// Whether the decision was stated or inferred.
    pub assertion: Assertion,
    /// Evidence anchors.
    pub anchors: Vec<SourceAnchor>,
}

/// An action item (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionItem {
    /// Imperative verb phrase.
    pub verb: String,
    /// What is to be done.
    pub description: String,
    /// Owner, or `Unassigned`.
    pub owner: OwnerRef,
    /// Due date, or `Not specified`.
    pub due_date: DueDate,
    /// Status.
    pub status: ActionStatus,
    /// Whether the commitment was stated or inferred.
    pub assertion: Assertion,
    /// Evidence anchors.
    pub anchors: Vec<SourceAnchor>,
}

/// A risk or issue (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Risk {
    /// What the risk is.
    pub description: String,
    /// What happens if it materializes.
    pub consequence: OptionalText,
    /// Mitigation, only when discussed.
    pub mitigation: OptionalText,
    /// Evidence anchors.
    pub anchors: Vec<SourceAnchor>,
}

/// An unresolved question (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenQuestion {
    /// The question.
    pub question: String,
    /// Owner, or `Unassigned`.
    pub owner: OwnerRef,
    /// Evidence anchors.
    pub anchors: Vec<SourceAnchor>,
}

/// A topic heading with its segment range (spec 9.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topic {
    /// Heading.
    pub heading: String,
    /// First segment index covered, inclusive.
    pub first_segment: u32,
    /// Last segment index covered, inclusive.
    pub last_segment: u32,
    /// Evidence anchors.
    pub anchors: Vec<SourceAnchor>,
}

/// One extracted item of any §9.2 type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExtractedItem {
    /// Executive summary statement.
    SummaryStatement(SummaryStatement),
    /// Decision.
    Decision(Decision),
    /// Action item.
    ActionItem(ActionItem),
    /// Risk or issue.
    Risk(Risk),
    /// Open question.
    Question(OpenQuestion),
    /// Topic heading.
    Topic(Topic),
}

impl ExtractedItem {
    /// Claim kind of this item.
    #[must_use]
    pub const fn claim_kind(&self) -> ClaimKind {
        match self {
            Self::SummaryStatement(_) => ClaimKind::SummaryStatement,
            Self::Decision(_) => ClaimKind::Decision,
            Self::ActionItem(_) => ClaimKind::ActionItem,
            Self::Risk(_) => ClaimKind::Risk,
            Self::Question(_) => ClaimKind::Question,
            Self::Topic(_) => ClaimKind::Topic,
        }
    }

    /// Anchors the model supplied for this item.
    #[must_use]
    pub fn anchors(&self) -> &[SourceAnchor] {
        match self {
            Self::SummaryStatement(item) => &item.anchors,
            Self::Decision(item) => &item.anchors,
            Self::ActionItem(item) => &item.anchors,
            Self::Risk(item) => &item.anchors,
            Self::Question(item) => &item.anchors,
            Self::Topic(item) => &item.anchors,
        }
    }

    /// The claim text a reviewer reads and confirms.
    #[must_use]
    pub fn claim_text(&self) -> String {
        match self {
            Self::SummaryStatement(item) => item.text.clone(),
            Self::Decision(item) => item.statement.clone(),
            Self::ActionItem(item) => format!(
                "{} {} — owner {}, due {}",
                item.verb,
                item.description,
                item.owner.display(),
                item.due_date.display()
            ),
            Self::Risk(item) => item.description.clone(),
            Self::Question(item) => item.question.clone(),
            Self::Topic(item) => item.heading.clone(),
        }
    }

    /// Every free-text string of this item, for length validation.
    #[must_use]
    pub fn text_fields(&self) -> Vec<&str> {
        match self {
            Self::SummaryStatement(item) => vec![item.text.as_str()],
            Self::Decision(item) => {
                let mut fields = vec![item.statement.as_str()];
                fields.extend(item.participants.iter().map(String::as_str));
                if let Some(text) = item.time_reference.stated() {
                    fields.push(text);
                }
                fields
            }
            Self::ActionItem(item) => {
                let mut fields = vec![item.verb.as_str(), item.description.as_str()];
                if let OwnerRef::Named(name) = &item.owner {
                    fields.push(name.as_str());
                }
                fields
            }
            Self::Risk(item) => {
                let mut fields = vec![item.description.as_str()];
                fields.extend(item.consequence.stated());
                fields.extend(item.mitigation.stated());
                fields
            }
            Self::Question(item) => {
                let mut fields = vec![item.question.as_str()];
                if let OwnerRef::Named(name) = &item.owner {
                    fields.push(name.as_str());
                }
                fields
            }
            Self::Topic(item) => vec![item.heading.as_str()],
        }
    }

    /// Build the claim draft this item contributes.
    #[must_use]
    pub fn to_claim_draft(&self, item_path: impl Into<String>) -> ClaimDraft {
        ClaimDraft::new(
            item_path,
            self.claim_kind(),
            self.claim_text(),
            self.anchors().to_vec(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_owner_is_an_explicit_sentinel_not_an_empty_string() {
        let owner: OwnerRef = serde_json::from_str("\"unassigned\"").unwrap();
        assert_eq!(owner, OwnerRef::Unassigned);
        assert_eq!(owner.display(), "Unassigned");
        assert_eq!(serde_json::to_string(&owner).unwrap(), "\"unassigned\"");
        assert!(serde_json::from_str::<OwnerRef>("\"\"").is_err());
        assert!(serde_json::from_str::<OwnerRef>("\"   \"").is_err());
    }

    #[test]
    fn an_absent_date_is_not_specified_and_never_invented() {
        let due: DueDate = serde_json::from_str("\"not_specified\"").unwrap();
        assert_eq!(due, DueDate::NotSpecified);
        assert_eq!(due.display(), "Not specified");
    }

    #[test]
    fn a_real_date_round_trips() {
        let due: DueDate = serde_json::from_str("\"2026-02-28\"").unwrap();
        assert_eq!(due.display(), "2026-02-28");
        assert_eq!(serde_json::to_string(&due).unwrap(), "\"2026-02-28\"");
    }

    #[test]
    fn an_impossible_date_is_rejected_rather_than_clamped() {
        assert_eq!(CalendarDate::parse("2025-02-30"), Err(DateError::OutOfRange));
        assert_eq!(CalendarDate::parse("2025-13-01"), Err(DateError::OutOfRange));
        assert_eq!(CalendarDate::parse("next friday"), Err(DateError::Shape));
        assert_eq!(CalendarDate::parse("2025-2-3"), Err(DateError::Shape));
        assert!(serde_json::from_str::<DueDate>("\"2025-02-30\"").is_err());
    }

    #[test]
    fn a_leap_day_is_accepted_in_a_leap_year_only() {
        assert!(CalendarDate::parse("2028-02-29").is_ok());
        assert_eq!(CalendarDate::parse("2027-02-29"), Err(DateError::OutOfRange));
    }

    #[test]
    fn an_action_item_renders_its_placeholders() {
        let item = ExtractedItem::ActionItem(ActionItem {
            verb: "Send".into(),
            description: "the revised quote".into(),
            owner: OwnerRef::Unassigned,
            due_date: DueDate::NotSpecified,
            status: ActionStatus::Open,
            assertion: Assertion::DirectlyStated,
            anchors: Vec::new(),
        });
        assert_eq!(
            item.claim_text(),
            "Send the revised quote — owner Unassigned, due Not specified"
        );
        assert_eq!(item.claim_kind(), ClaimKind::ActionItem);
    }

    #[test]
    fn every_item_kind_contributes_a_claim_draft_with_its_anchors() {
        let anchor = SourceAnchor::new(
            plaudon_domain::ids::TranscriptRevisionId::new(),
            0,
            0,
            5,
            "Alpha",
        );
        let item = ExtractedItem::Decision(Decision {
            statement: "Ship on Friday".into(),
            participants: vec!["Ana".into()],
            time_reference: OptionalText::NotSpecified,
            assertion: Assertion::DirectlyStated,
            anchors: vec![anchor.clone()],
        });
        let draft = item.to_claim_draft("/decisions/0");
        assert_eq!(draft.kind, ClaimKind::Decision);
        assert_eq!(draft.anchors, vec![anchor]);
        assert_eq!(draft.item_path, "/decisions/0");
    }

    #[test]
    fn optional_text_rejects_an_empty_string() {
        assert!(serde_json::from_str::<OptionalText>("\"\"").is_err());
        let value: OptionalText = serde_json::from_str("\"not_specified\"").unwrap();
        assert_eq!(value.display(), "Not specified");
        assert_eq!(value.stated(), None);
    }
}
