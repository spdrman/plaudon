//! The setup wizard (App. B.8, spec §4.2A).
//!
//! Exactly three options. Each branch states what it involves, and none of
//! them silently becomes another: a slow or unsupported direct connection
//! never switches to the cloud on its own, and the account route says plainly
//! that it is PLAUD Cloud and needs a network.

use egui::Ui;

/// The three ways to bring recordings in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupOption {
    /// Discover, check support, authorize, preview, import.
    Direct,
    /// The official cloud command-line tool.
    Account,
    /// Files already on this computer.
    Files,
}

/// The options, in the order the appendix lists them.
pub const SETUP_OPTIONS: [SetupOption; 3] = [
    SetupOption::Direct,
    SetupOption::Account,
    SetupOption::Files,
];

impl SetupOption {
    /// Title of the option.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Direct => "Connect directly (offline after setup)",
            Self::Account => "Use PLAUD account",
            Self::Files => "Import files",
        }
    }

    /// One-line summary of what the option involves.
    #[must_use]
    pub const fn summary(self) -> &'static str {
        match self {
            Self::Direct => "Discover, check support, authorize by an approved method, preview the recordings on the device, import and choose local processing.",
            Self::Account => "The official cloud command-line tool, for historical and account-side recordings.",
            Self::Files => "Works without hardware, an account or a network connection.",
        }
    }
}

/// What the wizard asked the screen to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardOutcome {
    /// Nothing this frame.
    None,
    /// Close the wizard.
    Close,
    /// Start a scan.
    Scan,
    /// Open the file dialog.
    ImportFiles,
    /// Go to the settings destination.
    OpenAccountSettings,
}

/// Render the wizard. `choice` is the user's current selection.
pub fn setup_wizard(
    ui: &mut Ui,
    choice: &mut Option<SetupOption>,
    scanning: bool,
) -> WizardOutcome {
    let mut outcome = WizardOutcome::None;
    ui.separator();
    ui.heading("Add a PLAUD device");
    if ui.button("Close setup").clicked() {
        outcome = WizardOutcome::Close;
    }
    ui.label("How do you want to bring recordings in?");
    for option in SETUP_OPTIONS {
        if ui.radio(*choice == Some(option), option.title()).clicked() {
            *choice = Some(option);
        }
        ui.label(option.summary());
    }

    match *choice {
        None => {}
        Some(SetupOption::Direct) => {
            ui.separator();
            ui.label(
                "1. Scan for nearby devices. Bluetooth permission is requested only when you scan.",
            );
            ui.label("2. Check support. Each device is shown with the status recorded for its exact model and firmware, and anything not yet verified is opt-in.");
            ui.label("3. Authorize with a token you already hold for your own device.");
            ui.label("4. Preview the recordings on the device, then import the ones you choose.");
            ui.label("A scan lists only what the device holds and advertises right now. The phone app may already have removed recordings from the device, so a direct scan is not a complete recording history.");
            ui.label("A slow Bluetooth transfer is shown as it is. It never switches to the cloud on its own.");
            if ui
                .add_enabled(!scanning, egui::Button::new("Scan now"))
                .clicked()
            {
                outcome = WizardOutcome::Scan;
            }
        }
        Some(SetupOption::Account) => {
            ui.separator();
            ui.label("This route uses the PLAUD cloud through the official command-line tool. It is PLAUD Cloud, not a direct connection to a device, and it needs an internet connection.");
            ui.label("Account sign-in and the tool's status are managed in Settings.");
            if ui.button("Open account settings").clicked() {
                outcome = WizardOutcome::OpenAccountSettings;
            }
        }
        Some(SetupOption::Files) => {
            ui.separator();
            ui.label("Choose audio files from this computer. This works without hardware, an account or a network connection.");
            if ui.button("Choose audio files").clicked() {
                outcome = WizardOutcome::ImportFiles;
            }
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_three_options_and_each_says_what_it_involves() {
        assert_eq!(SETUP_OPTIONS.len(), 3);
        for option in SETUP_OPTIONS {
            assert_ne!(option.title(), "");
            assert!(option.summary().len() > 20, "{option:?} has no summary");
        }
    }

    #[test]
    fn the_account_route_names_the_cloud_it_uses() {
        let summary = SetupOption::Account.summary();
        assert!(summary.contains("cloud"), "{summary}");
    }
}
