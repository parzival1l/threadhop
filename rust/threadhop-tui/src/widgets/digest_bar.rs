//! Top-of-screen digest bar showing observation counts.
//! Placeholder — Phase 5 task 5.2.

#[allow(dead_code)]
pub struct DigestBarWidget<'a> {
    pub theme: &'a threadhop_core::theme::Theme,
    // Wave 1 fills in (observation_count, conflict_count, etc.)
}

#[allow(dead_code)]
impl<'a> ratatui::widgets::Widget for DigestBarWidget<'a> {
    fn render(self, _area: ratatui::layout::Rect, _buf: &mut ratatui::buffer::Buffer) {
        // Wave 1
    }
}
