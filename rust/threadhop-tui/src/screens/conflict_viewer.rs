//! Conflict viewer modal — surfaces cross-session decision conflicts from
//! the reflector pipeline (ADR-020).
//!
//! Mirrors `threadhop conflicts` (see `threadhop_core/cli/commands/conflicts.py`
//! and `threadhop_core/cli/queries.py::query_conflicts`). The modal lists
//! `type: "conflict"` rows from the per-session observation JSONLs joined
//! against the `conflict_reviews` table.
//!
//! ## Data shape
//!
//! Each row carries 2+ `session_ids` (the conflicting sessions, with the
//! origin session first followed by the entries in the observation's `refs`
//! list), a free-text `text` summary, a unix `timestamp`, and a `reviewed`
//! flag pulled from `conflict_reviews`. The App fills this from disk + DB
//! in Wave 2; the modal stays App-agnostic.
//!
//! ## Behavior
//!
//! * `j` / `Down`, `k` / `Up` — move selection
//! * `Enter` — jump to the *first* conflicting session (origin session). The
//!   user can re-open the modal from the other side to triangulate; jumping
//!   to one side is the conservative call. (The Python CLI prints all refs;
//!   the TUI surfaces them in the row but only jumps to the primary.)
//! * `r` — post `MarkResolved`; the App shells out to `threadhop conflicts
//!   --resolved …` (task 5.3 step 3) and refreshes.
//! * `t` — toggle `show_resolved` (reviewed rows hidden by default to match
//!   the CLI's default behavior).
//! * `Esc` / `q` / `Ctrl-C` — `Cancelled`.

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
use threadhop_core::theme::{hex_to_rgb, Theme};

// ---- public types -----------------------------------------------------------

/// Flat row carrying everything the viewer needs to render a conflict and
/// route a resolve/jump action back to the App.
///
/// `session_ids` is non-empty; index 0 is the origin session (the JSONL the
/// conflict was recorded in) and the remainder are the `refs` from the
/// observation entry. `Enter` jumps to `session_ids[0]`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConflictRow {
    pub id: i64,
    pub text: String,
    pub session_ids: Vec<String>,
    pub timestamp: f64,
    pub reviewed: bool,
}

/// Modal state.
///
/// `conflicts` is the *unfiltered* list as supplied by the caller. The
/// visible-row computation applies `show_resolved` at render-time so that
/// toggling the filter doesn't lose the caller's data. `selected_index`
/// indexes into the *visible* rows, not the underlying vec — keeps key
/// handling consistent with what the user sees.
#[derive(Debug, Clone, Default)]
pub struct State {
    pub conflicts: Vec<ConflictRow>,
    pub selected_index: usize,
    pub show_resolved: bool,
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the conflict list and clamp selection. Used by Wave 2 after
    /// a successful refresh (e.g. after `MarkResolved` returns).
    pub fn set_conflicts(&mut self, rows: Vec<ConflictRow>) {
        self.conflicts = rows;
        self.clamp_selection();
    }

    /// Indices into `self.conflicts` for the rows that should currently be
    /// rendered. Filters out reviewed rows when `!show_resolved`.
    fn visible_indices(&self) -> Vec<usize> {
        self.conflicts
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                if self.show_resolved || !row.reviewed {
                    Some(i)
                } else {
                    None
                }
            })
            .collect()
    }

    fn visible_len(&self) -> usize {
        self.visible_indices().len()
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_len();
        if len == 0 {
            self.selected_index = 0;
        } else if self.selected_index >= len {
            self.selected_index = len - 1;
        }
    }

    /// The currently-selected row (resolved through the visible filter).
    fn current(&self) -> Option<&ConflictRow> {
        let visible = self.visible_indices();
        let idx = *visible.get(self.selected_index)?;
        self.conflicts.get(idx)
    }
}

/// Result posted back to the App.
///
/// `MarkResolved` and `JumpToSession` are not close signals on their own —
/// the App decides whether to keep the modal open after handling them. The
/// modal posts; the App routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictViewerResult {
    Cancelled,
    JumpToSession { session_id: String },
    MarkResolved { conflict_id: i64 },
}

// ---- key handling -----------------------------------------------------------

/// Handle one key event.
///
/// Filters `KeyEventKind::Release` so terminals that emit both press and
/// release edges don't double-fire navigation (same trap the bookmark and
/// search modals already guard against).
pub fn handle_key(state: &mut State, key: KeyEvent) -> Option<ConflictViewerResult> {
    if matches!(key.kind, KeyEventKind::Release) {
        return None;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => Some(ConflictViewerResult::Cancelled),
        (KeyCode::Char('q'), KeyModifiers::NONE) => Some(ConflictViewerResult::Cancelled),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(ConflictViewerResult::Cancelled),

        (KeyCode::Enter, _) => state.current().and_then(|row| {
            row.session_ids
                .first()
                .cloned()
                .map(|session_id| ConflictViewerResult::JumpToSession { session_id })
        }),

        (KeyCode::Char('r'), KeyModifiers::NONE) => state
            .current()
            .map(|row| ConflictViewerResult::MarkResolved { conflict_id: row.id }),

        (KeyCode::Char('t'), KeyModifiers::NONE) => {
            state.show_resolved = !state.show_resolved;
            // Visible length changes — re-clamp so selection stays valid.
            state.clamp_selection();
            None
        }

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

        _ => None,
    }
}

fn move_selection(state: &mut State, delta: i32) {
    let len = state.visible_len();
    if len == 0 {
        state.selected_index = 0;
        return;
    }
    let next = (state.selected_index as i32 + delta).clamp(0, len as i32 - 1);
    state.selected_index = next as usize;
}

// ---- rendering --------------------------------------------------------------

/// Render the modal. Caller is expected to have computed a centered popup
/// rect (see [`centered_rect`]). `Clear` is called for safety so the modal
/// is opaque over the underlying screen.
///
/// Layout (top-to-bottom inside the bordered block):
///   1. Status row (height 1) — `<n> conflicts · resolved hidden|shown`
///   2. Conflict list (fills)
///   3. Help row (height 1)
pub fn draw(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(theme))
        .padding(Padding::new(2, 2, 1, 1))
        .style(modal_bg_style(theme))
        .title(Span::styled(" Conflicts ", title_style(theme)));
    let inner = block.inner(area);
    block.render(area, buf);

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
    let visible = state.visible_len();
    let total = state.conflicts.len();
    let hidden = total.saturating_sub(visible);
    let label = match visible {
        0 => " 0 conflicts ".to_string(),
        1 => " 1 conflict ".to_string(),
        n => format!(" {} conflicts ", n),
    };
    let filter = if state.show_resolved {
        " · resolved shown ".to_string()
    } else if hidden > 0 {
        format!(" · {} resolved hidden ", hidden)
    } else {
        " · resolved hidden ".to_string()
    };
    Paragraph::new(Line::from(vec![
        Span::styled(label, muted),
        Span::styled(filter, muted),
    ]))
    .render(area, buf);
}

fn render_help(theme: &Theme, area: Rect, buf: &mut Buffer) {
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    Paragraph::new(Line::from(Span::styled(
        " j/k navigate · Enter jump · r resolve · t toggle resolved · Esc close ".to_string(),
        muted,
    )))
    .render(area, buf);
}

fn render_list(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let visible = state.visible_indices();
    if visible.is_empty() {
        let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
        let msg = if state.conflicts.is_empty() {
            "No conflicts."
        } else {
            "No unresolved conflicts. Press t to show resolved."
        };
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

    let now = wall_clock_now();
    let items: Vec<ListItem> = visible
        .iter()
        .map(|i| render_row(&state.conflicts[*i], theme, now))
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

fn render_row(row: &ConflictRow, theme: &Theme, now: f64) -> ListItem<'static> {
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    let session_style = Style::default()
        .fg(theme_color(&theme.secondary, Color::Cyan))
        .add_modifier(Modifier::BOLD);
    let marker_style = if row.reviewed {
        Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray))
    } else {
        Style::default()
            .fg(theme_color(&theme.accent, Color::Red))
            .add_modifier(Modifier::BOLD)
    };

    let marker = if row.reviewed { "✓ " } else { "! " };
    let sessions = format_sessions(&row.session_ids);
    let text = render_text(&row.text);
    let age = format_age(row.timestamp, now);

    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(marker.to_string(), marker_style),
        Span::styled(sessions, session_style),
        Span::styled(" · ", muted),
    ];

    if !text.is_empty() {
        spans.push(Span::raw(text));
        spans.push(Span::styled(" · ", muted));
    }

    spans.push(Span::styled(age, muted));

    if row.reviewed {
        spans.push(Span::styled(" · resolved".to_string(), muted));
    }

    ListItem::new(Line::from(spans))
}

// ---- helpers ----------------------------------------------------------------

/// Render the conflicting-session ids: first two short ids joined with `↔`,
/// plus a `+N` suffix if there are more. Matches the digest-bar density.
fn format_sessions(ids: &[String]) -> String {
    if ids.is_empty() {
        return "(no sessions)".to_string();
    }
    let short: Vec<String> = ids.iter().take(2).map(|s| short_session(s)).collect();
    let mut out = short.join(" ↔ ");
    if ids.len() > 2 {
        out.push_str(&format!(" +{}", ids.len() - 2));
    }
    out
}

fn short_session(sid: &str) -> String {
    sid.chars().take(8).collect()
}

/// Flatten newlines, collapse whitespace, truncate to 120 chars. Wider than
/// the bookmark snippet because the conflict text is the row's payload.
fn render_text(raw: &str) -> String {
    let cleaned: String = raw
        .replace(['\n', '\t'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if cleaned.chars().count() > 120 {
        let truncated: String = cleaned.chars().take(120).collect();
        format!("{}…", truncated)
    } else {
        cleaned
    }
}

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

/// Centered popup rect — same helper shape as the bookmark browser so the
/// App can place all modals through one interface.
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

    fn row(id: i64, sessions: &[&str], text: &str, reviewed: bool) -> ConflictRow {
        ConflictRow {
            id,
            text: text.to_string(),
            session_ids: sessions.iter().map(|s| s.to_string()).collect(),
            timestamp: 0.0,
            reviewed,
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
        s.set_conflicts(vec![
            row(1, &["s1", "s2"], "a", false),
            row(2, &["s3", "s4"], "b", false),
            row(3, &["s5", "s6"], "c", false),
        ]);
        handle_key(&mut s, press_char('j'));
        assert_eq!(s.selected_index, 1);
        handle_key(&mut s, press(KeyCode::Down));
        assert_eq!(s.selected_index, 2);
        handle_key(&mut s, press_char('j'));
        assert_eq!(s.selected_index, 2);
    }

    #[test]
    fn k_and_up_move_selection_up() {
        let mut s = State::new();
        s.set_conflicts(vec![
            row(1, &["s1", "s2"], "a", false),
            row(2, &["s3", "s4"], "b", false),
            row(3, &["s5", "s6"], "c", false),
        ]);
        s.selected_index = 2;
        handle_key(&mut s, press_char('k'));
        assert_eq!(s.selected_index, 1);
        handle_key(&mut s, press(KeyCode::Up));
        assert_eq!(s.selected_index, 0);
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

    // ---- enter / r / esc -------------------------------------------

    #[test]
    fn enter_posts_jump_to_first_conflicting_session() {
        let mut s = State::new();
        s.set_conflicts(vec![
            row(1, &["origin-a", "ref-b"], "a", false),
            row(2, &["origin-c", "ref-d", "ref-e"], "b", false),
        ]);
        s.selected_index = 1;
        let got = handle_key(&mut s, press(KeyCode::Enter));
        assert_eq!(
            got,
            Some(ConflictViewerResult::JumpToSession {
                session_id: "origin-c".into(),
            })
        );
    }

    #[test]
    fn enter_on_empty_list_returns_none() {
        let mut s = State::new();
        assert!(handle_key(&mut s, press(KeyCode::Enter)).is_none());
    }

    #[test]
    fn r_posts_mark_resolved_with_conflict_id() {
        let mut s = State::new();
        s.set_conflicts(vec![row(42, &["s1", "s2"], "a", false)]);
        let got = handle_key(&mut s, press_char('r'));
        assert_eq!(
            got,
            Some(ConflictViewerResult::MarkResolved { conflict_id: 42 })
        );
    }

    #[test]
    fn r_on_empty_list_returns_none() {
        let mut s = State::new();
        assert!(handle_key(&mut s, press_char('r')).is_none());
    }

    #[test]
    fn esc_q_and_ctrl_c_post_cancelled() {
        let mut s = State::new();
        s.set_conflicts(vec![row(1, &["s1", "s2"], "a", false)]);
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Esc)),
            Some(ConflictViewerResult::Cancelled)
        );
        assert_eq!(
            handle_key(&mut s, press_char('q')),
            Some(ConflictViewerResult::Cancelled)
        );
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            handle_key(&mut s, ctrl_c),
            Some(ConflictViewerResult::Cancelled)
        );
    }

    // ---- show_resolved toggle --------------------------------------

    #[test]
    fn t_toggles_show_resolved_and_filter_hides_reviewed_by_default() {
        let mut s = State::new();
        s.set_conflicts(vec![
            row(1, &["s1", "s2"], "open", false),
            row(2, &["s3", "s4"], "done", true),
            row(3, &["s5", "s6"], "also open", false),
        ]);
        assert_eq!(s.visible_len(), 2);
        let got = handle_key(&mut s, press(KeyCode::Enter));
        assert_eq!(
            got,
            Some(ConflictViewerResult::JumpToSession {
                session_id: "s1".into(),
            })
        );

        handle_key(&mut s, press_char('t'));
        assert!(s.show_resolved);
        assert_eq!(s.visible_len(), 3);

        handle_key(&mut s, press_char('t'));
        assert!(!s.show_resolved);
        assert_eq!(s.visible_len(), 2);
    }

    #[test]
    fn toggle_clamps_selection_when_filter_shrinks_list() {
        let mut s = State::new();
        s.set_conflicts(vec![
            row(1, &["s1", "s2"], "open", false),
            row(2, &["s3", "s4"], "done-a", true),
            row(3, &["s5", "s6"], "done-b", true),
        ]);
        s.show_resolved = true;
        s.selected_index = 2;
        handle_key(&mut s, press_char('t'));
        assert!(!s.show_resolved);
        assert_eq!(s.visible_len(), 1);
        assert_eq!(s.selected_index, 0);
    }

    #[test]
    fn nav_indexes_visible_rows_not_underlying_vec() {
        let mut s = State::new();
        s.set_conflicts(vec![
            row(1, &["s1", "s2"], "open-a", false),
            row(2, &["s3", "s4"], "done", true),
            row(3, &["s5", "s6"], "open-b", false),
        ]);
        handle_key(&mut s, press_char('j'));
        assert_eq!(s.selected_index, 1);
        let got = handle_key(&mut s, press(KeyCode::Enter));
        assert_eq!(
            got,
            Some(ConflictViewerResult::JumpToSession {
                session_id: "s5".into(),
            })
        );
    }

    // ---- release filter --------------------------------------------

    #[test]
    fn release_events_are_ignored() {
        let mut s = State::new();
        s.set_conflicts(vec![
            row(1, &["s1", "s2"], "a", false),
            row(2, &["s3", "s4"], "b", false),
        ]);
        let ev = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert!(handle_key(&mut s, ev).is_none());
        assert_eq!(s.selected_index, 0);
    }

    #[test]
    fn repeat_events_are_accepted() {
        let mut s = State::new();
        s.set_conflicts(vec![
            row(1, &["s1", "s2"], "a", false),
            row(2, &["s3", "s4"], "b", false),
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

    // ---- set_conflicts / clamp -------------------------------------

    #[test]
    fn set_conflicts_clamps_selection_to_new_length() {
        let mut s = State::new();
        s.selected_index = 5;
        s.set_conflicts(vec![row(1, &["s1", "s2"], "a", false)]);
        assert_eq!(s.selected_index, 0);
    }

    #[test]
    fn set_conflicts_empty_resets_selection() {
        let mut s = State::new();
        s.selected_index = 3;
        s.set_conflicts(Vec::new());
        assert_eq!(s.selected_index, 0);
    }

    // ---- helpers ----------------------------------------------------

    #[test]
    fn format_sessions_joins_two_ids_and_summarizes_extras() {
        assert_eq!(format_sessions(&[]), "(no sessions)");
        assert_eq!(
            format_sessions(&["abcdef123456".into(), "ghijkl789012".into()]),
            "abcdef12 ↔ ghijkl78"
        );
        assert_eq!(
            format_sessions(&[
                "abcdef123456".into(),
                "ghijkl789012".into(),
                "mnopqr345678".into(),
                "stuvwx901234".into(),
            ]),
            "abcdef12 ↔ ghijkl78 +2"
        );
    }

    #[test]
    fn render_text_collapses_whitespace_and_truncates() {
        assert_eq!(render_text("hello\n\nworld"), "hello world");
        let long = "a".repeat(200);
        let out = render_text(&long);
        assert_eq!(out.chars().count(), 121);
        assert!(out.ends_with('…'));
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
        assert!(all.contains("Conflicts"), "title missing:\n{all}");
        assert!(all.contains("No conflicts."), "empty msg missing:\n{all}");
    }

    #[test]
    fn draw_with_rows_renders_sessions_and_text() {
        let mut s = State::new();
        s.set_conflicts(vec![ConflictRow {
            id: 1,
            text: "model differs on retention window".into(),
            session_ids: vec!["abcdef123456".into(), "ghijkl789012".into()],
            timestamp: 0.0,
            reviewed: false,
        }]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(95, 80, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let all = snapshot_buffer(term.backend().buffer());
        assert!(all.contains("abcdef12"), "first session missing:\n{all}");
        assert!(all.contains("ghijkl78"), "second session missing:\n{all}");
        assert!(
            all.contains("retention window"),
            "text missing:\n{all}"
        );
        assert!(all.contains("Conflicts"), "title missing:\n{all}");
    }

    #[test]
    fn draw_hides_reviewed_by_default_and_shows_them_after_toggle() {
        let mut s = State::new();
        s.set_conflicts(vec![
            row(1, &["aaaa1111", "bbbb2222"], "open one", false),
            row(2, &["cccc3333", "dddd4444"], "already resolved", true),
        ]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();

        term.draw(|f| {
            let area = centered_rect(95, 80, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let hidden = snapshot_buffer(term.backend().buffer());
        assert!(hidden.contains("open one"), "open row missing:\n{hidden}");
        assert!(
            !hidden.contains("already resolved"),
            "resolved row should be hidden:\n{hidden}"
        );

        handle_key(&mut s, press_char('t'));
        term.draw(|f| {
            let area = centered_rect(95, 80, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let shown = snapshot_buffer(term.backend().buffer());
        assert!(
            shown.contains("already resolved"),
            "resolved row should be visible after toggle:\n{shown}"
        );
    }

    #[test]
    fn draw_tiny_area_does_not_panic() {
        let mut s = State::new();
        s.set_conflicts(vec![row(1, &["s1", "s2"], "a", false)]);
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
        let mut s = State::new();
        s.set_conflicts(vec![
            row(1, &["sess-a", "sess-b"], "first", false),
            row(2, &["sess-c", "sess-d"], "second", false),
            row(3, &["sess-e", "sess-f"], "third", false),
        ]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();

        term.draw(|f| {
            let area = centered_rect(95, 80, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let before = snapshot_buffer_styles(term.backend().buffer());

        handle_key(&mut s, press_char('j'));
        handle_key(&mut s, press_char('j'));
        assert_eq!(s.selected_index, 2);

        term.draw(|f| {
            let area = centered_rect(95, 80, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let after = snapshot_buffer_styles(term.backend().buffer());

        assert_ne!(
            before, after,
            "buffer should differ after moving selection (highlight should follow)"
        );
    }

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
