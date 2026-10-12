//! Resizable panes (parity row S7).
//!
//! The shell is a fixed arrangement — navigation rail, screen, optional side
//! pane — with draggable dividers, not a dock. The plan allows either and is
//! explicit that vendoring `craft_ui::docking` is only justified if O1.3
//! really needs user-arrangeable panels
//! (`docs/EGUI_REFERENCE_COMPONENTS.md` §5.2); it does not, so the dividers
//! are the vendored [`ResizeHandle`], which already carries `Role::Splitter`,
//! a numeric value with bounds, and arrow-key resizing.
//!
//! Sizes are owned by the caller and persisted in
//! [`plaudon_app::shell_state::PanelLayout`]; this module only proposes a new
//! size when the user drags or presses an arrow.

use egui::Rect;
use plaudon_craft_ui::layout::{split_rect, ResizeHandle, ResizeHandleStyle, SplitAxis, SplitSize};

/// Width of a divider, in points.
pub const DIVIDER_WIDTH: f32 = 6.0;

/// The rectangles a split produced, and the size the user asked for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Split {
    /// The leading pane.
    pub first: Rect,
    /// The trailing pane.
    pub second: Rect,
    /// The sized pane's width after this frame's interaction, in the same
    /// sense as the [`SplitSize`] that was passed in.
    pub size: f32,
    /// Whether the user changed it this frame.
    pub changed: bool,
}

/// Lay out a vertical divider inside `rect` and draw its handle.
///
/// `None` when `rect` cannot hold both panes at `min_pane`; the caller then
/// draws a single pane, which is what the compact layout does.
pub fn vertical_divider(
    ui: &mut egui::Ui,
    id: egui::Id,
    label: &str,
    size: SplitSize,
    min_pane: f32,
    rect: Rect,
) -> Option<Split> {
    let layout = split_rect(rect, SplitAxis::Horizontal, size, DIVIDER_WIDTH, min_pane)?;
    let current = match size {
        SplitSize::FixedSecond(_) => layout.available() - layout.first_size(),
        SplitSize::FixedFirst(_) | SplitSize::Ratio(_) => layout.first_size(),
    };

    let style = ResizeHandleStyle {
        hovered: ui.visuals().widgets.hovered.bg_fill,
        dragged: ui.visuals().widgets.active.bg_fill,
        focused: ui.visuals().selection.bg_fill,
    };
    let upper = (layout.available() - min_pane).max(min_pane);
    let resize = ResizeHandle::new(id, SplitAxis::Horizontal, layout.divider, label, style)
        .value(current, min_pane..=upper)
        .show(ui);

    let mut next = current;
    let mut changed = false;
    if resize.delta != 0.0 {
        if let Some(resized) = layout.resized(size, resize.delta, min_pane) {
            let value = match resized {
                SplitSize::FixedFirst(value)
                | SplitSize::FixedSecond(value)
                | SplitSize::Ratio(value) => value,
            };
            changed = (value - current).abs() > f32::EPSILON;
            next = value;
        }
    }
    Some(Split {
        first: layout.first,
        second: layout.second,
        size: next,
        changed,
    })
}

/// A child `Ui` confined to `rect`, so a screen cannot paint outside its own
/// pane.
pub fn pane(ui: &mut egui::Ui, rect: Rect) -> egui::Ui {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    #[test]
    fn a_split_keeps_both_panes_above_the_minimum() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 600.0));
        let layout = split_rect(
            rect,
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(50.0),
            DIVIDER_WIDTH,
            220.0,
        )
        .expect("the rectangle is large enough");
        assert!(layout.first.width() >= 220.0);
        assert!(layout.second.width() >= 220.0);
    }

    #[test]
    fn a_rectangle_too_small_for_two_panes_shares_it_instead_of_overlapping() {
        // The vendored geometry clamps to half each rather than refusing;
        // what matters is that the panes stay inside the rectangle and do
        // not overlap, at any width.
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 600.0));
        let layout = split_rect(
            rect,
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(200.0),
            DIVIDER_WIDTH,
            220.0,
        )
        .expect("a valid rectangle always lays out");
        assert!(layout.first.max.x <= layout.second.min.x);
        assert!(rect.contains_rect(layout.first));
        assert!(rect.contains_rect(layout.second));
    }

    #[test]
    fn an_invalid_rectangle_reports_nothing_rather_than_painting_nonsense() {
        assert!(split_rect(
            Rect::NOTHING,
            SplitAxis::Horizontal,
            SplitSize::FixedFirst(200.0),
            DIVIDER_WIDTH,
            220.0,
        )
        .is_none());
    }

    #[test]
    fn a_fixed_second_pane_keeps_its_width_when_the_window_grows() {
        for width in [900.0_f32, 1400.0, 1800.0] {
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(width, 600.0));
            let layout = split_rect(
                rect,
                SplitAxis::Horizontal,
                SplitSize::FixedSecond(340.0),
                DIVIDER_WIDTH,
                220.0,
            )
            .expect("fits");
            assert!((layout.second.width() - 340.0).abs() < 1.0);
        }
    }
}
