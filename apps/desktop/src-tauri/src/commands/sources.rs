//! Validated logic behind the source commands (spec §15.2): `plaud_probe`,
//! `plaud_connect`, `plaud_disconnect`, `plaud_list`, `plaud_import`,
//! `source_add_folder`, `source_sync`, `source_reconcile` and
//! `list_source_artifacts`.
//!
//! Every function validates in Rust and then calls the
//! [`crate::ports::SourcesPort`]; none of them knows the connector, the
//! process runner or the folder service exists.

use plaudon_domain::error::AppError;

use crate::dto::{
    ListSourceArtifactsRequest, ListSourceArtifactsResponse, PlaudConnectRequest,
    PlaudConnectorSnapshot, PlaudDisconnectResponse, PlaudImportRequest, PlaudImportResponse,
    PlaudListRequest, PlaudListResponse, SourceAddFolderRequest, SourceAddFolderResponse,
    SourceReconcileRequest, SourceReconcileResponse, SourceSyncRequest, SourceSyncResponse,
};
use crate::validate::{
    parse_id, plaud_connect_intent, plaud_cursor, plaud_external_ids, source_id,
};
use crate::AppPorts;

/// Report the connector's status, diagnostics and disconnect wording.
pub async fn plaud_probe(ports: &AppPorts) -> Result<PlaudConnectorSnapshot, AppError> {
    ports.sources.plaud_probe().await
}

/// Discover the CLI, then let it authenticate the user.
pub async fn plaud_connect(
    ports: &AppPorts,
    request: PlaudConnectRequest,
) -> Result<PlaudConnectorSnapshot, AppError> {
    let intent = plaud_connect_intent(&request.user_intent)?;
    ports.sources.plaud_connect(intent).await
}

/// Drop the app's association with the CLI.
pub async fn plaud_disconnect(ports: &AppPorts) -> Result<PlaudDisconnectResponse, AppError> {
    ports.sources.plaud_disconnect().await
}

/// One page of the account's recordings.
pub async fn plaud_list(
    ports: &AppPorts,
    request: PlaudListRequest,
) -> Result<PlaudListResponse, AppError> {
    let cursor = plaud_cursor(request.cursor.as_deref())?;
    ports.sources.plaud_list(cursor).await
}

/// Import recordings from the last listing.
pub async fn plaud_import(
    ports: &AppPorts,
    request: PlaudImportRequest,
) -> Result<PlaudImportResponse, AppError> {
    let ids = plaud_external_ids(&request.external_ids)?;
    ports.sources.plaud_import(ids).await
}

/// Register a watched folder chosen with the OS picker.
///
/// The path is validated inside the port, because only the backend knows the
/// approved roots.
pub async fn source_add_folder(
    ports: &AppPorts,
    request: SourceAddFolderRequest,
) -> Result<SourceAddFolderResponse, AppError> {
    ports.sources.source_add_folder(request).await
}

/// Run one sync of a source.
pub async fn source_sync(
    ports: &AppPorts,
    request: SourceSyncRequest,
) -> Result<SourceSyncResponse, AppError> {
    let id = source_id(&request.source_id)?;
    ports.sources.source_sync(id, request.trigger).await
}

/// Reconcile a source against the library.
pub async fn source_reconcile(
    ports: &AppPorts,
    request: SourceReconcileRequest,
) -> Result<SourceReconcileResponse, AppError> {
    let id = source_id(&request.source_id)?;
    ports.sources.source_reconcile(id).await
}

/// Vendor exports found alongside a recording.
pub async fn list_source_artifacts(
    ports: &AppPorts,
    request: ListSourceArtifactsRequest,
) -> Result<ListSourceArtifactsResponse, AppError> {
    let recording_id = parse_id("recording_id", &request.recording_id)?;
    ports.sources.list_source_artifacts(recording_id).await
}
