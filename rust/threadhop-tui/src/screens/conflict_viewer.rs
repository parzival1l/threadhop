//! Conflict viewer modal — surfaces cross-session decision conflicts from
//! the reflector pipeline. Placeholder — Phase 5 task 5.3.

#[allow(dead_code)]
pub struct State {
    // Wave 1 fills in
}

#[allow(dead_code)]
pub enum ConflictViewerResult {
    Cancelled,
    MarkResolved { conflict_id: i64 },
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
) -> Option<ConflictViewerResult> {
    None
}

#[allow(dead_code)]
pub fn draw(
    _state: &State,
    _theme: &threadhop_core::theme::Theme,
    _area: ratatui::layout::Rect,
    _buf: &mut ratatui::buffer::Buffer,
) {
}
