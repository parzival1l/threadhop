//! FTS search modal — opens on `s` from the main screen, queries against the
//! existing FTS5 index, displays hits with snippets, and posts back a
//! [`SearchResult::JumpToMessage`] when the user presses Enter on a hit.
//!
//! Phase 3 tasks 3.1 (scaffold), 3.2 (debounce), 3.6 (filter pills). Wave 2
//! wires the modal into [`crate::app::App`] (tasks 3.3 jump-to-message and 3.5
//! `recent_searches` persistence) — this file is App-agnostic.
//!
//! ## Phase 7 seam
//!
//! The modal queries through [`threadhop_core::fts::search`] and renders
//! [`threadhop_core::fts::Hit`] values. Phase 7's planned `search_semantic` /
//! `search_hybrid` composers return the same `Hit` shape, so they can be
//! swapped in without touching this file.
//!
//! ## Modal-result channel pattern (for Wave 2)
//!
//! [`handle_key`] returns `Option<SearchResult>` — `None` means the modal
//! stays open, `Some(_)` means it closes. The App holds the modal in an
//! `Option<SearchState>`, drains the result on each key press, and reacts:
//! `Cancelled` clears the option; `JumpToMessage` clears the option then
//! pushes the jump into the transcript state. The modal never owns a channel
//! or DB connection — the App passes a `&Connection` to [`execute_query`]
//! when [`should_execute`] returns true during a render tick.

#![allow(dead_code)] // Wave 2 wires the App integration; until then the
                     // binary doesn't reference these helpers.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, StatefulWidget, Widget},
};
use threadhop_core::{
    fts::{self, Filters, Hit},
    models::MessageRole,
    theme::{hex_to_rgb, Theme},
};

/// Debounce window — keystrokes within this duration coalesce into a single
/// FTS query. 150ms is comfortable on a fast machine and still feels live.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

// ---- state ------------------------------------------------------------------

/// Modal state — what the modal owns, distinct from App state.
///
/// `query_input` is what the user is typing; `committed_query` is the last
/// query string [`execute_query`] actually ran. The two diverge while the
/// debounce window is open and converge as soon as the next [`execute_query`]
/// fires.
#[derive(Debug, Clone)]
pub struct SearchState {
    /// Raw text in the input box, including any filter modifiers the user
    /// has typed but not yet "lifted" to the pill row.
    pub query_input: String,

    /// Last query string actually executed against the DB. Used by
    /// [`should_execute`] to detect that the user has typed since the last
    /// run.
    pub committed_query: String,

    /// Structured filters parsed out of the latest `query_input` via
    /// [`fts::parse_query`]. Re-derived on every keystroke so the pill row
    /// stays in lockstep with the input.
    pub filters: Filters,

    /// Most recent FTS result set. Empty until [`execute_query`] runs.
    pub hits: Vec<Hit>,

    /// Highlighted hit. Always points into `hits` (clamped on every update).
    pub selected_index: usize,

    /// Surfaces an [`fts::FtsError`] to the modal banner. Cleared on the
    /// next successful query.
    pub error: Option<String>,

    /// Set on every keystroke. [`should_execute`] uses this plus
    /// [`DEBOUNCE`] to gate the next FTS run.
    pub last_keystroke_at: Instant,

    /// MRU recent-search list, populated by the App from
    /// [`threadhop_core::recent_searches::get_recent_searches`] when the modal
    /// opens. Surfaced in [`draw`] when the input is empty so the user has a
    /// fallback list to choose from before typing anything.
    pub recents: Vec<String>,
}

impl SearchState {
    /// Construct an empty modal — typically called when the App first opens
    /// the search screen.
    pub fn new() -> Self {
        Self {
            query_input: String::new(),
            committed_query: String::new(),
            filters: Filters::default(),
            hits: Vec::new(),
            selected_index: 0,
            error: None,
            last_keystroke_at: Instant::now(),
            recents: Vec::new(),
        }
    }

    /// Re-derive [`Self::filters`] from the current `query_input`. Pure —
    /// runs after every keystroke so the pill row stays current.
    fn reparse_filters(&mut self) {
        let (filters, _rest) = fts::parse_query(&self.query_input);
        self.filters = filters;
    }

    /// Clamp `selected_index` into `hits`. Called after every result update
    /// so an old selection past the end of a shorter result list snaps back.
    fn clamp_selection(&mut self) {
        if self.hits.is_empty() {
            self.selected_index = 0;
        } else if self.selected_index >= self.hits.len() {
            self.selected_index = self.hits.len() - 1;
        }
    }

    /// Remove the trailing modifier token from `query_input`. Returns true
    /// if anything was stripped. Used by Backspace-on-empty-tail to "delete
    /// the last pill" — equivalent to deleting the trailing
    /// `project:foo` / `user:` / `assistant:` token.
    fn pop_last_modifier(&mut self) -> bool {
        let trimmed_end = self.query_input.trim_end();
        if trimmed_end.is_empty() {
            return false;
        }
        // Find the start of the last whitespace-separated token.
        let last_space = trimmed_end.rfind(char::is_whitespace);
        let tok_start = last_space.map(|i| i + 1).unwrap_or(0);
        let last_tok = &trimmed_end[tok_start..];
        if last_tok.starts_with("project:")
            || last_tok == "user:"
            || last_tok == "assistant:"
        {
            self.query_input.truncate(tok_start);
            // Tidy: strip trailing whitespace that was between the pill and
            // (now removed) tail.
            let new_len = self.query_input.trim_end().len();
            self.query_input.truncate(new_len);
            self.reparse_filters();
            true
        } else {
            false
        }
    }
}

impl Default for SearchState {
    fn default() -> Self {
        Self::new()
    }
}

// ---- result -----------------------------------------------------------------

/// Result the modal posts back to the App when it closes.
///
/// Wave 2's App loop calls [`handle_key`] for every keystroke while the modal
/// is open and matches on the returned `Option<SearchResult>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchResult {
    /// User hit Escape (or Ctrl-C) — discard the modal, don't change
    /// transcript selection.
    Cancelled,
    /// User hit Enter on a highlighted hit — App should select the session
    /// and scroll to the message anchor.
    JumpToMessage {
        session_id: String,
        message_uuid: String,
    },
}

// ---- debounce ---------------------------------------------------------------

/// True when the modal's `query_input` differs from `committed_query` *and*
/// at least [`DEBOUNCE`] has elapsed since the last keystroke.
///
/// The App's render tick is the natural debouncer — it calls this on each
/// tick and, if true, runs [`execute_query`]. Keystrokes refresh
/// `last_keystroke_at` to `Instant::now()`, restarting the window.
pub fn should_execute(state: &SearchState) -> bool {
    if state.query_input == state.committed_query {
        return false;
    }
    state.last_keystroke_at.elapsed() >= DEBOUNCE
}

// ---- query execution --------------------------------------------------------

/// Run the FTS query and update `state.hits` / `state.error` /
/// `state.committed_query`. The caller (App) owns the SQLite connection so
/// the modal stays decoupled from DB lifetime.
///
/// Empty queries (no remainder after stripping modifiers) clear `hits`
/// without touching the DB — matches [`fts::search`]'s short-circuit.
pub fn execute_query(state: &mut SearchState, conn: &rusqlite::Connection) {
    let query = state.query_input.clone();
    match fts::search(conn, &query) {
        Ok(hits) => {
            state.hits = hits;
            state.error = None;
        }
        Err(e) => {
            state.hits.clear();
            state.error = Some(e.to_string());
        }
    }
    state.committed_query = query;
    state.clamp_selection();
}

// ---- key handling -----------------------------------------------------------

/// Handle one key event. Returns `Some(SearchResult)` if the modal should
/// close, `None` if it stays open.
///
/// Recognised keys:
/// * Esc / Ctrl-C → [`SearchResult::Cancelled`]
/// * Enter → [`SearchResult::JumpToMessage`] (if a hit is selected)
/// * Up / Down (Ctrl-P / Ctrl-N) → move selection
/// * Backspace → delete last char; if input is empty, pop the last pill
/// * Any other printable char → append to `query_input`
///
/// Every text-mutating branch updates `last_keystroke_at` and reparses
/// filters so the pill row stays in lockstep.
pub fn handle_key(state: &mut SearchState, key: KeyEvent) -> Option<SearchResult> {
    // Accept Press and Repeat — some terminals only send Repeat for held keys,
    // and unit tests synthesize Press. Skip Release so we don't double-fire
    // on up-edges. Matches the find_bar widget's permissive handling.
    if matches!(
        key.kind,
        crossterm::event::KeyEventKind::Release
    ) {
        return None;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => Some(SearchResult::Cancelled),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(SearchResult::Cancelled),

        (KeyCode::Enter, _) => state.hits.get(state.selected_index).map(|h| {
            SearchResult::JumpToMessage {
                session_id: h.session_id.clone(),
                message_uuid: h.message_uuid.clone(),
            }
        }),

        (KeyCode::Down, _) | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
            if !state.hits.is_empty() && state.selected_index + 1 < state.hits.len() {
                state.selected_index += 1;
            }
            None
        }
        (KeyCode::Up, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
            if state.selected_index > 0 {
                state.selected_index -= 1;
            }
            None
        }

        (KeyCode::Backspace, _) => {
            // If the input ends in a modifier token, treat Backspace as
            // "delete the last pill" (matches the Phase 3.6 spec). Otherwise
            // delete one char from the input tail.
            if !state.pop_last_modifier() {
                state.query_input.pop();
                state.reparse_filters();
            }
            state.last_keystroke_at = Instant::now();
            None
        }

        (KeyCode::Char(c), mods) => {
            // Reject control-modified chars we don't recognise — they'd
            // otherwise pollute the input with stray characters.
            if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) {
                return None;
            }
            state.query_input.push(c);
            state.reparse_filters();
            state.last_keystroke_at = Instant::now();
            None
        }

        _ => None,
    }
}

// ---- rendering --------------------------------------------------------------

/// Render the search modal at the given area. The caller is expected to
/// have computed a centered popup rect (see [`centered_rect`]) and to call
/// `Clear` on it beforehand — but this function calls `Clear` itself for
/// safety so it always renders an opaque popup.
///
/// Layout (top-to-bottom):
///  1. Pill row (height 1, only when filters are set)
///  2. Input row (height 1) — `> query`
///  3. Hit list (fills)
///  4. Status row (height 1) — `<n> hits | <filter summary>`
pub fn draw(state: &SearchState, theme: &Theme, area: Rect, buf: &mut Buffer) {
    // Opaque background — without Clear, the underlying main-screen content
    // would bleed through any cells we don't explicitly write to.
    Clear.render(area, buf);

    // Outer block with title — title carries the live query so the modal
    // stays informative even when the input row is offscreen.
    let title_text = if state.query_input.is_empty() {
        " Search ".to_string()
    } else {
        format!(" Search: {} ", state.query_input)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(theme))
        .title(Span::styled(title_text, title_style(theme)));
    let inner = block.inner(area);
    block.render(area, buf);

    // Layout: pills (1 if any) + input (1) + body (fill) + status (1).
    let has_pills = state.filters.project.is_some() || state.filters.role.is_some();
    let pill_h: u16 = if has_pills { 1 } else { 0 };

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(pill_h),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    if has_pills {
        render_pills(&state.filters, theme, v[0], buf);
    }
    render_input(&state.query_input, theme, v[1], buf);
    render_hits(state, theme, v[2], buf);
    render_status(state, theme, v[3], buf);
}

/// Pill row — renders each active filter as `[label]` with a themed
/// background. Chose the bracketed-label style over rounded borders because
/// pills sit on a single text row (a 3-line rounded box would dominate the
/// modal); the bracketed form is the conventional CLI cue and matches what
/// the Python TUI does in `find_bar.py`.
fn render_pills(filters: &Filters, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let pill_style = Style::default()
        .fg(theme_color(&theme.background, Color::Black))
        .bg(theme_color(&theme.accent, Color::Magenta))
        .add_modifier(Modifier::BOLD);

    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw(" "));
    if let Some(p) = &filters.project {
        spans.push(Span::styled(format!(" project:{} ", p), pill_style));
        spans.push(Span::raw(" "));
    }
    if let Some(r) = filters.role {
        let label = match r {
            MessageRole::User => " role:user ",
            MessageRole::Assistant => " role:assistant ",
        };
        spans.push(Span::styled(label.to_string(), pill_style));
        spans.push(Span::raw(" "));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

/// Input row — `> <query>` with a block caret at the end. We don't use
/// ratatui's cursor positioning because the modal may not have OS focus
/// during tests; rendering an explicit caret span keeps snapshot tests
/// deterministic.
fn render_input(query: &str, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let prompt_style = Style::default()
        .fg(theme_color(&theme.primary, Color::Yellow))
        .add_modifier(Modifier::BOLD);
    let caret_style = Style::default()
        .bg(theme_color(&theme.foreground, Color::White))
        .fg(theme_color(&theme.background, Color::Black));

    let line = Line::from(vec![
        Span::styled("> ", prompt_style),
        Span::raw(query.to_string()),
        Span::styled(" ", caret_style),
    ]);
    Paragraph::new(line).render(area, buf);
}

/// Hit list. Each row: `<session-id-short> │ <snippet>`.
///
/// `snippet` is already wrapped in `[...]` brackets by FTS5 (the `snippet()`
/// call in `fts::prefix_search`); we render those characters as bold so the
/// match stands out. We don't parse them out — the brackets are visual, and
/// any escaping would be lossy.
fn render_hits(state: &SearchState, theme: &Theme, area: Rect, buf: &mut Buffer) {
    // Empty input + no committed query → surface the MRU recents list as a
    // pre-search fallback. Phase 3 task 3.5: the only way to *use* a recent
    // entry is to read it and re-type it; we don't bind Enter to "load a
    // recent" today because the modal doesn't track a separate "in recents
    // vs in hits" selection. Future enhancement.
    if state.hits.is_empty() && state.query_input.is_empty() && !state.recents.is_empty() {
        let header_style = Style::default()
            .fg(theme_color(&theme.text_muted, Color::DarkGray))
            .add_modifier(Modifier::DIM);
        let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
        let mut items: Vec<ListItem> = Vec::with_capacity(state.recents.len() + 1);
        items.push(ListItem::new(Line::from(Span::styled(
            " recent searches ".to_string(),
            header_style,
        ))));
        for r in &state.recents {
            items.push(ListItem::new(Line::from(vec![
                Span::raw("  "),
                Span::styled(r.clone(), muted),
            ])));
        }
        Widget::render(List::new(items), area, buf);
        return;
    }

    if state.hits.is_empty() {
        let msg = if state.committed_query.is_empty() {
            "Type to search..."
        } else if state.error.is_some() {
            "" // error row is shown in the status bar
        } else {
            "No results."
        };
        let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
        let centered_y = area.y + area.height / 2;
        let centered_x = area.x
            + area
                .width
                .saturating_sub(msg.chars().count() as u16)
                / 2;
        let target = Rect {
            x: centered_x,
            y: centered_y,
            width: area.width.saturating_sub(centered_x - area.x),
            height: 1,
        };
        Paragraph::new(Span::styled(msg.to_string(), muted)).render(target, buf);
        return;
    }

    let items: Vec<ListItem> = state
        .hits
        .iter()
        .map(|h| {
            let sid = short_session(&h.session_id);
            let snippet_spans = snippet_to_spans(&h.snippet, theme);
            let mut spans: Vec<Span<'static>> = vec![
                Span::styled(
                    format!("{:<10}", sid),
                    Style::default().fg(theme_color(&theme.secondary, Color::Cyan)),
                ),
                Span::raw(" │ "),
            ];
            spans.extend(snippet_spans);
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_index));

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(theme_color(&theme.background_element, Color::DarkGray))
            .add_modifier(Modifier::BOLD),
    );
    StatefulWidget::render(list, area, buf, &mut list_state);
}

/// Status row — hit count on the left, error (if any) on the right.
fn render_status(state: &SearchState, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    let err_style = Style::default().fg(theme_color(&theme.error, Color::Red));

    let count = match state.hits.len() {
        0 => " 0 hits ".to_string(),
        1 => " 1 hit ".to_string(),
        n => format!(" {} hits ", n),
    };

    let mut spans: Vec<Span<'static>> = vec![Span::styled(count, muted)];
    if let Some(err) = &state.error {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(format!("err: {}", err), err_style));
    }
    Paragraph::new(Line::from(spans)).render(area, buf);
}

// ---- helpers ----------------------------------------------------------------

/// Snip out the first 8 chars of a session id for the hit-list left column.
/// Mirrors what the Python TUI does when listing sessions in tight columns.
fn short_session(sid: &str) -> String {
    sid.chars().take(8).collect()
}

/// Convert a snippet like `the quick [fox] jumps` into ratatui `Span`s where
/// the `[...]` segments render bold (the match), and the surrounding text
/// renders normal. Brackets themselves are stripped — the styling carries
/// the same information.
fn snippet_to_spans(snippet: &str, theme: &Theme) -> Vec<Span<'static>> {
    let bold = Style::default()
        .fg(theme_color(&theme.primary, Color::Yellow))
        .add_modifier(Modifier::BOLD);

    let mut out: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut in_match = false;

    for ch in snippet.chars() {
        match ch {
            '[' if !in_match => {
                if !buf.is_empty() {
                    out.push(Span::raw(std::mem::take(&mut buf)));
                }
                in_match = true;
            }
            ']' if in_match => {
                if !buf.is_empty() {
                    out.push(Span::styled(std::mem::take(&mut buf), bold));
                }
                in_match = false;
            }
            c => buf.push(c),
        }
    }
    if !buf.is_empty() {
        let style = if in_match { bold } else { Style::default() };
        out.push(Span::styled(buf, style));
    }
    out
}

fn theme_color(hex: &str, fallback: Color) -> Color {
    match hex_to_rgb(hex) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => fallback,
    }
}

fn border_style(theme: &Theme) -> Style {
    Style::default().fg(theme_color(&theme.border_active, Color::White))
}

fn title_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme_color(&theme.primary, Color::Yellow))
        .add_modifier(Modifier::BOLD)
}

/// Centered popup rect — exposed so the App can place the modal over the
/// main screen consistently. `pct_w` and `pct_h` are 1..=100.
pub fn centered_rect(pct_w: u16, pct_h: u16, parent: Rect) -> Rect {
    let h = parent.height.saturating_mul(pct_h) / 100;
    let w = parent.width.saturating_mul(pct_w) / 100;
    let x = parent.x + (parent.width.saturating_sub(w)) / 2;
    let y = parent.y + (parent.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

// ---- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use rusqlite::Connection;

    // ---- helpers -----------------------------------------------------

    /// Mirror of the in-memory FTS schema in `threadhop_core::fts::tests`.
    fn open_in_memory_with_fts() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"
            CREATE TABLE sessions (
                session_id   TEXT PRIMARY KEY,
                session_path TEXT,
                project      TEXT,
                cwd          TEXT,
                created_at   REAL,
                modified_at  REAL
            );
            CREATE TABLE messages (
                uuid         TEXT PRIMARY KEY,
                session_id   TEXT NOT NULL,
                role         TEXT NOT NULL,
                text         TEXT NOT NULL,
                timestamp    TEXT,
                cwd          TEXT,
                parent_uuid  TEXT,
                is_sidechain INTEGER NOT NULL DEFAULT 0
            );
            CREATE VIRTUAL TABLE messages_fts USING fts5(
                text,
                content='messages',
                content_rowid='rowid',
                tokenize='porter unicode61'
            );
            CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text);
            END;
            "#,
        )
        .unwrap();
        c
    }

    fn seed(conn: &Connection, sid: &str, project: Option<&str>, uuid: &str, role: &str, text: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO sessions (session_id, project) VALUES (?, ?)",
            rusqlite::params![sid, project],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages (uuid, session_id, role, text, timestamp) VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![uuid, sid, role, text, "2026-05-20T10:00:00Z"],
        )
        .unwrap();
    }

    fn press(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn press_char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    // ---- should_execute ---------------------------------------------

    #[test]
    fn should_execute_false_when_input_matches_committed() {
        let mut s = SearchState::new();
        s.query_input = "abc".into();
        s.committed_query = "abc".into();
        // Even past the debounce window, no-change -> no run.
        s.last_keystroke_at = Instant::now() - Duration::from_secs(1);
        assert!(!should_execute(&s));
    }

    #[test]
    fn should_execute_false_within_debounce_window() {
        let mut s = SearchState::new();
        s.query_input = "abc".into();
        // committed_query stays empty -> input != committed.
        s.last_keystroke_at = Instant::now(); // 0ms ago, well inside DEBOUNCE
        assert!(!should_execute(&s));
    }

    #[test]
    fn should_execute_true_after_debounce_when_input_diverged() {
        let mut s = SearchState::new();
        s.query_input = "abc".into();
        s.last_keystroke_at = Instant::now() - DEBOUNCE - Duration::from_millis(10);
        assert!(should_execute(&s));
    }

    // ---- handle_key -------------------------------------------------

    #[test]
    fn handle_key_typing_appends_to_input() {
        let mut s = SearchState::new();
        assert!(handle_key(&mut s, press_char('h')).is_none());
        assert!(handle_key(&mut s, press_char('i')).is_none());
        assert_eq!(s.query_input, "hi");
    }

    #[test]
    fn handle_key_backspace_deletes_one_char() {
        let mut s = SearchState::new();
        s.query_input = "hello".into();
        handle_key(&mut s, press(KeyCode::Backspace));
        assert_eq!(s.query_input, "hell");
    }

    #[test]
    fn handle_key_esc_returns_cancelled() {
        let mut s = SearchState::new();
        assert_eq!(handle_key(&mut s, press(KeyCode::Esc)), Some(SearchResult::Cancelled));
    }

    #[test]
    fn handle_key_ctrl_c_returns_cancelled() {
        let mut s = SearchState::new();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut s, ev), Some(SearchResult::Cancelled));
    }

    #[test]
    fn handle_key_enter_with_hits_returns_jump() {
        let mut s = SearchState::new();
        s.hits = vec![Hit {
            message_uuid: "u1".into(),
            session_id: "s1".into(),
            snippet: "snip".into(),
            score: -1.0,
        }];
        s.selected_index = 0;
        let got = handle_key(&mut s, press(KeyCode::Enter));
        assert_eq!(
            got,
            Some(SearchResult::JumpToMessage {
                session_id: "s1".into(),
                message_uuid: "u1".into()
            })
        );
    }

    #[test]
    fn handle_key_enter_with_no_hits_returns_none() {
        let mut s = SearchState::new();
        assert!(handle_key(&mut s, press(KeyCode::Enter)).is_none());
    }

    #[test]
    fn handle_key_arrow_keys_move_selection() {
        let mut s = SearchState::new();
        s.hits = (0..3)
            .map(|i| Hit {
                message_uuid: format!("u{i}"),
                session_id: "s1".into(),
                snippet: "snip".into(),
                score: -1.0,
            })
            .collect();
        handle_key(&mut s, press(KeyCode::Down));
        assert_eq!(s.selected_index, 1);
        handle_key(&mut s, press(KeyCode::Down));
        assert_eq!(s.selected_index, 2);
        // Past end is a no-op (clamped).
        handle_key(&mut s, press(KeyCode::Down));
        assert_eq!(s.selected_index, 2);
        handle_key(&mut s, press(KeyCode::Up));
        assert_eq!(s.selected_index, 1);
    }

    #[test]
    fn handle_key_typing_updates_last_keystroke() {
        let mut s = SearchState::new();
        // Force last_keystroke to the past so we can detect the bump.
        s.last_keystroke_at = Instant::now() - Duration::from_secs(10);
        let before = s.last_keystroke_at;
        handle_key(&mut s, press_char('x'));
        assert!(s.last_keystroke_at > before);
    }

    #[test]
    fn handle_key_release_events_ignored() {
        let mut s = SearchState::new();
        let ev = KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert!(handle_key(&mut s, ev).is_none());
        assert_eq!(s.query_input, "");
    }

    // ---- filter pills / parsing -------------------------------------

    #[test]
    fn typing_project_modifier_lifts_to_pill() {
        let mut s = SearchState::new();
        for c in "project:foo bar".chars() {
            handle_key(&mut s, press_char(c));
        }
        assert_eq!(s.filters.project.as_deref(), Some("foo"));
        // remainder is `bar` per parse_query — query_input keeps the full
        // text, but the pill is derived from `filters`.
        let (_, rest) = fts::parse_query(&s.query_input);
        assert_eq!(rest, "bar");
    }

    #[test]
    fn typing_user_modifier_lifts_to_role_pill() {
        let mut s = SearchState::new();
        for c in "user: bugs".chars() {
            handle_key(&mut s, press_char(c));
        }
        assert_eq!(s.filters.role, Some(MessageRole::User));
    }

    #[test]
    fn backspace_removes_trailing_pill_when_at_end() {
        let mut s = SearchState::new();
        s.query_input = "project:foo".into();
        s.reparse_filters();
        assert!(s.filters.project.is_some());
        let popped = s.pop_last_modifier();
        assert!(popped);
        assert_eq!(s.query_input, "");
        assert!(s.filters.project.is_none());
    }

    #[test]
    fn backspace_on_non_modifier_tail_falls_through_to_char_delete() {
        let mut s = SearchState::new();
        s.query_input = "project:foo hello".into();
        s.reparse_filters();
        handle_key(&mut s, press(KeyCode::Backspace));
        // `hello` -> `hell`; pill still present.
        assert_eq!(s.query_input, "project:foo hell");
        assert_eq!(s.filters.project.as_deref(), Some("foo"));
    }

    // ---- execute_query --------------------------------------------

    #[test]
    fn execute_query_populates_hits_from_seeded_db() {
        let c = open_in_memory_with_fts();
        seed(&c, "s1", Some("proj"), "u1", "user", "hello world");
        let mut s = SearchState::new();
        s.query_input = "hello".into();
        execute_query(&mut s, &c);
        assert_eq!(s.hits.len(), 1);
        assert_eq!(s.hits[0].message_uuid, "u1");
        assert_eq!(s.committed_query, "hello");
        assert!(s.error.is_none());
    }

    #[test]
    fn execute_query_applies_project_filter_from_input() {
        let c = open_in_memory_with_fts();
        seed(&c, "s1", Some("proj-a"), "u1", "user", "shared topic");
        seed(&c, "s2", Some("proj-b"), "u2", "user", "shared topic");
        let mut s = SearchState::new();
        s.query_input = "project:proj-a shared".into();
        s.reparse_filters();
        execute_query(&mut s, &c);
        assert_eq!(s.hits.len(), 1);
        assert_eq!(s.hits[0].session_id, "s1");
    }

    #[test]
    fn execute_query_empty_input_clears_hits() {
        let c = open_in_memory_with_fts();
        seed(&c, "s1", None, "u1", "user", "hello world");
        let mut s = SearchState::new();
        s.hits = vec![Hit {
            message_uuid: "stale".into(),
            session_id: "s1".into(),
            snippet: "old".into(),
            score: -1.0,
        }];
        s.query_input = "".into();
        execute_query(&mut s, &c);
        assert!(s.hits.is_empty());
        assert!(s.error.is_none());
    }

    #[test]
    fn execute_query_clamps_selection_to_new_hit_count() {
        let c = open_in_memory_with_fts();
        seed(&c, "s1", None, "u1", "user", "alpha");
        let mut s = SearchState::new();
        s.selected_index = 42;
        s.query_input = "alpha".into();
        execute_query(&mut s, &c);
        // 1 hit, selection clamps to 0.
        assert_eq!(s.selected_index, 0);
    }

    // ---- draw -------------------------------------------------------

    #[test]
    fn draw_empty_state_does_not_panic() {
        let s = SearchState::new();
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(70, 60, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
    }

    #[test]
    fn draw_with_hits_renders_session_id() {
        let mut s = SearchState::new();
        s.hits = vec![Hit {
            message_uuid: "u1".into(),
            session_id: "abcdef123456".into(),
            snippet: "the [fox] jumps".into(),
            score: -1.0,
        }];
        s.query_input = "fox".into();
        s.committed_query = "fox".into();
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(70, 60, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut all = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                all.push_str(buf[(x, y)].symbol());
            }
            all.push('\n');
        }
        // Truncated session id (first 8 chars) should appear in the list.
        assert!(all.contains("abcdef12"), "buffer missing session id:\n{all}");
        // Title carries the query.
        assert!(all.contains("Search"), "buffer missing title:\n{all}");
    }

    #[test]
    fn draw_renders_pill_row_when_filter_set() {
        let mut s = SearchState::new();
        s.query_input = "project:foo".into();
        s.reparse_filters();
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(80, 60, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut all = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                all.push_str(buf[(x, y)].symbol());
            }
            all.push('\n');
        }
        assert!(
            all.contains("project:foo"),
            "buffer missing project pill:\n{all}"
        );
    }

    #[test]
    fn draw_tiny_area_does_not_panic() {
        // Modal must degrade gracefully even when the user shrinks the
        // terminal below the modal's preferred size.
        let s = SearchState::new();
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(20, 6)).unwrap();
        term.draw(|f| {
            let area = centered_rect(70, 60, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
    }

    // ---- snippet_to_spans ------------------------------------------

    #[test]
    fn snippet_to_spans_splits_on_brackets() {
        let theme = Theme::default_dark();
        let spans = snippet_to_spans("a [b] c", &theme);
        // 3 segments: "a ", "b" (bold), " c". Each segment becomes one span.
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "a ");
        assert_eq!(spans[1].content, "b");
        assert_eq!(spans[2].content, " c");
        // Middle span is bold.
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn snippet_to_spans_handles_no_brackets() {
        let theme = Theme::default_dark();
        let spans = snippet_to_spans("plain text", &theme);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "plain text");
    }

    // ---- centered_rect ---------------------------------------------

    #[test]
    fn centered_rect_centers_within_parent() {
        let parent = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 50,
        };
        let r = centered_rect(50, 50, parent);
        assert_eq!(r.width, 50);
        assert_eq!(r.height, 25);
        assert_eq!(r.x, 25);
        assert_eq!(r.y, 12);
    }
}
