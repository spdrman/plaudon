//! The activity bell in the header (parity row J12).
//!
//! The shell being replaced had no such control: the activity history existed
//! but nothing in the chrome said that anything had happened, so a user who
//! was not looking at the history never learned that a job had finished or
//! that something needed a decision.
//!
//! Two rules decide how it is drawn, and they are the reason it is a button
//! with words rather than the coloured dot a web header would use:
//!
//! * **Status is never colour alone (§5.3).** The count is *text*, the
//!   accessible name is a sentence that states the count, and the colour only
//!   reinforces what both already say. A dot would be invisible to a screen
//!   reader and ambiguous to anyone who cannot distinguish it from the
//!   background.
//! * **Nothing is invented.** Zero unread entries is drawn as the bell with
//!   no number and announced as "no unread notifications" — not hidden (the
//!   way to the history would disappear with it) and not decorated with a
//!   badge that would imply something is waiting.
//!
//! The count itself is never computed here. It arrives from
//! [`crate::activity::unread`], which is the same function the history itself
//! counts with, so the header and the page cannot disagree.

use egui::{accesskit, RichText};

use crate::theme::{Tokens, MIN_TARGET_SIZE};

/// The word the control is drawn and announced with.
///
/// Not a glyph: the default font Plaudon ships has no bell, and a glyph that
/// renders as a replacement box in one font and as a picture in another is
/// not a label anyone can rely on.
pub const BELL_WORD: &str = "Activity";

/// The visible text of the bell: the word, and the count when there is one.
#[must_use]
pub fn caption(unread: usize) -> String {
    if unread == 0 {
        BELL_WORD.to_owned()
    } else {
        format!("{BELL_WORD} {unread}")
    }
}

/// What a screen reader announces, which always states the count in words.
///
/// Singular and plural are written out rather than suffixed with `(s)`,
/// because this string is read aloud.
#[must_use]
pub fn label(unread: usize) -> String {
    match unread {
        0 => format!("{BELL_WORD}: no unread notifications"),
        1 => format!("{BELL_WORD}: 1 unread notification"),
        count => format!("{BELL_WORD}: {count} unread notifications"),
    }
}

/// Draw the bell. Returns `true` when the user activated it.
///
/// Activation is a click, Enter or Space: it is an ordinary egui button, so
/// it is in the Tab order and the focus ring is the shell's own.
pub fn activity_bell(ui: &mut egui::Ui, unread: usize) -> bool {
    let tokens = Tokens::get(ui.ctx());
    let caption = if unread == 0 {
        RichText::new(caption(unread)).color(tokens.color.text_secondary)
    } else {
        // `status_running` is the role the status bar already uses for "work
        // the user has not dealt with yet"; the bold weight carries the same
        // emphasis for anyone the colour does not reach.
        RichText::new(caption(unread))
            .color(tokens.color.status_running)
            .strong()
    };
    let response = ui.add(
        egui::Button::new(caption)
            .min_size(egui::vec2(MIN_TARGET_SIZE, MIN_TARGET_SIZE))
            .corner_radius(tokens.radius.small),
    );
    let announced = label(unread);
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_role(accesskit::Role::Button);
        node.set_label(announced.clone());
    });
    response.on_hover_text(announced).clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_accessible_name_states_the_count_in_words() {
        assert_eq!(label(0), "Activity: no unread notifications");
        assert_eq!(label(1), "Activity: 1 unread notification");
        assert_eq!(label(7), "Activity: 7 unread notifications");
    }

    #[test]
    fn an_empty_history_is_drawn_without_a_number() {
        assert_eq!(caption(0), "Activity");
        assert_eq!(caption(3), "Activity 3");
    }
}
