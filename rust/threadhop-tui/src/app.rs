//! Application state. Wave A holds just enough to draw the boot frame and
//! route Quit; widgets, screens, and workers land in later waves.

use crossterm::event::KeyEvent;
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use threadhop_core::{models::Session, theme::Theme};

use crate::keys::{self, Command, Scope};

/// Top-level application state.
///
/// Fields are intentionally exposed for later waves to read directly; in this
/// crate everything lives behind the `App` boundary so the orchestrator wave
/// can grow the struct without a churn of getters.
///
/// `dead_code` is allowed because most fields are Wave A scaffolding — Wave B
/// widgets and Wave C workers will start reading them. Removing them now and
/// re-adding them would just churn the struct.
#[allow(dead_code)]
pub struct App {
    /// Set by `handle_key` when the user requests Quit. The event loop polls
    /// this after every iteration.
    pub should_quit: bool,

    /// Sessions to render in the sidebar. Empty in Wave A — populated by the
    /// session_scanner worker in Wave C.
    pub sessions: Vec<Session>,

    /// Index into `sessions` of the currently selected row. None when the
    /// list is empty (Wave A always-empty case).
    pub selected_session_id: Option<String>,

    /// Vertical scroll position of the transcript pane. Reset to 0 on session
    /// switch by later waves.
    pub scroll: u16,

    /// Theme for widget styling. Loaded once at startup; reload is a future
    /// feature.
    pub theme: Theme,

    /// Banner text shown above the transcript. Used for read-only mode
    /// warnings, schema-mismatch notices, and worker errors.
    pub status_message: Option<String>,

    /// True when the schema-version handshake forced read-only mode. Wave A
    /// always boots writable.
    pub read_only: bool,

    /// Active UI scope, used to look up key bindings + render the footer.
    pub scope: Scope,
}

impl App {
    /// Construct an App with defaults. Wave A uses this directly; later waves
    /// will add a constructor that takes the DB connection + CLI flags.
    pub fn new() -> Self {
        Self {
            should_quit: false,
            sessions: Vec::new(),
            selected_session_id: None,
            scroll: 0,
            theme: Theme::default_dark(),
            status_message: None,
            read_only: false,
            scope: Scope::MainScreen,
        }
    }

    /// Draw one frame. Wave A renders a centered boot banner so we can verify
    /// the binary actually enters ratatui without crashing.
    pub fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);

        let body = vec![
            Line::from(""),
            Line::from(Span::styled(
                "ThreadHop (Rust)",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from("Phase 2 Wave A boot."),
            Line::from("Widgets land in Wave B."),
            Line::from(""),
            Line::from(Span::styled(
                "Press q to quit.",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ];

        let paragraph = Paragraph::new(body)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title(" threadhop-tui "));
        frame.render_widget(paragraph, layout[0]);

        // Minimal footer hint — full contextual_footer arrives in Wave B.
        let footer = Paragraph::new(Line::from(Span::styled(
            " q  quit ",
            Style::default().add_modifier(Modifier::REVERSED),
        )));
        frame.render_widget(footer, layout[1]);
    }

    /// Dispatch a key event through the keys registry. Returns the matched
    /// command for tests; the event loop ignores the return value and just
    /// re-checks `self.should_quit`.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Command> {
        let cmd = keys::lookup(self.scope, key)?;
        match cmd {
            Command::Quit => self.should_quit = true,
        }
        Some(cmd)
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    #[test]
    fn new_app_is_not_quitting() {
        let app = App::new();
        assert!(!app.should_quit);
        assert!(app.sessions.is_empty());
        assert_eq!(app.scope, Scope::MainScreen);
    }

    #[test]
    fn q_sets_should_quit() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn ctrl_c_sets_should_quit() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit);
    }

    #[test]
    fn unknown_key_is_a_noop() {
        let mut app = App::new();
        let result = app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(result.is_none());
        assert!(!app.should_quit);
    }

    #[test]
    fn draw_does_not_panic_on_tiny_buffer() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut terminal = Terminal::new(TestBackend::new(10, 5)).unwrap();
        let app = App::new();
        terminal.draw(|f| app.draw(f)).unwrap();
    }
}
