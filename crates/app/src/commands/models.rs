//! Validated logic behind the local-model commands (matrix row A13).
//!
//! Spec §12 and §1.2(2): "we will never silently download multi-gigabyte
//! weights". The validation here is what makes the approval real rather than
//! decorative — an install is refused unless the request carries the licence
//! identifier the pack itself declares and an explicit acknowledgement of the
//! footprint, so a view that forgot to render the disclosure cannot install
//! anything.

use std::path::PathBuf;

use plaudon_domain::error::AppError;

use crate::dto::{
    InspectModelPackRequest, InspectModelPackResponse, InstallModelFromPackRequest,
    InstallModelResponse, ListLocalModelsResponse, RemoveLocalModelRequest,
    RemoveLocalModelResponse, VerifyLocalModelRequest, VerifyLocalModelResponse,
};
use crate::error::invalid_argument;
use crate::AppPorts;

/// Models installed on this machine, with the disk they occupy.
pub async fn list_local_models(ports: &AppPorts) -> Result<ListLocalModelsResponse, AppError> {
    ports.models.list_local_models().await
}

/// Read a pack and report the approval gate for every model it offers.
///
/// Installs nothing: the response is what the user decides from.
pub async fn inspect_model_pack(
    ports: &AppPorts,
    request: InspectModelPackRequest,
) -> Result<InspectModelPackResponse, AppError> {
    ports
        .models
        .inspect_pack(pack_path(&request.pack_path)?)
        .await
}

/// Install one model from a pack.
///
/// Three gates, in order, before the port is reached:
///
/// 1. the pack path is a real directory;
/// 2. the footprint was acknowledged;
/// 3. the licence identifier in the request matches the one the pack declares
///    for that model — a mismatch means the user approved something else.
pub async fn install_model_from_pack(
    ports: &AppPorts,
    request: InstallModelFromPackRequest,
) -> Result<InstallModelResponse, AppError> {
    let path = pack_path(&request.pack_path)?;
    if !request.footprint_acknowledged {
        return Err(invalid_argument(
            "an install needs the disk and memory footprint to be acknowledged",
        ));
    }
    if request.licence_accepted_spdx_id.trim().is_empty() {
        return Err(invalid_argument(
            "an install needs the model's licence to be accepted",
        ));
    }

    let inspection = ports.models.inspect_pack(path.clone()).await?;
    let offer = inspection
        .offers
        .iter()
        .find(|offer| offer.gate.model_id == request.model_id)
        .ok_or_else(|| invalid_argument("that pack does not offer that model"))?;
    if offer.gate.licence.spdx_id != request.licence_accepted_spdx_id {
        return Err(invalid_argument(
            "the accepted licence is not the licence this model ships under",
        ));
    }
    if offer.already_installed {
        return Err(invalid_argument("that model is already installed"));
    }

    ports
        .models
        .install_from_pack(path, request.model_id, request.keep_offline)
        .await
}

/// Delete an installed model.
///
/// Spec §14.7: a destructive action carries an explicit confirmation, and
/// without it nothing is removed.
pub async fn remove_local_model(
    ports: &AppPorts,
    request: RemoveLocalModelRequest,
) -> Result<RemoveLocalModelResponse, AppError> {
    if !request.confirmed {
        return Err(invalid_argument(
            "removing a model needs an explicit confirmation",
        ));
    }
    ports.models.remove_local_model(request.model_id).await
}

/// Re-hash an installed model against what the registry recorded.
pub async fn verify_local_model(
    ports: &AppPorts,
    request: VerifyLocalModelRequest,
) -> Result<VerifyLocalModelResponse, AppError> {
    ports.models.verify_local_model(request.model_id).await
}

/// A pack path is a directory the user picked, not free text.
fn pack_path(raw: &str) -> Result<PathBuf, AppError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_argument("a model pack path is required"));
    }
    let path = PathBuf::from(trimmed);
    if !path.is_dir() {
        return Err(invalid_argument(
            "that model pack is not a folder on this machine",
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_pack_path_is_refused_before_anything_is_opened() {
        assert!(pack_path("   ").is_err());
    }

    #[test]
    fn a_pack_path_that_is_not_a_folder_is_refused() {
        assert!(pack_path("/definitely/not/here/plaudon-pack").is_err());
    }

    #[test]
    fn a_real_folder_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            pack_path(&dir.path().display().to_string()).unwrap(),
            dir.path()
        );
    }
}
