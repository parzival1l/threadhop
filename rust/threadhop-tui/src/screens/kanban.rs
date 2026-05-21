//! Kanban modal — sessions grouped by status, columns navigable with h/l.
//! Placeholder — Phase 5 task 5.1.

#[allow(dead_code)]
pub struct State {
    // Wave 1 fills in
}

#[allow(dead_code)]
pub enum KanbanResult {
    Cancelled,
    JumpToSession { session_id: String },
    StatusChanged {
        session_id: String,
        new_status: threadhop_core::models::SessionStatus,
    },
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
) -> Option<KanbanResult> {
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
