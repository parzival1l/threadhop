//! Label prompt: picks a status label (backlog / in_progress / in_review /
//! done / archived) for the currently selected session.
//!
//! Mirrors `threadhop_core/tui/screens/label_prompt.py`. Wave 2 wires the
//! `s` keybinding on the main screen to open this modal; the modal posts
//! back a `LabelSelected { label }` which the App writes through to the
//! `sessions.status` column.
//!
//! Placeholder — implementation lands in Phase 4 Wave 1.

#[allow(dead_code)]
pub struct State {
    // Wave 1 fills this in (label options, selected_index, current status).
}

#[allow(dead_code)]
pub enum Result {
    Cancelled,
    // Wave 1 adds LabelSelected { label: SessionStatus }.
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
