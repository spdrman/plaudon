//! Backend implementation of [`crate::ports::OutputsPort`].

use async_trait::async_trait;
use plaudon_persistence::db::Database;
use sqlx::Row;

use crate::backend::LocalBackend;
use crate::error::internal;
use crate::ports::OutputsPort;

use plaudon_domain::claim::{Claim, ClaimKind, ClaimState, HumanConfirmation};
use plaudon_domain::error::AppError;
use plaudon_domain::ids::{ClaimId, JobId, OutputId, RecordingId};
use plaudon_domain::output::CoverageState;
use plaudon_domain::time::Timestamp;
use plaudon_domain::transcript::AlignmentOrigin;
use plaudon_persistence::intelligence::{OutputHeader, OutputRepository};
use serde_json::Value as JsonValue;

use crate::dto::{
    ActionItemFields, AnchorDto, ClaimDto, CoverageDto, DecisionFields, OutputDto, QuestionFields,
    RenderedClaimFields, RiskFields, StaleDto, TopicFields,
};
use crate::ports::{ClaimWithFields, ItemFields, OutputWithClaims};

#[async_trait]
impl OutputsPort for LocalBackend {
    async fn list_outputs(
        &self,
        recording_id: RecordingId,
    ) -> Result<Vec<crate::dto::OutputDto>, AppError> {
        let repo = OutputRepository::new(self.store().db().clone());
        let headers = repo
            .headers_for(recording_id)
            .await
            .map_err(AppError::from)?;

        let mut result = Vec::new();
        for header in headers {
            // Fetch the body JSON to extract rendered fields
            let body_json = fetch_output_body(self.store().db(), header.id).await?;
            let output_with_claims = build_output_with_claims(&repo, header, body_json).await?;
            result.push(to_output_dto(output_with_claims));
        }
        Ok(result)
    }

    async fn review_claim(
        &self,
        claim_id: ClaimId,
        accepted: bool,
        note: Option<String>,
    ) -> Result<ClaimState, AppError> {
        let repo = OutputRepository::new(self.store().db().clone());
        let confirmation = if accepted {
            let mut conf = HumanConfirmation::accept("desktop-user".to_owned(), Timestamp::now());
            if let Some(n) = note {
                conf = conf.with_note(n);
            }
            conf
        } else {
            let mut conf = HumanConfirmation::reject("desktop-user".to_owned(), Timestamp::now());
            if let Some(n) = note {
                conf = conf.with_note(n);
            }
            conf
        };
        repo.review_claim(claim_id, &confirmation)
            .await
            .map_err(AppError::from)
    }

    async fn regenerate_output(&self, _output_id: OutputId) -> Result<JobId, AppError> {
        // Regeneration requires the pipeline to be wired up, which is a separate EPIC.
        // For now, reject with a typed error.
        Err(AppError::new(
            plaudon_domain::error::AppErrorCode::CapabilityUnverified,
            "output regeneration is not yet implemented",
        ))
    }
}

/// Fetch the output body JSON from the database.
async fn fetch_output_body(db: &Database, output_id: OutputId) -> Result<JsonValue, AppError> {
    let row = sqlx::query("SELECT body_json FROM ai_outputs WHERE id = ?1")
        .bind(output_id.to_string())
        .fetch_optional(db.pool())
        .await
        .map_err(|e| internal(format!("failed to fetch output body: {e}")))?
        .ok_or_else(|| internal(format!("output {output_id} not found")))?;
    let body_text: String = row.get("body_json");
    serde_json::from_str(&body_text)
        .map_err(|e| internal(format!("failed to parse output body JSON: {e}")))
}

/// Build an [`OutputWithClaims`] from the repository data.
pub async fn build_output_with_claims(
    repo: &OutputRepository,
    header: OutputHeader,
    body: JsonValue,
) -> Result<OutputWithClaims, AppError> {
    let claims_domain = repo.claims_for(header.id).await.map_err(AppError::from)?;
    let claims = claims_domain
        .into_iter()
        .map(|claim| {
            let fields = extract_fields(&claim, &body)?;
            Ok(ClaimWithFields { claim, fields })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(OutputWithClaims { header, claims })
}

/// Extract the rendered §9.2 fields for a claim from the output body JSON.
fn extract_fields(claim: &Claim, body: &JsonValue) -> Result<ItemFields, AppError> {
    let pointer = &claim.item_path;
    let item = body.pointer(pointer).ok_or_else(|| {
        AppError::new(
            plaudon_domain::error::AppErrorCode::InternalError,
            format!("claim item_path {pointer} not found in output body"),
        )
    })?;

    match claim.kind {
        ClaimKind::SummaryStatement => Ok(ItemFields::SummaryStatement),
        ClaimKind::Decision => {
            let statement = item
                .get("statement")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let participants = item
                .get("participants")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_owned()))
                        .collect()
                })
                .unwrap_or_default();
            let time_reference = item
                .get("time_reference")
                .and_then(|v| v.as_str())
                .unwrap_or("not_specified");
            let time_reference = if time_reference == "not_specified" {
                "Not specified".to_owned()
            } else {
                time_reference.to_owned()
            };
            let assertion = item
                .get("assertion")
                .and_then(|v| v.as_str())
                .unwrap_or("model_interpretation")
                .to_owned();
            Ok(ItemFields::Decision {
                statement,
                participants,
                time_reference,
                assertion,
            })
        }
        ClaimKind::ActionItem => {
            let verb = item
                .get("verb")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let description = item
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let owner = item
                .get("owner")
                .and_then(|v| v.as_str())
                .unwrap_or("unassigned");
            let owner = if owner == "unassigned" {
                "Unassigned".to_owned()
            } else {
                owner.to_owned()
            };
            let due_date = item
                .get("due_date")
                .and_then(|v| v.as_str())
                .unwrap_or("not_specified");
            let due_date = if due_date == "not_specified" {
                "Not specified".to_owned()
            } else {
                due_date.to_owned()
            };
            let status = item
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_owned();
            let assertion = item
                .get("assertion")
                .and_then(|v| v.as_str())
                .unwrap_or("model_interpretation")
                .to_owned();
            Ok(ItemFields::ActionItem {
                verb,
                description,
                owner,
                due_date,
                status,
                assertion,
            })
        }
        ClaimKind::Risk => {
            let description = item
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let consequence = item
                .get("consequence")
                .and_then(|v| v.as_str())
                .unwrap_or("not_specified");
            let consequence = if consequence == "not_specified" {
                "Not specified".to_owned()
            } else {
                consequence.to_owned()
            };
            let mitigation = item
                .get("mitigation")
                .and_then(|v| v.as_str())
                .unwrap_or("not_specified");
            let mitigation = if mitigation == "not_specified" {
                "Not specified".to_owned()
            } else {
                mitigation.to_owned()
            };
            Ok(ItemFields::Risk {
                description,
                consequence,
                mitigation,
            })
        }
        ClaimKind::Question => {
            let question = item
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let owner = item
                .get("owner")
                .and_then(|v| v.as_str())
                .unwrap_or("unassigned");
            let owner = if owner == "unassigned" {
                "Unassigned".to_owned()
            } else {
                owner.to_owned()
            };
            Ok(ItemFields::Question { question, owner })
        }
        ClaimKind::Topic => {
            let heading = item
                .get("heading")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            let first_segment = item
                .get("first_segment")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let last_segment = item
                .get("last_segment")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            Ok(ItemFields::Topic {
                heading,
                first_segment,
                last_segment,
            })
        }
    }
}

/// Convert [`OutputWithClaims`] to the wire DTO.
pub fn to_output_dto(output: OutputWithClaims) -> OutputDto {
    let header = output.header;
    let claims = output
        .claims
        .into_iter()
        .map(|claim_with_fields| {
            let claim = &claim_with_fields.claim;
            let fields = match claim_with_fields.fields {
                ItemFields::SummaryStatement => RenderedClaimFields::SummaryStatement,
                ItemFields::Decision {
                    statement,
                    participants,
                    time_reference,
                    assertion,
                } => RenderedClaimFields::Decision(DecisionFields {
                    statement,
                    participants,
                    time_reference,
                    assertion,
                }),
                ItemFields::ActionItem {
                    verb,
                    description,
                    owner,
                    due_date,
                    status,
                    assertion,
                } => RenderedClaimFields::ActionItem(ActionItemFields {
                    verb,
                    description,
                    owner,
                    due_date,
                    status,
                    assertion,
                }),
                ItemFields::Risk {
                    description,
                    consequence,
                    mitigation,
                } => RenderedClaimFields::Risk(RiskFields {
                    description,
                    consequence,
                    mitigation,
                }),
                ItemFields::Question { question, owner } => {
                    RenderedClaimFields::Question(QuestionFields { question, owner })
                }
                ItemFields::Topic {
                    heading,
                    first_segment,
                    last_segment,
                } => RenderedClaimFields::Topic(TopicFields {
                    heading,
                    first_segment,
                    last_segment,
                }),
            };

            let anchors = claim
                .anchors
                .iter()
                .map(|anchor| {
                    let verified = anchor.verification.is_verified();
                    let (rejection_code, matched_text, alignment_origin, seek_ms) =
                        match &anchor.verification {
                            plaudon_domain::anchor::AnchorVerification::Verified(v) => (
                                None,
                                Some(v.matched_text.clone()),
                                match v.alignment_origin {
                                    AlignmentOrigin::None => "none",
                                    AlignmentOrigin::Native => "native",
                                    AlignmentOrigin::Derived => "derived",
                                }
                                .to_owned(),
                                v.seek_ms,
                            ),
                            plaudon_domain::anchor::AnchorVerification::Rejected(r) => {
                                (Some(r.code().to_owned()), None, "none".to_owned(), None)
                            }
                        };

                    AnchorDto {
                        seq_index: anchor.anchor.seq_index,
                        char_start: anchor.anchor.char_start,
                        char_end: anchor.anchor.char_end,
                        excerpt: anchor.anchor.excerpt.clone(),
                        verified,
                        rejection_code,
                        matched_text,
                        alignment_origin,
                        seek_ms,
                    }
                })
                .collect();

            ClaimDto {
                claim_id: claim.id,
                item_path: claim.item_path.clone(),
                text: claim.text.clone(),
                state: claim.state().as_str().to_owned(),
                review_required: claim.review_required(),
                review_reasons: claim
                    .review_reasons()
                    .iter()
                    .map(|r| r.as_str().to_owned())
                    .collect(),
                entailment: match claim.entailment {
                    plaudon_domain::claim::EntailmentCheck::Supported => "supported",
                    plaudon_domain::claim::EntailmentCheck::Unsupported => "not_supported",
                    plaudon_domain::claim::EntailmentCheck::NotChecked => "not_checked",
                }
                .to_owned(),
                category: claim.category.as_str().to_owned(),
                fields,
                anchors,
            }
        })
        .collect();

    let coverage = match header.coverage {
        CoverageState::Full => CoverageDto {
            state: "full".to_owned(),
            chunks: None,
        },
        CoverageState::ModelReportedPartial { ref chunks } => CoverageDto {
            state: "model_reported_partial".to_owned(),
            chunks: Some(chunks.clone()),
        },
    };

    let stale = header.stale.as_ref().map(|s| StaleDto {
        stale_at: s
            .stale_at
            .to_rfc3339()
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
        reason: s.reason.clone(),
    });

    OutputDto {
        output_id: header.id,
        recording_id: header.recording_id,
        transcript_revision_id: header.transcript_revision_id,
        output_type: header.output_type,
        template_id: header.template_id,
        template_version: header.template_version,
        pipeline_version: header.pipeline_version,
        status: header.status.as_str().to_owned(),
        coverage,
        stale,
        created_at: header
            .created_at
            .to_rfc3339()
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
        claims,
    }
}
