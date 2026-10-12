//! The tray, modelled as data — and an honest answer about backends.
//!
//! # The decision
//!
//! **This build ships the tray *model* and no tray backend.** [`support`]
//! reports [`TraySupport::UnavailableInBuild`] with a reason on every
//! platform, [`NativeTray::apply`] returns that same reason as a
//! [`TrayError::Unsupported`], and nothing anywhere claims an icon was drawn.
//!
//! Why, when `tray-icon` exists and is already in `Cargo.lock`:
//!
//! * It is **not in the resolved dependency graph** of this workspace. Adding
//!   it widens the dependency and licence surface (it pulls the platform
//!   shell crates, and on Linux GTK/libayatana), which ADR-0009 requires a
//!   row in `docs/licensing/third-party-register.md` for, and which the EPIC
//!   O licence review has not covered.
//! * A tray icon is not a free-standing object: on macOS and Windows it must
//!   be created on the thread that owns the event loop and its menu events
//!   have to be pumped by that loop, so a real backend is a change to the
//!   **shell's** event loop (`apps/plaudon-egui`), not something a library
//!   crate can honestly own on its own.
//! * On Linux there is no tray at all unless the desktop runs a
//!   StatusNotifierItem host; several do not. A crate that reported
//!   "supported" there would be lying on a large fraction of machines.
//!
//! Migration plan row C11 says "tray — where supported", and this is the
//! truthful reading of *where supported*: the model, the menu ids, the event
//! vocabulary and the support report are real and tested, so wiring a backend
//! later is an additive change in the shell plus a licence row — and until
//! that happens the UI can ask [`support`] and simply not offer the setting.
//!
//! # The model
//!
//! A [`TrayModel`] is a tooltip plus a list of [`TrayItem`]s with stable ids;
//! the shell rebuilds it whenever its state changes and calls
//! [`Tray::apply`]. Events arrive by polling [`Tray::poll_events`] once per
//! frame, which matches how an egui shell already works and keeps the tray
//! from needing a channel into the view.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Mutex;

use crate::{lock, Platform};

/// Stable identifier of a tray menu item.
///
/// Stable means: the shell matches on it, so it is part of the contract
/// between the menu and the command it raises — not a label, which is user
/// visible text and may be translated.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TrayItemId(String);

impl TrayItemId {
    /// Wrap an identifier.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identifier as a string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TrayItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a row in the tray menu is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayItemKind {
    /// A plain command.
    Action,
    /// A command with a checkmark.
    Toggle {
        /// Whether the checkmark is currently shown.
        checked: bool,
    },
    /// A horizontal rule; carries no id and cannot be selected.
    Separator,
}

/// One row of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayItem {
    /// Stable id, empty for a [`TrayItemKind::Separator`].
    pub id: TrayItemId,
    /// User-visible label.
    pub label: String,
    /// Whether the row can be selected.
    pub enabled: bool,
    /// What kind of row this is.
    pub kind: TrayItemKind,
}

impl TrayItem {
    /// An enabled command row.
    #[must_use]
    pub fn action(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: TrayItemId::new(id),
            label: label.into(),
            enabled: true,
            kind: TrayItemKind::Action,
        }
    }

    /// An enabled toggle row.
    #[must_use]
    pub fn toggle(id: impl Into<String>, label: impl Into<String>, checked: bool) -> Self {
        Self {
            id: TrayItemId::new(id),
            label: label.into(),
            enabled: true,
            kind: TrayItemKind::Toggle { checked },
        }
    }

    /// A separator.
    #[must_use]
    pub fn separator() -> Self {
        Self {
            id: TrayItemId::new(String::new()),
            label: String::new(),
            enabled: false,
            kind: TrayItemKind::Separator,
        }
    }

    /// The same row, disabled.
    #[must_use]
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Whether this row carries an id a selection can name.
    #[must_use]
    pub fn is_selectable(&self) -> bool {
        !matches!(self.kind, TrayItemKind::Separator)
    }
}

/// Everything the tray shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayModel {
    /// Hover text. Generic: never a recording title or a file name.
    pub tooltip: String,
    /// Menu rows, top to bottom.
    pub items: Vec<TrayItem>,
}

impl TrayModel {
    /// Build a model.
    #[must_use]
    pub fn new(tooltip: impl Into<String>, items: Vec<TrayItem>) -> Self {
        Self {
            tooltip: tooltip.into(),
            items,
        }
    }

    /// Check the model is well formed before it reaches a backend.
    ///
    /// # Errors
    ///
    /// [`TrayError::EmptyItemId`] for a selectable row without an id and
    /// [`TrayError::DuplicateItemId`] when two rows share one, because a menu
    /// whose ids collide cannot route its own events.
    pub fn validate(&self) -> Result<(), TrayError> {
        let mut seen = BTreeSet::new();
        for item in &self.items {
            if !item.is_selectable() {
                continue;
            }
            if item.id.as_str().is_empty() {
                return Err(TrayError::EmptyItemId {
                    label: item.label.clone(),
                });
            }
            if !seen.insert(item.id.clone()) {
                return Err(TrayError::DuplicateItemId {
                    id: item.id.clone(),
                });
            }
        }
        Ok(())
    }
}

/// Why a tray operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrayError {
    /// There is no tray backend to apply the model to.
    #[error("no tray is available: {reason}")]
    Unsupported {
        /// Honest reason, safe to show in the settings screen.
        reason: String,
    },
    /// A selectable row has no id.
    #[error("the tray menu row {label:?} has no id")]
    EmptyItemId {
        /// Label of the offending row.
        label: String,
    },
    /// Two rows share an id.
    #[error("the tray menu uses the id {id} twice")]
    DuplicateItemId {
        /// The duplicated id.
        id: TrayItemId,
    },
}

/// Something the user did to the tray.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayEvent {
    /// The icon itself was clicked: show and focus the window.
    IconActivated,
    /// A menu row was selected.
    MenuItemSelected(TrayItemId),
}

/// How much of a tray this build can actually offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraySupport {
    /// A backend is linked and will draw an icon.
    Supported {
        /// Which backend, for diagnostics.
        backend: &'static str,
    },
    /// The platform could support a tray, but this build links no backend.
    UnavailableInBuild {
        /// Why, in words the settings screen can show.
        reason: String,
    },
    /// The platform has no tray concept we can target.
    Unsupported {
        /// Why, in words the settings screen can show.
        reason: String,
    },
}

impl TraySupport {
    /// Whether an icon would actually appear.
    #[must_use]
    pub const fn is_supported(&self) -> bool {
        matches!(self, Self::Supported { .. })
    }

    /// The reason a tray is not available, or `None` when it is.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Supported { .. } => None,
            Self::UnavailableInBuild { reason } | Self::Unsupported { reason } => Some(reason),
        }
    }
}

/// What this build can offer on the host platform.
#[must_use]
pub fn support() -> TraySupport {
    support_for(Platform::host())
}

/// What this build can offer on a named platform.
///
/// Pure, so the per-platform wording is tested everywhere rather than only on
/// the platform that would show it.
#[must_use]
pub fn support_for(platform: Option<Platform>) -> TraySupport {
    match platform {
        Some(Platform::MacOs | Platform::Windows) => TraySupport::UnavailableInBuild {
            reason: "this build links no tray backend; the tray model is ready but no icon \
                     is drawn"
                .to_owned(),
        },
        Some(Platform::Linux) => TraySupport::UnavailableInBuild {
            reason: "this build links no tray backend, and a Linux tray additionally needs a \
                     StatusNotifierItem host, which many desktops do not run"
                .to_owned(),
        },
        None => TraySupport::Unsupported {
            reason: "this operating system has no tray Plaudon can target".to_owned(),
        },
    }
}

/// Apply a tray model and collect what the user did with it.
pub trait Tray: Send + Sync + fmt::Debug {
    /// Show `model`, replacing whatever was shown before.
    ///
    /// # Errors
    ///
    /// [`TrayError::Unsupported`] when no icon can be drawn, and the
    /// validation errors of [`TrayModel::validate`].
    fn apply(&self, model: &TrayModel) -> Result<(), TrayError>;

    /// Everything the user did since the last poll, oldest first.
    fn poll_events(&self) -> Vec<TrayEvent>;
}

/// The host tray adapter.
///
/// Validates the model — a malformed menu is a bug worth catching whether or
/// not an icon can be drawn — and then reports [`support`]'s reason instead of
/// pretending to have shown something. It never silently succeeds, so a shell
/// that ignores the error cannot end up believing a tray exists.
#[derive(Debug, Clone, Default)]
pub struct NativeTray;

impl NativeTray {
    /// Build the host adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Tray for NativeTray {
    fn apply(&self, model: &TrayModel) -> Result<(), TrayError> {
        model.validate()?;
        match support() {
            TraySupport::Supported { backend } => {
                // No backend is linked, so this arm is unreachable today; it
                // exists so that adding one is a change in `support_for` plus
                // this match, not a redesign.
                tracing::debug!(backend, "tray backend reported support but none is linked");
                Err(TrayError::Unsupported {
                    reason: format!("the {backend} backend is not linked into this build"),
                })
            }
            TraySupport::UnavailableInBuild { reason } | TraySupport::Unsupported { reason } => {
                Err(TrayError::Unsupported { reason })
            }
        }
    }

    fn poll_events(&self) -> Vec<TrayEvent> {
        Vec::new()
    }
}

impl<T: Tray + ?Sized> Tray for std::sync::Arc<T> {
    fn apply(&self, model: &TrayModel) -> Result<(), TrayError> {
        (**self).apply(model)
    }

    fn poll_events(&self) -> Vec<TrayEvent> {
        (**self).poll_events()
    }
}

/// In-memory tray for tests: records models, replays scripted events.
#[derive(Debug, Default)]
pub struct FakeTray {
    applied: Mutex<Vec<TrayModel>>,
    pending: Mutex<Vec<TrayEvent>>,
}

impl FakeTray {
    /// A fresh fake with nothing applied and no events.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue an event for the next [`Tray::poll_events`].
    pub fn push_event(&self, event: TrayEvent) {
        lock(&self.pending).push(event);
    }

    /// Every model applied so far, oldest first.
    #[must_use]
    pub fn applied(&self) -> Vec<TrayModel> {
        lock(&self.applied).clone()
    }

    /// The model currently shown, if any.
    #[must_use]
    pub fn current(&self) -> Option<TrayModel> {
        lock(&self.applied).last().cloned()
    }
}

impl Tray for FakeTray {
    fn apply(&self, model: &TrayModel) -> Result<(), TrayError> {
        model.validate()?;
        lock(&self.applied).push(model.clone());
        Ok(())
    }

    fn poll_events(&self) -> Vec<TrayEvent> {
        std::mem::take(&mut *lock(&self.pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> TrayModel {
        TrayModel::new(
            "Plaudon",
            vec![
                TrayItem::action("open", "Open Plaudon"),
                TrayItem::separator(),
                TrayItem::toggle("pause_jobs", "Pause processing", true),
                TrayItem::action("import", "Import audio…").disabled(),
                TrayItem::separator(),
                TrayItem::action("quit", "Quit"),
            ],
        )
    }

    #[test]
    fn a_well_formed_model_validates() {
        assert_eq!(model().validate(), Ok(()));
    }

    #[test]
    fn item_state_is_part_of_the_model() {
        let model = model();
        assert_eq!(model.tooltip, "Plaudon");
        assert_eq!(model.items.len(), 6);
        assert!(model.items[0].enabled);
        assert_eq!(model.items[2].kind, TrayItemKind::Toggle { checked: true });
        assert!(!model.items[3].enabled, "a disabled row stays disabled");
        assert!(!model.items[1].is_selectable(), "a separator has no id");
        assert_eq!(model.items[5].id.as_str(), "quit");
        assert_eq!(model.items[5].id.to_string(), "quit");
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let model = TrayModel::new(
            "Plaudon",
            vec![TrayItem::action("open", "A"), TrayItem::action("open", "B")],
        );
        assert_eq!(
            model.validate(),
            Err(TrayError::DuplicateItemId {
                id: TrayItemId::new("open")
            })
        );
    }

    #[test]
    fn a_selectable_row_without_an_id_is_rejected() {
        let model = TrayModel::new("Plaudon", vec![TrayItem::action("", "Nameless")]);
        assert_eq!(
            model.validate(),
            Err(TrayError::EmptyItemId {
                label: "Nameless".to_owned()
            })
        );
    }

    #[test]
    fn support_is_reported_honestly_for_every_platform() {
        for platform in [Platform::MacOs, Platform::Windows] {
            let support = support_for(Some(platform));
            assert!(!support.is_supported());
            assert!(matches!(support, TraySupport::UnavailableInBuild { .. }));
            assert!(support
                .reason()
                .expect("a reason")
                .contains("no tray backend"));
        }

        let linux = support_for(Some(Platform::Linux));
        assert!(!linux.is_supported());
        assert!(linux
            .reason()
            .expect("a reason")
            .contains("StatusNotifierItem"));

        let unknown = support_for(None);
        assert!(matches!(unknown, TraySupport::Unsupported { .. }));
        assert!(!unknown.is_supported());
    }

    #[test]
    fn the_host_report_never_claims_an_icon_is_drawn() {
        let support = support();
        assert!(
            !support.is_supported(),
            "no tray backend is linked; claiming support would be false"
        );
        assert!(support.reason().is_some_and(|reason| !reason.is_empty()));
    }

    #[test]
    fn the_native_tray_refuses_instead_of_silently_doing_nothing() {
        let tray = NativeTray::new();
        let err = tray.apply(&model()).expect_err("no backend is linked");
        assert!(matches!(err, TrayError::Unsupported { .. }));
        assert_eq!(
            err.to_string(),
            format!(
                "no tray is available: {}",
                support().reason().expect("a reason")
            )
        );
        assert!(
            tray.poll_events().is_empty(),
            "a tray that was never shown produces no events"
        );
    }

    #[test]
    fn the_native_tray_still_catches_a_malformed_menu() {
        let tray = NativeTray::new();
        let broken = TrayModel::new(
            "Plaudon",
            vec![TrayItem::action("quit", "A"), TrayItem::action("quit", "B")],
        );
        assert_eq!(
            tray.apply(&broken),
            Err(TrayError::DuplicateItemId {
                id: TrayItemId::new("quit")
            })
        );
    }

    #[test]
    fn the_fake_records_models_and_replays_events() {
        let tray = FakeTray::new();
        assert!(tray.current().is_none());
        tray.apply(&model()).expect("apply");

        let updated = TrayModel::new("Plaudon — 2 jobs", vec![TrayItem::action("quit", "Quit")]);
        tray.apply(&updated).expect("apply");
        assert_eq!(tray.applied().len(), 2);
        assert_eq!(tray.current(), Some(updated));

        tray.push_event(TrayEvent::IconActivated);
        tray.push_event(TrayEvent::MenuItemSelected(TrayItemId::new("quit")));
        assert_eq!(
            tray.poll_events(),
            vec![
                TrayEvent::IconActivated,
                TrayEvent::MenuItemSelected(TrayItemId::new("quit")),
            ]
        );
        assert!(tray.poll_events().is_empty(), "polling drains the queue");
    }

    #[test]
    fn the_fake_rejects_the_same_malformed_menu_the_real_one_does() {
        let tray = FakeTray::new();
        let broken = TrayModel::new("Plaudon", vec![TrayItem::action("", "Nameless")]);
        assert!(tray.apply(&broken).is_err());
        assert!(
            tray.applied().is_empty(),
            "a rejected model must not be recorded as shown"
        );
    }

    #[test]
    fn the_port_is_usable_as_a_trait_object() {
        let tray: Box<dyn Tray> = Box::new(FakeTray::new());
        tray.apply(&model()).expect("apply");
        assert!(tray.poll_events().is_empty(), "nothing was clicked");
        let _: &dyn Tray = &NativeTray::new();
    }
}
