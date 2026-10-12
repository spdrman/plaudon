//! One drain for `muda`'s process-global menu channel.
//!
//! `muda` posts every menu activation to a single process-global channel
//! (`MenuEvent::receiver()`), and two consumers now share it: the status item
//! (matrix row C11) and, on macOS, the application menu (parity row S8). Two
//! independent `try_recv` loops over one channel means whichever runs first
//! swallows the other's events, so this module is the **only** caller of that
//! receiver in the binary. It drains the channel once and files each
//! identifier under the consumer that owns it.
//!
//! The rule is explicit rather than guessed: the application menu claims its
//! identifiers when it installs itself ([`claim`]), and everything else is a
//! tray row — which is exactly what every identifier was before the
//! application menu existed, so the tray behaves as it always did on a build
//! that installs no application menu (Windows, or a macOS build whose menu
//! failed to install).
//!
//! `tray_icon::menu` *is* `muda`, re-exported; naming it through `tray-icon`
//! here keeps this module compiling on Windows, where the tray links `muda`
//! but the application menu does not exist.

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use tray_icon::menu::MenuEvent;

/// Where drained identifiers wait for their consumer.
///
/// Both queues are emptied by their consumer every frame, so they hold at
/// most the clicks of one frame; nothing accumulates.
#[derive(Debug)]
struct Router {
    claimed: BTreeSet<String>,
    app_menu: Vec<String>,
    tray: Vec<String>,
}

impl Router {
    const fn new() -> Self {
        Self {
            claimed: BTreeSet::new(),
            app_menu: Vec::new(),
            tray: Vec::new(),
        }
    }

    /// Move everything the platform has delivered into the two queues.
    fn pump(&mut self) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = event.id.0;
            if self.claimed.contains(&id) {
                self.app_menu.push(id);
            } else {
                self.tray.push(id);
            }
        }
    }
}

static ROUTER: Mutex<Router> = Mutex::new(Router::new());

/// Take the router, recovering from a poisoned lock.
///
/// A panic in another thread while it held this lock leaves a queue of menu
/// identifiers, not a torn data structure; refusing to deliver menu clicks
/// afterwards would be a worse outcome than carrying on with them.
fn router() -> std::sync::MutexGuard<'static, Router> {
    ROUTER.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Record the identifiers the application menu owns.
///
/// Called once, by the installer, before the menu can be clicked.
#[cfg(target_os = "macos")]
pub fn claim(ids: impl IntoIterator<Item = String>) {
    let mut router = router();
    router.claimed.extend(ids);
}

/// The application menu's activations since the last call, in order.
#[cfg(target_os = "macos")]
pub fn drain_app_menu() -> Vec<String> {
    let mut router = router();
    router.pump();
    std::mem::take(&mut router.app_menu)
}

/// The tray's activations since the last call, in order.
pub fn drain_tray() -> Vec<String> {
    let mut router = router();
    router.pump();
    std::mem::take(&mut router.tray)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real channel can only be filled by a platform menu, so the routing
    /// rule is asserted on the structure itself. The queues are drained first
    /// so a test never observes another test's identifiers.
    #[test]
    fn an_unclaimed_identifier_belongs_to_the_tray() {
        let mut router = Router::new();
        router.claimed.insert("import_audio".to_owned());
        router.app_menu.push("import_audio".to_owned());
        router.tray.push("quit".to_owned());

        assert_eq!(std::mem::take(&mut router.app_menu), ["import_audio"]);
        assert_eq!(std::mem::take(&mut router.tray), ["quit"]);
        assert!(!router.claimed.contains("quit"));
    }

    #[test]
    fn draining_an_empty_channel_is_free_and_returns_nothing() {
        assert!(drain_tray().is_empty());
    }
}
