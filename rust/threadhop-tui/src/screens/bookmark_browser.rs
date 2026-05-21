//! Bookmark browser: modal list of pinned messages with jump-to-message.
//!
//! Mirrors `threadhop_core/tui/screens/bookmark.py`. The modal lists every
//! bookmark row (newest first) with `session · note · snippet · age`. Pressing
//! Enter posts back a [`BookmarkBrowserResult::JumpToMessage`] that the App
//! resolves via `pending_jump_message_uuid` (the same channel
//! `screens::search` already uses for cross-modal jumps).
//!
//! Phase 4 task 4.3. Wave 2 wires App integration:
//!   * The App seeds `state.bookmarks` from a cross-session query and refreshes
//!     after each render tick or write.
//!   * `Result::DeleteRequested` is routed into the Confirm modal
//!     (task 4.4); only on `ConfirmYes` does the App call
//!     `threadhop_core::db::delete_bookmark`.
//!
//! ## Why a dedicated `BookmarkRow` instead of `models::Bookmark`?
//!
//! `models::Bookmark` is keyed by `message_uuid`. The browser needs the
//! `session_id` to pull off a jump, and a `snippet` for rendering. Rather
//! than force every consumer to do the JOIN themselves, the modal accepts a
//! flat row type that Wave 2 fills from a `list_bookmarks_with_context`
//! helper (planned per the Phase 4 plan).

#![allow(dead_code)] // Wave 2 wires the App integration; until then the
                     // binary doesn't reference these helpers.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        block::Padding, Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph,
        StatefulWidget, Widget,
    },
};
use threadhop_core::{
    models::BookmarkKind,
    theme::{hex_to_rgb, Theme},
};

// ---- public types -----------------------------------------------------------

/// Flat row carrying everything the browser needs to render and to post back
/// a jump request. Wave 2 fills this from a JOINed DB query
/// (`bookmarks ⨝ messages ⨝ sessions`) so the modal stays App-agnostic.
#[derive(Debug, Clone, PartialEq)]
pub struct BookmarkRow {
    pub id: i64,
    pub session_id: String,
    pub message_uuid: String,
    pub kind: BookmarkKind,
    pub note: Option<String>,
    pub snippet: Option<String>,
    pub created_at: f64,
}

/// Modal state.
///
/// The list is rendered in the order supplied by the caller — Wave 2 sorts
/// newest-first server-side (`ORDER BY created_at DESC`), matching Python's
/// `db.list_bookmarks`.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub bookmarks: Vec<BookmarkRow>,
    pub selected_index: usize,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the bookmark list and clamp selection. Used by Wave 2 after
    /// a successful refresh.
    pub fn set_bookmarks(&mut self, rows: Vec<BookmarkRow>) {
        self.bookmarks = rows;
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        if self.bookmarks.is_empty() {
            self.selected_index = 0;
        } else if self.selected_index >= self.bookmarks.len() {
            self.selected_index = self.bookmarks.len() - 1;
        }
    }

    fn current(&self) -> Option<&BookmarkRow> {
        self.bookmarks.get(self.selected_index)
    }
}

/// Result posted back to the App when the modal closes (or requests an
/// out-of-band action like delete).
///
/// `DeleteRequested` is not a close signal — the App opens a Confirm modal
/// over the browser and only deletes on `ConfirmYes`. The browser stays open
/// behind the Confirm so cancelling delete returns the user to the same
/// selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookmarkBrowserResult {
    Cancelled,
    JumpToMessage {
        session_id: String,
        message_uuid: String,
    },
    DeleteRequested {
        bookmark_id: i64,
    },
}

// ---- key handling -----------------------------------------------------------

/// Handle one key event. Returns `Some(BookmarkBrowserResult)` if the App
/// should react (close or open the Confirm modal), `None` if the modal stays
/// open and absorbs the keystroke (e.g. navigation).
///
/// Recognised keys:
/// * `Esc` / `q` / `Ctrl-C` → [`BookmarkBrowserResult::Cancelled`]
/// * `Enter` → [`BookmarkBrowserResult::JumpToMessage`] (if a row is selected)
/// * `j` / `Down` / `Ctrl-N` → move selection down
/// * `k` / `Up` / `Ctrl-P` → move selection up
/// * `d` → [`BookmarkBrowserResult::DeleteRequested`] (App routes through
///   Confirm — task 4.4)
pub fn handle_key(state: &mut State, key: KeyEvent) -> Option<BookmarkBrowserResult> {
    // Skip Release; accept Press + Repeat. Some terminals only emit Repeat
    // for held keys, and unit tests synthesize Press. Without this filter,
    // every key fires twice (down-edge + up-edge), which manifests as
    // double-stepping in nav — the same bug we hit on the search modal.
    if matches!(key.kind, KeyEventKind::Release) {
        return None;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => Some(BookmarkBrowserResult::Cancelled),
        (KeyCode::Char('q'), KeyModifiers::NONE) => Some(BookmarkBrowserResult::Cancelled),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(BookmarkBrowserResult::Cancelled),

        (KeyCode::Enter, _) => state.current().map(|row| {
            BookmarkBrowserResult::JumpToMessage {
                session_id: row.session_id.clone(),
                message_uuid: row.message_uuid.clone(),
            }
        }),

        (KeyCode::Down, _)
        | (KeyCode::Char('j'), KeyModifiers::NONE)
        | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
            move_selection(state, 1);
            None
        }
        (KeyCode::Up, _)
        | (KeyCode::Char('k'), KeyModifiers::NONE)
        | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
            move_selection(state, -1);
            None
        }

        (KeyCode::Char('d'), KeyModifiers::NONE) => state
            .current()
            .map(|row| BookmarkBrowserResult::DeleteRequested { bookmark_id: row.id }),

        _ => None,
    }
}

fn move_selection(state: &mut State, delta: i32) {
    if state.bookmarks.is_empty() {
        state.selected_index = 0;
        return;
    }
    let len = state.bookmarks.len() as i32;
    let next = (state.selected_index as i32 + delta).clamp(0, len - 1);
    state.selected_index = next as usize;
}

// ---- rendering --------------------------------------------------------------

/// Render the modal. Caller is expected to have computed a centered popup
/// rect (see [`centered_rect`]). [`Clear`] is called for safety so the modal
/// is opaque over the underlying screen.
///
/// Layout (top-to-bottom inside the bordered block):
///  1. Status row (height 1) — `<n> bookmarks`
///  2. Bookmark list (fills)
///  3. Help row (height 1) — `↑/↓ navigate • Enter jump • d delete • Esc close`
pub fn draw(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(theme))
        .padding(Padding::new(2, 2, 1, 1))
        .style(modal_bg_style(theme))
        .title(Span::styled(" Bookmarks ", title_style(theme)));
    let inner = block.inner(area);
    block.render(area, buf);

    // Skip rendering inner rows if the inner rect is degenerate (caller
    // gave us less than 3 rows of usable height). Block still renders.
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    render_status(state, theme, v[0], buf);
    render_list(state, theme, v[1], buf);
    render_help(theme, v[2], buf);
}

fn render_status(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    let count = match state.bookmarks.len() {
        0 => " 0 bookmarks ".to_string(),
        1 => " 1 bookmark ".to_string(),
        n => format!(" {} bookmarks ", n),
    };
    Paragraph::new(Line::from(Span::styled(count, muted))).render(area, buf);
}

fn render_help(theme: &Theme, area: Rect, buf: &mut Buffer) {
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    Paragraph::new(Line::from(Span::styled(
        " j/k navigate · Enter jump · d delete · Esc close ".to_string(),
        muted,
    )))
    .render(area, buf);
}

fn render_list(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    if state.bookmarks.is_empty() {
        let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
        let msg = "No bookmarks.";
        // Center horizontally on the first row.
        let centered_x = area.x
            + area
                .width
                .saturating_sub(msg.chars().count() as u16)
                / 2;
        let target = Rect {
            x: centered_x,
            y: area.y + area.height / 2,
            width: area.width.saturating_sub(centered_x - area.x),
            height: 1,
        };
        Paragraph::new(Span::styled(msg.to_string(), muted)).render(target, buf);
        return;
    }

    // `now` is captured once for the whole list so age columns stay coherent
    // within a single render (avoid `now` drift between rows).
    let now = wall_clock_now();

    let items: Vec<ListItem> = state
        .bookmarks
        .iter()
        .map(|row| render_row(row, theme, now))
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_index));

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(theme_color(&theme.accent, Color::Magenta))
            .fg(theme_color(&theme.background, Color::Black))
            .add_modifier(Modifier::BOLD),
    );
    StatefulWidget::render(list, area, buf, &mut list_state);
}

fn render_row(row: &BookmarkRow, theme: &Theme, now: f64) -> ListItem<'static> {
    let kind_style = match row.kind {
        BookmarkKind::Research => Style::default()
            .fg(theme_color(&theme.accent, Color::Magenta))
            .add_modifier(Modifier::BOLD),
        BookmarkKind::Bookmark => Style::default()
            .fg(theme_color(&theme.primary, Color::Yellow))
            .add_modifier(Modifier::BOLD),
    };
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    let session_style = Style::default()
        .fg(theme_color(&theme.secondary, Color::Cyan))
        .add_modifier(Modifier::BOLD);

    let kind_label = match row.kind {
        BookmarkKind::Research => "[research]",
        BookmarkKind::Bookmark => "[bookmark]",
    };

    let sid = short_session(&row.session_id);
    let snippet = render_snippet(row.snippet.as_deref().unwrap_or(""));
    let age = format_age(row.created_at, now);

    let mut spans: Vec<Span<'static>> = vec![
        Span::styled("★ ", Style::default().fg(theme_color(&theme.primary, Color::Yellow))),
        Span::styled(format!("{} ", kind_label), kind_style),
        Span::styled(format!("{:<10}", sid), session_style),
        Span::styled(" · ", muted),
    ];

    if let Some(note) = row.note.as_deref() {
        let trimmed = note.trim();
        if !trimmed.is_empty() {
            spans.push(Span::styled(
                trimmed.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(" · ", muted));
        }
    }

    if !snippet.is_empty() {
        spans.push(Span::raw(snippet));
        spans.push(Span::styled(" · ", muted));
    }

    spans.push(Span::styled(age, muted));

    ListItem::new(Line::from(spans))
}

// ---- helpers ----------------------------------------------------------------

/// First 8 chars of a session id — same convention as the search modal.
fn short_session(sid: &str) -> String {
    sid.chars().take(8).collect()
}

/// Flatten newlines, collapse whitespace, truncate to 80 chars with an
/// ellipsis. Mirrors what `BookmarkItem.compose` does in the Python TUI but
/// at a tighter width (the row already carries kind + session + note).
fn render_snippet(raw: &str) -> String {
    let cleaned: String = raw
        .replace(['\n', '\t'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.chars().count() > 80 {
        let truncated: String = cleaned.chars().take(80).collect();
        format!("{}…", truncated)
    } else {
        cleaned
    }
}

/// Same age buckets as `widgets::session_list::format_age` (kept duplicated
/// to avoid a cross-module dep; this is a 4-line helper and the bucket
/// thresholds are stable).
fn format_age(timestamp: f64, now: f64) -> String {
    let age = (now - timestamp).max(0.0);
    if age < 60.0 {
        format!("{}s", age as i64)
    } else if age < 3600.0 {
        format!("{}m", (age / 60.0) as i64)
    } else if age < 86_400.0 {
        format!("{}h", (age / 3600.0) as i64)
    } else {
        format!("{}d", (age / 86_400.0) as i64)
    }
}

fn wall_clock_now() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
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
        .fg(theme_color(&theme.accent, Color::Magenta))
        .add_modifier(Modifier::BOLD)
}

fn modal_bg_style(theme: &Theme) -> Style {
    Style::default().bg(theme_color(&theme.background_panel, Color::Reset))
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

    fn row(id: i64, session: &str, uuid: &str, note: Option<&str>) -> BookmarkRow {
        BookmarkRow {
            id,
            session_id: session.into(),
            message_uuid: uuid.into(),
            kind: BookmarkKind::Bookmark,
            note: note.map(|s| s.into()),
            snippet: Some("hello world".into()),
            created_at: 0.0,
        }
    }

    fn press(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn press_char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    // ---- nav --------------------------------------------------------

    #[test]
    fn j_and_down_move_selection_down() {
        let mut s = State::new();
        s.set_bookmarks(vec![
            row(1, "s1", "u1", None),
            row(2, "s2", "u2", None),
            row(3, "s3", "u3", None),
        ]);
        handle_key(&mut s, press_char('j'));
        assert_eq!(s.selected_index, 1);
        handle_key(&mut s, press(KeyCode::Down));
        assert_eq!(s.selected_index, 2);
        // Past the end clamps.
        handle_key(&mut s, press_char('j'));
        assert_eq!(s.selected_index, 2);
    }

    #[test]
    fn k_and_up_move_selection_up() {
        let mut s = State::new();
        s.set_bookmarks(vec![
            row(1, "s1", "u1", None),
            row(2, "s2", "u2", None),
            row(3, "s3", "u3", None),
        ]);
        s.selected_index = 2;
        handle_key(&mut s, press_char('k'));
        assert_eq!(s.selected_index, 1);
        handle_key(&mut s, press(KeyCode::Up));
        assert_eq!(s.selected_index, 0);
        // Below 0 clamps.
        handle_key(&mut s, press_char('k'));
        assert_eq!(s.selected_index, 0);
    }

    #[test]
    fn nav_on_empty_list_is_noop() {
        let mut s = State::new();
        handle_key(&mut s, press_char('j'));
        handle_key(&mut s, press_char('k'));
        assert_eq!(s.selected_index, 0);
    }

    // ---- enter / esc / d --------------------------------------------

    #[test]
    fn enter_posts_jump_to_message_for_selected_row() {
        let mut s = State::new();
        s.set_bookmarks(vec![
            row(1, "s1", "u1", None),
            row(2, "session-two", "uuid-two", Some("note")),
        ]);
        s.selected_index = 1;
        let got = handle_key(&mut s, press(KeyCode::Enter));
        assert_eq!(
            got,
            Some(BookmarkBrowserResult::JumpToMessage {
                session_id: "session-two".into(),
                message_uuid: "uuid-two".into(),
            })
        );
    }

    #[test]
    fn enter_on_empty_list_returns_none() {
        let mut s = State::new();
        assert!(handle_key(&mut s, press(KeyCode::Enter)).is_none());
    }

    #[test]
    fn d_posts_delete_requested_with_bookmark_id() {
        let mut s = State::new();
        s.set_bookmarks(vec![row(42, "s1", "u1", None)]);
        let got = handle_key(&mut s, press_char('d'));
        assert_eq!(
            got,
            Some(BookmarkBrowserResult::DeleteRequested { bookmark_id: 42 })
        );
    }

    #[test]
    fn d_on_empty_list_returns_none() {
        let mut s = State::new();
        assert!(handle_key(&mut s, press_char('d')).is_none());
    }

    #[test]
    fn esc_posts_cancelled() {
        let mut s = State::new();
        s.set_bookmarks(vec![row(1, "s1", "u1", None)]);
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Esc)),
            Some(BookmarkBrowserResult::Cancelled)
        );
    }

    #[test]
    fn q_posts_cancelled() {
        let mut s = State::new();
        assert_eq!(
            handle_key(&mut s, press_char('q')),
            Some(BookmarkBrowserResult::Cancelled)
        );
    }

    #[test]
    fn ctrl_c_posts_cancelled() {
        let mut s = State::new();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            handle_key(&mut s, ev),
            Some(BookmarkBrowserResult::Cancelled)
        );
    }

    // ---- release filter --------------------------------------------

    #[test]
    fn release_events_are_ignored() {
        let mut s = State::new();
        s.set_bookmarks(vec![
            row(1, "s1", "u1", None),
            row(2, "s2", "u2", None),
        ]);
        let ev = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert!(handle_key(&mut s, ev).is_none());
        // Selection unchanged — Release must not double-fire.
        assert_eq!(s.selected_index, 0);
    }

    #[test]
    fn repeat_events_are_accepted() {
        let mut s = State::new();
        s.set_bookmarks(vec![
            row(1, "s1", "u1", None),
            row(2, "s2", "u2", None),
        ]);
        let ev = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Repeat,
            state: crossterm::event::KeyEventState::NONE,
        };
        handle_key(&mut s, ev);
        assert_eq!(s.selected_index, 1);
    }

    // ---- set_bookmarks / clamp -------------------------------------

    #[test]
    fn set_bookmarks_clamps_selection_to_new_length() {
        let mut s = State::new();
        s.selected_index = 5;
        s.set_bookmarks(vec![row(1, "s1", "u1", None)]);
        assert_eq!(s.selected_index, 0);
    }

    #[test]
    fn set_bookmarks_empty_resets_selection() {
        let mut s = State::new();
        s.selected_index = 3;
        s.set_bookmarks(Vec::new());
        assert_eq!(s.selected_index, 0);
    }

    // ---- helpers ----------------------------------------------------

    #[test]
    fn render_snippet_collapses_whitespace_and_truncates() {
        assert_eq!(render_snippet("hello\n\nworld"), "hello world");
        let long = "a".repeat(100);
        let out = render_snippet(&long);
        assert_eq!(out.chars().count(), 81); // 80 + ellipsis
        assert!(out.ends_with('…'));
    }

    #[test]
    fn short_session_truncates_to_8_chars() {
        assert_eq!(short_session("abcdef123456"), "abcdef12");
        assert_eq!(short_session("short"), "short");
    }

    #[test]
    fn format_age_buckets() {
        assert_eq!(format_age(0.0, 30.0), "30s");
        assert_eq!(format_age(0.0, 120.0), "2m");
        assert_eq!(format_age(0.0, 7200.0), "2h");
        assert_eq!(format_age(0.0, 2.0 * 86_400.0), "2d");
    }

    // ---- draw -------------------------------------------------------

    fn snapshot_buffer(buf: &Buffer) -> String {
        let mut all = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                all.push_str(buf[(x, y)].symbol());
            }
            all.push('\n');
        }
        all
    }

    #[test]
    fn draw_empty_state_does_not_panic() {
        let s = State::new();
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(70, 60, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let all = snapshot_buffer(term.backend().buffer());
        assert!(all.contains("Bookmarks"), "title missing:\n{all}");
        assert!(all.contains("No bookmarks."), "empty msg missing:\n{all}");
    }

    #[test]
    fn draw_with_rows_renders_session_id_and_note() {
        let mut s = State::new();
        s.set_bookmarks(vec![BookmarkRow {
            id: 1,
            session_id: "abcdef123456".into(),
            message_uuid: "u1".into(),
            kind: BookmarkKind::Bookmark,
            note: Some("ship it".into()),
            snippet: Some("the quick brown fox".into()),
            created_at: 0.0,
        }]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(90, 80, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let all = snapshot_buffer(term.backend().buffer());
        assert!(all.contains("abcdef12"), "session id missing:\n{all}");
        assert!(all.contains("ship it"), "note missing:\n{all}");
        assert!(all.contains("quick brown fox"), "snippet missing:\n{all}");
        assert!(all.contains("Bookmarks"), "title missing:\n{all}");
    }

    #[test]
    fn draw_tiny_area_does_not_panic() {
        // Modal must degrade gracefully when the user shrinks the
        // terminal below its preferred size.
        let s = State::new();
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(20, 6)).unwrap();
        term.draw(|f| {
            let area = centered_rect(70, 60, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
    }

    #[test]
    fn draw_buffer_changes_after_navigation() {
        // Frame-buffer test: render once, navigate, render again, assert
        // the buffer differs (highlight moves with selection).
        let mut s = State::new();
        s.set_bookmarks(vec![
            row(1, "session-a", "u1", Some("first")),
            row(2, "session-b", "u2", Some("second")),
            row(3, "session-c", "u3", Some("third")),
        ]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();

        term.draw(|f| {
            let area = centered_rect(90, 80, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let before = snapshot_buffer_styles(term.backend().buffer());

        handle_key(&mut s, press_char('j'));
        handle_key(&mut s, press_char('j'));
        assert_eq!(s.selected_index, 2);

        term.draw(|f| {
            let area = centered_rect(90, 80, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let after = snapshot_buffer_styles(term.backend().buffer());

        assert_ne!(
            before, after,
            "buffer should differ after moving selection (highlight should follow)"
        );
    }

    /// Snapshot symbols + styles, not just symbols — the highlight changes
    /// background style, which a pure-symbol snapshot would miss.
    fn snapshot_buffer_styles(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                let cell = &buf[(x, y)];
                out.push_str(cell.symbol());
                out.push_str(&format!(
                    "[fg={:?},bg={:?},m={:?}]",
                    cell.fg, cell.bg, cell.modifier
                ));
            }
            out.push('\n');
        }
        out
    }
}
