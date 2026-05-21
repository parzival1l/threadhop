//! Application state. Wave A holds just enough to draw the boot frame and
//! route Quit; widgets, screens, and workers land in later waves.

use crossterm::event::KeyEvent;
use ratatui::Frame;
use threadhop_core::{jsonl::CleanedMessage, models::Session, theme::Theme};

use crate::keys::{self, Command, Scope};
use crate::widgets::session_list::SessionListItem;

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

    /// Cleaned messages for the currently selected session. Populated by the
    /// Wave C/E worker via `threadhop_core::jsonl::parse_byte_range` — the
    /// transcript widget consumes this slice directly (ADR-003: already
    /// cleaned, never re-parsed at render time).
    pub transcript: Vec<CleanedMessage>,

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
            transcript: Vec::new(),
            theme: Theme::default_dark(),
            status_message: None,
            read_only: false,
            scope: Scope::MainScreen,
        }
    }

    /// Draw one frame. Delegates to `screens::main` — Wave A's centered
    /// boot banner has been replaced by the real sidebar + transcript +
    /// footer layout.
    pub fn draw(&self, frame: &mut Frame) {
        crate::screens::main::draw(self, frame);
    }

    /// Derive the sidebar view-model from the App's `Vec<Session>`.
    ///
    /// Pure (clones strings — no I/O, no time access). Used by
    /// `screens::main` until the session_scanner worker emits
    /// `SessionsRefreshed(Vec<SessionListItem>)` directly. Runtime fields
    /// (is_active / is_working / has_observations) default to `false`
    /// because the active-detector worker isn't wired in yet; the
    /// last-active timestamp falls back to `modified_at`.
    pub fn sidebar_items(&self) -> Vec<SessionListItem> {
        self.sessions
            .iter()
            .map(|s| SessionListItem {
                session_id: s.session_id.clone(),
                display_name: s
                    .custom_name
                    .clone()
                    .or_else(|| s.project.clone())
                    .unwrap_or_else(|| s.session_id.clone()),
                is_active: false,
                is_working: false,
                has_observations: false,
                last_active_at: s.modified_at,
            })
            .collect()
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
