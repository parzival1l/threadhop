//! Bookmark-note prompt: edit the note on an existing bookmark.
//!
//! **Pre-pop stub.** The struct shape and `draw` entry point are wired
//! so Worker D can fill in the input-handling logic + DB persistence in
//! the deferral-cleanup wave without changing module-public APIs.
//!
//! Mirrors the shape of `threadhop_core/tui/screens/label_prompt.py`
//! (a single-line text-input modal with Enter to save, Esc to cancel).
//! Worker D wires the `L` key in selection mode to
//! [`Command::OpenBookmarkNotePrompt`](crate::keys::Command::OpenBookmarkNotePrompt),
//! and adds a result-channel + real input handler.
//!
//! This pre-pop intentionally provides a placeholder `draw` that
//! renders a titled block but doesn't yet capture keystrokes — the
//! binary's runtime behaviour is unchanged because Worker D hasn't
//! wired the App branch that opens the modal yet.

#![allow(dead_code)] // Worker D fills these in; pre-pop just lays the shape.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    widgets::{Block, BorderType, Borders, Clear, Widget},
};
use threadhop_core::theme::{hex_to_rgb, Theme};

/// Modal state — what the modal owns, distinct from App state. Worker D
/// will likely grow this with a `cursor: usize` for the input caret and
/// possibly the originally-loaded note so Esc reverts cleanly.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Bookmark rowid the note is being edited for. `0` is sentinel for
    /// "no bookmark" — pre-pop callers don't construct State directly.
    pub bookmark_id: i64,
    /// Current edit-buffer contents. Empty string saves a blank note,
    /// which Worker D maps to NULL in the bookmarks table.
    pub text: String,
}

impl State {
    /// Constructor — Worker D's `open_*` path will populate
    /// `text` with the pre-existing note so the user is editing rather
    /// than retyping.
    pub fn new(bookmark_id: i64, text: impl Into<String>) -> Self {
        Self {
            bookmark_id,
            text: text.into(),
        }
    }
}

/// Draw a placeholder modal frame so the App can mount the screen
/// without panicking. Worker D replaces the body with a real input
/// widget + theme-tinted padding. Until then this just paints a
/// rounded-border block titled "edit note" so reviewers see the
/// scaffolding is wired.
pub fn draw(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    // Clear ensures the underlying transcript doesn't show through.
    Clear.render(area, buf);
    let border_color = hex_to_rgb(&theme.border_active)
        .map(|(r, g, b)| Color::Rgb(r, g, b))
        .unwrap_or(Color::Gray);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(format!(" edit note (bookmark #{}) ", state.bookmark_id));
    block.render(area, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_fields() {
        let s = State::new(42, "important hop");
        assert_eq!(s.bookmark_id, 42);
        assert_eq!(s.text, "important hop");
    }

    #[test]
    fn default_state_is_empty() {
        let s = State::default();
        assert_eq!(s.bookmark_id, 0);
        assert!(s.text.is_empty());
    }

    #[test]
    fn draw_does_not_panic_on_minimum_rect() {
        // Pre-pop sanity: the placeholder draw must not crash on a
        // narrow rect — Worker D will exercise this from a real modal
        // dispatch and the harness doesn't want a panic regressing.
        let theme = Theme::default_dark();
        let state = State::new(1, "");
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        draw(&state, &theme, area, &mut buf);
    }
}
