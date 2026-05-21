//! Application state. Phase 3 Wave 2 adds:
//!   - the [`screens::search::SearchState`] modal handle,
//!   - the [`widgets::find_bar::FindState`] overlay handle,
//!   - a `rusqlite::Connection` owned by the App for FTS queries,
//!   - a `pending_jump_message_uuid` resolved by the event loop after
//!     `WorkerEvent::TranscriptRefreshed` arrives.
//!
//! Wave E (the previous wave) wired the worker-fed `sidebar` snapshot, the
//! `active_session_tx` watch sender, and sidebar/transcript key handling.

use crossterm::event::{KeyEvent, KeyModifiers};
use ratatui::Frame;
use threadhop_core::{jsonl::CleanedMessage, models::Session, theme::Theme};
use tokio::sync::watch;

use crate::keys::{self, Command, Scope};
use crate::screens::search::{SearchResult, SearchState};
use crate::widgets::find_bar::{FindResult, FindState};
use crate::widgets::session_list::SessionListItem;
use crate::widgets::transcript::message_to_line_index;

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

    /// FTS search modal handle. `Some` while the modal is open. Phase 3
    /// Wave 2.
    pub search: Option<SearchState>,

    /// In-transcript find bar handle. `Some` while the bar is open. Phase 3
    /// Wave 2.
    pub find_state: Option<FindState>,

    /// SQLite connection used by `screens::search::execute_query`. Opened in
    /// [`App::new`] against `threadhop_core::paths::db_path()`. On open
    /// failure we surface the error via `status_message` and fall back to an
    /// in-memory connection so the TUI still boots — the search modal will
    /// just return zero hits in that degraded state.
    pub db: rusqlite::Connection,

    /// Pending jump after `fs_watcher` reloads the transcript. When the user
    /// hits Enter in the search modal, we set this and switch sessions; once
    /// `WorkerEvent::TranscriptRefreshed` arrives, the event loop resolves
    /// the UUID to a scroll position and clears this back to `None`.
    pub pending_jump_message_uuid: Option<String>,
}

impl App {
    /// Construct an App with defaults. Wave A uses this directly; later waves
    /// will add a constructor that takes the DB connection + CLI flags.
    ///
    /// Creates the active-session watch channel. The caller must take
    /// `active_session_rx()` and hand it to `workers::spawn_all` before the
    /// fs_watcher can fire.
    ///
    /// Opens the SQLite DB used by the search modal. On error we fall back to
    /// an in-memory connection — the modal still works (returns zero hits)
    /// but the rest of the TUI stays usable. The failure is surfaced via
    /// `status_message`.
    pub fn new() -> Self {
        let (active_session_tx, _initial_rx) = watch::channel::<Option<String>>(None);
        let (db, status_message) = match threadhop_core::db::open(&threadhop_core::paths::db_path())
        {
            Ok(c) => (c, None),
            Err(e) => {
                tracing::warn!("opening sessions.db failed: {e} — falling back to in-memory");
                // open_in_memory cannot realistically fail in a healthy
                // process; expect() is fine here because if it did, the
                // process couldn't continue anyway.
                let c = rusqlite::Connection::open_in_memory()
                    .expect("in-memory sqlite open should always succeed");
                (c, Some(format!("DB unavailable: {e}")))
            }
        };
        Self {
            should_quit: false,
            sessions: Vec::new(),
            sidebar: Vec::new(),
            selected_session_id: None,
            active_session_tx,
            scroll: 0,
            transcript: Vec::new(),
            theme: Theme::default_dark(),
            status_message,
            read_only: false,
            scope: Scope::MainScreen,
            search: None,
            find_state: None,
            db,
            pending_jump_message_uuid: None,
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

    /// Dispatch a key event. Modal-first: if the search modal or find bar is
    /// open, route the event there before consulting the main-screen
    /// registry. Returns the matched [`Command`] when the event was handled
    /// by the main-screen registry (used by tests); modal keypresses return
    /// `None` because the modal owns its own result type.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Command> {
        // 1. Search modal — exclusive focus.
        if self.search.is_some() {
            // SAFETY: just checked is_some().
            let state = self.search.as_mut().unwrap();
            let result = crate::screens::search::handle_key(state, key);
            match result {
                Some(SearchResult::Cancelled) => {
                    self.search = None;
                    self.scope = Scope::MainScreen;
                }
                Some(SearchResult::JumpToMessage {
                    session_id,
                    message_uuid,
                }) => {
                    // Capture committed_query for the recent_searches push
                    // *before* we drop the modal. The Esc-without-jump path
                    // intentionally does NOT save — only successful jumps go
                    // to the MRU list, matching the Python TUI's contract.
                    let committed = std::mem::take(&mut state.committed_query);
                    self.search = None;
                    self.scope = Scope::MainScreen;
                    self.pending_jump_message_uuid = Some(message_uuid);
                    if self.selected_session_id.as_deref() != Some(session_id.as_str()) {
                        self.selected_session_id = Some(session_id.clone());
                        let _ = self.active_session_tx.send(Some(session_id));
                        self.scroll = 0;
                    } else {
                        // Same session — no fs_watcher reload will fire, so
                        // resolve the jump immediately against the current
                        // transcript.
                        self.try_resolve_pending_jump();
                    }
                    if !committed.trim().is_empty() {
                        if let Err(e) = threadhop_core::recent_searches::save_recent_search(
                            &committed,
                        ) {
                            tracing::warn!("save_recent_search failed: {e}");
                        }
                    }
                }
                None => { /* modal stays open */ }
            }
            return None;
        }

        // 2. Find bar — overlay on top of MainScreen.
        if self.find_state.is_some() {
            let transcript_clone = &self.transcript; // borrow split
            let result = {
                let state = self.find_state.as_mut().unwrap();
                crate::widgets::find_bar::handle_key(state, key, transcript_clone)
            };
            match result {
                Some(FindResult::Closed) => {
                    self.find_state = None;
                    self.scope = Scope::MainScreen;
                }
                Some(FindResult::JumpedToMatch { message_index }) => {
                    if let Some(msg) = self.transcript.get(message_index) {
                        if let Some(line) =
                            message_to_line_index(&self.transcript, &msg.uuid)
                        {
                            self.scroll = line;
                        }
                    }
                    self.find_state = None;
                    self.scope = Scope::MainScreen;
                }
                None => { /* find bar stays open */ }
            }
            return None;
        }

        // 3. Normal main-screen dispatch.
        let cmd = keys::lookup(self.scope, key)?;
        match cmd {
            Command::Quit => self.should_quit = true,
            Command::SelectNextSession => self.move_selection(1),
            Command::SelectPrevSession => self.move_selection(-1),
            Command::ScrollTop => {
                self.scroll = 0;
                tracing::debug!(target: "threadhop_tui", "scroll command g applied scroll={}", self.scroll);
            }
            Command::ScrollBottom => {
                self.scroll = u16::MAX;
                tracing::debug!(target: "threadhop_tui", "scroll command G applied scroll={}", self.scroll);
            }
            Command::ScrollDownHalf => {
                self.scroll = self.scroll.saturating_add(HALF_PAGE);
                tracing::debug!(target: "threadhop_tui", "scroll command DownHalf applied scroll={}", self.scroll);
            }
            Command::ScrollUpHalf => {
                self.scroll = self.scroll.saturating_sub(HALF_PAGE);
                tracing::debug!(target: "threadhop_tui", "scroll command UpHalf applied scroll={}", self.scroll);
            }
            Command::OpenSearchModal => {
                let mut state = SearchState::new();
                // Pre-populate recents so the empty-input fallback list has
                // something to show. Failure here is non-fatal — the modal
                // just opens with an empty recents list.
                if let Ok(recents) = threadhop_core::recent_searches::get_recent_searches() {
                    state.recents = recents;
                }
                self.search = Some(state);
                self.scope = Scope::SearchModal;
            }
            Command::OpenFindBar => {
                let mut state = FindState::default();
                state.recompute_matches(&self.transcript);
                self.find_state = Some(state);
                self.scope = Scope::FindBar;
            }
            // No-ops on the main screen; these labels show up in the footer
            // when the modals are open (they're handled by modal-first
            // dispatch above).
            Command::CloseFindBar
            | Command::JumpToCurrentMatch
            | Command::NextMatch
            | Command::PrevMatch => {}
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
            let send_result = self.active_session_tx.send(Some(next_id.clone()));
            tracing::debug!(
                target: "threadhop_tui",
                "selection change session={next_id} delta={delta} send_ok={}",
                send_result.is_ok()
            );
            self.scroll = 0;
        }
    }

    /// If [`Self::pending_jump_message_uuid`] points at a message currently in
    /// `transcript`, scroll to it and clear the pending state. Called from
    /// the event loop after `TranscriptRefreshed` (and synchronously from
    /// `handle_key` when the jump target is the same session — no reload
    /// will arrive in that case).
    pub fn try_resolve_pending_jump(&mut self) {
        let Some(uuid) = self.pending_jump_message_uuid.clone() else {
            return;
        };
        if let Some(line) = message_to_line_index(&self.transcript, &uuid) {
            self.scroll = line;
            self.pending_jump_message_uuid = None;
        }
        // Otherwise: leave pending_jump set; a later TranscriptRefreshed may
        // contain the message (e.g. partial reload).
    }
}

// `KeyModifiers` is referenced via crossterm re-export only when the
// modifier-aware paths need it. Keep the import here so future edits don't
// have to re-add it.
#[allow(dead_code)]
const _USED_KEYMODIFIERS: KeyModifiers = KeyModifiers::NONE;

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

    fn msg(uuid: &str, role: &str, text: &str) -> CleanedMessage {
        CleanedMessage {
            uuid: uuid.into(),
            session_id: None,
            role: role.into(),
            text: text.into(),
            timestamp: None,
            cwd: None,
            parent_uuid: None,
            is_sidechain: 0,
            message_id: None,
        }
    }

    #[test]
    fn new_app_is_not_quitting() {
        let app = App::new();
        assert!(!app.should_quit);
        assert!(app.sessions.is_empty());
        assert!(app.sidebar.is_empty());
        assert_eq!(app.scope, Scope::MainScreen);
        assert!(app.search.is_none());
        assert!(app.find_state.is_none());
        assert!(app.pending_jump_message_uuid.is_none());
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

    // ---- Phase 3 Wave 2 -----------------------------------------------------

    #[test]
    fn slash_opens_search_modal_and_changes_scope() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        assert!(app.search.is_some());
        assert_eq!(app.scope, Scope::SearchModal);
    }

    #[test]
    fn esc_in_search_modal_closes_it() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.search.is_none());
        assert_eq!(app.scope, Scope::MainScreen);
    }

    #[test]
    fn typing_in_search_modal_routes_to_modal_input() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        for c in "abc".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert_eq!(app.search.as_ref().unwrap().query_input, "abc");
        // Should NOT have triggered j-selection.
        assert!(app.selected_session_id.is_none());
    }

    #[test]
    fn f_opens_find_bar_and_changes_scope() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(app.find_state.is_some());
        assert_eq!(app.scope, Scope::FindBar);
    }

    #[test]
    fn esc_in_find_bar_closes_it() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.find_state.is_none());
        assert_eq!(app.scope, Scope::MainScreen);
    }

    #[test]
    fn typing_in_find_bar_routes_to_bar_input() {
        let mut app = App::new();
        app.transcript = vec![msg("u1", "user", "hello world")];
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        for c in "hello".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let fs = app.find_state.as_ref().unwrap();
        assert_eq!(fs.query, "hello");
        assert_eq!(fs.matches.len(), 1);
    }

    #[test]
    fn find_bar_enter_scrolls_to_message_and_closes() {
        let mut app = App::new();
        // 2 messages — match is in #2. Avoid 'n' in the query because the
        // find bar treats `n` as "next match" before "type a literal n".
        app.transcript = vec![
            msg("u1", "user", "alpha"),
            msg("u2", "assistant", "target here"),
        ];
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        for c in "target".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        // Sanity: the query made it to the bar and matched the second message.
        {
            let fs = app.find_state.as_ref().unwrap();
            assert_eq!(fs.query, "target");
            assert_eq!(fs.matches.len(), 1);
            assert_eq!(fs.matches[0].message_index, 1);
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.find_state.is_none());
        // Expected scroll: message 0 header(1) + body(1) + sep(1) = 3.
        assert_eq!(app.scroll, 3);
    }

    #[test]
    fn search_modal_jump_sets_pending_uuid_and_session() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
        // Force a hit + Enter without exercising FTS.
        {
            let state = app.search.as_mut().unwrap();
            state.hits.push(threadhop_core::fts::Hit {
                message_uuid: "msg-42".into(),
                session_id: "sess-xyz".into(),
                snippet: "snip".into(),
                score: -1.0,
            });
            state.committed_query = "needle".into();
        }
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.search.is_none());
        assert_eq!(
            app.pending_jump_message_uuid.as_deref(),
            Some("msg-42")
        );
        assert_eq!(app.selected_session_id.as_deref(), Some("sess-xyz"));
        assert_eq!(app.scope, Scope::MainScreen);
    }

    #[test]
    fn try_resolve_pending_jump_sets_scroll_when_uuid_present() {
        let mut app = App::new();
        app.transcript = vec![
            msg("a", "user", "alpha"),
            msg("b", "assistant", "beta line"),
        ];
        app.pending_jump_message_uuid = Some("b".into());
        app.try_resolve_pending_jump();
        // Message "b" starts after message "a" (1 header + 1 body) + 1 sep = 3.
        assert_eq!(app.scroll, 3);
        assert!(app.pending_jump_message_uuid.is_none());
    }

    #[test]
    fn try_resolve_pending_jump_is_noop_when_uuid_missing() {
        let mut app = App::new();
        app.transcript = vec![msg("a", "user", "alpha")];
        app.pending_jump_message_uuid = Some("does-not-exist".into());
        app.scroll = 7;
        app.try_resolve_pending_jump();
        // No-op: keep scroll, keep pending so a later refresh can pick it up.
        assert_eq!(app.scroll, 7);
        assert!(app.pending_jump_message_uuid.is_some());
    }

    // ---- Frame-buffer regression tests (second-fix) -----------------------
    //
    // The previous regression tests asserted only on state mutations
    // (`app.scroll`, `app.selected_session_id`). They missed two bugs:
    //   * `G` moved `scroll` but the rendered transcript pane didn't
    //     visibly change for the long-transcript case the user hit.
    //   * `k` moved selection but `app.transcript` never refreshed when
    //     returning to a previously-viewed session, so the pane kept
    //     showing the prior session's content.
    // These tests render to a `TestBackend` and diff actual buffer cells.

    fn long_msg(uuid: &str, marker: &str) -> CleanedMessage {
        CleanedMessage {
            uuid: uuid.into(),
            session_id: None,
            role: "user".into(),
            // 3 body lines per message, prefixed with the marker so we can
            // assert the marker appears in the rendered buffer.
            text: format!("{marker}-line-1\n{marker}-line-2\n{marker}-line-3"),
            timestamp: None,
            cwd: None,
            parent_uuid: None,
            is_sidechain: 0,
            message_id: None,
        }
    }

    fn render_to_string(app: &App, w: u16, h: u16) -> String {
        use ratatui::{backend::TestBackend, Terminal};
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer();
        let mut s = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    #[test]
    fn pressing_shift_g_visibly_scrolls_transcript_to_bottom() {
        // Long transcript: 50 messages, distinct markers per message so we
        // can assert that "TOP-MARKER" is visible before the keypress and
        // "BOTTOM-MARKER" is visible after.
        let mut app = App::new();
        let mut msgs: Vec<CleanedMessage> = (0..50)
            .map(|i| long_msg(&format!("u{i}"), &format!("msg{i:02}")))
            .collect();
        // Distinct sentinel markers — these strings must not appear
        // anywhere else in the rendered buffer.
        msgs[0] = long_msg("u0", "TOPSENTINEL");
        msgs[49] = long_msg("u49", "BOTTOMSENTINEL");
        app.transcript = msgs;
        app.scroll = 0;

        let before = render_to_string(&app, 120, 30);
        assert!(
            before.contains("TOPSENTINEL"),
            "TOPSENTINEL must be visible at scroll=0; got:\n{before}"
        );

        // Simulate Shift+G.
        app.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        let after = render_to_string(&app, 120, 30);
        assert_ne!(
            before, after,
            "Shift+G did not visibly change the rendered transcript"
        );
        assert!(
            after.contains("BOTTOMSENTINEL"),
            "BOTTOMSENTINEL must be visible after Shift+G; got:\n{after}"
        );
        assert!(
            !after.contains("TOPSENTINEL"),
            "TOPSENTINEL must scroll off-screen after Shift+G; got:\n{after}"
        );
    }

    #[test]
    fn pressing_pgdn_visibly_scrolls_transcript() {
        // PageDown / Ctrl-d move HALF_PAGE rows — must actually move the
        // rendered transcript, not just bump `app.scroll`.
        let mut app = App::new();
        let msgs: Vec<CleanedMessage> = (0..50)
            .map(|i| long_msg(&format!("u{i}"), &format!("PGMARK{i:02}")))
            .collect();
        app.transcript = msgs;
        app.scroll = 0;

        let before = render_to_string(&app, 120, 30);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE));
        let after = render_to_string(&app, 120, 30);
        assert_ne!(
            before, after,
            "PageDown did not visibly change the rendered transcript"
        );
    }

    #[test]
    fn pressing_k_visibly_switches_transcript_to_prev_session() {
        // Three sessions, starting at the middle one. Simulate the
        // worker-fed initial transcript for "s2" (the down-direction
        // session would have been loaded by the first j → k path the
        // user reported).
        let mut app = App::new();
        app.sidebar = vec![item("s1"), item("s2"), item("s3")];
        app.selected_session_id = Some("s2".into());
        // Pre-load the current pane with s2's transcript via the worker
        // event — exactly what the live loop does.
        crate::event::handle_worker_event(
            &mut app,
            crate::workers::WorkerEvent::TranscriptRefreshed {
                session_id: "s2".into(),
                messages: vec![long_msg("m1", "SESSION-TWO-CONTENT")],
            },
        );
        // Sanity: s2 content rendered.
        let s2_frame = render_to_string(&app, 120, 30);
        assert!(
            s2_frame.contains("SESSION-TWO-CONTENT"),
            "expected s2 content visible before k; got:\n{s2_frame}"
        );

        // Simulate having previously visited s1, so the worker would not
        // organically re-emit (sig_cache hit). The bug manifests when the
        // worker delivers — or fails to deliver — the previous session's
        // transcript on the k transition.
        //
        // Press k → selection moves to s1.
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.selected_session_id.as_deref(), Some("s1"));

        // Now simulate the fs_watcher delivering s1's transcript. This is
        // what the *fixed* fs_watcher must do on every session switch,
        // even if the file's signature is unchanged.
        crate::event::handle_worker_event(
            &mut app,
            crate::workers::WorkerEvent::TranscriptRefreshed {
                session_id: "s1".into(),
                messages: vec![long_msg("m2", "SESSION-ONE-CONTENT")],
            },
        );
        let s1_frame = render_to_string(&app, 120, 30);
        assert!(
            s1_frame.contains("SESSION-ONE-CONTENT"),
            "k did not switch transcript content; got:\n{s1_frame}"
        );
        assert!(
            !s1_frame.contains("SESSION-TWO-CONTENT"),
            "stale s2 transcript still visible after k; got:\n{s1_frame}"
        );
    }
}
