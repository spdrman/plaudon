//! Validated logic behind the capability, settings, secret and activity
//! commands.

use plaudon_domain::error::{AppError, AppErrorCode};

use crate::dto::{
    ActivityPage, Capabilities, CompleteOnboardingRequest, ListActivityRequest,
    LocalOnlyModeResponse, NotificationCategory, NotificationSettings,
    NotificationSettingsResponse, Preferences, SetLocalOnlyModeRequest,
    SetNotificationSettingsRequest, SetOpenRouterKeyRequest, StatusResponse,
};
use crate::error::invalid_argument;
use crate::secrets::SecretValue;
use crate::validate::{cursor, limit, ValidatedListActivity};
use crate::AppPorts;

/// Everything the shell knows about this machine.
pub async fn get_capabilities(ports: &AppPorts) -> Result<Capabilities, AppError> {
    ports.capabilities.capabilities().await
}

/// Replace the notification preferences.
pub async fn set_notification_settings(
    ports: &AppPorts,
    request: SetNotificationSettingsRequest,
) -> Result<NotificationSettingsResponse, AppError> {
    validate_notification_settings(&request.settings)?;
    ports
        .settings
        .set_notification_settings(request.settings)
        .await
}

/// Turn local-only mode on or off.
///
/// Spec §15.2: the command carries an explicit confirmation, and without it
/// nothing changes.
pub async fn set_local_only_mode(
    ports: &AppPorts,
    request: SetLocalOnlyModeRequest,
) -> Result<LocalOnlyModeResponse, AppError> {
    if !request.confirmed {
        return Err(AppError::new(
            AppErrorCode::ConfirmationRequired,
            "set_local_only_mode requires confirmed = true",
        ));
    }
    ports.settings.set_local_only_mode(request.enabled).await
}

/// Store the OpenRouter key in the OS keychain.
///
/// The key is moved straight into [`SecretValue`], which has no `Debug`, no
/// `Display` and no `Serialize`, and the response is a bare status.
// Async like every other command: the keychain port is synchronous today and
// a replacement may not be.
#[allow(clippy::unused_async)]
pub async fn set_openrouter_key(
    ports: &AppPorts,
    request: SetOpenRouterKeyRequest,
) -> Result<StatusResponse, AppError> {
    let secret = SecretValue::new(request.key)?;
    ports.secrets.set_openrouter_key(&secret)?;
    Ok(StatusResponse::ok())
}

/// Remove the OpenRouter key. Removing an absent key succeeds.
#[allow(clippy::unused_async)]
pub async fn remove_openrouter_key(ports: &AppPorts) -> Result<StatusResponse, AppError> {
    ports.secrets.remove_openrouter_key()?;
    Ok(StatusResponse::ok())
}

/// One page of the activity history.
pub async fn list_activity(
    ports: &AppPorts,
    request: ListActivityRequest,
) -> Result<ActivityPage, AppError> {
    let query = ValidatedListActivity {
        cursor: cursor(request.cursor.as_deref())?,
        limit: limit(request.limit)?,
    };
    ports.activity.list_activity(query).await
}

/// Persist the onboarding result.
pub async fn complete_onboarding(
    ports: &AppPorts,
    request: CompleteOnboardingRequest,
) -> Result<Preferences, AppError> {
    validate_notification_settings(&request.notifications)?;
    if let Some(engine) = &request.default_stt_engine_id {
        if engine.trim().is_empty() {
            return Err(invalid_argument("default_stt_engine_id is blank"));
        }
    }
    if let Some(engine) = &request.default_text_engine_id {
        if engine.trim().is_empty() {
            return Err(invalid_argument("default_text_engine_id is blank"));
        }
    }
    ports.settings.complete_onboarding(request).await
}

/// Every notification category must carry an explicit choice, so a missing
/// key can never be read as "enabled".
fn validate_notification_settings(settings: &NotificationSettings) -> Result<(), AppError> {
    for category in NotificationCategory::ALL {
        if !settings.categories.contains_key(category) {
            return Err(invalid_argument(format!(
                "notification category {} is missing",
                category.as_str()
            )));
        }
    }
    if settings.categories.len() != NotificationCategory::ALL.len() {
        return Err(invalid_argument("unknown notification category"));
    }
    Ok(())
}
