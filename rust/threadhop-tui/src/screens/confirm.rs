//! Confirm modal: generic yes/no confirmation prompt.
//!
//! Mirrors `threadhop_core/tui/screens/confirm.py` (a 39-line Textual modal
//! with `y` / `Enter` → yes and `n` / `Esc` → no). Drives destructive actions
//! such as the bookmark-browser delete flow (Phase 4 task 4.4). The caller
//! constructs a [`State`] with a one-line question — and optionally a second
//! line of context via [`State::with_detail`] (for example, the bookmark note
//! the user is about to delete) — and the modal posts back a
//! [`ConfirmResult`].
//!
//! ## Modal-result channel pattern
//!
//! Same shape as `screens::search`: [`handle_key`] returns
//! `Option<ConfirmResult>`. `None` means the modal stays open; `Some(_)`
//! means it closes and the App should react. This file holds no DB handle
//! and no App reference — the App owns the modal in an `Option<State>` and
//! interprets the result.

#![allow(dead_code)] // Wave 2 wires this into the App's bookmark-delete flow;
                     // until then the binary doesn't reference the helpers.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{block::Padding, Block, BorderType, Borders, Clear, Paragraph, Widget},
};
use threadhop_core::theme::{hex_to_rgb, Theme};

// ---- state ------------------------------------------------------------------

/// Modal state — what the modal owns, distinct from App state.
///
/// `question` is the headline rendered prominently; `detail` is an optional
/// muted second line (typically a quoted value such as the bookmark note).
/// The two-field shape lets the caller mirror the Python TUI's title + hint
/// stack without requiring the modal to do any string composition itself.
#[derive(Debug, Clone)]
pub struct State {
    /// The primary question. Should be a short, plain English sentence such
    /// as "Delete bookmark?".
    pub question: String,

    /// Optional context line. Rendered below the question in a muted style
    /// — for example, the quoted bookmark note being deleted.
    pub detail: Option<String>,
}

impl State {
    /// Construct a yes/no prompt with the given question and no detail line.
    pub fn new(question: impl Into<String>) -> Self {
        Self {
            question: question.into(),
            detail: None,
        }
    }

    /// Builder: attach an optional second line of context.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

// ---- result -----------------------------------------------------------------

/// Outcome the modal posts back to the App when it closes.
///
/// Wave 2's App loop calls [`handle_key`] for each keystroke while the modal
/// is open and matches on the returned `Option<ConfirmResult>`. The action
/// being confirmed (delete-bookmark id, archive-session id, etc.) is owned
/// by the App; this enum only carries the user's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmResult {
    /// User accepted — `y` or `Enter`.
    Yes,
    /// User rejected — `n`, `Esc`, or `Ctrl-C`.
    No,
}

// ---- key handling -----------------------------------------------------------

/// Handle one key event. Returns `Some(ConfirmResult)` if the modal should
/// close, `None` if it stays open (currently only unhandled keys leave it
/// open — every recognised key terminates the modal, matching the Python
/// `ConfirmScreen` behaviour where `y/n/Enter/Esc` all dismiss).
///
/// Recognised keys:
/// * `y` / `Y` / `Enter` → [`ConfirmResult::Yes`]
/// * `n` / `N` / `Esc` / `Ctrl-C` → [`ConfirmResult::No`]
///
/// Matches the Python `BINDINGS` table exactly; the extra `Ctrl-C → No`
/// mapping is added for consistency with `screens::search` so the user has
/// the same global "get me out of this modal" shortcut everywhere.
pub fn handle_key(_state: &mut State, key: KeyEvent) -> Option<ConfirmResult> {
    // Accept Press and Repeat — some terminals only send Repeat for held
    // keys, and unit tests synthesize Press. Skip Release so we don't
    // double-fire on up-edges. Matches `screens::search::handle_key`.
    if matches!(key.kind, KeyEventKind::Release) {
        return None;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => Some(ConfirmResult::Yes),
        (KeyCode::Char('y'), m) | (KeyCode::Char('Y'), m)
            if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
        {
            Some(ConfirmResult::Yes)
        }

        (KeyCode::Esc, _) => Some(ConfirmResult::No),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(ConfirmResult::No),
        (KeyCode::Char('n'), m) | (KeyCode::Char('N'), m)
            if !m.contains(KeyModifiers::CONTROL) && !m.contains(KeyModifiers::ALT) =>
        {
            Some(ConfirmResult::No)
        }

        _ => None,
    }
}

// ---- rendering --------------------------------------------------------------

/// Render the confirm modal at the given area. The caller is expected to
/// have computed a centered popup rect (a small one — typically 50%w × ~5h);
/// this function calls `Clear` itself so it always renders an opaque popup
/// over whatever sat below.
///
/// Layout (top-to-bottom, inside the bordered block):
///  1. Question row (height 1) — bold, centered.
///  2. Optional detail row (height 1) — muted, centered.
///  3. Spacer (fills remaining height).
///  4. Hint row (height 1) — `[y] yes  [n] no` centered, muted.
pub fn draw(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    // Opaque background — without Clear, the underlying main-screen content
    // would bleed through any cells we don't explicitly write to.
    Clear.render(area, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(theme))
        .padding(Padding::new(2, 2, 1, 1))
        .style(modal_bg_style(theme))
        .title(Span::styled(" Confirm ", title_style(theme)));
    let inner = block.inner(area);
    block.render(area, buf);

    let has_detail = state.detail.is_some();
    let detail_h: u16 = if has_detail { 1 } else { 0 };

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),        // question
            Constraint::Length(detail_h), // optional detail
            Constraint::Min(0),           // spacer
            Constraint::Length(1),        // hint
        ])
        .split(inner);

    render_centered_line(
        &state.question,
        question_style(theme),
        v[0],
        buf,
    );

    if let Some(detail) = &state.detail {
        render_centered_line(detail, detail_style(theme), v[1], buf);
    }

    render_hint(theme, v[3], buf);
}

/// Render `text` centered horizontally in `area` with `style`. Truncates
/// gracefully if the area is narrower than the text — ratatui's Paragraph
/// handles the clipping for us.
fn render_centered_line(text: &str, style: Style, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let line = Line::from(Span::styled(text.to_string(), style));
    Paragraph::new(line)
        .alignment(ratatui::layout::Alignment::Center)
        .render(area, buf);
}

/// Hint row — `[y] yes   [n] no`, key letters highlighted, labels muted.
/// Matches the Python `confirm-hint` Static (`y/Enter = yes • n/Esc = no`)
/// in spirit; we use bracketed key cues instead of bullet-separator prose
/// because it's the convention the rest of the Rust TUI footer uses.
fn render_hint(theme: &Theme, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let key_style = Style::default()
        .fg(theme_color(&theme.primary, Color::Yellow))
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));

    let spans: Vec<Span<'static>> = vec![
        Span::styled("[y]".to_string(), key_style),
        Span::styled(" yes   ".to_string(), muted),
        Span::styled("[n]".to_string(), key_style),
        Span::styled(" no".to_string(), muted),
    ];
    Paragraph::new(Line::from(spans))
        .alignment(ratatui::layout::Alignment::Center)
        .render(area, buf);
}

// ---- helpers ----------------------------------------------------------------

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

fn question_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme_color(&theme.foreground, Color::White))
        .add_modifier(Modifier::BOLD)
}

fn detail_style(theme: &Theme) -> Style {
    Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray))
}

/// Centered popup rect — exposed so the App can place the modal over the
/// main screen consistently. `pct_w` and `pct_h` are 1..=100. Mirrors
/// `screens::search::centered_rect`; duplicated rather than shared because
/// each modal tends to want its own tuning over time (the search modal is
/// large, the confirm modal is tiny).
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
    use crossterm::event::KeyEventState;
    use ratatui::{backend::TestBackend, Terminal};

    fn press(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn press_char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    // ---- handle_key -------------------------------------------------------

    #[test]
    fn handle_key_y_returns_yes() {
        let mut s = State::new("Delete bookmark?");
        assert_eq!(handle_key(&mut s, press_char('y')), Some(ConfirmResult::Yes));
    }

    #[test]
    fn handle_key_uppercase_y_returns_yes() {
        let mut s = State::new("Delete bookmark?");
        assert_eq!(handle_key(&mut s, press_char('Y')), Some(ConfirmResult::Yes));
    }

    #[test]
    fn handle_key_enter_returns_yes() {
        let mut s = State::new("Delete bookmark?");
        assert_eq!(handle_key(&mut s, press(KeyCode::Enter)), Some(ConfirmResult::Yes));
    }

    #[test]
    fn handle_key_n_returns_no() {
        let mut s = State::new("Delete bookmark?");
        assert_eq!(handle_key(&mut s, press_char('n')), Some(ConfirmResult::No));
    }

    #[test]
    fn handle_key_uppercase_n_returns_no() {
        let mut s = State::new("Delete bookmark?");
        assert_eq!(handle_key(&mut s, press_char('N')), Some(ConfirmResult::No));
    }

    #[test]
    fn handle_key_esc_returns_no() {
        let mut s = State::new("Delete bookmark?");
        assert_eq!(handle_key(&mut s, press(KeyCode::Esc)), Some(ConfirmResult::No));
    }

    #[test]
    fn handle_key_ctrl_c_returns_no() {
        let mut s = State::new("Delete bookmark?");
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut s, ev), Some(ConfirmResult::No));
    }

    #[test]
    fn handle_key_unrecognised_keeps_modal_open() {
        let mut s = State::new("Delete bookmark?");
        assert!(handle_key(&mut s, press_char('x')).is_none());
        assert!(handle_key(&mut s, press(KeyCode::Down)).is_none());
        assert!(handle_key(&mut s, press(KeyCode::Tab)).is_none());
    }

    #[test]
    fn handle_key_release_events_ignored() {
        let mut s = State::new("Delete bookmark?");
        let ev = KeyEvent {
            code: KeyCode::Char('y'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };
        // Release of `y` must NOT confirm — otherwise we'd double-fire on
        // every up-edge.
        assert!(handle_key(&mut s, ev).is_none());
    }

    #[test]
    fn handle_key_repeat_events_accepted() {
        // Held-key repeats should still fire — some terminals only emit
        // Repeat for held keys.
        let mut s = State::new("Delete bookmark?");
        let ev = KeyEvent {
            code: KeyCode::Char('y'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Repeat,
            state: KeyEventState::NONE,
        };
        assert_eq!(handle_key(&mut s, ev), Some(ConfirmResult::Yes));
    }

    #[test]
    fn handle_key_ctrl_y_does_not_confirm() {
        // Stray Ctrl-modified `y` shouldn't accidentally accept.
        let mut s = State::new("Delete bookmark?");
        let ev = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert!(handle_key(&mut s, ev).is_none());
    }

    // ---- builder ---------------------------------------------------------

    #[test]
    fn builder_pattern_with_detail_sets_detail() {
        let s = State::new("Delete bookmark?").with_detail("\"fix the parser\"");
        assert_eq!(s.question, "Delete bookmark?");
        assert_eq!(s.detail.as_deref(), Some("\"fix the parser\""));
    }

    #[test]
    fn new_without_detail_leaves_it_none() {
        let s = State::new("Archive session?");
        assert_eq!(s.question, "Archive session?");
        assert!(s.detail.is_none());
    }

    // ---- draw ------------------------------------------------------------

    #[test]
    fn draw_renders_question_into_frame_buffer() {
        let s = State::new("Delete bookmark?");
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
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
            all.contains("Delete bookmark?"),
            "buffer missing question:\n{all}"
        );
        // Hint row should render the bracketed key cues.
        assert!(all.contains("[y]"), "buffer missing yes hint:\n{all}");
        assert!(all.contains("[n]"), "buffer missing no hint:\n{all}");
        // Title.
        assert!(all.contains("Confirm"), "buffer missing title:\n{all}");
    }

    #[test]
    fn draw_renders_detail_line_when_present() {
        let s = State::new("Delete bookmark?").with_detail("\"fix the parser\"");
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
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
            all.contains("fix the parser"),
            "buffer missing detail line:\n{all}"
        );
    }

    #[test]
    fn draw_tiny_area_does_not_panic() {
        // Modal must degrade gracefully even when the user shrinks the
        // terminal below the modal's preferred size.
        let s = State::new("Delete bookmark?").with_detail("very long detail line");
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        term.draw(|f| {
            let area = centered_rect(70, 60, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
    }

    // ---- centered_rect ---------------------------------------------------

    #[test]
    fn centered_rect_centers_within_parent() {
        let parent = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 50,
        };
        let r = centered_rect(50, 20, parent);
        assert_eq!(r.width, 50);
        assert_eq!(r.height, 10);
        assert_eq!(r.x, 25);
        assert_eq!(r.y, 20);
    }
}
