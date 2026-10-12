//! Validated logic behind the cloud commands (spec §10, ADR-0022).
//!
//! Every request is validated here, in Rust, before a port sees it. Nothing in
//! this module can reach the network or the filesystem.

use plaudon_domain::error::AppError;

use crate::dto::{
    CloudBudget, GetUsageRequest, GetUsageResponse, GrantCloudConsentRequest,
    GrantCloudConsentResponse, ListModelsRequest, ListModelsResponse, RevokeCloudConsentRequest,
    SetCloudBudgetRequest, SetModelPreferenceRequest, SetModelPreferenceResponse,
    StartProcessingRequest, StartProcessingResponse, StatusResponse, VerifyModelRequest,
    VerifyModelResponse,
};
use crate::validate;
use crate::AppPorts;

/// The model picker for a stage.
pub async fn list_models(
    ports: &AppPorts,
    request: ListModelsRequest,
) -> Result<ListModelsResponse, AppError> {
    ports
        .cloud
        .list_models(validate::list_models(&request)?)
        .await
}

/// Ledger usage in a range, plus the budget.
pub async fn get_usage(
    ports: &AppPorts,
    request: GetUsageRequest,
) -> Result<GetUsageResponse, AppError> {
    ports
        .cloud
        .get_usage(validate::usage_range(&request.from, &request.to)?)
        .await
}

/// Replace the local spending budget.
pub async fn set_cloud_budget(
    ports: &AppPorts,
    request: SetCloudBudgetRequest,
) -> Result<CloudBudget, AppError> {
    ports
        .cloud
        .set_cloud_budget(validate::budget(&request)?)
        .await
}

/// Record a consent grant.
pub async fn grant_cloud_consent(
    ports: &AppPorts,
    request: GrantCloudConsentRequest,
) -> Result<GrantCloudConsentResponse, AppError> {
    ports
        .cloud
        .grant_cloud_consent(validate::consent(&request)?)
        .await
}

/// Revoke a consent grant.
pub async fn revoke_cloud_consent(
    ports: &AppPorts,
    request: RevokeCloudConsentRequest,
) -> Result<StatusResponse, AppError> {
    let consent_id = validate::label("consent_id", &request.consent_id)?;
    ports.cloud.revoke_cloud_consent(consent_id).await?;
    Ok(StatusResponse::ok())
}

/// Write a default or per-recording model preference.
pub async fn set_model_preference(
    ports: &AppPorts,
    request: SetModelPreferenceRequest,
) -> Result<SetModelPreferenceResponse, AppError> {
    ports
        .cloud
        .set_model_preference(validate::preference(&request)?)
        .await
}

/// Start a processing stage.
pub async fn start_processing(
    ports: &AppPorts,
    request: StartProcessingRequest,
) -> Result<StartProcessingResponse, AppError> {
    ports
        .cloud
        .start_processing(validate::start_processing(&request)?)
        .await
}

/// Run the capability check for a model.
pub async fn verify_model(
    ports: &AppPorts,
    request: VerifyModelRequest,
) -> Result<VerifyModelResponse, AppError> {
    ports
        .cloud
        .verify_model(validate::verify_model(&request)?)
        .await
}
