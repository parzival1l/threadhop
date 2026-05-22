//! Bookmark-note prompt: edit the note on an existing bookmark.
//!
//! Single-line text-input modal opened from selection mode via `L` when the
//! cursored message already carries a bookmark. Mirrors the shape of
//! `label_prompt.rs`'s `CustomName` mode (modal frame + input row + footer
//! hint) but keeps a much smaller surface — there is no second sub-mode here.
//!
//! ## API
//!
//! [`State::open_for`] builds the state pre-populated with the bookmark's
//! current note. [`handle_key`] consumes one [`KeyEvent`] and returns
//! `Some(NoteAction)` when the modal should close: [`NoteAction::Save`] with
//! the (possibly empty) trimmed text on Enter, or [`NoteAction::Cancel`] on
//! Esc / Ctrl-C. Char input appends to the buffer at the cursor; Backspace
//! deletes the char before the cursor. The cursor stays at the tail of the
//! buffer today — left/right movement is a future polish item, mirroring
//! Python's single-line input widget which also tail-appends.
//!
//! The App owns the persistence side: on `Save` it calls
//! [`threadhop_core::db::upsert_bookmark`] keyed by `message_uuid`, which
//! updates the existing row in place (preserving its rowid) and stores the
//! note as TEXT or NULL when blank — matching the Python TUI's note semantics.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{block::Padding, Block, BorderType, Borders, Clear, Paragraph, Widget},
};
use threadhop_core::{
    models::BookmarkKind,
    theme::{hex_to_rgb, Theme},
};

// ---- state -----------------------------------------------------------------

/// Modal state — what the modal owns, distinct from App state.
#[derive(Debug, Clone, Default)]
pub struct State {
    /// Bookmark rowid the note is being edited for. Informational; the
    /// upsert path keys off `message_uuid`, but the App carries the id so it
    /// can keep its bookmark caches in sync.
    ///
    /// Unread in the live code-path (persistence uses `message_uuid` + `kind`)
    /// but tests round-trip the field so a future cache-refresh helper has
    /// something to look up by.
    #[allow(dead_code)]
    pub bookmark_id: i64,

    /// Message UUID the bookmark targets. Used by the App's save path to
    /// route the upsert to the correct row (the bookmarks table is keyed by
    /// `message_uuid`, not by rowid, so this is what persistence needs).
    pub message_uuid: String,

    /// Existing bookmark kind. We preserve it across the note edit so a
    /// research-tagged bookmark doesn't silently flip back to plain.
    pub kind: BookmarkKind,

    /// Current edit-buffer contents. Empty string saves a blank note,
    /// which `upsert_bookmark` maps to NULL in the bookmarks table.
    pub text: String,

    /// Caret column inside `text`. Pre-pop kept this as a public field so
    /// future left/right cursor support can land without a struct break.
    /// Today every mutation appends at the tail, so `cursor == text.len()`
    /// after every keystroke.
    pub cursor: usize,
}

impl State {
    /// Build a fresh prompt pre-populated with `initial_note` (or empty when
    /// the bookmark has no note yet). `bookmark_id` is informational; the
    /// caller still has to look it up so it can refresh its caches after
    /// the App writes the upsert.
    pub fn open_for(
        bookmark_id: i64,
        message_uuid: impl Into<String>,
        kind: BookmarkKind,
        initial_note: Option<String>,
    ) -> Self {
        let text = initial_note.unwrap_or_default();
        let cursor = text.chars().count();
        Self {
            bookmark_id,
            message_uuid: message_uuid.into(),
            kind,
            text,
            cursor,
        }
    }

    /// Back-compat helper used by the pre-pop stub tests. New callers should
    /// prefer [`Self::open_for`] which carries the full context.
    #[allow(dead_code)]
    pub fn new(bookmark_id: i64, text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self {
            bookmark_id,
            message_uuid: String::new(),
            kind: BookmarkKind::Bookmark,
            text,
            cursor,
        }
    }
}

// ---- result ----------------------------------------------------------------

/// Result the modal posts back to the App when it closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteAction {
    /// User submitted the note. The string is the trimmed buffer — empty
    /// string means "clear the note" (App stores NULL).
    Save(String),
    /// User hit Escape (or Ctrl-C) — discard the edit.
    Cancel,
}

// ---- key handling ----------------------------------------------------------

/// Handle one key event. Returns `Some(NoteAction)` when the modal should
/// close, `None` otherwise.
///
/// Recognised keys:
/// * Esc / Ctrl-C → [`NoteAction::Cancel`]
/// * Enter → [`NoteAction::Save`] (with the trimmed buffer)
/// * Backspace → pop the trailing char
/// * Printable chars (no Ctrl/Alt) → append to the buffer
pub fn handle_key(state: &mut State, key: KeyEvent) -> Option<NoteAction> {
    if matches!(key.kind, KeyEventKind::Release) {
        return None;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => Some(NoteAction::Cancel),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(NoteAction::Cancel),
        (KeyCode::Enter, _) => Some(NoteAction::Save(state.text.trim().to_string())),
        (KeyCode::Backspace, _) => {
            if state.text.pop().is_some() {
                state.cursor = state.text.chars().count();
            }
            None
        }
        (KeyCode::Char(c), mods) => {
            if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) {
                return None;
            }
            state.text.push(c);
            state.cursor = state.text.chars().count();
            None
        }
        _ => None,
    }
}

// ---- rendering -------------------------------------------------------------

/// Draw the bookmark-note modal. The caller computes a centered rect; this
/// function `Clear`s it so the underlying layout doesn't bleed through.
///
/// Layout (top-to-bottom inside the bordered block):
///   1. Header row (1) — "Note (blank to clear)"
///   2. Input row (1) — `> {text}█`
///   3. Filler (fill) — keeps the modal a comfortable height
///   4. Footer (1) — `Enter to save • Esc to cancel`
pub fn draw(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(theme))
        .padding(Padding::new(2, 2, 1, 1))
        .style(modal_bg_style(theme))
        .title(Span::styled(
            " edit bookmark note ".to_string(),
            title_style(theme),
        ));
    let inner = block.inner(area);
    block.render(area, buf);

    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    render_header(theme, v[0], buf);
    render_input(state, theme, v[1], buf);
    render_footer(theme, v[3], buf);
}

fn render_header(theme: &Theme, area: Rect, buf: &mut Buffer) {
    let muted = Style::default()
        .fg(theme_color(&theme.text_muted, Color::DarkGray))
        .add_modifier(Modifier::DIM);
    Paragraph::new(Line::from(Span::styled(
        " Note (blank to clear) ".to_string(),
        muted,
    )))
    .render(area, buf);
}

fn render_input(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
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

    // Paint the input row with the field bg so it reads as a text field.
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_bg(field_bg);
            }
        }
    }

    let line = Line::from(vec![
        Span::styled(" > ", prompt_style),
        Span::styled(state.text.clone(), text_style),
        Span::styled(" ", caret_style),
    ]);
    Paragraph::new(line)
        .style(Style::default().bg(field_bg))
        .render(area, buf);
}

fn render_footer(theme: &Theme, area: Rect, buf: &mut Buffer) {
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    Paragraph::new(Line::from(Span::styled(
        " Enter to save • Esc to cancel ".to_string(),
        muted,
    )))
    .render(area, buf);
}

// ---- helpers ---------------------------------------------------------------

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

/// Centered popup rect. Exposed so the App can place the modal over the
/// main screen consistently.
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

    #[test]
    fn state_round_trips_fields() {
        let s = State::new(42, "important hop");
        assert_eq!(s.bookmark_id, 42);
        assert_eq!(s.text, "important hop");
        assert_eq!(s.cursor, "important hop".chars().count());
    }

    #[test]
    fn default_state_is_empty() {
        let s = State::default();
        assert_eq!(s.bookmark_id, 0);
        assert!(s.text.is_empty());
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn draw_does_not_panic_on_minimum_rect() {
        let theme = Theme::default_dark();
        let state = State::new(1, "");
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        draw(&state, &theme, area, &mut buf);
    }

    #[test]
    fn open_for_pre_populates_existing_note() {
        let s = State::open_for(
            7,
            "msg-uuid-1",
            BookmarkKind::Research,
            Some("notes go here".into()),
        );
        assert_eq!(s.bookmark_id, 7);
        assert_eq!(s.message_uuid, "msg-uuid-1");
        assert_eq!(s.text, "notes go here");
        assert_eq!(s.cursor, "notes go here".chars().count());
        assert_eq!(s.kind, BookmarkKind::Research);
    }

    #[test]
    fn open_for_with_no_existing_note_starts_empty() {
        let s = State::open_for(1, "u", BookmarkKind::Bookmark, None);
        assert!(s.text.is_empty());
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn handle_key_inserts_chars_into_text() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, None);
        for c in "hi".chars() {
            assert!(handle_key(&mut s, press_char(c)).is_none());
        }
        assert_eq!(s.text, "hi");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn handle_key_enter_returns_save_with_text() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, Some("foo".into()));
        let got = handle_key(&mut s, press(KeyCode::Enter));
        assert_eq!(got, Some(NoteAction::Save("foo".into())));
    }

    #[test]
    fn handle_key_enter_trims_whitespace_around_text() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, Some("  spaced  ".into()));
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Enter)),
            Some(NoteAction::Save("spaced".into()))
        );
    }

    #[test]
    fn handle_key_enter_with_blank_returns_save_empty() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, Some("   ".into()));
        assert_eq!(
            handle_key(&mut s, press(KeyCode::Enter)),
            Some(NoteAction::Save(String::new()))
        );
    }

    #[test]
    fn handle_key_esc_returns_cancel() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, Some("draft".into()));
        assert_eq!(handle_key(&mut s, press(KeyCode::Esc)), Some(NoteAction::Cancel));
        assert_eq!(s.text, "draft");
    }

    #[test]
    fn handle_key_ctrl_c_returns_cancel() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, None);
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut s, ev), Some(NoteAction::Cancel));
    }

    #[test]
    fn handle_key_backspace_pops_one_char() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, Some("hello".into()));
        handle_key(&mut s, press(KeyCode::Backspace));
        assert_eq!(s.text, "hell");
        assert_eq!(s.cursor, 4);
    }

    #[test]
    fn handle_key_backspace_on_empty_text_is_noop() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, None);
        assert!(handle_key(&mut s, press(KeyCode::Backspace)).is_none());
        assert!(s.text.is_empty());
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn handle_key_ctrl_modified_chars_rejected() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, None);
        let ev = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL);
        handle_key(&mut s, ev);
        assert!(s.text.is_empty());
    }

    #[test]
    fn handle_key_release_events_ignored() {
        let mut s = State::open_for(1, "u", BookmarkKind::Bookmark, None);
        let ev = KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert!(handle_key(&mut s, ev).is_none());
        assert!(s.text.is_empty());
    }

    #[test]
    fn draw_renders_title_and_text() {
        let s = State::open_for(
            3,
            "u-3",
            BookmarkKind::Bookmark,
            Some("draft note".into()),
        );
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
        assert!(all.contains("edit bookmark note"), "missing title:\n{all}");
        assert!(all.contains("draft note"), "missing pre-populated text:\n{all}");
        assert!(all.contains("Enter to save"), "missing footer:\n{all}");
    }

    #[test]
    fn draw_tiny_area_does_not_panic() {
        let s = State::open_for(1, "u", BookmarkKind::Bookmark, None);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(24, 6)).unwrap();
        term.draw(|f| {
            let area = centered_rect(80, 80, f.area());
            draw(&s, &theme, area, f.buffer_mut());
        })
        .unwrap();
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
