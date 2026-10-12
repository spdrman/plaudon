//! Validated logic behind `import_files`.

use plaudon_domain::error::AppError;

use crate::dto::{ImportFilesRequest, ImportFilesResponse};
use crate::validate::import_paths;
use crate::AppPorts;

/// Import every user-approved path, one outcome per path in request order.
///
/// Paths are validated inside the port, because a single bad path must become
/// a `rejected` outcome rather than failing the whole request.
pub async fn import_files(
    ports: &AppPorts,
    request: ImportFilesRequest,
) -> Result<ImportFilesResponse, AppError> {
    let paths = import_paths(&request.paths)?;
    ports
        .import
        .import_files(paths, request.duplicate_policy)
        .await
}
