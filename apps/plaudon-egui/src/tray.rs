//! The system tray (matrix row C11, "tray — where supported").
//!
//! The tray *model* — what rows exist, what they are called, which are
//! toggles — belongs to `plaudon_platform::tray`, which has no windowing
//! dependency and is unit-tested without one. The *backend* has to live in the
//! binary, because a tray icon must be created on the thread that owns the
//! event loop and its events are delivered to a process-global channel the
//! render loop drains. A library crate cannot honestly own either.
//!
//! Where a tray is drawn:
//!
//! | Platform | Backend | Why |
//! | --- | --- | --- |
//! | macOS | `tray-icon` 0.25 (`NSStatusItem`) | a status item is the native idiom |
//! | Windows | `tray-icon` 0.25 (`Shell_NotifyIcon`) | the notification area is the native idiom |
//! | Linux | none | `tray-icon`'s Linux backends need GTK and `libappindicator` system libraries, which the headless build and the MARS container do not carry, and a tray additionally needs a StatusNotifierItem host that many desktops do not run. [`plaudon_platform::tray::NativeTray`] reports that in words instead of linking a dependency that would fail to build. |
//!
//! Nothing here ever reports success it did not have: on a platform with no
//! backend, [`host_tray`] returns the adapter whose `apply` fails with the
//! honest reason, and [`host_support`] is what the Settings screen renders.

use plaudon_platform::tray::{NativeTray, Tray, TraySupport};

/// Side length of the generated status icon, in pixels.
#[cfg(any(target_os = "macos", target_os = "windows"))]
const ICON_SIZE: u32 = 32;

/// The tray adapter for this host.
#[must_use]
pub fn host_tray() -> Box<dyn Tray> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        match backend::SystemTray::new() {
            Ok(tray) => Box::new(tray),
            Err(reason) => {
                tracing::warn!(%reason, "the system tray could not be created");
                Box::new(NativeTray::new())
            }
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Box::new(NativeTray::new())
    }
}

/// What this build can offer on this host.
///
/// Differs from [`plaudon_platform::tray::support`] precisely because that
/// function answers for a build that links no backend; this one answers for
/// the shipped binary.
#[must_use]
pub fn host_support() -> TraySupport {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        TraySupport::Supported {
            backend: "tray-icon",
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        plaudon_platform::tray::support()
    }
}

/// The default menu: show the window, and the four destinations a background
/// user is most likely to want.
#[must_use]
pub fn default_model(local_only: bool) -> plaudon_platform::tray::TrayModel {
    use plaudon_platform::tray::{TrayItem, TrayModel};
    TrayModel::new(
        "Plaudon",
        vec![
            TrayItem::action("show", "Show Plaudon"),
            TrayItem::separator(),
            TrayItem::action("open_library", "Library"),
            TrayItem::action("open_jobs", "Jobs"),
            TrayItem::action("open_settings", "Settings"),
            TrayItem::separator(),
            TrayItem::toggle("local_only", "Local-only mode", local_only),
            TrayItem::separator(),
            TrayItem::action("quit", "Quit Plaudon"),
        ],
    )
}

/// A 32×32 RGBA status glyph, generated rather than shipped as an asset.
///
/// A ring with a solid centre: legible at status-item size, and on macOS it is
/// installed as a template image so the system tints it for the menu bar's
/// appearance instead of the application guessing light or dark.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn status_icon_rgba() -> Vec<u8> {
    let size = ICON_SIZE as i32;
    let mut pixels = Vec::with_capacity((ICON_SIZE * ICON_SIZE * 4) as usize);
    let centre = f64::from(size - 1) / 2.0;
    let outer = f64::from(size) * 0.46;
    let inner = f64::from(size) * 0.32;
    let core = f64::from(size) * 0.16;
    for y in 0..size {
        for x in 0..size {
            let dx = f64::from(x) - centre;
            let dy = f64::from(y) - centre;
            let distance = dx.mul_add(dx, dy * dy).sqrt();
            let solid = distance <= core || (distance <= outer && distance >= inner);
            let alpha = if solid { 0xFF } else { 0x00 };
            pixels.extend_from_slice(&[0x00, 0x00, 0x00, alpha]);
        }
    }
    pixels
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod backend {
    use std::sync::Mutex;

    use plaudon_platform::tray::{Tray, TrayError, TrayEvent, TrayItemId, TrayItemKind, TrayModel};
    use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

    /// A real status item.
    ///
    /// `TrayIcon` is not `Send`, and on macOS it must only be touched from the
    /// main thread; the whole struct therefore lives on the thread that built
    /// it, and the `unsafe impl Send + Sync` the trait would need is
    /// deliberately *not* written. Instead the handle sits behind a mutex and
    /// the type is only ever constructed and used from the eframe update
    /// callback, which runs on the main thread.
    pub struct SystemTray {
        icon: Mutex<TrayIcon>,
    }

    impl std::fmt::Debug for SystemTray {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("SystemTray(tray-icon)")
        }
    }

    // Safety is not claimed: `TrayIcon` contains platform handles that are not
    // `Send`. The shell constructs and uses the tray only on the main thread,
    // and the mutex makes the shared reference sound for the `Tray` trait's
    // `Send + Sync` bound. This is the one place the invariant is stated, and
    // `host_tray` is the only constructor.
    #[allow(unsafe_code)]
    unsafe impl Send for SystemTray {}
    #[allow(unsafe_code)]
    unsafe impl Sync for SystemTray {}

    impl SystemTray {
        /// Create the status item. Must be called on the main thread, after
        /// the event loop has started.
        pub fn new() -> Result<Self, String> {
            let rgba = super::status_icon_rgba();
            let icon = Icon::from_rgba(rgba, super::ICON_SIZE, super::ICON_SIZE)
                .map_err(|error| error.to_string())?;
            let builder = TrayIconBuilder::new()
                .with_tooltip("Plaudon")
                .with_icon(icon)
                .with_icon_as_template(true);
            let tray = builder.build().map_err(|error| error.to_string())?;
            Ok(Self {
                icon: Mutex::new(tray),
            })
        }

        fn build_menu(model: &TrayModel) -> Result<Menu, TrayError> {
            let menu = Menu::new();
            for item in &model.items {
                let appended = match &item.kind {
                    TrayItemKind::Separator => menu.append(&PredefinedMenuItem::separator()),
                    TrayItemKind::Action => menu.append(&MenuItem::with_id(
                        item.id.as_str(),
                        &item.label,
                        item.enabled,
                        None,
                    )),
                    TrayItemKind::Toggle { checked } => menu.append(&CheckMenuItem::with_id(
                        item.id.as_str(),
                        &item.label,
                        item.enabled,
                        *checked,
                        None,
                    )),
                };
                appended.map_err(|error| TrayError::Unsupported {
                    reason: format!("the tray menu could not be built: {error}"),
                })?;
            }
            Ok(menu)
        }
    }

    impl Tray for SystemTray {
        fn apply(&self, model: &TrayModel) -> Result<(), TrayError> {
            model.validate()?;
            let menu = Self::build_menu(model)?;
            let icon = self.icon.lock().map_err(|_| TrayError::Unsupported {
                reason: "the tray handle was poisoned by an earlier panic".to_owned(),
            })?;
            icon.set_menu(Some(Box::new(menu)));
            icon.set_tooltip(Some(&model.tooltip))
                .map_err(|error| TrayError::Unsupported {
                    reason: format!("the tray tooltip could not be set: {error}"),
                })?;
            icon.set_visible(true)
                .map_err(|error| TrayError::Unsupported {
                    reason: format!("the tray icon could not be shown: {error}"),
                })
        }

        fn poll_events(&self) -> Vec<TrayEvent> {
            let mut events = Vec::new();
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                // Only a click on the icon itself asks for the window; hover
                // and move events are noise and must not raise the window.
                if matches!(event, TrayIconEvent::Click { .. }) {
                    events.push(TrayEvent::IconActivated);
                }
            }
            // Not `MenuEvent::receiver()`: that channel is shared with the
            // macOS application menu, so one router hands each consumer its
            // own identifiers (see `crate::menu_events`).
            for id in crate::menu_events::drain_tray() {
                events.push(TrayEvent::MenuItemSelected(TrayItemId::new(&id)));
            }
            events
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plaudon_platform::tray::TrayItemKind;

    #[test]
    fn the_default_menu_has_unique_ids_and_a_quit_row() {
        let model = default_model(true);
        model.validate().expect("the shipped menu is well formed");
        assert!(model.items.iter().any(|item| item.id.as_str() == "quit"));
        assert!(model.items.iter().any(|item| item.id.as_str() == "show"));
    }

    #[test]
    fn the_local_only_row_reflects_the_stored_setting() {
        for enabled in [true, false] {
            let model = default_model(enabled);
            let row = model
                .items
                .iter()
                .find(|item| item.id.as_str() == "local_only")
                .expect("the toggle exists");
            assert_eq!(row.kind, TrayItemKind::Toggle { checked: enabled });
        }
    }

    #[test]
    fn support_is_reported_honestly_for_this_host() {
        let support = host_support();
        if cfg!(any(target_os = "macos", target_os = "windows")) {
            assert!(
                support.is_supported(),
                "a backend is linked on this platform"
            );
            assert_eq!(support.reason(), None);
        } else {
            assert!(!support.is_supported());
            assert!(
                support.reason().is_some_and(|reason| !reason.is_empty()),
                "an unavailable tray must say why"
            );
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn a_platform_without_a_backend_fails_instead_of_pretending() {
        let tray = host_tray();
        let error = tray
            .apply(&default_model(false))
            .expect_err("no icon can be drawn");
        assert!(matches!(
            error,
            plaudon_platform::tray::TrayError::Unsupported { .. }
        ));
        assert!(
            tray.poll_events().is_empty(),
            "polling an idle tray yields no event"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    #[test]
    fn the_menu_builder_rejects_a_model_with_a_duplicate_id() {
        use plaudon_platform::tray::{TrayItem, TrayModel};
        let model = TrayModel::new(
            "Plaudon",
            vec![
                TrayItem::action("show", "Show Plaudon"),
                TrayItem::action("show", "Show again"),
            ],
        );
        assert!(model.validate().is_err());
    }
}
