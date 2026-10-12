//! Validated logic behind `list_outputs`, `review_claim` and `regenerate_output`.

use plaudon_domain::error::{AppError, AppErrorCode};
use plaudon_domain::ids::{ClaimId, OutputId, RecordingId};

use crate::dto::{
    ListOutputsRequest, ListOutputsResponse, RegenerateOutputRequest, RegenerateOutputResponse,
    ReviewClaimRequest, ReviewClaimResponse,
};
use crate::validate::parse_id;
use crate::AppPorts;

/// Every generated output of a recording, with claims.
pub async fn list_outputs(
    ports: &AppPorts,
    request: ListOutputsRequest,
) -> Result<ListOutputsResponse, AppError> {
    let recording_id: RecordingId = parse_id("recording_id", &request.recording_id)?;
    let outputs = ports.outputs.list_outputs(recording_id).await?;
    Ok(ListOutputsResponse { outputs })
}

/// Record a human review decision on one claim.
pub async fn review_claim(
    ports: &AppPorts,
    request: ReviewClaimRequest,
) -> Result<ReviewClaimResponse, AppError> {
    let _output_id: OutputId = parse_id("output_id", &request.output_id)?;
    let claim_id: ClaimId = parse_id("claim_id", &request.claim_id)?;
    let state = ports
        .outputs
        .review_claim(claim_id, request.accepted, request.note)
        .await?;
    Ok(ReviewClaimResponse {
        state: state.as_str().to_owned(),
    })
}

/// Trigger regeneration of an output, refusing without acknowledgement.
pub async fn regenerate_output(
    ports: &AppPorts,
    request: RegenerateOutputRequest,
) -> Result<RegenerateOutputResponse, AppError> {
    if !request.acknowledged {
        return Err(AppError::new(
            AppErrorCode::ConfirmationRequired,
            "regenerate_output requires explicit acknowledgement",
        ));
    }
    let output_id: OutputId = parse_id("output_id", &request.output_id)?;
    let job_id = ports.outputs.regenerate_output(output_id).await?;
    Ok(RegenerateOutputResponse { job_id })
}
