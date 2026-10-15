//! The one place a §15.4 error envelope becomes something a user can act on.
//!
//! The envelope already carries the vendor-neutral message and, for most
//! codes, the affordance that would fix the problem. This widget renders the
//! message and exactly the control the declared [`ErrorAction`] implies — never
//! a stack trace, never a bare code, and never a dead end: an envelope with no
//! declared action but `retryable` still offers Retry.

use egui::{RichText, Ui};
use plaudon_domain::error::{ErrorAction, ErrorEnvelope};

/// The corrective control an envelope implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorControl {
    /// Nothing the user can do here.
    None,
    /// Offer the same operation again, under this label.
    Retry(&'static str),
    /// Send the user to settings, under this label.
    Settings(&'static str),
    /// Offer both ways to get a speech model: install one, or import a file
    /// the user already has (issue #305).
    InstallOrImportModel,
}

/// Label of the install control a missing speech model offers.
pub const INSTALL_MODEL_LABEL: &str = "Install a model";

/// Label of the import control a missing speech model offers.
pub const IMPORT_MODEL_LABEL: &str = "Import a model file";

/// What the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorNoticeAction {
    /// Run the failed operation again.
    Retry,
    /// Open the settings screen.
    OpenSettings,
    /// Install a speech model (issue #305).
    InstallSpeechModel,
    /// Import a model file the user already has (issue #305).
    ImportModelFile,
}

/// The control an envelope implies.
#[must_use]
pub fn control_for(envelope: &ErrorEnvelope) -> ErrorControl {
    match envelope.action {
        Some(ErrorAction::RetryImport) => ErrorControl::Retry("Retry import"),
        Some(ErrorAction::WaitAndRetry) => ErrorControl::Retry("Retry"),
        Some(ErrorAction::RefreshSourceLink) => ErrorControl::Retry("Refresh and retry"),
        Some(ErrorAction::FreeDiskSpace) => ErrorControl::Settings("Open storage settings"),
        Some(ErrorAction::ChooseAnotherModel) => ErrorControl::Settings("Choose another engine"),
        // There is no other engine to choose when none has a model: offer the
        // two things that fix it, right here.
        Some(ErrorAction::InstallSpeechModel) => ErrorControl::InstallOrImportModel,
        Some(ErrorAction::ReviewLocalOnlyMode) => ErrorControl::Settings("Review local-only mode"),
        Some(ErrorAction::ReviewConsent) => ErrorControl::Settings("Review cloud consent"),
        Some(ErrorAction::OpenSettings) => ErrorControl::Settings("Open settings"),
        Some(ErrorAction::OpenPlaudConnect) => {
            ErrorControl::Settings("Open PLAUD connection settings")
        }
        Some(ErrorAction::OpenCliSetup) => ErrorControl::Settings("Open PLAUD CLI setup"),
        Some(ErrorAction::OpenPermissionSettings) => {
            ErrorControl::Settings("Open permission settings")
        }
        Some(ErrorAction::OpenDeviceDiagnostics | ErrorAction::OpenDiagnostics) => {
            ErrorControl::Settings("Open diagnostics")
        }
        // Charging is a physical act; there is no control that performs it.
        Some(ErrorAction::ChargeDevice) => ErrorControl::None,
        None => {
            if envelope.retryable {
                ErrorControl::Retry("Retry")
            } else {
                ErrorControl::None
            }
        }
    }
}

/// Render `envelope` with `context` in front of its message.
///
/// `context` states what failed in the caller's own words — "Recordings could
/// not be loaded." — because the envelope's message describes the cause, not
/// the operation.
pub fn error_notice(
    ui: &mut Ui,
    envelope: &ErrorEnvelope,
    context: &str,
) -> Option<ErrorNoticeAction> {
    let mut chosen = None;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        let message = if context.is_empty() {
            envelope.message.clone()
        } else {
            format!("{context} {}", envelope.message)
        };
        let alert =
            ui.label(RichText::new(format!("✕ {message}")).color(ui.visuals().error_fg_color));
        alert.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &message);
            info.current_text_value = Some(message.clone());
            info
        });
        ui.ctx().accesskit_node_builder(alert.id, |node| {
            node.set_role(egui::accesskit::Role::Alert);
            node.set_label(message.clone());
        });

        match control_for(envelope) {
            ErrorControl::None => {}
            ErrorControl::Retry(label) => {
                if ui.button(label).clicked() {
                    chosen = Some(ErrorNoticeAction::Retry);
                }
            }
            ErrorControl::Settings(label) => {
                if ui.button(label).clicked() {
                    chosen = Some(ErrorNoticeAction::OpenSettings);
                }
            }
            ErrorControl::InstallOrImportModel => {
                ui.horizontal_wrapped(|ui| {
                    if ui.button(INSTALL_MODEL_LABEL).clicked() {
                        chosen = Some(ErrorNoticeAction::InstallSpeechModel);
                    }
                    if ui.button(IMPORT_MODEL_LABEL).clicked() {
                        chosen = Some(ErrorNoticeAction::ImportModelFile);
                    }
                });
            }
        }

        ui.label(
            RichText::new(format!("Diagnostic id: {}", envelope.diagnostic_id))
                .small()
                .color(ui.visuals().weak_text_color()),
        );
    });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_domain::error::{AppError, AppErrorCode, DiagnosticLog};
    use plaudon_domain::time::Timestamp;

    fn envelope_for(code: AppErrorCode) -> ErrorEnvelope {
        let mut log = DiagnosticLog::new();
        AppError::new(code, "internal detail").to_envelope(&mut log, Timestamp::now())
    }

    #[test]
    fn every_retryable_code_offers_a_control_unless_the_remedy_is_physical() {
        for code in AppErrorCode::ALL {
            let envelope = envelope_for(*code);
            if !envelope.retryable {
                continue;
            }
            let control = control_for(&envelope);
            if envelope.action == Some(ErrorAction::ChargeDevice) {
                // Charging a device is not something a button can do, and
                // offering Retry next to "charge the device" would send the
                // user round the same failure.
                assert_eq!(control, ErrorControl::None, "{code}");
                continue;
            }
            assert!(
                !matches!(control, ErrorControl::None),
                "{code} is retryable but offers no way to retry"
            );
        }
    }

    #[test]
    fn a_retryable_envelope_without_a_declared_action_still_offers_retry() {
        let mut envelope = envelope_for(AppErrorCode::InternalError);
        envelope.action = None;
        envelope.retryable = true;
        assert_eq!(control_for(&envelope), ErrorControl::Retry("Retry"));
    }

    #[test]
    fn a_physical_remedy_offers_no_control_rather_than_a_fake_one() {
        let mut envelope = envelope_for(AppErrorCode::InternalError);
        envelope.action = Some(ErrorAction::ChargeDevice);
        envelope.retryable = true;
        assert_eq!(control_for(&envelope), ErrorControl::None);
    }

    #[test]
    fn the_import_action_gets_the_import_wording() {
        let mut envelope = envelope_for(AppErrorCode::InternalError);
        envelope.action = Some(ErrorAction::RetryImport);
        assert_eq!(control_for(&envelope), ErrorControl::Retry("Retry import"));
    }
}
