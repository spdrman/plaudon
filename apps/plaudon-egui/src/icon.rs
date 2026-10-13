//! The application icon, read from the brand kit.
//!
//! `brand/kit/icons/desktop/` is the single source of truth for Plaudon's
//! artwork: the macOS bundle copies `plaudon.icns` from it, the Windows
//! resource script links `plaudon.ico` from it, and the window the shell
//! opens carries the PNG embedded here. Nothing in `apps/` keeps a second
//! copy, so the icon the window shows cannot drift from the icon the
//! installers ship.
//!
//! 256×256 is the size embedded: winit hands the image to the compositor,
//! which scales it down for the title bar and up for the Dock and the
//! task switcher, and 256 is the largest size every platform's window icon
//! path accepts without complaint.

/// The brand icon as a PNG, embedded at compile time.
pub const ICON_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../brand/kit/icons/desktop/plaudon-icon-256.png"
));

/// The brand icon decoded for `winit`, or `None` if it could not be decoded.
///
/// A window with no icon is a cosmetic loss, not a reason to refuse to start,
/// so a decode failure is logged and the window opens with the platform
/// default.
#[must_use]
pub fn window_icon() -> Option<egui::IconData> {
    match image::load_from_memory_with_format(ICON_PNG, image::ImageFormat::Png) {
        Ok(image) => {
            let image = image.into_rgba8();
            let (width, height) = image.dimensions();
            Some(egui::IconData {
                rgba: image.into_raw(),
                width,
                height,
            })
        }
        Err(error) => {
            tracing::warn!(%error, "the brand icon could not be decoded; the window opens without one");
            None
        }
    }
}

/// The brand icon at status-item size, embedded at compile time.
#[cfg(target_os = "windows")]
pub const TRAY_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../brand/kit/icons/desktop/plaudon-icon-32.png"
));

/// The 32×32 brand icon as RGBA for the notification area, or `None` if it
/// could not be decoded — in which case the tray falls back to the generated
/// glyph rather than going iconless.
#[cfg(target_os = "windows")]
#[must_use]
pub fn tray_rgba() -> Option<Vec<u8>> {
    match image::load_from_memory_with_format(TRAY_PNG, image::ImageFormat::Png) {
        Ok(image) => Some(image.into_rgba8().into_raw()),
        Err(error) => {
            tracing::warn!(%error, "the brand tray icon could not be decoded");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_icon_is_the_brand_kits_png() {
        assert_eq!(&ICON_PNG[1..4], b"PNG", "the embedded asset must be a PNG");
        let expected = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../brand/kit/icons/desktop/plaudon-icon-256.png"),
        )
        .expect("the brand kit is checked out");
        assert_eq!(ICON_PNG, expected.as_slice(), "no second copy may exist");
    }

    #[test]
    fn the_window_icon_decodes_to_a_256_square() {
        let icon = window_icon().expect("the brand PNG decodes");
        assert_eq!((icon.width, icon.height), (256, 256));
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
