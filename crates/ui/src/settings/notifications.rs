//! Desktop notification preferences (matrix row J16).
//!
//! Three obligations:
//!
//! * `set_notification_settings` rejects a partial category map, so every
//!   change sends all four categories, filled from the stored settings.
//! * A denied permission is not lost events: the permission copy
//!   ([`notification_permission_copy`]) says so in every branch, and it is
//!   always rendered.
//! * Before `get_capabilities` answers, the switches are not actionable —
//!   sending a guessed map would overwrite preferences that were never read.

use std::collections::BTreeMap;

use egui::{RichText, Ui};
use plaudon_app::command::AppCommand;
use plaudon_app::dto::{
    NotificationCategory, NotificationSettings, OsNotificationPermission, PermissionState,
    SetNotificationSettingsRequest,
};
use plaudon_app::settings_presentation::{notification_permission_copy, NOTIFICATIONS_NOTE};

use super::{section, Section, SettingsData, SettingsOutput, SettingsTokens};

/// Label of the master switch.
const MASTER: &str = "Show desktop notifications";

/// Legend of the category group, as the React fieldset had it.
const CATEGORY_LEGEND: &str = "Notify me about";

/// Shown while `get_capabilities` has not answered.
const PREFERENCES_UNREAD: &str = "The stored preferences have not been read yet.";

/// Key of the notification permission in `Capabilities::permissions`.
const NOTIFICATION_PERMISSION_KEY: &str = "notifications";

/// What each category is called in the preference list.
///
/// Neither `plaudon-app` nor `plaudon-notifications` publishes a user-facing
/// label for a category — they publish the wire string and the privacy-safe
/// notification bodies — so the settings list names them here, in the user's
/// terms rather than the wire's.
const fn category_label(category: NotificationCategory) -> &'static str {
    match category {
        NotificationCategory::ImportFinished => "An import finished",
        NotificationCategory::TranscriptionFinished => "A transcription finished",
        NotificationCategory::RequiresAttention => "Something needs your attention",
        NotificationCategory::CloudBudgetExceeded => "The cloud budget was exhausted",
    }
}

/// The notification permission as the OS-permission inventory reports it.
const fn permission_from_state(state: PermissionState) -> OsNotificationPermission {
    match state {
        PermissionState::Granted => OsNotificationPermission::Granted,
        PermissionState::Denied => OsNotificationPermission::Denied,
        PermissionState::NotRequested => OsNotificationPermission::NotRequested,
    }
}

/// Every category, present and explicit, whatever the stored map omitted.
fn complete(settings: Option<&NotificationSettings>) -> NotificationSettings {
    let categories: BTreeMap<NotificationCategory, bool> = NotificationCategory::ALL
        .iter()
        .map(|&category| {
            let value = settings
                .and_then(|settings| settings.categories.get(&category))
                .copied()
                .unwrap_or(false);
            (category, value)
        })
        .collect();
    NotificationSettings {
        enabled: settings.is_some_and(|settings| settings.enabled),
        categories,
    }
}

/// Opt-in desktop notifications (matrix row J16).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NotificationsSection;

impl Section for NotificationsSection {
    fn title(&self) -> &'static str {
        "Notifications"
    }

    fn ui(
        &mut self,
        ui: &mut Ui,
        tokens: &dyn SettingsTokens,
        data: &SettingsData,
        out: &mut SettingsOutput,
    ) {
        section::note(ui, tokens, NOTIFICATIONS_NOTE);

        let stored = data
            .preferences()
            .map(|preferences| &preferences.notifications);
        if stored.is_none() {
            section::note(ui, tokens, PREFERENCES_UNREAD);
        }
        let mut settings = complete(stored);

        let mut changed = false;
        let mut enabled = settings.enabled;
        let master = ui.add_enabled(stored.is_some(), egui::Checkbox::new(&mut enabled, MASTER));
        if master.changed() {
            settings.enabled = enabled;
            changed = true;
        }

        ui.add_enabled_ui(stored.is_some() && enabled, |ui| {
            ui.label(RichText::new(CATEGORY_LEGEND).strong().color(tokens.text()));
            for &category in NotificationCategory::ALL {
                let mut on = settings.categories.get(&category).copied().unwrap_or(false);
                if ui.checkbox(&mut on, category_label(category)).changed() {
                    settings.categories.insert(category, on);
                    changed = true;
                }
            }
        });

        if changed {
            out.command(AppCommand::SetNotificationSettings(
                SetNotificationSettingsRequest { settings },
            ));
        }

        let permission = data.notification_permission.or_else(|| {
            data.capabilities
                .as_ref()
                .and_then(|capabilities| capabilities.permissions.get(NOTIFICATION_PERMISSION_KEY))
                .copied()
                .map(permission_from_state)
        });
        section::note(ui, tokens, notification_permission_copy(permission));
    }
}
