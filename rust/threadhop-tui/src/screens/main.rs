//! Main screen — wires session_list (sidebar) + transcript (right pane) +
//! contextual_footer (bottom).
//!
//! Layout (per design spec §4):
//! - Vertical: content area (fills) + 1-line footer (fixed).
//! - Within content: horizontal 36-char sidebar + remaining transcript pane.
//!
//! Wave E wiring: the App holds `Vec<SessionListItem>` populated by the
//! `session_scanner` worker via `SessionsRefreshed`. The screen renders it
//! straight through — no derivation.

use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    layout::{Constraint, Direction, Layout},
    Frame,
};

use crate::app::App;
use crate::widgets::{
    contextual_footer::ContextualFooterWidget,
    find_bar::FindBarWidget,
    session_list::SessionListWidget,
    transcript::TranscriptWidget,
};

/// Sidebar width in cells. Matches the Python TUI's fixed column.
const SIDEBAR_WIDTH: u16 = 36;

/// Render one frame of the main screen.
///
/// The function takes `&App` (no mutation) — anything that would need a
/// mutable borrow (e.g. spinner tick, selection clamping) belongs in the
/// event-loop tick, not the renderer.
pub fn draw(app: &App, frame: &mut Frame) {
    let area = frame.area();

    // Outer: content (Min 1) + footer (Length 1).
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    // Content row: 36-char sidebar + transcript fills the rest.
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(1)])
        .split(outer[0]);

    // Sidebar — view-model already populated by the session_scanner worker.
    let sidebar = SessionListWidget {
        items: &app.sidebar,
        selected_session_id: app.selected_session_id.as_deref(),
        spinner_frame: 0,
        now: now_epoch(),
    };
    frame.render_widget(sidebar, content[0]);

    // Transcript pane. When the find bar is open, reserve the bottom row of
    // the transcript pane for it so the bar sits flush with the footer.
    let has_find = app.find_state.is_some();
    let transcript_area = if has_find && content[1].height > 1 {
        ratatui::layout::Rect {
            x: content[1].x,
            y: content[1].y,
            width: content[1].width,
            height: content[1].height - 1,
        }
    } else {
        content[1]
    };
    let transcript = TranscriptWidget::new(&app.transcript, app.scroll, &app.theme)
        .find_state(app.find_state.as_ref())
        .message_cursor(Some(app.message_cursor));
    frame.render_widget(transcript, transcript_area);

    if has_find && content[1].height > 1 {
        let bar_area = ratatui::layout::Rect {
            x: content[1].x,
            y: content[1].y + content[1].height - 1,
            width: content[1].width,
            height: 1,
        };
        let bar = FindBarWidget::new(app.find_state.as_ref().unwrap(), &app.theme);
        frame.render_widget(bar, bar_area);
    }

    // Footer — scope-aware, surfaces read-only and status banner.
    let footer = ContextualFooterWidget::new(app.scope, &app.theme)
        .read_only(app.read_only)
        .status(app.status_message.as_deref());
    frame.render_widget(footer, outer[1]);

    // Modals are stacked in z-order (later = on top). `ratatui::widgets::Clear`
    // (called inside each modal's draw) wipes the background, so the
    // underlying main layout never bleeds through. The confirm modal lands
    // last because it stacks over the bookmark browser during delete flow.
    if let Some(state) = app.bookmark_browser.as_ref() {
        let modal_area =
            crate::screens::bookmark_browser::centered_rect(80, 70, frame.area());
        crate::screens::bookmark_browser::draw(
            state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
    if let Some(state) = app.label_prompt.as_ref() {
        let modal_area =
            crate::screens::label_prompt::centered_rect(60, 60, frame.area());
        crate::screens::label_prompt::draw(
            state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
    if let Some(search_state) = app.search.as_ref() {
        let modal_area =
            crate::screens::search::centered_rect(70, 60, frame.area());
        crate::screens::search::draw(
            search_state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
    // Confirm is the top layer — it stacks over the bookmark browser when
    // delete is pending.
    if let Some(req) = app.confirm.as_ref() {
        // Small popup — confirm only needs ~5 rows.
        let modal_area =
            crate::screens::confirm::centered_rect(50, 30, frame.area());
        crate::screens::confirm::draw(
            &req.state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
}

/// Current unix timestamp in seconds. Defined here (not in the widget) so
/// tests of the widget stay deterministic — the widget takes `now` by
/// parameter; only the live renderer reads the wall clock.
fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn draw_renders_empty_app_without_panic() {
        // The default App has no sessions and no transcript — the screen
        // should still lay out cleanly with sidebar + transcript + footer.
        let app = App::new();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
    }

    #[test]
    fn draw_renders_tiny_buffer_without_panic() {
        // Layouts must degrade gracefully — a 10x5 terminal still needs to
        // produce a valid frame even though only fragments of each pane fit.
        let app = App::new();
        let mut term = Terminal::new(TestBackend::new(10, 5)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
    }

    #[test]
    fn draw_renders_footer_with_quit_hint() {
        // The bottom row must show the global `quit` hint so the user knows
        // how to exit. Sanity-checks the footer wiring without coupling to
        // specific column positions.
        let app = App::new();
        let mut term = Terminal::new(TestBackend::new(80, 5)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
        let buf = term.backend().buffer();
        let mut footer_row = String::new();
        let last = buf.area().height - 1;
        for x in 0..buf.area().width {
            footer_row.push_str(buf[(x, last)].symbol());
        }
        assert!(
            footer_row.contains("quit"),
            "expected quit hint on footer row, got {footer_row:?}"
        );
    }
}
