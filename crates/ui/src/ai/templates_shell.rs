//! How the Templates destination plugs into the shell (matrix row A12).
//!
//! The screen itself is [`TemplatesPlaceholder`], which states what a
//! template picker would need and does not pretend to offer one. This is the
//! three lines that make it a [`Screen`], so the destination renders the
//! honest placeholder rather than the registry's "not built yet" state —
//! those are different claims, and the user is owed the first one.
//!
//! **Temporary.** EPIC O task O2.6 (#224) adds the template commands and a
//! real `plaudon_ui::screens::templates::TemplatesScreen`. When it lands,
//! this file and [`TemplatesPlaceholder`] are deleted and the binary's
//! registration line names that screen instead.

use crate::shell::route::Destination;
use crate::shell::screen::{Screen, ScreenCtx};

use super::templates::TemplatesPlaceholder;

/// The Templates destination, as the shell sees it.
#[derive(Debug, Default)]
pub struct TemplatesShellScreen {
    screen: TemplatesPlaceholder,
}

impl TemplatesShellScreen {
    /// A fresh screen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Screen for TemplatesShellScreen {
    fn destination(&self) -> Destination {
        Destination::Templates
    }

    fn ui(&mut self, ui: &mut egui::Ui, _cx: &mut ScreenCtx<'_>) {
        self.screen.ui(ui);
    }
}
