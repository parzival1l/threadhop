//! Help overlay (Phase 6 task 6.2).
//!
//! Displays the keybindings active in the scope that was on top when help was
//! opened. The overlay is itself a modal — it captures key dispatch so that
//! `?` and `Esc` both close it without leaking through to the underlying
//! screen.
//!
//! Layout: a centered rectangle with a titled border. Inside, one row per
//! binding: `key  label`, grouped by scope (Global first, then the
//! scope the user invoked help from). The widget owns no state besides the
//! "previous scope" — what the App captures in `App::previous_scope` so it
//! can restore the right scope when the overlay closes.

#![allow(dead_code)] // the binary references the public helpers via App;
                     // tests cover the internals.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{block::Padding, Block, BorderType, Borders, Clear, Paragraph, Widget},
};
use threadhop_core::theme::{hex_to_rgb, Theme};

use crate::keys::{self, CommandBinding, Scope};

/// Modal state. The App stores this in `Option<State>` and inspects
/// [`handle_key`]'s return value to close it.
#[derive(Debug, Clone)]
pub struct State {
    /// The scope active when the user pressed `?`. Help shows that scope's
    /// bindings (plus Global), and the App restores it on close.
    pub origin_scope: Scope,
}

impl State {
    pub fn new(origin_scope: Scope) -> Self {
        Self { origin_scope }
    }
}

/// Outcome the modal posts back to the App on close.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpResult {
    /// User dismissed the overlay. The App restores `origin_scope` and clears
    /// the help-state field.
    Closed,
}

/// Handle one key event. Returns `Some(Closed)` when the user wants the
/// overlay to dismiss; `None` while it stays open.
///
/// Recognised keys: `Esc`, `q`, `?`, `Enter`, `Ctrl-C`. Anything else is a
/// no-op so the user can scroll-spam without accidental close.
pub fn handle_key(_state: &mut State, key: KeyEvent) -> Option<HelpResult> {
    if matches!(
        key.kind,
        KeyEventKind::Release
    ) {
        return None;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _)
        | (KeyCode::Char('?'), KeyModifiers::NONE)
        | (KeyCode::Char('?'), KeyModifiers::SHIFT)
        | (KeyCode::Char('q'), KeyModifiers::NONE)
        | (KeyCode::Enter, _) => Some(HelpResult::Closed),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(HelpResult::Closed),
        _ => None,
    }
}

// ---- render -----------------------------------------------------------------

/// Render the help overlay into `area`. The caller (`screens::main`) wipes the
/// background with `Clear` and computes a centered rectangle; this fn just
/// fills it.
pub fn draw(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);

    let border_color = theme_color(&theme.border_active, Color::DarkGray);
    let title_color = theme_color(&theme.accent, Color::Magenta);
    let scope_color = theme_color(&theme.primary, Color::Yellow);
    let key_color = theme_color(&theme.warning, Color::Yellow);
    let label_color = theme_color(&theme.foreground, Color::White);
    let muted = theme_color(&theme.text_muted, Color::Gray);

    let panel_bg = theme_color(&theme.background_panel, Color::Reset);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .padding(Padding::new(2, 2, 1, 1))
        .style(Style::default().bg(panel_bg))
        .title(Span::styled(
            " Help ",
            Style::default()
                .fg(title_color)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    block.render(area, buf);

    // Outer vertical layout: header (2 rows) + body (rest) + footer hint (1).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let header_lines = vec![
        Line::from(vec![Span::styled(
            "Keybindings",
            Style::default()
                .fg(title_color)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled(
            format!("Scope: {}", scope_label(state.origin_scope)),
            Style::default().fg(muted),
        )]),
    ];
    Paragraph::new(header_lines).render(chunks[0], buf);

    // Body: one group per scope (Global + origin). For each group, two columns
    // — key (left, fixed width 16) and label (right).
    let mut lines: Vec<Line<'_>> = Vec::new();
    push_group(
        &mut lines,
        "Global",
        keys::commands_for_scope(Scope::Global),
        scope_color,
        key_color,
        label_color,
    );
    if state.origin_scope != Scope::Global {
        push_group(
            &mut lines,
            scope_label(state.origin_scope),
            keys::commands_for_scope(state.origin_scope),
            scope_color,
            key_color,
            label_color,
        );
    }
    Paragraph::new(lines).render(chunks[1], buf);

    // Footer hint.
    let hint = Line::from(vec![Span::styled(
        "Esc / ? / q  close",
        Style::default().fg(muted),
    )]);
    Paragraph::new(hint).render(chunks[2], buf);
}

fn push_group<'a>(
    out: &mut Vec<Line<'a>>,
    label: &'a str,
    bindings: &'a [CommandBinding],
    _scope_color: Color,
    key_color: Color,
    label_color: Color,
) {
    // Phase E: scope-group section header. Render as dim italic with a
    // 1-row top margin so groups visually separate. Mirrors the Python
    // `css/help.tcss::.help-scope` rule: `color: $text-muted; text-style:
    // italic; margin-top: 1`.
    let muted = Style::default()
        .fg(_scope_color) // already a primary-ish color; OK to keep here
        .add_modifier(Modifier::DIM | Modifier::ITALIC);
    out.push(Line::from(""));
    out.push(Line::from(vec![Span::styled(
        format!("── {label} ──"),
        muted,
    )]));
    // Dedup: many scopes register the same Command under both letter+arrow
    // aliases. Show each label once per scope, keyed on (key_text, label).
    let mut seen: std::collections::HashSet<(String, &'static str)> =
        std::collections::HashSet::new();
    for b in bindings {
        let k = format_key(&b.key);
        let pair = (k.clone(), b.label);
        if !seen.insert(pair) {
            continue;
        }
        let key_cell = format!("  {:<14}", k);
        out.push(Line::from(vec![
            Span::styled(key_cell, Style::default().fg(key_color)),
            Span::styled(b.label.to_string(), Style::default().fg(label_color)),
        ]));
    }
}

/// Human label for a scope — drives the help title and group headings.
fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Global => "Global",
        Scope::MainScreen => "Main Screen",
        Scope::SearchModal => "Search",
        Scope::FindBar => "Find Bar",
        Scope::BookmarkModal | Scope::BookmarkBrowser => "Bookmarks",
        Scope::KanbanModal | Scope::Kanban => "Kanban",
        Scope::HelpOverlay => "Help",
        Scope::LabelPrompt => "Label",
        Scope::Confirm | Scope::ConfirmModal => "Confirm",
        Scope::ConflictViewer => "Conflicts",
        Scope::Selection => "Selection Mode",
    }
}

/// Render a `KeyEvent` as a short human string. Mirrors common conventions
/// (`Ctrl+D`, `Shift+G`, `Esc`, `PgDn`).
fn format_key(ev: &KeyEvent) -> String {
    use crossterm::event::KeyModifiers as M;

    let base = match ev.code {
        KeyCode::Char(c) => {
            if ev.modifiers.contains(M::SHIFT) && c.is_ascii_lowercase() {
                c.to_ascii_uppercase().to_string()
            } else {
                c.to_string()
            }
        }
        KeyCode::Enter => "Enter".into(),
        KeyCode::Esc => "Esc".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::BackTab => "Shift+Tab".into(),
        KeyCode::Up => "↑".into(),
        KeyCode::Down => "↓".into(),
        KeyCode::Left => "←".into(),
        KeyCode::Right => "→".into(),
        KeyCode::PageUp => "PgUp".into(),
        KeyCode::PageDown => "PgDn".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Delete => "Del".into(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    };
    let mut prefix = String::new();
    if ev.modifiers.contains(M::CONTROL) {
        prefix.push_str("Ctrl+");
    }
    if ev.modifiers.contains(M::ALT) {
        prefix.push_str("Alt+");
    }
    // SHIFT alone for letter keys is already encoded by the upper-cased char,
    // so don't double-print "Shift+". For non-character keys (e.g. Shift+Tab),
    // BackTab handles that.
    format!("{prefix}{base}")
}

fn theme_color(hex: &str, fallback: Color) -> Color {
    match hex_to_rgb(hex) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => fallback,
    }
}

/// Centered helper — mirrors the same helper in other modal screens.
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn esc_closes_help() {
        let mut state = State::new(Scope::MainScreen);
        let ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(handle_key(&mut state, ev), Some(HelpResult::Closed));
    }

    #[test]
    fn question_mark_closes_help() {
        let mut state = State::new(Scope::MainScreen);
        let ev = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(handle_key(&mut state, ev), Some(HelpResult::Closed));
    }

    #[test]
    fn unknown_key_keeps_help_open() {
        let mut state = State::new(Scope::MainScreen);
        let ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(handle_key(&mut state, ev).is_none());
    }

    #[test]
    fn ctrl_c_closes_help() {
        let mut state = State::new(Scope::MainScreen);
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut state, ev), Some(HelpResult::Closed));
    }

    #[test]
    fn draw_renders_help_overlay_with_title_and_quit_binding() {
        // Frame-buffer assertion: overlay shows "Help" + Global "quit" hint.
        let state = State::new(Scope::MainScreen);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(80, 30)).unwrap();
        term.draw(|f| {
            let area = centered_rect(80, 80, f.area());
            draw(&state, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut whole = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                whole.push_str(buf[(x, y)].symbol());
            }
            whole.push('\n');
        }
        assert!(whole.contains("Help"), "title missing: {whole}");
        assert!(whole.contains("quit"), "global quit binding missing: {whole}");
        assert!(whole.contains("Main Screen"), "scope label missing: {whole}");
    }

    #[test]
    fn format_key_handles_common_shapes() {
        let ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(format_key(&ev), "q");
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(format_key(&ev), "Ctrl+c");
        let ev = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(format_key(&ev), "G");
        let ev = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(format_key(&ev), "PgDn");
    }
}
