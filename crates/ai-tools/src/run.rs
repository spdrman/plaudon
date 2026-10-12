//! The record of one tool generation (spec 9.7, 13.3).
//!
//! Spec 9.7: "Each generation records input/output hashes, model/prompt
//! identity, cloud consent policy and estimated/actual cost where known."
//! Every one of those is a required field here, so a run cannot be constructed
//! without them. Cost is a [`MoneyEstimate`], which has an explicit `Unknown`:
//! an unknown cost is never summed as zero.

use plaudon_ai_contracts::capabilities::{PrivacyClass, RequestFamily};
use plaudon_domain::ids::{RecordingId, TranscriptRevisionId};
use plaudon_domain::media::Sha256Digest;
use plaudon_domain::model::ModelIdentity;
use plaudon_domain::money::MoneyEstimate;
use plaudon_domain::time::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::tool::{ToolId, TranscriptToolDefinition};

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRunStatus {
    /// Output was produced, validated and grounded.
    Succeeded,
    /// Output arrived but failed validation or grounding and was discarded.
    Rejected,
    /// The engine or the policy gate failed the run.
    Failed,
    /// The user or shutdown cancelled the run.
    Canceled,
}

impl ToolRunStatus {
    /// Stable wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

/// The consent and routing posture that applied to a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsentPolicySnapshot {
    /// Where the request was processed.
    pub privacy_class: PrivacyClass,
    /// Whether the route required explicit cloud consent.
    pub required_cloud_consent: bool,
    /// Identifier of the grant that authorized it, when one was needed.
    pub consent_id: Option<Uuid>,
    /// Whether Local-only Mode was enabled when the run was decided.
    pub local_only_mode: bool,
}

impl ConsentPolicySnapshot {
    /// The posture of a run that never left the machine.
    #[must_use]
    pub const fn on_device(local_only_mode: bool) -> Self {
        Self {
            privacy_class: PrivacyClass::OnDevice,
            required_cloud_consent: false,
            consent_id: None,
            local_only_mode,
        }
    }
}

/// One generation attempt, complete enough to audit after the fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolRunRecord {
    /// Run identity.
    pub id: Uuid,
    /// Recording the run belongs to.
    pub recording_id: RecordingId,
    /// The exact transcript revision the run consumed.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Tool identity.
    pub tool_id: ToolId,
    /// Tool definition version.
    pub tool_version: u32,
    /// SHA-256 of the tool definition, so an edited tool is detectable.
    pub tool_definition_sha256: String,
    /// Model that produced the output.
    pub model: ModelIdentity,
    /// Request family used.
    pub request_family: RequestFamily,
    /// Prompt template version.
    pub prompt_version: String,
    /// Output schema version.
    pub schema_version: String,
    /// SHA-256 of the exact prompt that was dispatched.
    pub input_sha256: String,
    /// SHA-256 of the accepted output, when there is one.
    pub output_sha256: Option<String>,
    /// How the run ended.
    pub status: ToolRunStatus,
    /// Consent and routing posture.
    pub consent_policy: ConsentPolicySnapshot,
    /// Pre-dispatch estimate.
    pub estimated_cost: MoneyEstimate,
    /// Provider-reported final cost.
    pub actual_cost: MoneyEstimate,
    /// When the run started.
    pub created_at: Timestamp,
    /// When the run reached a terminal state.
    pub finished_at: Option<Timestamp>,
}

/// Everything required to open a run, before it has an outcome.
#[derive(Debug, Clone)]
pub struct ToolRunDraft {
    /// Recording the run belongs to.
    pub recording_id: RecordingId,
    /// The exact transcript revision the run consumes.
    pub transcript_revision_id: TranscriptRevisionId,
    /// Model that will produce the output.
    pub model: ModelIdentity,
    /// Request family to be used.
    pub request_family: RequestFamily,
    /// The exact prompt text that will be dispatched.
    pub prompt_text: String,
    /// Consent and routing posture.
    pub consent_policy: ConsentPolicySnapshot,
    /// Pre-dispatch estimate, `Unknown` when the provider publishes no basis.
    pub estimated_cost: MoneyEstimate,
    /// Start instant.
    pub created_at: Timestamp,
}

impl ToolRunRecord {
    /// Open a run against a definition.
    ///
    /// The definition is captured by identity, version and hash at this point,
    /// so a later edit of the tool cannot change what this run says it ran.
    #[must_use]
    pub fn open(tool: &TranscriptToolDefinition, draft: ToolRunDraft) -> Self {
        Self {
            id: Uuid::now_v7(),
            recording_id: draft.recording_id,
            transcript_revision_id: draft.transcript_revision_id,
            tool_id: tool.id.clone(),
            tool_version: tool.version,
            tool_definition_sha256: tool.definition_sha256(),
            model: draft.model,
            request_family: draft.request_family,
            prompt_version: tool.prompt_version.clone(),
            schema_version: tool.output_schema_version.clone(),
            input_sha256: sha256_hex(draft.prompt_text.as_bytes()),
            output_sha256: None,
            status: ToolRunStatus::Failed,
            consent_policy: draft.consent_policy,
            estimated_cost: draft.estimated_cost,
            actual_cost: MoneyEstimate::Unknown,
            created_at: draft.created_at,
            finished_at: None,
        }
    }

    /// Close the run as successful with the accepted output.
    #[must_use]
    pub fn succeeded(
        mut self,
        output: &serde_json::Value,
        actual_cost: MoneyEstimate,
        finished_at: Timestamp,
    ) -> Self {
        self.output_sha256 = Some(sha256_hex(output.to_string().as_bytes()));
        self.status = ToolRunStatus::Succeeded;
        self.actual_cost = actual_cost;
        self.finished_at = Some(finished_at);
        self
    }

    /// Close the run without an accepted output.
    #[must_use]
    pub fn closed(
        mut self,
        status: ToolRunStatus,
        actual_cost: MoneyEstimate,
        finished_at: Timestamp,
    ) -> Self {
        debug_assert_ne!(
            status,
            ToolRunStatus::Succeeded,
            "use `succeeded` so the output hash is recorded"
        );
        self.status = status;
        self.actual_cost = actual_cost;
        self.finished_at = Some(finished_at);
        self
    }

    /// Whether this run spent money that the provider has confirmed.
    #[must_use]
    pub fn has_confirmed_cost(&self) -> bool {
        self.actual_cost.is_finalized()
    }
}

/// Lowercase hex SHA-256.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// SHA-256 of raw bytes as a domain digest.
#[must_use]
pub fn digest_of(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::of(bytes)
}

/// Sum costs across runs, propagating unknown.
///
/// Spec 13.2: unknown is not zero. If any contributing amount is unknown, or
/// if two amounts cannot be added because they differ in currency or scale,
/// the total is [`MoneyEstimate::Unknown`] rather than a misleading partial
/// sum. A total is only [`MoneyEstimate::Finalized`] when every contribution
/// was finalized.
#[must_use]
pub fn aggregate_cost(costs: &[MoneyEstimate]) -> MoneyEstimate {
    let mut total: Option<plaudon_domain::money::Money> = None;
    let mut all_finalized = true;

    for cost in costs {
        match cost {
            MoneyEstimate::Unknown => return MoneyEstimate::Unknown,
            MoneyEstimate::Estimated(amount) => {
                all_finalized = false;
                total = Some(match total {
                    None => *amount,
                    Some(current) => match current.checked_add(*amount) {
                        Some(sum) => sum,
                        None => return MoneyEstimate::Unknown,
                    },
                });
            }
            MoneyEstimate::Finalized(amount) => {
                total = Some(match total {
                    None => *amount,
                    Some(current) => match current.checked_add(*amount) {
                        Some(sum) => sum,
                        None => return MoneyEstimate::Unknown,
                    },
                });
            }
        }
    }

    match total {
        None => MoneyEstimate::Unknown,
        Some(amount) if all_finalized => MoneyEstimate::Finalized(amount),
        Some(amount) => MoneyEstimate::Estimated(amount),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::ToolRegistry;
    use plaudon_domain::model::EngineId;
    use plaudon_domain::money::{Currency, Money};

    fn usd(cents: i128) -> Money {
        Money::new(cents, 2, Currency::USD)
    }

    fn draft() -> ToolRunDraft {
        ToolRunDraft {
            recording_id: RecordingId::new(),
            transcript_revision_id: TranscriptRevisionId::new(),
            model: ModelIdentity::new(EngineId::new("openrouter"), "test/model"),
            request_family: RequestFamily::AudioChat,
            prompt_text: "Task: summarize".to_string(),
            consent_policy: ConsentPolicySnapshot {
                privacy_class: PrivacyClass::CloudZeroRetention,
                required_cloud_consent: true,
                consent_id: Some(Uuid::nil()),
                local_only_mode: false,
            },
            estimated_cost: MoneyEstimate::Estimated(usd(12)),
            created_at: Timestamp::from_millis(1),
        }
    }

    fn tool() -> TranscriptToolDefinition {
        ToolRegistry::with_defaults()
            .latest(&ToolId::new("meeting_summary"))
            .unwrap()
            .clone()
    }

    #[test]
    fn an_open_run_captures_the_definition_hash_and_the_prompt_hash() {
        let tool = tool();
        let run = ToolRunRecord::open(&tool, draft());
        assert_eq!(run.tool_definition_sha256, tool.definition_sha256());
        assert_eq!(run.input_sha256, sha256_hex(b"Task: summarize"));
        assert_eq!(run.prompt_version, tool.prompt_version);
        assert_eq!(run.schema_version, tool.output_schema_version);
    }

    #[test]
    fn a_later_tool_edit_cannot_change_what_a_past_run_recorded() {
        let mut registry = ToolRegistry::with_defaults();
        let original = registry
            .latest(&ToolId::new("meeting_summary"))
            .unwrap()
            .clone();
        let run = ToolRunRecord::open(&original, draft());

        registry
            .apply_override(&crate::registry::ToolConfigOverride {
                tool_id: "meeting_summary".to_string(),
                instruction: "Completely different".to_string(),
                prompt_version: "meeting_summary/2".to_string(),
                output_schema: None,
                output_schema_version: None,
            })
            .unwrap();

        assert_eq!(run.tool_version, 1);
        assert_eq!(
            run.tool_definition_sha256,
            registry
                .get(&ToolId::new("meeting_summary"), 1)
                .unwrap()
                .definition_sha256()
        );
    }

    #[test]
    fn a_successful_run_records_an_output_hash() {
        let run = ToolRunRecord::open(&tool(), draft()).succeeded(
            &serde_json::json!({"objectives": []}),
            MoneyEstimate::Finalized(usd(9)),
            Timestamp::from_millis(2),
        );
        assert_eq!(run.status, ToolRunStatus::Succeeded);
        assert_eq!(run.output_sha256.as_ref().map(String::len), Some(64));
        assert!(run.has_confirmed_cost());
    }

    #[test]
    fn a_rejected_run_keeps_its_estimate_and_has_no_output_hash() {
        let run = ToolRunRecord::open(&tool(), draft()).closed(
            ToolRunStatus::Rejected,
            MoneyEstimate::Unknown,
            Timestamp::from_millis(2),
        );
        assert_eq!(run.status, ToolRunStatus::Rejected);
        assert_eq!(run.output_sha256, None);
        assert_eq!(run.estimated_cost, MoneyEstimate::Estimated(usd(12)));
        assert_eq!(run.actual_cost, MoneyEstimate::Unknown);
    }

    #[test]
    fn one_unknown_cost_makes_the_whole_total_unknown() {
        let total = aggregate_cost(&[
            MoneyEstimate::Finalized(usd(10)),
            MoneyEstimate::Unknown,
            MoneyEstimate::Finalized(usd(5)),
        ]);
        assert_eq!(total, MoneyEstimate::Unknown);
    }

    #[test]
    fn a_total_is_finalized_only_when_every_contribution_is() {
        assert_eq!(
            aggregate_cost(&[
                MoneyEstimate::Finalized(usd(10)),
                MoneyEstimate::Finalized(usd(5)),
            ]),
            MoneyEstimate::Finalized(usd(15))
        );
        assert_eq!(
            aggregate_cost(&[
                MoneyEstimate::Finalized(usd(10)),
                MoneyEstimate::Estimated(usd(5)),
            ]),
            MoneyEstimate::Estimated(usd(15))
        );
    }

    #[test]
    fn mixed_currencies_do_not_silently_add_up() {
        let eur = Money::new(500, 2, Currency::parse("EUR").unwrap());
        assert_eq!(
            aggregate_cost(&[
                MoneyEstimate::Finalized(usd(10)),
                MoneyEstimate::Finalized(eur)
            ]),
            MoneyEstimate::Unknown
        );
    }

    #[test]
    fn an_empty_total_is_unknown_rather_than_zero() {
        assert_eq!(aggregate_cost(&[]), MoneyEstimate::Unknown);
    }
}
