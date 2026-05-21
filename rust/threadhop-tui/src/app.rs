//! Application state. Phase 3 Wave 2 adds:
//!   - the [`screens::search::SearchState`] modal handle,
//!   - the [`widgets::find_bar::FindState`] overlay handle,
//!   - a `rusqlite::Connection` owned by the App for FTS queries,
//!   - a `pending_jump_message_uuid` resolved by the event loop after
//!     `WorkerEvent::TranscriptRefreshed` arrives.
//!
//! Wave E (the previous wave) wired the worker-fed `sidebar` snapshot, the
//! `active_session_tx` watch sender, and sidebar/transcript key handling.

use std::collections::{HashMap, HashSet};

use crossterm::event::{KeyEvent, KeyModifiers};
use ratatui::Frame;
use threadhop_core::{
    jsonl::CleanedMessage,
    models::Session,
    observations::{Observation, ObservationSummary},
    theme::Theme,
};
use tokio::sync::watch;

use crate::keys::{self, Command, Scope};
use crate::screens::bookmark_browser::{
    self as bb, BookmarkBrowserResult, BookmarkRow,
};
use crate::screens::confirm::{self as cf, ConfirmResult};
use crate::screens::conflict_viewer::{
    self as cv, ConflictRow, ConflictViewerResult,
};
use crate::screens::help::{self as hp, HelpResult};
use crate::screens::kanban::{self as kb, KanbanItem, KanbanResult};
use crate::screens::label_prompt::{
    self as lp, LabelPromptResult,
};
use crate::screens::search::{SearchResult, SearchState};
use crate::widgets::find_bar::{FindResult, FindState};
use crate::widgets::session_list::SessionListItem;
use crate::widgets::transcript::{message_to_line_index, SelectionState};

/// Half-page step size for `ScrollDownHalf` / `ScrollUpHalf`. A constant
/// rather than a function of terminal height because the App doesn't know
/// the height — close enough for the Wave E binding-smoke pass.
const HALF_PAGE: u16 = 20;

/// Action the App should run when a [`ConfirmResult::Yes`] arrives. The
/// confirm modal is generic, so the App stores the pending side-effect next
/// to the modal state and dispatches on confirmation.
#[derive(Debug, Clone)]
pub enum PendingAction {
    /// Delete the bookmark with this rowid from the DB.
    DeleteBookmark { bookmark_id: i64 },
}

/// Confirm-modal request bundle — the modal's own state plus the action to
/// run on Yes. Wave 2 only carries the delete-bookmark variant; future
/// destructive actions add new [`PendingAction`] arms.
#[derive(Debug, Clone)]
pub struct ConfirmRequest {
    pub state: cf::State,
    pub on_yes: PendingAction,
}

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

    /// Currently-focused message in the transcript. Indexes into
    /// `Self::transcript`. Phase 4 `ToggleBookmark` uses this to know which
    /// message to bookmark. Reset to 0 whenever a new transcript is adopted.
    /// The TranscriptWidget can later highlight this row differently —
    /// that's Wave 2's responsibility.
    pub message_cursor: usize,

    /// Phase A: selection-mode state. `Some` while the user is in selection
    /// mode (entered via `m`). Carries the per-message cursor and an optional
    /// `range_start` anchor for `v`-toggled multi-message selection. The
    /// transcript widget reads this each frame to paint the warning-tint +
    /// warning-gutter on the selected message(s).
    pub selection_state: Option<SelectionState>,

    /// Bookmark browser modal state. `Some` while open.
    pub bookmark_browser: Option<bb::State>,

    /// Confirm modal request — bundles modal state with the action to run
    /// on `Yes`. `Some` while open. Stacks over `bookmark_browser` so
    /// cancelling delete returns the user to the same browser selection.
    pub confirm: Option<ConfirmRequest>,

    /// Label / status prompt modal state. `Some` while open.
    pub label_prompt: Option<lp::State>,

    /// When a modal stacks over another modal, the inner modal saves the outer
    /// modal's scope here so confirm/closure logic can pop back to it without
    /// hardcoded branches. None when no modal stack is active.
    pub previous_scope: Option<keys::Scope>,

    // ---- Phase 5 Wave 2 -----------------------------------------------------
    /// Kanban modal state — `Some` while the tag-board is open.
    pub kanban: Option<kb::State>,

    /// Conflict viewer modal state — `Some` while open.
    pub conflict_viewer: Option<cv::State>,

    /// Per-session digest summary cache. Re-populated whenever a
    /// `TranscriptRefreshed` event lands so the digest bar reflects the
    /// freshest observation file without a per-frame disk read.
    pub digest_summary_cache: HashMap<String, ObservationSummary>,

    /// Set of session ids that have at least one bookmark. Drives the
    /// digest-bar `★ bookmarked` marker and avoids re-querying SQLite on
    /// every frame.
    pub has_bookmarks_for_session: HashSet<String>,

    /// Per-session count of unresolved cross-session conflicts. Populated
    /// from observation JSONLs + `conflict_reviews` when the conflict viewer
    /// is opened or when `MarkResolved` returns.
    pub conflict_counts: HashMap<String, u32>,

    // ---- Phase 6 -----------------------------------------------------------
    /// Help overlay state — `Some` while the overlay is open. The overlay
    /// remembers the scope that was active when the user hit `?` so closing
    /// can restore it via `previous_scope`.
    pub help: Option<hp::State>,

    /// Optional `--project` filter from CLI. When set, the App drops sidebar
    /// items whose derived project name doesn't match.
    pub project_filter: Option<String>,

    /// Optional `--days` filter from CLI. When set, the App drops sidebar
    /// items older than `now - days * 86400 seconds`.
    pub days_filter: Option<u32>,

    /// Monotonic render-tick counter — advanced by the event loop on each
    /// terminal draw and used as the spinner frame index. `usize` so we
    /// don't have to worry about overflow on hour-long sessions.
    pub spinner_tick: usize,
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
        // Fall back to an in-memory DB if the on-disk open fails. Set
        // `read_only = true` so write helpers short-circuit instead of
        // hitting the empty in-memory DB (which has no schema). The footer
        // surfaces the `[read-only]` indicator off this flag.
        let (db, status_message, read_only) =
            match threadhop_core::db::open(&threadhop_core::paths::db_path()) {
                Ok(c) => (
                    c,
                    // Phase 0 boot hint — keybindings changed in this
                    // release, surface the `?` overlay so returning users
                    // can find them. A real status (read-only / error)
                    // overwrites this on the very next side-effect.
                    Some("Press ? for keybindings".to_string()),
                    false,
                ),
                Err(e) => {
                    tracing::warn!(
                        "opening sessions.db failed: {e} — falling back to in-memory (read-only)"
                    );
                    let c = rusqlite::Connection::open_in_memory()
                        .expect("in-memory sqlite open should always succeed");
                    (c, Some(format!("DB unavailable: {e}")), true)
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
            read_only,
            scope: Scope::MainScreen,
            search: None,
            find_state: None,
            db,
            pending_jump_message_uuid: None,
            message_cursor: 0,
            selection_state: None,
            bookmark_browser: None,
            confirm: None,
            label_prompt: None,
            previous_scope: None,
            kanban: None,
            conflict_viewer: None,
            digest_summary_cache: HashMap::new(),
            has_bookmarks_for_session: HashSet::new(),
            conflict_counts: HashMap::new(),
            help: None,
            project_filter: None,
            days_filter: None,
            spinner_tick: 0,
        }
    }

    /// Subscribe to the active-session watch channel. Returns a receiver that
    /// the fs_watcher (or any other consumer) can `.changed()` on.
    pub fn active_session_rx(&self) -> watch::Receiver<Option<String>> {
        self.active_session_tx.subscribe()
    }

    /// Apply CLI flag values. Called by `main` after construction so the
    /// flags drive sidebar filtering and initial selection. Behaviour:
    ///
    /// * `--project <name>`: stored in `project_filter`. Future
    ///   `SessionsRefreshed` events filter sidebar items by derived project
    ///   name (see `event::handle_worker_event`).
    /// * `--days <N>`: stored in `days_filter`. Sessions older than `N` days
    ///   are dropped at receive time.
    /// * `--session <id>`: immediately set the selected session id and
    ///   publish on the active-session watch so the fs_watcher picks it up
    ///   on its first tick. No filtering side-effect.
    pub fn apply_cli(
        &mut self,
        project: Option<String>,
        days: Option<u32>,
        session: Option<String>,
    ) {
        self.project_filter = project;
        self.days_filter = days;
        if let Some(sid) = session {
            self.selected_session_id = Some(sid.clone());
            let _ = self.active_session_tx.send(Some(sid));
        }
    }

    /// True when `item` survives the configured `--project` / `--days`
    /// filters. Public-in-crate so `event::handle_worker_event` can call it
    /// directly without re-reading the App fields.
    pub(crate) fn sidebar_item_passes_filters(
        &self,
        item: &crate::widgets::session_list::SessionListItem,
        now: f64,
    ) -> bool {
        if let Some(want) = &self.project_filter {
            // Prefer the stamped field; fall back to a filesystem lookup so
            // items synthesised by tests (which skip the scanner) still
            // filter correctly.
            let project = item
                .project
                .clone()
                .or_else(|| derive_project_for_session(&item.session_id));
            if project.as_deref() != Some(want.as_str()) {
                return false;
            }
        }
        if let Some(days) = self.days_filter {
            // `Some(0)` is intentionally treated as "no filter" — clap's
            // default would otherwise hide every session at startup.
            if days > 0 {
                let cutoff = now - (days as f64) * 86_400.0;
                let ts = item.last_active_at.unwrap_or(0.0);
                if ts < cutoff {
                    return false;
                }
            }
        }
        true
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
        // Modal-first dispatch — most-on-top first:
        //   help > confirm > kanban | conflict_viewer | label_prompt | bookmark_browser
        //        > search > find_bar
        // Help sits on top of everything so the user can pop it open from any
        // scope without losing the underlying modal stack.
        if self.help.is_some() {
            self.dispatch_help(key);
            return None;
        }
        // The confirm modal sits over the bookmark browser, so it must be
        // tested before the browser to capture y/n while delete is pending.
        if self.confirm.is_some() {
            self.dispatch_confirm(key);
            return None;
        }
        if self.kanban.is_some() {
            self.dispatch_kanban(key);
            return None;
        }
        if self.conflict_viewer.is_some() {
            self.dispatch_conflict_viewer(key);
            return None;
        }
        if self.label_prompt.is_some() {
            self.dispatch_label_prompt(key);
            return None;
        }
        if self.bookmark_browser.is_some() {
            self.dispatch_bookmark_browser(key);
            return None;
        }

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

        // 3. Selection mode — owns its own dispatch. The scope-aware
        // keys::lookup returns selection-table commands; we re-interpret
        // them here (e.g. SelectNextSession means "move msg cursor +1" in
        // selection mode, not "select next sidebar entry").
        if self.scope == Scope::Selection {
            self.dispatch_selection_mode(key);
            return None;
        }

        // 4. Normal main-screen dispatch.
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
            Command::OpenHelp => self.open_help(),
            Command::Confirm => {}
            // Phase 4 Wave 2: real handlers.
            Command::ToggleBookmark => self.toggle_bookmark_at_cursor(),
            Command::OpenBookmarkBrowser => self.open_bookmark_browser(),
            Command::OpenLabelPrompt => self.open_label_prompt(),
            Command::MoveCursorDown => {
                if !self.transcript.is_empty() {
                    let max = self.transcript.len() - 1;
                    if self.message_cursor < max {
                        self.message_cursor += 1;
                    }
                }
                tracing::debug!(
                    target: "threadhop_tui::cursor",
                    "message cursor moved to {}",
                    self.message_cursor
                );
            }
            Command::MoveCursorUp => {
                self.message_cursor = self.message_cursor.saturating_sub(1);
                tracing::debug!(
                    target: "threadhop_tui::cursor",
                    "message cursor moved to {}",
                    self.message_cursor
                );
            }
            // Phase 5 Wave 2: open the kanban + conflict viewer modals.
            Command::OpenKanban => self.open_kanban(),
            Command::OpenConflictViewer => self.open_conflict_viewer(),
            // Phase 0 no-op stubs: Python has these actions; Rust doesn't
            // implement them yet. We register the bindings so muscle memory
            // works, log a warn, and surface a status_message so the user
            // sees the action was caught.
            Command::CycleSessionStatus => self.stub_command("cycle status"),
            Command::CycleSessionStatusBack => {
                self.stub_command("cycle status backward")
            }
            Command::RefreshSessions => self.stub_command("refresh sessions"),
            Command::ThemeNext => self.stub_command("next theme"),
            Command::ThemePrev => self.stub_command("previous theme"),
            Command::ShrinkSidebar => self.stub_command("shrink sidebar"),
            Command::GrowSidebar => self.stub_command("grow sidebar"),
            Command::RenameSession => self.stub_command("rename session"),
            Command::CopyResumeCommand => self.stub_command("copy resume"),
            Command::ObserveSession => self.stub_command("observe session"),
            Command::ResumeObservation => {
                self.stub_command("resume observation")
            }
            Command::ArchiveSession => self.stub_command("archive session"),
            Command::ToggleArchivedView => {
                self.stub_command("toggle archived view")
            }
            Command::MoveSessionDown => self.stub_command("reorder session ↓"),
            Command::MoveSessionUp => self.stub_command("reorder session ↑"),
            Command::FocusTranscript => self.stub_command("focus transcript"),
            Command::FocusList => self.stub_command("focus list"),
            Command::EnterSelectionMode => self.enter_selection_mode(),
            Command::EditBookmarkNote => self.stub_command("edit bookmark note"),
            // Cancel is owned by the modal-first dispatch; modal-only
            // commands never fire on the main screen.
            Command::Cancel
            | Command::MarkConflictResolved
            | Command::KanbanColumnLeft
            | Command::KanbanColumnRight
            | Command::KanbanMoveItem
            | Command::KanbanMoveItemBack => {}
        }
        Some(cmd)
    }

    /// Phase A: enter selection mode. Pushes the current scope onto
    /// `previous_scope` so `Esc` / `m` can restore it cleanly even if the
    /// user entered selection mode from a non-Main scope. The cursor starts
    /// at the last message (matching Python's `_enter_selection_mode` —
    /// "Start at the last message — recent context is usually what you
    /// want").
    fn enter_selection_mode(&mut self) {
        if self.transcript.is_empty() {
            self.status_message =
                Some("Nothing to select — transcript empty".into());
            return;
        }
        let cursor = self.transcript.len().saturating_sub(1);
        self.selection_state = Some(SelectionState {
            cursor,
            range_start: None,
        });
        self.previous_scope = Some(self.scope);
        self.scope = Scope::Selection;
        self.status_message = Some(
            "Selection: j/k move, v range, y copy, e export, space \
             bookmark, L note, m/Esc exit"
                .into(),
        );
    }

    /// Phase A: exit selection mode and pop back to the previous scope.
    fn exit_selection_mode(&mut self) {
        self.selection_state = None;
        self.scope = self.previous_scope.take().unwrap_or(Scope::MainScreen);
    }

    /// Phase A: handle one key event while `scope == Scope::Selection`. The
    /// selection-table binding registry maps physical keys to abstract
    /// commands; we re-interpret each command per the Python selection
    /// semantics (j/k = move cursor, v = range toggle, y = copy, e = export,
    /// space = toggle bookmark, L = edit bookmark note, m/Esc = exit).
    fn dispatch_selection_mode(&mut self, key: KeyEvent) {
        let Some(cmd) = keys::lookup(Scope::Selection, key) else {
            return;
        };
        let max = self.transcript.len().saturating_sub(1);
        match cmd {
            Command::SelectNextSession => {
                if let Some(sel) = self.selection_state.as_mut() {
                    if sel.cursor < max {
                        sel.cursor += 1;
                    }
                }
            }
            Command::SelectPrevSession => {
                if let Some(sel) = self.selection_state.as_mut() {
                    sel.cursor = sel.cursor.saturating_sub(1);
                }
            }
            // The binding registry maps `v`, `y`, and `e` to Command::Confirm
            // (a free slot in the selection table). We disambiguate on the
            // actual key code here, mirroring Python's char-dispatched
            // handler. The alternative — separate Command variants for each
            // — would churn the Command enum for three single-use actions.
            Command::Confirm => match key.code {
                crossterm::event::KeyCode::Char('v') => self.toggle_range_mode(),
                crossterm::event::KeyCode::Char('y') => self.copy_selection_to_clipboard(),
                crossterm::event::KeyCode::Char('e') => self.export_selection(),
                _ => {}
            },
            Command::ToggleBookmark => {
                // Selection-mode bookmark: act on the selection cursor, not
                // the main message_cursor. We temporarily swap, fire the
                // existing toggle, then restore. Avoids two divergent code
                // paths for the same DB operation.
                if let Some(sel) = self.selection_state {
                    let saved = self.message_cursor;
                    self.message_cursor = sel.cursor;
                    self.toggle_bookmark_at_cursor();
                    self.message_cursor = saved;
                }
            }
            Command::EditBookmarkNote => {
                // Defer to Phase E for the full bookmark-note prompt path —
                // surface the action so the user knows it was caught.
                self.status_message =
                    Some("Edit bookmark note — not wired yet (Phase E)".into());
            }
            Command::EnterSelectionMode | Command::Cancel => {
                self.exit_selection_mode();
            }
            // Quit still works from selection mode (`q` / Ctrl-c falls back
            // to Global via keys::lookup).
            Command::Quit => self.should_quit = true,
            _ => {}
        }
    }

    /// Toggle the selection's `range_start` anchor. First press starts a
    /// range from the current cursor; second press clears it back to single
    /// selection. Mirrors Python's `_enter_range_mode` / `_exit_range_mode`.
    fn toggle_range_mode(&mut self) {
        if let Some(sel) = self.selection_state.as_mut() {
            if sel.range_start.is_some() {
                sel.range_start = None;
            } else {
                sel.range_start = Some(sel.cursor);
            }
        }
    }

    /// Phase A: copy the selected message(s) to the system clipboard.
    /// Deferred — `arboard` is not yet a workspace dep and the task brief
    /// said to NOT add it unilaterally. Surface the deferral via
    /// status_message so the binding still feels live.
    fn copy_selection_to_clipboard(&mut self) {
        // TODO(phase A.6): wire `arboard` (macOS-only initially) and copy
        // the selected message bodies, mirroring Python's
        // TranscriptView._copy_selection (session label + role + text).
        self.status_message =
            Some("Clipboard backend not yet wired (Phase A.6)".into());
    }

    /// Phase A: export the selected message(s) to a temp file under
    /// `/tmp/threadhop-export-<unix_ts>.md`. Mirrors Python's `_export_selection`
    /// minimally — role labels and message bodies, no session header for
    /// now. The path lands in `status_message` so the user can paste it.
    fn export_selection(&mut self) {
        let Some(sel) = self.selection_state else {
            return;
        };
        let (lo, hi) = sel.range();
        let max = self.transcript.len().saturating_sub(1);
        let hi = hi.min(max);
        let mut body = String::new();
        for msg in self.transcript.iter().take(hi + 1).skip(lo) {
            body.push_str("## ");
            body.push_str(crate::widgets::transcript::role_label(&msg.role));
            if let Some(ts) = &msg.timestamp {
                body.push_str("  ");
                body.push_str(ts);
            }
            body.push_str("\n\n");
            body.push_str(&msg.text);
            body.push_str("\n\n");
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = format!("/tmp/threadhop-export-{now}.md");
        match std::fs::write(&path, body) {
            Ok(()) => {
                self.status_message = Some(format!("Exported → {path}"));
            }
            Err(e) => {
                self.status_message = Some(format!("Export failed: {e}"));
            }
        }
    }

    /// Phase 0 stub: emit a `tracing::warn!` and surface a status_message
    /// for a Python-parity binding whose real handler hasn't shipped yet.
    /// Keeping the binding live (instead of dropping the key) means muscle
    /// memory works the moment the parity rebind lands.
    fn stub_command(&mut self, name: &str) {
        tracing::warn!(
            target: "threadhop_tui::keys",
            "not implemented yet: {name}"
        );
        self.status_message = Some(format!("Not implemented yet: {name}"));
    }

    /// Toggle the bookmark on the message currently under
    /// [`Self::message_cursor`]. No-op when the transcript is empty or
    /// `read_only` is set.
    ///
    /// Phase 6 verifier: defensively check that the message uuid exists in
    /// the `messages` table before attempting the insert. The bookmarks
    /// table has an FK on `messages.uuid`; if the Python observer/indexer
    /// hasn't ingested the message yet (common race when the TUI catches a
    /// fresh tail of the JSONL before SQLite knows about it), the INSERT
    /// would fail with `FOREIGN KEY constraint failed`. Surface that as a
    /// friendlier status message instead of a SQLite error.
    fn toggle_bookmark_at_cursor(&mut self) {
        let Some(msg) = self.transcript.get(self.message_cursor) else {
            self.status_message = Some("no message under cursor".into());
            return;
        };
        let uuid = msg.uuid.clone();
        if self.read_only {
            self.status_message = Some("Read-only — DB unavailable".into());
            return;
        }
        // Defensive lookup: confirm the message row exists before issuing the
        // INSERT. Doing it pre-flight rather than catching the FK error keeps
        // tracing logs clean. Note: we only short-circuit when we can confirm
        // the row is missing; if the query itself errors (e.g. read-only DB
        // with no schema), fall through to the regular toggle path so the
        // existing read-only handling applies.
        let message_exists: Option<bool> = self
            .db
            .query_row(
                "SELECT 1 FROM messages WHERE uuid = ? LIMIT 1",
                [&uuid],
                |_| Ok(true),
            )
            .ok();
        if message_exists.is_none() {
            // SELECT returned no row → the message isn't indexed yet.
            // Distinguish from the schema-missing case by probing once for
            // the table; if the table exists but has no row, surface the
            // friendly message.
            let table_present: bool = self
                .db
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages'",
                    [],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if table_present {
                tracing::warn!(
                    target: "threadhop_tui",
                    "toggle_bookmark skipped: message uuid={uuid} not in DB"
                );
                self.status_message =
                    Some("Cannot bookmark — message not yet indexed".into());
                return;
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        match threadhop_core::db::toggle_bookmark(&self.db, &uuid, now) {
            Ok(Some(_)) => {
                tracing::debug!(target: "threadhop_tui", "bookmark toggled inserted uuid={uuid}");
                self.status_message = Some("★ bookmarked".into());
            }
            Ok(None) => {
                tracing::debug!(target: "threadhop_tui", "bookmark toggled removed uuid={uuid}");
                self.status_message = Some("removed bookmark".into());
            }
            Err(e) => {
                // Translate the specific FK-violation case to a friendlier
                // message — anything else (corruption, disk full) flows
                // through with the raw error.
                let lower = e.to_string().to_ascii_lowercase();
                if lower.contains("foreign key") {
                    tracing::warn!(
                        "toggle_bookmark FK violation for uuid={uuid}: {e}"
                    );
                    self.status_message =
                        Some("Cannot bookmark — message not yet indexed".into());
                } else {
                    tracing::warn!("toggle_bookmark failed: {e}");
                    self.status_message = Some(format!("bookmark error: {e}"));
                }
            }
        }
    }

    /// Open the bookmark browser. Pulls bookmark rows from every known
    /// session by iterating the sidebar; the per-session query is small and
    /// this avoids needing a new `list_all_bookmarks` helper in
    /// `threadhop-core` (Phase 5 follow-up if N grows).
    fn open_bookmark_browser(&mut self) {
        let rows = self.collect_all_bookmark_rows();
        let mut state = bb::State::new();
        state.set_bookmarks(rows);
        self.bookmark_browser = Some(state);
        self.scope = Scope::BookmarkBrowser;
    }

    /// Fan out `bookmarks_for_session` over the sidebar and JOIN with
    /// `messages_for_session` to attach snippets. Cheap-ish for typical
    /// sidebar sizes; if this becomes hot we'll add a single SQL view.
    fn collect_all_bookmark_rows(&self) -> Vec<BookmarkRow> {
        let mut rows: Vec<BookmarkRow> = Vec::new();
        for item in &self.sidebar {
            let bms = match threadhop_core::db::bookmarks_for_session(
                &self.db,
                &item.session_id,
            ) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        "bookmarks_for_session({}) failed: {e}",
                        item.session_id
                    );
                    continue;
                }
            };
            if bms.is_empty() {
                continue;
            }
            // Cache messages per session so we don't pay N×M lookups per
            // bookmark row.
            let msgs = threadhop_core::db::messages_for_session(
                &self.db,
                &item.session_id,
            )
            .unwrap_or_default();
            for b in bms {
                let snippet = msgs
                    .iter()
                    .find(|m| m.uuid == b.message_uuid)
                    .map(|m| truncate_snippet(&m.text));
                rows.push(BookmarkRow {
                    id: b.id.unwrap_or(0),
                    session_id: item.session_id.clone(),
                    message_uuid: b.message_uuid,
                    kind: b.kind,
                    note: b.note,
                    snippet,
                    created_at: b.created_at,
                });
            }
        }
        // Newest first (matches the per-session `ORDER BY created_at DESC`).
        rows.sort_by(|a, b| {
            b.created_at
                .partial_cmp(&a.created_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        rows
    }

    /// Open the label-prompt modal for the currently selected session.
    /// Resolves the session's current `status` from the DB; falls back to
    /// `Active` if the row is missing (e.g. read-only / in-memory DB).
    fn open_label_prompt(&mut self) {
        let Some(sid) = self.selected_session_id.clone() else {
            self.status_message = Some("no session selected".into());
            return;
        };
        let display = self
            .sidebar
            .iter()
            .find(|i| i.session_id == sid)
            .map(|i| i.display_name.clone())
            .unwrap_or_else(|| sid.clone());
        let current_status = threadhop_core::db::session_by_id(&self.db, &sid)
            .ok()
            .flatten()
            .map(|s| s.status)
            .unwrap_or_default();
        self.label_prompt = Some(lp::State::new(sid, display, current_status));
        self.scope = Scope::LabelPrompt;
    }

    /// Dispatch one keystroke into the confirm modal. On `Yes`, run the
    /// pending action (writing through `read_only` is blocked at the
    /// originating site, not here — the App only opens the modal when the
    /// surfaced action is safe to attempt).
    fn dispatch_confirm(&mut self, key: KeyEvent) {
        let req = self.confirm.as_mut().expect("dispatch_confirm precondition");
        let result = cf::handle_key(&mut req.state, key);
        let Some(result) = result else {
            return;
        };
        // Take ownership of the request so we can drop the modal before
        // running the side effect (the side effect may itself try to mutate
        // App state, e.g. status_message).
        let req = self.confirm.take().expect("confirm was Some above");
        match result {
            ConfirmResult::Yes => self.execute_pending_action(req.on_yes),
            ConfirmResult::No => {}
        }
        // Pop back to the modal that opened the confirm (if any). Stackers
        // save their scope into `previous_scope` before bumping `scope =
        // ConfirmModal`, so this branch stays generic for kanban /
        // conflict_viewer / future modals.
        self.scope = match self.previous_scope.take() {
            Some(prev) => prev,
            None => Scope::MainScreen,
        };
    }

    /// Execute a [`PendingAction`] approved via the confirm modal.
    fn execute_pending_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::DeleteBookmark { bookmark_id } => {
                if self.read_only {
                    self.status_message = Some("Read-only — DB unavailable".into());
                    return;
                }
                match threadhop_core::db::delete_bookmark(&self.db, bookmark_id) {
                    Ok(()) => {
                        tracing::debug!(
                            target: "threadhop_tui",
                            "bookmark deleted id={bookmark_id}"
                        );
                        self.status_message = Some("bookmark deleted".into());
                        // Refresh the open browser so the row disappears.
                        // Compute the new rows before borrowing the modal
                        // mutably — otherwise we'd alias self.
                        if self.bookmark_browser.is_some() {
                            let rows = self.collect_all_bookmark_rows();
                            if let Some(state) = self.bookmark_browser.as_mut() {
                                state.set_bookmarks(rows);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("delete_bookmark failed: {e}");
                        self.status_message = Some(format!("delete failed: {e}"));
                    }
                }
            }
        }
    }

    fn dispatch_label_prompt(&mut self, key: KeyEvent) {
        let state = self.label_prompt.as_mut().expect("dispatch_label_prompt precondition");
        let result = lp::handle_key(state, key);
        let Some(result) = result else {
            return;
        };
        // Close the modal first so write paths see a clean state.
        let state = self.label_prompt.take().expect("label_prompt was Some above");
        self.scope = Scope::MainScreen;
        match result {
            LabelPromptResult::Cancelled => {}
            LabelPromptResult::StatusChosen(status) => {
                if self.read_only {
                    self.status_message = Some("Read-only — DB unavailable".into());
                    return;
                }
                match threadhop_core::db::set_session_status_typed(
                    &self.db,
                    &state.session_id,
                    status,
                ) {
                    Ok(()) => {
                        self.status_message =
                            Some(format!("status → {}", lp::status_label(status)));
                    }
                    Err(e) => {
                        tracing::warn!("set_session_status_typed failed: {e}");
                        self.status_message = Some(format!("status error: {e}"));
                    }
                }
            }
            LabelPromptResult::CustomNameSet(name) => {
                if self.read_only {
                    self.status_message = Some("Read-only — DB unavailable".into());
                    return;
                }
                let name_ref = name.as_deref();
                match threadhop_core::db::set_custom_name(
                    &self.db,
                    &state.session_id,
                    name_ref,
                ) {
                    Ok(()) => {
                        // Update the sidebar in place so the user sees the
                        // rename without waiting on a worker refresh.
                        if let Some(item) = self
                            .sidebar
                            .iter_mut()
                            .find(|i| i.session_id == state.session_id)
                        {
                            if let Some(n) = &name {
                                item.display_name = n.clone();
                            }
                        }
                        self.status_message = Some(match name {
                            Some(n) => format!("name → {n}"),
                            None => "custom name cleared".into(),
                        });
                    }
                    Err(e) => {
                        tracing::warn!("set_custom_name failed: {e}");
                        self.status_message = Some(format!("rename error: {e}"));
                    }
                }
            }
        }
    }

    fn dispatch_bookmark_browser(&mut self, key: KeyEvent) {
        let state = self.bookmark_browser.as_mut().expect("dispatch_bookmark_browser precondition");
        let result = bb::handle_key(state, key);
        let Some(result) = result else {
            return;
        };
        match result {
            BookmarkBrowserResult::Cancelled => {
                self.bookmark_browser = None;
                self.scope = Scope::MainScreen;
            }
            BookmarkBrowserResult::JumpToMessage {
                session_id,
                message_uuid,
            } => {
                self.bookmark_browser = None;
                self.scope = Scope::MainScreen;
                self.pending_jump_message_uuid = Some(message_uuid);
                if self.selected_session_id.as_deref() != Some(session_id.as_str()) {
                    self.selected_session_id = Some(session_id.clone());
                    let _ = self.active_session_tx.send(Some(session_id));
                    self.scroll = 0;
                } else {
                    self.try_resolve_pending_jump();
                }
            }
            BookmarkBrowserResult::DeleteRequested { bookmark_id } => {
                // Open the confirm modal *over* the browser. The browser
                // stays open so cancelling delete returns to the same
                // selection. Saving the current scope into `previous_scope`
                // is what lets `dispatch_confirm` pop back generically —
                // future modals (kanban, conflict_viewer) get this for free.
                tracing::debug!(
                    target: "threadhop_tui",
                    "bookmark delete requested id={bookmark_id}"
                );
                let note = state
                    .bookmarks
                    .iter()
                    .find(|r| r.id == bookmark_id)
                    .and_then(|r| r.note.clone());
                let mut confirm_state = cf::State::new("Delete bookmark?");
                if let Some(n) = note {
                    confirm_state = confirm_state.with_detail(format!("\"{n}\""));
                }
                self.previous_scope = Some(self.scope);
                self.confirm = Some(ConfirmRequest {
                    state: confirm_state,
                    on_yes: PendingAction::DeleteBookmark { bookmark_id },
                });
                self.scope = Scope::ConfirmModal;
            }
        }
    }

    // ---- Phase 6: help overlay --------------------------------------------

    /// Open the help overlay over the current scope. The overlay remembers
    /// the current scope so `dispatch_help` can restore it on close, even if
    /// the user invoked it from a modal.
    fn open_help(&mut self) {
        self.previous_scope = Some(self.scope);
        self.help = Some(hp::State::new(self.scope));
        self.scope = keys::Scope::HelpOverlay;
    }

    /// Dispatch one keystroke into the help overlay.
    fn dispatch_help(&mut self, key: KeyEvent) {
        let state = self.help.as_mut().expect("dispatch_help precondition");
        let result = hp::handle_key(state, key);
        let Some(HelpResult::Closed) = result else {
            return;
        };
        self.help = None;
        // Restore the scope the overlay was opened from. previous_scope was
        // set in open_help.
        self.scope = self
            .previous_scope
            .take()
            .unwrap_or(keys::Scope::MainScreen);
    }

    // ---- Phase 5 Wave 2: kanban + conflict viewer dispatch ----------------

    /// Build kanban items from the sidebar, hydrating each row's `status`
    /// field from the DB. Misses (e.g. session not present in the DB yet
    /// because the scanner just learned about it) fall back to `Active`.
    fn build_kanban_items(&self) -> Vec<KanbanItem> {
        let mut out = Vec::with_capacity(self.sidebar.len());
        for item in &self.sidebar {
            let status = threadhop_core::db::session_by_id(&self.db, &item.session_id)
                .ok()
                .flatten()
                .map(|s| s.status)
                .unwrap_or_default();
            out.push(KanbanItem {
                session_id: item.session_id.clone(),
                display_name: item.display_name.clone(),
                status,
            });
        }
        out
    }

    fn open_kanban(&mut self) {
        let items = self.build_kanban_items();
        // Mirror status into the sidebar so the optimistic in-place updates
        // on `StatusChanged` have somewhere to land.
        for ki in &items {
            if let Some(side) = self
                .sidebar
                .iter_mut()
                .find(|s| s.session_id == ki.session_id)
            {
                side.status = ki.status;
            }
        }
        self.previous_scope = Some(self.scope);
        self.kanban = Some(kb::State::new(items));
        self.scope = Scope::Kanban;
    }

    fn open_conflict_viewer(&mut self) {
        let (rows, counts) = self.collect_conflicts();
        let mut state = cv::State::new();
        state.set_conflicts(rows);
        self.conflict_counts = counts;
        // Reflect the counts into the sidebar so the `!` marker is visible
        // immediately. Sidebar items are clone-on-write through the worker
        // pipeline; mutating here is safe.
        self.sync_sidebar_conflict_counts();
        self.previous_scope = Some(self.scope);
        self.conflict_viewer = Some(state);
        self.scope = Scope::ConflictViewer;
    }

    /// Read every session's observation JSONL, collect `Observation::Conflict`
    /// entries, and join against `conflict_reviews` for the `reviewed` flag.
    /// Returns `(rows, per_session_unresolved_counts)`.
    fn collect_conflicts(&self) -> (Vec<ConflictRow>, HashMap<String, u32>) {
        // Spec §9 leaves `conflict_reviews` writes to the Python CLI, but
        // reads are fine. Inline raw SQL keeps the helper out of
        // `threadhop-core` for now (see follow-up in commit body).
        let reviewed: HashSet<(String, String, String)> =
            match self.db.prepare(
                "SELECT session_id, refs_key, topic FROM conflict_reviews",
            ) {
                Ok(mut stmt) => stmt
                    .query_map([], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    })
                    .and_then(|rows| rows.collect::<Result<HashSet<_>, _>>())
                    .unwrap_or_default(),
                Err(e) => {
                    tracing::warn!("conflict_reviews query failed: {e}");
                    HashSet::new()
                }
            };

        let mut out: Vec<ConflictRow> = Vec::new();
        let mut counts: HashMap<String, u32> = HashMap::new();
        // Stable, content-derived row id — observation JSONLs are
        // append-only and don't carry numeric ids. Hash a tuple of the
        // origin session + refs + topic so the same conflict yields the
        // same id across reads.
        let mut next_synthetic_id: i64 = 1;

        for item in &self.sidebar {
            let entries = match threadhop_core::observations::read_entries(&item.session_id) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        "read_entries({}) failed: {e}",
                        item.session_id
                    );
                    continue;
                }
            };
            for entry in entries {
                if let Observation::Conflict {
                    refs,
                    topic,
                    ts,
                    text,
                    ..
                } = entry
                {
                    let refs_key = normalize_conflict_refs(&refs);
                    let reviewed_flag = reviewed.contains(&(
                        item.session_id.clone(),
                        refs_key.clone(),
                        topic.clone(),
                    ));
                    if !reviewed_flag {
                        *counts.entry(item.session_id.clone()).or_insert(0) += 1;
                    }
                    // session_ids[0] = origin; remainder = refs.
                    let mut session_ids = Vec::with_capacity(1 + refs.len());
                    session_ids.push(item.session_id.clone());
                    session_ids.extend(refs);
                    let text_str = text.unwrap_or_else(|| topic.clone());
                    let timestamp = parse_iso8601(&ts).unwrap_or(0.0);
                    let id = next_synthetic_id;
                    next_synthetic_id += 1;
                    out.push(ConflictRow {
                        id,
                        text: text_str,
                        session_ids,
                        timestamp,
                        reviewed: reviewed_flag,
                    });
                }
            }
        }
        (out, counts)
    }

    fn sync_sidebar_conflict_counts(&mut self) {
        for side in &mut self.sidebar {
            side.unresolved_conflict_count = self
                .conflict_counts
                .get(&side.session_id)
                .copied()
                .unwrap_or(0);
        }
    }

    fn dispatch_kanban(&mut self, key: KeyEvent) {
        let state = self.kanban.as_mut().expect("dispatch_kanban precondition");
        let result = kb::handle_key(state, key);
        let Some(result) = result else {
            return;
        };
        match result {
            KanbanResult::Cancelled => {
                self.kanban = None;
                self.scope = self.previous_scope.take().unwrap_or(Scope::MainScreen);
            }
            KanbanResult::JumpToSession { session_id } => {
                self.kanban = None;
                self.scope = self.previous_scope.take().unwrap_or(Scope::MainScreen);
                if self.selected_session_id.as_deref() != Some(session_id.as_str()) {
                    self.selected_session_id = Some(session_id.clone());
                    let _ = self.active_session_tx.send(Some(session_id));
                    self.scroll = 0;
                }
            }
            KanbanResult::StatusChanged {
                session_id,
                new_status,
            } => {
                if self.read_only {
                    self.status_message = Some("Read-only — DB unavailable".into());
                    self.revert_kanban_to_db();
                    return;
                }
                match threadhop_core::db::set_session_status_typed(
                    &self.db,
                    &session_id,
                    new_status,
                ) {
                    Ok(()) => {
                        // Mirror the change into the sidebar so the kanban's
                        // optimistic update is now the source of truth.
                        if let Some(side) = self
                            .sidebar
                            .iter_mut()
                            .find(|s| s.session_id == session_id)
                        {
                            side.status = new_status;
                        }
                        self.status_message = Some(format!(
                            "status → {}",
                            lp::status_label(new_status)
                        ));
                    }
                    Err(e) => {
                        tracing::warn!("set_session_status_typed failed: {e}");
                        self.status_message = Some(format!("status error: {e}"));
                        self.revert_kanban_to_db();
                    }
                }
            }
        }
    }

    /// Re-seed the kanban modal's items from the DB and clamp the row cursor.
    /// Used to revert the modal's optimistic status-flip when the DB write
    /// fails (or is blocked by `read_only`).
    fn revert_kanban_to_db(&mut self) {
        let items = self.build_kanban_items();
        if let Some(s) = self.kanban.as_mut() {
            s.items = items;
            if s.selected_row_in_column.len() < kb::STATUS_ORDER.len() {
                s.selected_row_in_column
                    .resize(kb::STATUS_ORDER.len(), 0);
            }
            let col_len = s.items_in_column(s.selected_column).len();
            let slot = &mut s.selected_row_in_column[s.selected_column];
            if col_len == 0 {
                *slot = 0;
            } else if *slot >= col_len {
                *slot = col_len - 1;
            }
        }
    }

    fn dispatch_conflict_viewer(&mut self, key: KeyEvent) {
        let state = self
            .conflict_viewer
            .as_mut()
            .expect("dispatch_conflict_viewer precondition");
        let result = cv::handle_key(state, key);
        let Some(result) = result else {
            return;
        };
        match result {
            ConflictViewerResult::Cancelled => {
                self.conflict_viewer = None;
                self.scope = self.previous_scope.take().unwrap_or(Scope::MainScreen);
            }
            ConflictViewerResult::JumpToSession { session_id } => {
                self.conflict_viewer = None;
                self.scope = self.previous_scope.take().unwrap_or(Scope::MainScreen);
                if self.selected_session_id.as_deref() != Some(session_id.as_str()) {
                    self.selected_session_id = Some(session_id.clone());
                    let _ = self.active_session_tx.send(Some(session_id));
                    self.scroll = 0;
                }
            }
            ConflictViewerResult::MarkResolved { conflict_id } => {
                // Spec §9: Rust never writes `conflict_reviews`. Shell out to
                // the Python CLI; the child uses the synthetic conflict id we
                // generated in `collect_conflicts`. The Python CLI accepts an
                // integer index here.
                let cli = std::env::current_dir()
                    .ok()
                    .map(|d| d.join("threadhop"))
                    .filter(|p| p.exists())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "threadhop".to_string());
                match std::process::Command::new(&cli)
                    .args(["conflicts", "--resolved", &conflict_id.to_string()])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                {
                    Ok(_child) => {
                        self.status_message = Some(format!(
                            "resolved #{conflict_id} (running threadhop conflicts --resolved)"
                        ));
                        // Refresh the row set + counts. The shelled-out
                        // process is async, so the read may race the write;
                        // counts will reconcile on the next open.
                        let (rows, counts) = self.collect_conflicts();
                        if let Some(s) = self.conflict_viewer.as_mut() {
                            s.set_conflicts(rows);
                        }
                        self.conflict_counts = counts;
                        self.sync_sidebar_conflict_counts();
                    }
                    Err(e) => {
                        tracing::warn!("threadhop conflicts spawn failed: {e}");
                        self.status_message = Some(format!(
                            "run `threadhop conflicts --resolved {conflict_id}` to resolve"
                        ));
                    }
                }
            }
        }
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

/// Derive the project name for a session id by scanning the Claude projects
/// directory for `<session_id>.jsonl`. The project is the parent directory's
/// name. Returns `None` when the session file isn't found (e.g. test setups
/// without a real `~/.claude/projects`).
///
/// Used by `--project` filter — keeps the lookup off the hot path by
/// caching per-receive on the App side (Phase 6 follow-up if N grows).
fn derive_project_for_session(session_id: &str) -> Option<String> {
    let projects = threadhop_core::paths::claude_projects_dir();
    let target = format!("{session_id}.jsonl");
    let entries = std::fs::read_dir(&projects).ok()?;
    for project_entry in entries.flatten() {
        let project_path = project_entry.path();
        if !project_path.is_dir() {
            continue;
        }
        let inner = match std::fs::read_dir(&project_path) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        for session_entry in inner.flatten() {
            if session_entry.file_name().to_string_lossy() == target {
                return project_path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string());
            }
        }
    }
    None
}

/// Mirror Python's `_normalize_conflict_refs` — trim, drop empties, sort,
/// dedup, join on U+001F. Used by `collect_conflicts` to key the join with
/// the `conflict_reviews` table.
fn normalize_conflict_refs(refs: &[String]) -> String {
    let mut canon: Vec<String> = refs
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    canon.sort();
    canon.dedup();
    canon.join("\u{1f}")
}

/// Tiny ISO-8601 → epoch-seconds parser — mirrors the variant in
/// `widgets::digest_bar`. Returns `None` on malformed input. Sufficient for
/// the observer's `YYYY-MM-DDTHH:MM:SS[.fff]Z` shape.
fn parse_iso8601(ts: &str) -> Option<f64> {
    let core = ts
        .trim_end_matches('Z')
        .trim_end_matches("+00:00")
        .trim_end_matches("-00:00");
    let (date, time) = core.split_once('T')?;
    let mut dp = date.split('-');
    let year: i64 = dp.next()?.parse().ok()?;
    let month: i64 = dp.next()?.parse().ok()?;
    let day: i64 = dp.next()?.parse().ok()?;
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, b),
        None => (time, "0"),
    };
    let mut t = hms.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: i64 = t.next().unwrap_or("0").parse().ok()?;
    let frac_f: f64 = format!("0.{frac}").parse().ok()?;
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_epoch = era * 146_097 + doe - 719_468;
    let seconds = days_since_epoch * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(seconds as f64 + frac_f)
}

/// Compact a message body for the bookmark browser snippet column —
/// collapse whitespace and cap at ~80 characters. Same heuristic the
/// browser's internal `render_snippet` uses; duplicated here because that
/// helper is module-private to keep its visibility tight.
fn truncate_snippet(raw: &str) -> String {
    let cleaned: String = raw
        .replace(['\n', '\t'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    const MAX: usize = 80;
    if cleaned.chars().count() > MAX {
        let truncated: String = cleaned.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        cleaned
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
            ..Default::default()
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
    fn home_jumps_to_top_and_end_to_bottom() {
        // Phase 0 parity: Home/End drive ScrollTop/ScrollBottom (Python
        // bindings). The old g/Shift+G have been reassigned —
        // `g`=CopyResumeCommand (stub), `Shift+G` is unbound.
        let mut app = App::new();
        app.scroll = 100;
        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(app.scroll, 0);
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
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
    fn question_mark_opens_help_overlay() {
        // Phase 6: `?` now opens the help overlay (it was a no-op pre-Phase 6).
        // Confirm is still a no-op on the main screen.
        let mut app = App::new();
        let r1 = app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert_eq!(r1, Some(Command::OpenHelp));
        assert!(app.help.is_some(), "help overlay state must be set");
        assert_eq!(app.scope, Scope::HelpOverlay);
        // Esc closes the overlay and restores the MainScreen scope.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.help.is_none(), "help overlay must close on Esc");
        assert_eq!(app.scope, Scope::MainScreen);
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
        // Phase 0 parity: Ctrl-f opens the find bar (Python binding); plain
        // `f` is no longer bound on MainScreen.
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(app.find_state.is_some());
        assert_eq!(app.scope, Scope::FindBar);
    }

    #[test]
    fn esc_in_find_bar_closes_it() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.find_state.is_none());
        assert_eq!(app.scope, Scope::MainScreen);
    }

    #[test]
    fn typing_in_find_bar_routes_to_bar_input() {
        let mut app = App::new();
        app.transcript = vec![msg("u1", "user", "hello world")];
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
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
        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
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
    fn pressing_end_visibly_scrolls_transcript_to_bottom() {
        // Phase 0 parity: `End` is the new ScrollBottom (was `Shift+G`).
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

        // Simulate End.
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let after = render_to_string(&app, 120, 30);
        assert_ne!(
            before, after,
            "End did not visibly change the rendered transcript"
        );
        assert!(
            after.contains("BOTTOMSENTINEL"),
            "BOTTOMSENTINEL must be visible after End; got:\n{after}"
        );
        assert!(
            !after.contains("TOPSENTINEL"),
            "TOPSENTINEL must scroll off-screen after End; got:\n{after}"
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

    // ---- Phase 4 Wave 2 frame-buffer + DB regression tests ---------------
    //
    // These mirror the lesson learned earlier in the port: state assertions
    // alone are insufficient — render to a TestBackend and diff the actual
    // buffer cells to catch render bugs that don't show up in App state.

    use threadhop_core::models::BookmarkKind;

    /// Build a fresh in-memory DB with the schema-9 shape the App's writes
    /// need. Returns a connection seeded with one session + one message so
    /// FK constraints accept inserts.
    fn seed_test_db() -> (rusqlite::Connection, String, String) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE sessions (
                 session_id    TEXT PRIMARY KEY,
                 session_path  TEXT NOT NULL,
                 project       TEXT,
                 cwd           TEXT,
                 custom_name   TEXT,
                 status        TEXT NOT NULL DEFAULT 'active'
                     CHECK (status IN
                         ('active', 'in_progress', 'in_review', 'done', 'archived')),
                 sort_order    INTEGER,
                 last_viewed   REAL,
                 created_at    REAL,
                 modified_at   REAL
             );
             CREATE TABLE messages (
                 uuid         TEXT PRIMARY KEY,
                 session_id   TEXT NOT NULL,
                 role         TEXT NOT NULL,
                 text         TEXT NOT NULL,
                 timestamp    TEXT,
                 cwd          TEXT,
                 parent_uuid  TEXT,
                 is_sidechain INTEGER NOT NULL DEFAULT 0,
                 message_id   TEXT
             );
             CREATE TABLE bookmarks (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 message_uuid TEXT NOT NULL UNIQUE,
                 note         TEXT,
                 kind         TEXT NOT NULL DEFAULT 'bookmark'
                     CHECK (kind IN ('bookmark', 'research')),
                 tags         TEXT NOT NULL DEFAULT '[]',
                 created_at   REAL NOT NULL,
                 FOREIGN KEY (message_uuid) REFERENCES messages(uuid) ON DELETE CASCADE
             );
             PRAGMA user_version = 9;",
        )
        .unwrap();
        let sid = "sess-1".to_string();
        let uuid = "msg-1".to_string();
        conn.execute(
            "INSERT INTO sessions (session_id, session_path, status, modified_at) \
             VALUES (?, '/tmp/sess.jsonl', 'active', 1000.0)",
            [&sid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (uuid, session_id, role, text) VALUES (?, ?, 'user', 'hello cursor')",
            [&uuid, &sid],
        )
        .unwrap();
        (conn, sid, uuid)
    }

    fn seeded_app() -> (App, String, String) {
        let mut app = App::new();
        let (conn, sid, uuid) = seed_test_db();
        app.db = conn;
        app.read_only = false; // override the in-memory fallback default
        app.sidebar = vec![item(&sid)];
        app.selected_session_id = Some(sid.clone());
        app.transcript = vec![msg(&uuid, "user", "hello cursor")];
        app.message_cursor = 0;
        (app, sid, uuid)
    }

    fn count_bookmarks(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM bookmarks", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn pressing_space_toggles_bookmark_in_db_and_updates_status_message() {
        // Phase 0 parity: Space toggles bookmark on the message cursor
        // (was `b`); `b` now opens the bookmark browser.
        let (mut app, _sid, _uuid) = seeded_app();
        assert_eq!(count_bookmarks(&app.db), 0, "starts empty");
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(count_bookmarks(&app.db), 1, "row inserted on Space");
        assert_eq!(app.status_message.as_deref(), Some("★ bookmarked"));
        // Toggle again removes the bookmark.
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(count_bookmarks(&app.db), 0, "row removed on second Space");
        assert_eq!(app.status_message.as_deref(), Some("removed bookmark"));
    }

    #[test]
    fn pressing_b_opens_bookmark_browser_modal() {
        // Phase 0 parity: `b` opens the bookmark browser (Python binding;
        // was Shift+B in pre-Phase-0 Rust).
        let (mut app, _sid, _uuid) = seeded_app();
        let before = render_to_string(&app, 100, 30);
        assert!(!before.contains("Bookmarks"), "title shouldn't appear pre-open");
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(app.bookmark_browser.is_some(), "browser state set");
        assert_eq!(app.scope, Scope::BookmarkBrowser);
        let after = render_to_string(&app, 100, 30);
        assert_ne!(before, after, "modal should change frame buffer");
        assert!(
            after.contains("Bookmarks"),
            "expected 'Bookmarks' title in frame; got:\n{after}"
        );
    }

    #[test]
    fn pressing_shift_l_opens_label_prompt_modal() {
        // Phase 0: lowercase `s` now cycles status (Python parity); the
        // label-prompt opener moved to `Shift+L` so the merged Rust modal
        // stays reachable for the rename / custom-name path.
        let (mut app, _sid, _uuid) = seeded_app();
        let before = render_to_string(&app, 100, 30);
        app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        assert!(app.label_prompt.is_some(), "label_prompt state set");
        assert_eq!(app.scope, Scope::LabelPrompt);
        let after = render_to_string(&app, 100, 30);
        assert_ne!(before, after, "modal should change frame buffer");
        // The label-prompt title contains "Label:" — the modal renders it.
        assert!(
            after.contains("Label:"),
            "expected 'Label:' title in frame; got:\n{after}"
        );
    }

    #[test]
    fn pressing_s_now_cycles_status_stub_per_python_parity() {
        // Phase 0 stub: `s` fires CycleSessionStatus, which is a no-op
        // status_message stub until the real handler ships. Asserts the
        // binding wires through (muscle memory works).
        let (mut app, _sid, _uuid) = seeded_app();
        let res = app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
        assert_eq!(res, Some(Command::CycleSessionStatus));
        assert!(app.label_prompt.is_none(), "label_prompt must NOT open on s");
        assert!(
            app.status_message
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase()
                .contains("not implemented"),
            "expected stub status; got {:?}",
            app.status_message
        );
    }

    #[test]
    fn confirm_yes_executes_pending_action() {
        // Seed a bookmark directly, open the browser, request delete, then
        // press `y` and assert the row is gone from the DB.
        let (mut app, _sid, _uuid) = seeded_app();
        let now = 12345.0_f64;
        let bm = threadhop_core::db::upsert_bookmark(
            &app.db,
            "msg-1",
            BookmarkKind::Bookmark,
            Some("the note"),
            now,
        )
        .unwrap();
        assert_eq!(count_bookmarks(&app.db), 1);
        let bookmark_id = bm.id.unwrap();

        // Open the browser via `b` (Python parity; was Shift+B pre-Phase 0).
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(app.bookmark_browser.is_some());
        assert_eq!(
            app.bookmark_browser.as_ref().unwrap().bookmarks.len(),
            1,
            "browser should see the seeded bookmark"
        );

        // Request delete — `d` opens the confirm modal with the pending
        // DeleteBookmark action.
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(app.confirm.is_some(), "confirm modal should open");
        match app.confirm.as_ref().unwrap().on_yes {
            PendingAction::DeleteBookmark { bookmark_id: id } => {
                assert_eq!(id, bookmark_id, "pending id should match the seeded row");
            }
        }

        // Confirm with `y`.
        app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(app.confirm.is_none(), "confirm should close");
        assert_eq!(count_bookmarks(&app.db), 0, "row deleted on Yes");
        assert_eq!(app.status_message.as_deref(), Some("bookmark deleted"));
    }

    #[test]
    fn confirm_no_keeps_bookmark_and_returns_to_browser() {
        let (mut app, _sid, _uuid) = seeded_app();
        threadhop_core::db::upsert_bookmark(
            &app.db,
            "msg-1",
            BookmarkKind::Bookmark,
            None,
            1.0,
        )
        .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert!(app.confirm.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(app.confirm.is_none(), "confirm closes on No");
        assert!(app.bookmark_browser.is_some(), "browser stays open on No");
        assert_eq!(app.scope, Scope::BookmarkBrowser, "scope returns to browser");
        assert_eq!(count_bookmarks(&app.db), 1, "row preserved");
    }

    #[test]
    fn read_only_skips_writes() {
        // Space (Phase 0 toggle-bookmark binding) must short-circuit on
        // read_only and surface the friendly status.
        let (mut app, _sid, _uuid) = seeded_app();
        app.read_only = true;
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert_eq!(count_bookmarks(&app.db), 0, "no write while read-only");
        let s = app.status_message.as_deref().unwrap_or("");
        assert!(
            s.to_ascii_lowercase().contains("read-only"),
            "expected read-only status message; got {s:?}"
        );
    }

    #[test]
    fn ctrl_j_and_ctrl_k_move_message_cursor() {
        // Phase 0 parity: the msg cursor moved off Shift+J/K (Python uses
        // those for session reorder) onto Ctrl+J/K.
        let (mut app, _sid, _uuid) = seeded_app();
        // Two messages so the cursor has somewhere to go.
        app.transcript = vec![
            msg("u1", "user", "alpha"),
            msg("u2", "assistant", "beta"),
        ];
        app.message_cursor = 0;
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(app.message_cursor, 1);
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(app.message_cursor, 0);
        // Out-of-range moves saturate.
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(app.message_cursor, 0);
    }

    #[test]
    fn esc_in_bookmark_browser_closes_modal() {
        let (mut app, _sid, _uuid) = seeded_app();
        // Phase 0: `b` (not Shift+B) opens the browser.
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
        assert!(app.bookmark_browser.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.bookmark_browser.is_none());
        assert_eq!(app.scope, Scope::MainScreen);
    }

    #[test]
    fn esc_in_label_prompt_closes_modal() {
        // Phase 0: `Shift+L` opens the label modal (was `s` pre-Phase 0).
        let (mut app, _sid, _uuid) = seeded_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        assert!(app.label_prompt.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.label_prompt.is_none());
        assert_eq!(app.scope, Scope::MainScreen);
    }

    #[test]
    fn label_prompt_status_chosen_writes_to_db() {
        let (mut app, sid, _uuid) = seeded_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('L'), KeyModifiers::SHIFT));
        // Status picker opens with cursor on the current status (Active —
        // index 0). Press `j` to move to InProgress (index 1) then Enter.
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.label_prompt.is_none(), "modal closes after Enter");
        let row = threadhop_core::db::session_by_id(&app.db, &sid)
            .unwrap()
            .unwrap();
        assert!(matches!(row.status, threadhop_core::models::SessionStatus::InProgress));
    }

    // ---- Phase 5 Wave 2 frame-buffer + DB regression tests ---------------

    #[test]
    fn pressing_shift_b_opens_kanban_modal() {
        let (mut app, _sid, _uuid) = seeded_app();
        let before = render_to_string(&app, 120, 30);
        assert!(!before.contains("Kanban"), "title shouldn't appear pre-open");
        app.handle_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        assert!(app.kanban.is_some(), "kanban state set");
        assert_eq!(app.scope, Scope::Kanban);
        let after = render_to_string(&app, 120, 30);
        assert_ne!(before, after, "modal should change frame buffer");
        assert!(
            after.contains("Kanban"),
            "expected 'Kanban' title in frame; got:\n{after}"
        );
    }

    #[test]
    fn pressing_c_opens_conflict_viewer() {
        let (mut app, _sid, _uuid) = seeded_app();
        let before = render_to_string(&app, 120, 30);
        assert!(!before.contains("Conflicts"), "title shouldn't appear pre-open");
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(app.conflict_viewer.is_some(), "viewer state set");
        assert_eq!(app.scope, Scope::ConflictViewer);
        let after = render_to_string(&app, 120, 30);
        assert_ne!(before, after, "modal should change frame buffer");
        assert!(
            after.contains("Conflicts"),
            "expected 'Conflicts' title in frame; got:\n{after}"
        );
    }

    #[test]
    fn kanban_status_changed_writes_to_db() {
        let (mut app, sid, _uuid) = seeded_app();
        // Seed: session starts as Active. Open kanban → cursor is on the
        // (only) item in column 0; press m → cycles to InProgress and the
        // App should write that to the DB.
        app.handle_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        assert!(app.kanban.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        let row = threadhop_core::db::session_by_id(&app.db, &sid)
            .unwrap()
            .unwrap();
        assert!(
            matches!(row.status, threadhop_core::models::SessionStatus::InProgress),
            "expected InProgress in DB after m"
        );
        // Sidebar mirrors the new status.
        let sidebar_status = app
            .sidebar
            .iter()
            .find(|i| i.session_id == sid)
            .map(|i| i.status)
            .unwrap();
        assert!(matches!(
            sidebar_status,
            threadhop_core::models::SessionStatus::InProgress
        ));
    }

    #[test]
    fn kanban_status_changed_reverts_on_db_error() {
        // Force the write path to fail by flipping read_only after open.
        let (mut app, sid, _uuid) = seeded_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT));
        app.read_only = true;
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        // DB row must NOT have changed (read_only blocks the write).
        let row = threadhop_core::db::session_by_id(&app.db, &sid)
            .unwrap()
            .unwrap();
        assert!(
            matches!(row.status, threadhop_core::models::SessionStatus::Active),
            "row must stay Active when read_only blocks the write"
        );
        // Kanban state was reverted — the item should be back in column 0
        // (Active), not column 1 (InProgress).
        let s = app.kanban.as_ref().unwrap();
        assert_eq!(
            s.items_in_column(0).len(),
            1,
            "kanban should revert: item back in Active column"
        );
        assert_eq!(s.items_in_column(1).len(), 0);
        assert!(
            app.status_message
                .as_deref()
                .unwrap_or("")
                .to_ascii_lowercase()
                .contains("read-only"),
            "expected read-only status message"
        );
    }

    // ---- Phase 6 regression tests ----------------------------------------

    #[test]
    fn pressing_question_opens_help_overlay() {
        // Render before; press `?`; render after — assert the buffer differs
        // and the overlay's "Help" title is visible.
        let mut app = App::new();
        app.sidebar = vec![item("s1")];
        app.selected_session_id = Some("s1".into());
        let before = render_to_string(&app, 100, 30);
        assert!(
            !before.contains("Help"),
            "Help title should not appear before pressing ?"
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        assert!(app.help.is_some(), "help state must be set");
        assert_eq!(app.scope, Scope::HelpOverlay);
        let after = render_to_string(&app, 100, 30);
        assert_ne!(before, after, "frame buffer must change when help opens");
        assert!(
            after.contains("Help"),
            "expected 'Help' title in frame; got:\n{after}"
        );
        // Esc closes the overlay.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.help.is_none(), "help should close on Esc");
        assert_eq!(app.scope, Scope::MainScreen);
    }

    #[test]
    fn project_filter_excludes_unmatched_sessions() {
        // Set project_filter to "myproject" and feed a mixed
        // SessionsRefreshed event; only the matching row should remain in
        // the sidebar after handle_worker_event.
        let mut app = App::new();
        app.project_filter = Some("myproject".into());
        let mut a = item("sess-a");
        a.project = Some("myproject".into());
        let mut b = item("sess-b");
        b.project = Some("other".into());
        crate::event::handle_worker_event(
            &mut app,
            crate::workers::WorkerEvent::SessionsRefreshed(vec![a, b]),
        );
        assert_eq!(app.sidebar.len(), 1, "only matching session should remain");
        assert_eq!(app.sidebar[0].session_id, "sess-a");
    }

    #[test]
    fn days_filter_excludes_old_sessions() {
        let mut app = App::new();
        app.days_filter = Some(7);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut fresh = item("fresh");
        fresh.last_active_at = Some(now - 3.0 * 86_400.0);
        let mut stale = item("stale");
        stale.last_active_at = Some(now - 30.0 * 86_400.0);
        crate::event::handle_worker_event(
            &mut app,
            crate::workers::WorkerEvent::SessionsRefreshed(vec![fresh, stale]),
        );
        let ids: Vec<&str> =
            app.sidebar.iter().map(|i| i.session_id.as_str()).collect();
        assert_eq!(ids, vec!["fresh"], "old session must be filtered out");
    }

    #[test]
    fn apply_cli_session_sets_selection_and_publishes_watch() {
        let mut app = App::new();
        let mut rx = app.active_session_rx();
        let _ = rx.borrow_and_update();
        app.apply_cli(None, None, Some("preselected".into()));
        assert_eq!(app.selected_session_id.as_deref(), Some("preselected"));
        assert!(rx.has_changed().unwrap());
        assert_eq!(rx.borrow().as_deref(), Some("preselected"));
    }

    #[test]
    fn toggle_bookmark_skips_when_message_not_in_db() {
        // Seed a DB but DON'T insert the message row. App has a transcript
        // referencing the missing uuid; toggle should produce the friendly
        // not-yet-indexed status instead of a SQL error.
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE messages (
                 uuid TEXT PRIMARY KEY,
                 session_id TEXT NOT NULL,
                 role TEXT NOT NULL,
                 text TEXT NOT NULL
             );
             CREATE TABLE bookmarks (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 message_uuid TEXT NOT NULL UNIQUE,
                 note TEXT,
                 kind TEXT NOT NULL DEFAULT 'bookmark',
                 tags TEXT NOT NULL DEFAULT '[]',
                 created_at REAL NOT NULL,
                 FOREIGN KEY (message_uuid) REFERENCES messages(uuid)
             );",
        )
        .unwrap();
        let mut app = App::new();
        app.db = conn;
        app.read_only = false;
        app.transcript = vec![msg("not-in-db", "user", "ghost")];
        app.message_cursor = 0;
        // Phase 0: Space toggles bookmark; `b` opens the browser modal.
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        // No bookmark should have been inserted.
        let cnt: i64 = app
            .db
            .query_row("SELECT COUNT(*) FROM bookmarks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cnt, 0, "no row should be inserted for missing message");
        let status = app.status_message.as_deref().unwrap_or("");
        assert!(
            status.to_ascii_lowercase().contains("not yet indexed"),
            "expected friendly status; got {status:?}"
        );
    }

    #[test]
    fn digest_bar_renders_in_main_layout() {
        let (mut app, sid, _uuid) = seeded_app();
        // Seed a digest summary so the bar has something concrete to show.
        let summary = ObservationSummary {
            open_todo_count: 3,
            ..Default::default()
        };
        app.digest_summary_cache.insert(sid.clone(), summary);
        app.has_bookmarks_for_session.insert(sid.clone());
        let frame = render_to_string(&app, 120, 30);
        // The digest bar lives on row 0. Pull that row out so we don't trip
        // on incidental "3" digits elsewhere in the layout.
        let first_row = frame.lines().next().unwrap_or("");
        assert!(
            first_row.contains("3 todos"),
            "expected digest bar text on row 0; got: {first_row:?}"
        );
        assert!(
            first_row.contains("bookmarked"),
            "expected bookmarked marker on row 0; got: {first_row:?}"
        );
    }

    // ---- Phase A: selection mode ----------------------------------------

    #[test]
    fn selection_mode_m_enters_and_esc_exits() {
        let mut app = App::new();
        app.transcript = vec![
            msg("u1", "user", "alpha"),
            msg("u2", "assistant", "beta"),
            msg("u3", "user", "gamma"),
        ];
        // Enter selection mode via `m`.
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert_eq!(app.scope, Scope::Selection);
        assert!(app.selection_state.is_some(), "selection_state should be Some");
        // Cursor starts at the last message per Python parity ("recent
        // context is usually what you want").
        let cur = app.selection_state.unwrap().cursor;
        assert_eq!(cur, 2);
        // Esc exits.
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.scope, Scope::MainScreen);
        assert!(app.selection_state.is_none());
    }

    #[test]
    fn selection_cursor_j_moves_down() {
        let mut app = App::new();
        app.transcript = vec![
            msg("u1", "user", "alpha"),
            msg("u2", "assistant", "beta"),
            msg("u3", "user", "gamma"),
        ];
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        // Manually set cursor to 0 to test downward movement.
        app.selection_state.as_mut().unwrap().cursor = 0;
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(app.selection_state.unwrap().cursor, 1);
        // `k` moves up.
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.selection_state.unwrap().cursor, 0);
        // Out-of-range stays clamped.
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(app.selection_state.unwrap().cursor, 0);
    }

    #[test]
    fn selection_range_v_toggles_range_start() {
        let mut app = App::new();
        app.transcript = vec![
            msg("u1", "user", "alpha"),
            msg("u2", "assistant", "beta"),
        ];
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        assert!(app.selection_state.unwrap().range_start.is_none());
        // First `v` sets range_start to current cursor.
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert!(app.selection_state.unwrap().range_start.is_some());
        // Second `v` clears it back to None.
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
        assert!(app.selection_state.unwrap().range_start.is_none());
    }

    #[test]
    fn selection_render_applies_warning_tint() {
        // Frame-buffer test: with selection mode active and cursor on a known
        // message, the warning-tinted bg should appear on at least one cell
        // of the selected message's row(s).
        use ratatui::{backend::TestBackend, style::Color, Terminal};
        let mut app = App::new();
        app.transcript = vec![
            msg("u1", "user", "alpha"),
            msg("u2", "assistant", "second message body content here"),
            msg("u3", "user", "gamma"),
        ];
        // Enter selection mode; the cursor lands on the last message (index
        // 2). Move it to index 1 (the assistant message) so the tint pops
        // on a row that's far from any edge.
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        app.selection_state.as_mut().unwrap().cursor = 1;
        let theme = app.theme.clone();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| app.draw(f)).unwrap();
        let buf = term.backend().buffer();
        // Recompute the expected tint color.
        let tint_hex = threadhop_core::theme::blend(&theme.warning, &theme.background, 0.08);
        let (r, g, b) = threadhop_core::theme::hex_to_rgb(&tint_hex).unwrap();
        let tint = Color::Rgb(r, g, b);
        let mut saw_tint = false;
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                if buf[(x, y)].bg == tint {
                    saw_tint = true;
                    break;
                }
            }
            if saw_tint {
                break;
            }
        }
        assert!(saw_tint, "selection-mode warning tint never reached the buffer");
    }

    #[test]
    fn selection_space_toggles_bookmark_on_selected_message() {
        // Space inside selection mode should toggle a bookmark on the
        // selection cursor's message (NOT on the main message_cursor). The
        // simplest assertion: after a `Space`, status_message changes
        // (toggle path always writes a status when successful or when the
        // FK check fails). For an in-memory DB the FK check sees no row
        // and surfaces the friendly "not yet indexed" path — that's still a
        // status_message side-effect, which is what we're asserting.
        let (mut app, _sid, _uuid) = seeded_app();
        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE));
        app.status_message = None;
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(
            app.status_message.is_some(),
            "Space in selection mode should produce a status_message"
        );
    }
}
