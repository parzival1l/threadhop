//! Bookmark browser: modal list of pinned messages with jump-to-message.
//!
//! Mirrors `threadhop_core/tui/screens/bookmark.py`. The modal lists every
//! row in the `bookmarks` table (optionally filtered by session); pressing
//! Enter posts back a [`Result::JumpToMessage`] that the App resolves via
//! `pending_jump_message_uuid` (same channel the search modal already uses).
//!
//! Placeholder — implementation lands in Phase 4 Wave 1.

#[allow(dead_code)]
pub struct State {
    // Wave 1 fills this in (bookmark rows, selected_index, optional session filter).
}

#[allow(dead_code)]
pub enum Result {
    Cancelled,
    // Wave 1 adds specific success variants
    // (likely: JumpToMessage { session_id, message_uuid }).
}

#[allow(dead_code)]
impl State {
    pub fn new() -> Self {
        Self {}
    }
}

#[allow(dead_code)]
pub fn handle_key(
    _state: &mut State,
    _key: crossterm::event::KeyEvent,
) -> Option<Result> {
    None
}

#[allow(dead_code)]
pub fn draw(
    _state: &State,
    _theme: &threadhop_core::theme::Theme,
    _area: ratatui::layout::Rect,
    _buf: &mut ratatui::buffer::Buffer,
) {
    // Wave 1
}
