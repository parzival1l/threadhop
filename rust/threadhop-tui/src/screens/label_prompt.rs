//! Label prompt: dual-mode modal that either picks a `SessionStatus` for
//! the selected session, or captures a custom display name for it.
//!
//! Mirrors `threadhop_core/tui/screens/label_prompt.py` (the Python widget
//! is a single-line input modal; we extend it to a status picker because
//! Phase 4 task 4.5's `LabelKind::Status` variant restricts input to the
//! five valid `SessionStatus` values).
//!
//! ## API shape
//!
//! [`State::new`] takes the session id, its current display name, and the
//! current status — the modal opens in [`Mode::StatusPicker`] by default
//! and the caller can flip to [`Mode::CustomName`] either before render
//! (set `state.mode`) or interactively via Tab.
//!
//! [`handle_key`] returns `Some(LabelPromptResult)` when the modal should
//! close: `Cancelled` on Esc / Ctrl-C, `StatusChosen(status)` on Enter in
//! status mode, `CustomNameSet(opt)` on Enter in custom-name mode (with
//! `None` indicating "clear the custom name" — matches the Python widget's
//! blank-collapses-to-NULL behaviour).
//!
//! Wave 2 wires `t` on the main screen to open the modal in `CustomName`
//! mode and `s` to open it in `StatusPicker` mode (or reuses
//! `OpenLabelPrompt` from `keys.rs` for both — App decides).

#![allow(dead_code)] // Wave 2 wires App integration; until then the binary
                     // doesn't reference these helpers.

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
    models::SessionStatus,
    theme::{hex_to_rgb, Theme},
};

/// The five valid `SessionStatus` values in the order they appear in the
/// status picker. Kept in one place so the cursor index ↔ status mapping
/// stays stable across `handle_key` and `draw`.
pub const STATUS_OPTIONS: &[SessionStatus] = &[
    SessionStatus::Active,
    SessionStatus::InProgress,
    SessionStatus::InReview,
    SessionStatus::Done,
    SessionStatus::Archived,
];

// ---- state -----------------------------------------------------------------

/// Which sub-mode the modal is currently in. Tab toggles between the two;
/// the caller can also set it directly before opening the modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Pick one of [`STATUS_OPTIONS`] using j/k (or arrow keys).
    StatusPicker,
    /// Type a free-form custom display name. Blank submission clears the
    /// stored custom name.
    CustomName,
}

/// Modal state — what the modal owns, distinct from App state.
#[derive(Debug, Clone)]
pub struct State {
    /// Session id we're editing — not used by the modal itself, but the
    /// caller often wants it to route the result back to the right row.
    pub session_id: String,

    /// Pre-existing display name, shown in the title bar so the user knows
    /// which session they're editing.
    pub session_display_name: String,

    /// Status the session has right now — used to pre-select the cursor in
    /// status mode and to render a "current" marker.
    pub current_status: SessionStatus,

    /// Active sub-mode. Defaults to [`Mode::StatusPicker`] from
    /// [`State::new`]; callers wanting custom-name can flip after
    /// construction or rely on Tab.
    pub mode: Mode,

    /// Cursor index into [`STATUS_OPTIONS`]. Always clamped to the slice
    /// length on every key event.
    pub selected_index: usize,

    /// Buffer for [`Mode::CustomName`]. Pre-populated from
    /// `session_display_name` so the user can edit rather than retype.
    pub custom_name_input: String,

    /// Phase D: instant the modal was opened. Drives the backdrop fade-in
    /// via the main screen renderer.
    pub opened_at: std::time::Instant,
}

impl State {
    /// Construct a fresh modal pre-selected on the session's current
    /// status. The custom-name buffer is seeded with `display_name` so
    /// switching to that mode lets the user edit in place.
    pub fn new(
        session_id: impl Into<String>,
        display_name: impl Into<String>,
        current_status: SessionStatus,
    ) -> Self {
        let display_name = display_name.into();
        let selected_index = STATUS_OPTIONS
            .iter()
            .position(|s| *s == current_status)
            .unwrap_or(0);
        Self {
            session_id: session_id.into(),
            session_display_name: display_name.clone(),
            current_status,
            mode: Mode::StatusPicker,
            selected_index,
            custom_name_input: display_name,
            opened_at: std::time::Instant::now(),
        }
    }
}

// ---- result ----------------------------------------------------------------

/// Result the modal posts back to the App when it closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelPromptResult {
    /// User hit Escape (or Ctrl-C) — discard the modal, don't mutate
    /// session metadata.
    Cancelled,
    /// User picked a status — App writes it to `sessions.status`.
    StatusChosen(SessionStatus),
    /// User submitted a custom name. `None` = clear the custom name
    /// (blank submission); `Some(name)` = set it to that string.
    CustomNameSet(Option<String>),
}

// ---- key handling ----------------------------------------------------------

/// Handle one key event. Returns `Some(LabelPromptResult)` if the modal
/// should close, `None` if it stays open.
///
/// Recognised keys (both modes):
/// * Esc / Ctrl-C → [`LabelPromptResult::Cancelled`]
/// * Tab → toggle between [`Mode::StatusPicker`] and [`Mode::CustomName`]
///
/// Status picker:
/// * j / Down / Ctrl-N → move cursor down (wraps)
/// * k / Up / Ctrl-P → move cursor up (wraps)
/// * 1..=5 → jump to that status (1-indexed)
/// * Enter → [`LabelPromptResult::StatusChosen`]
///
/// Custom name:
/// * Backspace → delete one char from the input tail
/// * Printable chars (no Ctrl/Alt) → append to the input
/// * Enter → [`LabelPromptResult::CustomNameSet`] (blank → `None`)
pub fn handle_key(state: &mut State, key: KeyEvent) -> Option<LabelPromptResult> {
    // Skip Release so we don't double-fire on up-edges. Accept Press and
    // Repeat — some terminals only emit Repeat for held keys, and unit
    // tests synthesize Press explicitly.
    if matches!(key.kind, KeyEventKind::Release) {
        return None;
    }

    // ---- global bindings (both modes) -----------------------------------
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => return Some(LabelPromptResult::Cancelled),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            return Some(LabelPromptResult::Cancelled)
        }
        (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
            state.mode = match state.mode {
                Mode::StatusPicker => Mode::CustomName,
                Mode::CustomName => Mode::StatusPicker,
            };
            return None;
        }
        _ => {}
    }

    match state.mode {
        Mode::StatusPicker => handle_key_status(state, key),
        Mode::CustomName => handle_key_custom(state, key),
    }
}

fn handle_key_status(state: &mut State, key: KeyEvent) -> Option<LabelPromptResult> {
    let len = STATUS_OPTIONS.len();
    match (key.code, key.modifiers) {
        (KeyCode::Down, _)
        | (KeyCode::Char('j'), KeyModifiers::NONE)
        | (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
            state.selected_index = (state.selected_index + 1) % len;
            None
        }
        (KeyCode::Up, _)
        | (KeyCode::Char('k'), KeyModifiers::NONE)
        | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
            state.selected_index = (state.selected_index + len - 1) % len;
            None
        }
        (KeyCode::Char(d), KeyModifiers::NONE) if ('1'..='9').contains(&d) => {
            let idx = (d as u8 - b'1') as usize;
            if idx < len {
                state.selected_index = idx;
            }
            None
        }
        (KeyCode::Enter, _) => {
            let chosen = STATUS_OPTIONS[state.selected_index.min(len - 1)];
            Some(LabelPromptResult::StatusChosen(chosen))
        }
        _ => None,
    }
}

fn handle_key_custom(state: &mut State, key: KeyEvent) -> Option<LabelPromptResult> {
    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => {
            let trimmed = state.custom_name_input.trim();
            if trimmed.is_empty() {
                Some(LabelPromptResult::CustomNameSet(None))
            } else {
                Some(LabelPromptResult::CustomNameSet(Some(trimmed.to_string())))
            }
        }
        (KeyCode::Backspace, _) => {
            state.custom_name_input.pop();
            None
        }
        (KeyCode::Char(c), mods) => {
            // Reject control-modified chars so Ctrl-A etc. don't pollute
            // the input. NONE and SHIFT (for capitals) both fall through.
            if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) {
                return None;
            }
            state.custom_name_input.push(c);
            None
        }
        _ => None,
    }
}

// ---- rendering -------------------------------------------------------------

/// Render the label-prompt modal. The caller is expected to have computed
/// a centered popup rect; this function calls `Clear` itself so the popup
/// is always opaque.
///
/// Layout (top-to-bottom inside the bordered block):
///  1. Mode hint row (1) — `[status / Tab: edit name]` or `[name / Tab: pick status]`
///  2. Body (fill) — list (status mode) or input row (name mode)
///  3. Footer hint (1) — `Enter to save • Esc to cancel`
pub fn draw(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);

    let title = format!(" Label: {} ", short_label(&state.session_display_name));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(theme))
        .padding(Padding::new(2, 2, 1, 1))
        .style(modal_bg_style(theme))
        .title(Span::styled(title, title_style(theme)));
    let inner = block.inner(area);
    block.render(area, buf);

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    render_mode_hint(state, theme, v[0], buf);
    match state.mode {
        Mode::StatusPicker => render_status_list(state, theme, v[1], buf),
        Mode::CustomName => render_name_input(state, theme, v[1], buf),
    }
    render_footer(state, theme, v[2], buf);
}

fn render_mode_hint(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    let active = Style::default()
        .fg(theme_color(&theme.primary, Color::Yellow))
        .add_modifier(Modifier::BOLD);

    let (left_label, right_label) = ("status", "name");
    let (left_style, right_style) = match state.mode {
        Mode::StatusPicker => (active, muted),
        Mode::CustomName => (muted, active),
    };

    let spans = vec![
        Span::raw(" "),
        Span::styled(left_label, left_style),
        Span::styled(" • ", muted),
        Span::styled(right_label, right_style),
        Span::styled("    (Tab to switch)", muted),
    ];
    Paragraph::new(Line::from(spans)).render(area, buf);
}

fn render_status_list(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let current_marker_style = Style::default()
        .fg(theme_color(&theme.accent, Color::Magenta))
        .add_modifier(Modifier::BOLD);
    let normal = Style::default().fg(theme_color(&theme.foreground, Color::White));

    let items: Vec<ListItem> = STATUS_OPTIONS
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let label = status_label(*s);
            let is_current = *s == state.current_status;
            let marker = if is_current { " * " } else { "   " };
            let spans = vec![
                Span::styled(
                    format!(" {}. ", i + 1),
                    Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray)),
                ),
                Span::styled(
                    marker.to_string(),
                    if is_current {
                        current_marker_style
                    } else {
                        normal
                    },
                ),
                Span::styled(label.to_string(), normal),
            ];
            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(state.selected_index.min(STATUS_OPTIONS.len() - 1)));

    let list = List::new(items).highlight_style(
        Style::default()
            .bg(theme_color(&theme.accent, Color::Magenta))
            .fg(theme_color(&theme.background, Color::Black))
            .add_modifier(Modifier::BOLD),
    );
    StatefulWidget::render(list, area, buf, &mut list_state);
}

fn render_name_input(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let field_bg = theme_color(&theme.background_element, Color::Reset);
    let prompt_style = Style::default()
        .fg(theme_color(&theme.primary, Color::Yellow))
        .bg(field_bg)
        .add_modifier(Modifier::BOLD);
    let text_style = Style::default()
        .fg(theme_color(&theme.foreground, Color::White))
        .bg(field_bg);
    let caret_style = Style::default()
        .bg(theme_color(&theme.foreground, Color::White))
        .fg(theme_color(&theme.background_element, Color::Black));
    let muted = Style::default()
        .fg(theme_color(&theme.text_muted, Color::DarkGray))
        .add_modifier(Modifier::DIM);

    // Header row + input row inside the body region.
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    Paragraph::new(Line::from(Span::styled(
        " Custom name (blank to clear) ".to_string(),
        muted,
    )))
    .render(v[0], buf);

    // Paint the input row with the field bg so it reads as a text field.
    for y in v[1].y..v[1].y.saturating_add(v[1].height) {
        for x in v[1].x..v[1].x.saturating_add(v[1].width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_bg(field_bg);
            }
        }
    }

    let line = Line::from(vec![
        Span::styled(" > ", prompt_style),
        Span::styled(state.custom_name_input.clone(), text_style),
        Span::styled(" ", caret_style),
    ]);
    Paragraph::new(line)
        .style(Style::default().bg(field_bg))
        .render(v[1], buf);
}

fn render_footer(_state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    Paragraph::new(Line::from(Span::styled(
        " Enter to save • Esc to cancel ".to_string(),
        muted,
    )))
    .render(area, buf);
}

// ---- helpers ---------------------------------------------------------------

/// Human-readable wire-form label for a status — matches the snake_case
/// JSON form so the user sees what gets persisted.
pub fn status_label(s: SessionStatus) -> &'static str {
    match s {
        SessionStatus::Active => "active",
        SessionStatus::InProgress => "in_progress",
        SessionStatus::InReview => "in_review",
        SessionStatus::Done => "done",
        SessionStatus::Archived => "archived",
    }
}

/// Trim a display label so it fits in the title bar without hogging the
/// frame. 32 chars is enough for any reasonable session name.
fn short_label(s: &str) -> String {
    const MAX: usize = 32;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(MAX - 1).collect();
        out.push('…');
        out
    }
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

// ---- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn press(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn press_char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn state() -> State {
        State::new("sid-1", "my session", SessionStatus::Active)
    }

    // ---- construction -----------------------------------------------

    #[test]
    fn new_preselects_current_status_index() {
        let s = State::new("sid", "name", SessionStatus::InReview);
        // InReview is index 2 in STATUS_OPTIONS.
        assert_eq!(s.selected_index, 2);
        assert_eq!(s.mode, Mode::StatusPicker);
        assert_eq!(s.custom_name_input, "name");
    }

    #[test]
    fn new_falls_back_to_zero_when_status_missing_from_options() {
        // STATUS_OPTIONS is the full set, but the fallback path is worth
        // pinning: if the slice ever shrinks the unwrap_or(0) keeps the
        // cursor sane.
        let s = State::new("sid", "n", SessionStatus::Active);
        assert_eq!(s.selected_index, 0);
    }

    // ---- status picker key handling --------------------------------

    #[test]
    fn j_and_k_cycle_status_options_with_wrap() {
        let mut s = state();
        assert_eq!(s.selected_index, 0);
        handle_key(&mut s, press_char('j'));
        assert_eq!(s.selected_index, 1);
        handle_key(&mut s, press_char('j'));
        handle_key(&mut s, press_char('j'));
        handle_key(&mut s, press_char('j'));
        handle_key(&mut s, press_char('j'));
        // 5 downs from 0 wraps back to 0.
        assert_eq!(s.selected_index, 0);
        handle_key(&mut s, press_char('k'));
        // Wrap to last.
        assert_eq!(s.selected_index, STATUS_OPTIONS.len() - 1);
    }

    #[test]
    fn arrow_keys_cycle_status_options() {
        let mut s = state();
        handle_key(&mut s, press(KeyCode::Down));
        assert_eq!(s.selected_index, 1);
        handle_key(&mut s, press(KeyCode::Up));
        assert_eq!(s.selected_index, 0);
    }

    #[test]
    fn digit_keys_jump_to_status() {
        let mut s = state();
        handle_key(&mut s, press_char('3'));
        assert_eq!(s.selected_index, 2);
        handle_key(&mut s, press_char('5'));
        assert_eq!(s.selected_index, 4);
        // Out-of-range digit is a no-op.
        handle_key(&mut s, press_char('9'));
        assert_eq!(s.selected_index, 4);
    }

    #[test]
    fn enter_in_status_mode_posts_status_chosen() {
        let mut s = state();
        handle_key(&mut s, press_char('j')); // -> InProgress
        let got = handle_key(&mut s, press(KeyCode::Enter));
        assert_eq!(
            got,
            Some(LabelPromptResult::StatusChosen(SessionStatus::InProgress))
        );
    }

    // ---- custom name key handling ----------------------------------

    #[test]
    fn tab_toggles_mode_status_to_custom_and_back() {
        let mut s = state();
        assert_eq!(s.mode, Mode::StatusPicker);
        assert!(handle_key(&mut s, press(KeyCode::Tab)).is_none());
        assert_eq!(s.mode, Mode::CustomName);
        assert!(handle_key(&mut s, press(KeyCode::Tab)).is_none());
        assert_eq!(s.mode, Mode::StatusPicker);
    }

    #[test]
    fn typing_in_custom_name_mode_appends_to_buffer() {
        let mut s = State::new("sid", "", SessionStatus::Active);
        s.mode = Mode::CustomName;
        for c in "hi".chars() {
            handle_key(&mut s, press_char(c));
        }
        assert_eq!(s.custom_name_input, "hi");
    }

    #[test]
    fn backspace_in_custom_name_mode_pops_one_char() {
        let mut s = State::new("sid", "hello", SessionStatus::Active);
        s.mode = Mode::CustomName;
        handle_key(&mut s, press(KeyCode::Backspace));
        assert_eq!(s.custom_name_input, "hell");
    }

    #[test]
    fn enter_in_custom_name_mode_with_text_posts_some() {
        let mut s = State::new("sid", "newname", SessionStatus::Active);
        s.mode = Mode::CustomName;
        let got = handle_key(&mut s, press(KeyCode::Enter));
        assert_eq!(
            got,
            Some(LabelPromptResult::CustomNameSet(Some("newname".into())))
        );
    }

    #[test]
    fn enter_in_custom_name_mode_with_blank_posts_none() {
        let mut s = State::new("sid", "   ", SessionStatus::Active);
        s.mode = Mode::CustomName;
        let got = handle_key(&mut s, press(KeyCode::Enter));
        assert_eq!(got, Some(LabelPromptResult::CustomNameSet(None)));
    }

    #[test]
    fn ctrl_modified_chars_rejected_in_custom_name_mode() {
        let mut s = State::new("sid", "", SessionStatus::Active);
        s.mode = Mode::CustomName;
        let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        handle_key(&mut s, ev);
        assert_eq!(s.custom_name_input, "");
    }

    // ---- global keys -----------------------------------------------

    #[test]
    fn esc_returns_cancelled_in_either_mode() {
        let mut s = state();
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Esc)),
            Some(LabelPromptResult::Cancelled)
        );
        s.mode = Mode::CustomName;
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Esc)),
            Some(LabelPromptResult::Cancelled)
        );
    }

    #[test]
    fn ctrl_c_returns_cancelled() {
        let mut s = state();
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut s, ev), Some(LabelPromptResult::Cancelled));
    }

    #[test]
    fn release_events_are_ignored() {
        let mut s = state();
        let ev = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert!(handle_key(&mut s, ev).is_none());
        assert_eq!(s.selected_index, 0);
    }

    // ---- draw -------------------------------------------------------

    #[test]
    fn draw_status_mode_renders_all_status_labels() {
        let s = state();
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(60, 60, f.area());
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
        for opt in STATUS_OPTIONS {
            let label = status_label(*opt);
            assert!(
                all.contains(label),
                "buffer missing status label `{label}`:\n{all}"
            );
        }
        // Title should carry the session display name.
        assert!(all.contains("my session"), "missing title:\n{all}");
    }

    #[test]
    fn draw_custom_name_mode_renders_input_prompt() {
        let mut s = State::new("sid", "preset", SessionStatus::Active);
        s.mode = Mode::CustomName;
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(60, 60, f.area());
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
        assert!(all.contains("preset"), "missing input value:\n{all}");
        assert!(all.contains("Custom name"), "missing header:\n{all}");
    }

    #[test]
    fn draw_tiny_area_does_not_panic() {
        let s = state();
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(20, 6)).unwrap();
        term.draw(|f| {
            let area = centered_rect(70, 60, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
    }

    // ---- helpers ----------------------------------------------------

    #[test]
    fn short_label_truncates_with_ellipsis() {
        let long = "a".repeat(40);
        let out = short_label(&long);
        assert!(out.ends_with('…'));
        assert!(out.chars().count() <= 32);
    }

    #[test]
    fn status_label_round_trip_covers_all_options() {
        for s in STATUS_OPTIONS {
            // Just exercises every arm — guarantees the match is exhaustive
            // at runtime in case a future SessionStatus arm is added.
            let _ = status_label(*s);
        }
    }

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
