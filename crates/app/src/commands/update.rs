//! Validated logic behind the update-state command (task N2.2).
//!
//! There is exactly one command, and it is read-only. Spec §22(15): with
//! Local-only Mode on the application performs no update check at all, so the
//! settings section reports the policy and the installed version without
//! giving the user a control that would quietly contradict it.

use plaudon_domain::error::AppError;

use crate::settings_presentation::UpdateStatus;
use crate::AppPorts;

/// The installed version, the pinned endpoint and whether checks are
/// suppressed. Makes no network request.
pub async fn get_update_status(ports: &AppPorts) -> Result<UpdateStatus, AppError> {
    ports.update.update_status().await
}
