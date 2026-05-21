//! Confirm modal: generic yes/no confirmation prompt.
//!
//! Mirrors `threadhop_core/tui/screens/confirm.py`. Drives destructive
//! actions (archive, delete-bookmark, etc.) — caller passes a prompt string,
//! modal posts back `Confirmed` or `Cancelled`.
//!
//! Placeholder — implementation lands in Phase 4 Wave 1.

#[allow(dead_code)]
pub struct State {
    // Wave 1 fills this in (prompt text, default action).
}

#[allow(dead_code)]
pub enum Result {
    Cancelled,
    // Wave 1 adds Confirmed (and possibly carries the original action token).
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
