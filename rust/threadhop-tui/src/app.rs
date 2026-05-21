//! Application state. Wave E adds the worker-fed `sidebar` snapshot, the
//! `active_session_tx` watch sender that retargets the fs_watcher, and
//! sidebar/transcript key-handler dispatch.

use crossterm::event::KeyEvent;
use ratatui::Frame;
use threadhop_core::{jsonl::CleanedMessage, models::Session, theme::Theme};
use tokio::sync::watch;

use crate::keys::{self, Command, Scope};
use crate::widgets::session_list::SessionListItem;

/// Half-page step size for `ScrollDownHalf` / `ScrollUpHalf`. A constant
/// rather than a function of terminal height because the App doesn't know
/// the height — close enough for the Wave E binding-smoke pass.
const HALF_PAGE: u16 = 20;

/// Top-level application state.
///
/// Fields are intentionally exposed for later waves to read directly; in this
/// crate everything lives behind the `App` boundary so the orchestrator wave
/// can grow the struct without a churn of getters.
///
/// `dead_code` is allowed because some fields are still scaffolding — Wave F
/// modal screens will start reading them. Removing them now and re-adding
/// them would just churn the struct.
#[allow(dead_code)]
pub struct App {
    /// Set by `handle_key` when the user requests Quit. The event loop polls
    /// this after every iteration.
    pub should_quit: bool,

    /// Raw session metadata snapshot. Reserved for future use (Wave F+ may
    /// re-derive richer view-models off this). Wave E populates the sidebar
    /// directly from worker events; this field is unused on the render path.
    pub sessions: Vec<Session>,

    /// Sidebar view-model, populated by `WorkerEvent::SessionsRefreshed`.
    /// Replaces the old `sidebar_items()` derivation that Wave C used as a
    /// stand-in.
    pub sidebar: Vec<SessionListItem>,

    /// Currently selected session id, or `None` when the sidebar is empty.
    /// Mutations go through key handling so the `active_session_tx` watch
    /// stays in lockstep with the visible selection.
    pub selected_session_id: Option<String>,

    /// Sender half of the active-session watch channel. The fs_watcher
    /// listens on the paired receiver and retargets its polling whenever this
    /// changes. Constructed in `App::new()`; the receiver is handed to
    /// `spawn_all` by `main`.
    pub active_session_tx: watch::Sender<Option<String>>,

    /// Vertical scroll position of the transcript pane. Reset to 0 on session
    /// switch and bound to `u16::MAX` for the "bottom" jump — the render path
    /// clamps to the last line.
    pub scroll: u16,

    /// Cleaned messages for the currently selected session. Populated by the
    /// fs_watcher via `WorkerEvent::TranscriptRefreshed`.
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
    ///
    /// Creates the active-session watch channel. The caller must take
    /// `active_session_rx()` and hand it to `workers::spawn_all` before the
    /// fs_watcher can fire.
    pub fn new() -> Self {
        let (active_session_tx, _initial_rx) = watch::channel::<Option<String>>(None);
        Self {
            should_quit: false,
            sessions: Vec::new(),
            sidebar: Vec::new(),
            selected_session_id: None,
            active_session_tx,
            scroll: 0,
            transcript: Vec::new(),
            theme: Theme::default_dark(),
            status_message: None,
            read_only: false,
            scope: Scope::MainScreen,
        }
    }

    /// Subscribe to the active-session watch channel. Returns a receiver that
    /// the fs_watcher (or any other consumer) can `.changed()` on.
    pub fn active_session_rx(&self) -> watch::Receiver<Option<String>> {
        self.active_session_tx.subscribe()
    }

    /// Draw one frame. Delegates to `screens::main` — Wave A's centered
    /// boot banner has been replaced by the real sidebar + transcript +
    /// footer layout.
    pub fn draw(&self, frame: &mut Frame) {
        crate::screens::main::draw(self, frame);
    }

    /// Dispatch a key event through the keys registry. Returns the matched
    /// command for tests; the event loop ignores the return value and just
    /// re-checks `self.should_quit`.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Command> {
        let cmd = keys::lookup(self.scope, key)?;
        match cmd {
            Command::Quit => self.should_quit = true,
            Command::SelectNextSession => self.move_selection(1),
            Command::SelectPrevSession => self.move_selection(-1),
            Command::ScrollTop => self.scroll = 0,
            Command::ScrollBottom => self.scroll = u16::MAX,
            Command::ScrollDownHalf => self.scroll = self.scroll.saturating_add(HALF_PAGE),
            Command::ScrollUpHalf => self.scroll = self.scroll.saturating_sub(HALF_PAGE),
            // Wave E no-ops — labels are advertised on the footer; the
            // overlay + open-on-enter handlers land in later phases.
            Command::OpenHelp => {}
            Command::Confirm => {}
        }
        Some(cmd)
    }

    /// Move sidebar selection by `delta` rows (+1 down, -1 up). No-op if the
    /// sidebar is empty. On success, retargets the fs_watcher via
    /// `active_session_tx` and resets transcript scroll.
    fn move_selection(&mut self, delta: i32) {
        if self.sidebar.is_empty() {
            return;
        }
        let current = self
            .selected_session_id
            .as_deref()
            .and_then(|id| self.sidebar.iter().position(|it| it.session_id == id))
            .unwrap_or(0);
        let len = self.sidebar.len() as i32;
        let next = ((current as i32) + delta).rem_euclid(len) as usize;
        let next_id = self.sidebar[next].session_id.clone();
        if self.selected_session_id.as_deref() != Some(next_id.as_str()) {
            self.selected_session_id = Some(next_id.clone());
            // Ignore send errors — `watch::Sender::send` only fails when all
            // receivers have been dropped, in which case the fs_watcher is
            // already gone and the App is shutting down.
            let _ = self.active_session_tx.send(Some(next_id));
            self.scroll = 0;
        }
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

    fn item(id: &str) -> SessionListItem {
        SessionListItem {
            session_id: id.to_string(),
            display_name: id.to_string(),
            is_active: false,
            is_working: false,
            has_observations: false,
            last_active_at: None,
        }
    }

    #[test]
    fn new_app_is_not_quitting() {
        let app = App::new();
        assert!(!app.should_quit);
        assert!(app.sessions.is_empty());
        assert!(app.sidebar.is_empty());
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

    #[test]
    fn j_moves_selection_down_and_publishes_watch() {
        let mut app = App::new();
        app.sidebar = vec![item("a"), item("b"), item("c")];
        app.selected_session_id = Some("a".into());
        let mut rx = app.active_session_rx();
        // Mark current value as seen so `has_changed` reflects only the j
        // press.
        let _ = rx.borrow_and_update();

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.selected_session_id.as_deref(), Some("b"));
        assert!(rx.has_changed().unwrap());
        assert_eq!(rx.borrow().as_deref(), Some("b"));
    }

    #[test]
    fn k_moves_selection_up_and_wraps() {
        let mut app = App::new();
        app.sidebar = vec![item("a"), item("b"), item("c")];
        app.selected_session_id = Some("a".into());
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        // Wrapping up from index 0 lands on the last entry.
        assert_eq!(app.selected_session_id.as_deref(), Some("c"));
    }

    #[test]
    fn moving_selection_resets_scroll() {
        let mut app = App::new();
        app.sidebar = vec![item("a"), item("b")];
        app.selected_session_id = Some("a".into());
        app.scroll = 42;
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn j_on_empty_sidebar_is_a_noop() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert!(app.selected_session_id.is_none());
    }

    #[test]
    fn g_jumps_to_top_and_shift_g_to_bottom() {
        let mut app = App::new();
        app.scroll = 100;
        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
        app.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert_eq!(app.scroll, u16::MAX);
    }

    #[test]
    fn page_down_adds_half_page_and_page_up_subtracts() {
        let mut app = App::new();
        app.scroll = 50;
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(app.scroll, 50 + HALF_PAGE);
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.scroll, 50);
    }

    #[test]
    fn ctrl_d_and_ctrl_u_mirror_page_keys() {
        let mut app = App::new();
        app.scroll = 100;
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.scroll, 100 - HALF_PAGE);
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(app.scroll, 100);
    }

    #[test]
    fn page_up_saturates_at_zero() {
        let mut app = App::new();
        app.scroll = 5;
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn help_and_confirm_are_noops_today() {
        let mut app = App::new();
        let r1 = app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(r1, Some(Command::OpenHelp));
        let r2 = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(r2, Some(Command::Confirm));
        assert!(!app.should_quit);
    }
}
