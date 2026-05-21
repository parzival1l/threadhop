//! Scope-aware single-line footer.
//!
//! Mirrors `threadhop_core/tui/widgets/contextual_footer.py`. Pulls the
//! bindings for the current scope from `keys::commands_for_scope` and renders
//! them as a single line of `key  label` pairs separated by a dim `·`.
//!
//! Additional Wave B responsibilities (not in the Python original):
//!   * If `read_only` is true, a `[read-only]` indicator is appended.
//!   * If `status` is set, it **replaces** the binding hints (the Python
//!     version appends it; the plan moves status to footer-priority so the
//!     user always sees the most recent action result).
//!
//! Two render entrypoints exist:
//!   * `ContextualFooterWidget` implements `ratatui::widgets::Widget` so the
//!     screen layer can render via `frame.render_widget(...)`.
//!   * `render(f, area, scope)` is a thin free function matching the
//!     signature used by `screens::main` in the plan (task 2.11). It is a
//!     convenience wrapper that constructs the widget with theme/read-only
//!     defaults and renders it.
//!
//! The widget owns no state — it is constructed per frame.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use threadhop_core::theme::Theme;

use crate::keys::{self, CommandBinding, Scope};

/// Single-line footer rendered at the bottom of a screen.
///
/// Lifetime `'a` borrows `theme` and `status` from `App` — the widget is
/// constructed per frame and dropped after `render`.
pub struct ContextualFooterWidget<'a> {
    pub scope: Scope,
    pub theme: &'a Theme,
    pub read_only: bool,
    pub status: Option<&'a str>,
}

impl<'a> ContextualFooterWidget<'a> {
    pub fn new(scope: Scope, theme: &'a Theme) -> Self {
        Self {
            scope,
            theme,
            read_only: false,
            status: None,
        }
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn status(mut self, status: Option<&'a str>) -> Self {
        self.status = status;
        self
    }

    /// Build the styled `Line` the widget will render. Split out so unit
    /// tests can assert on span shape without going through a `Buffer`.
    fn build_line(&self) -> Line<'static> {
        let key_color = hex_to_color(&self.theme.text_muted).unwrap_or(Color::Gray);
        let label_color = hex_to_color(&self.theme.foreground).unwrap_or(Color::White);
        let dim_color = hex_to_color(&self.theme.text_muted).unwrap_or(Color::DarkGray);

        // Status message takes priority — when something needs the user's
        // attention, the hints can wait. Match the Python widget's `[dim]`
        // styling so the bar doesn't visually shout.
        if let Some(note) = self.status {
            let mut spans = vec![Span::styled(
                format!(" {} ", note),
                Style::default().fg(dim_color).add_modifier(Modifier::DIM),
            )];
            if self.read_only {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    "[read-only]",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                ));
            }
            return Line::from(spans);
        }

        let bindings = collect_bindings(self.scope);

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(bindings.len() * 4 + 2);
        let mut first = true;
        for (key_text, label) in bindings {
            if !first {
                spans.push(Span::styled(
                    "  ·  ",
                    Style::default().fg(dim_color).add_modifier(Modifier::DIM),
                ));
            }
            first = false;
            spans.push(Span::styled(
                format!(" {} ", key_text),
                Style::default()
                    .fg(key_color)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(
                format!(" {}", label),
                Style::default().fg(label_color),
            ));
        }

        if self.read_only {
            if !first {
                spans.push(Span::styled(
                    "  ·  ",
                    Style::default().fg(dim_color).add_modifier(Modifier::DIM),
                ));
            }
            spans.push(Span::styled(
                "[read-only]",
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        }

        Line::from(spans)
    }
}

impl<'a> Widget for ContextualFooterWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let line = self.build_line();
        Paragraph::new(line).render(area, buf);
    }
}

/// Free-function entrypoint matching the signature used by `screens::main`
/// in the plan. Builds the widget with defaults (no status, writable) and
/// renders it into `area`.
///
/// Screens that need to render banner / read-only state should construct
/// `ContextualFooterWidget` directly and pass it to `frame.render_widget`.
#[allow(dead_code)]
pub fn render(f: &mut ratatui::Frame<'_>, area: Rect, scope: Scope) {
    // Theme is required by the widget but the free-function callers in the
    // plan don't have one to pass — fall back to the default dark palette.
    // Real callers go through `ContextualFooterWidget` directly.
    let theme = Theme::default_dark();
    let widget = ContextualFooterWidget::new(scope, &theme);
    f.render_widget(widget, area);
}

/// Walk the registry for `scope`, falling back to Global so the help / quit
/// hints are always visible. Returns `(key_text, label)` pairs in registry
/// order, deduplicated by key string so a key bound in both Global and the
/// active scope appears once.
fn collect_bindings(scope: Scope) -> Vec<(String, &'static str)> {
    let mut out: Vec<(String, &'static str)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    let mut push = |b: &CommandBinding| {
        let key_text = key_event_label(&b.key);
        if seen.iter().any(|k| k == &key_text) {
            return;
        }
        seen.push(key_text.clone());
        out.push((key_text, b.label));
    };

    // Global first so `q` / `?` sit on the left across every scope.
    for b in keys::commands_for_scope(Scope::Global) {
        push(b);
    }
    if scope != Scope::Global {
        for b in keys::commands_for_scope(scope) {
            push(b);
        }
    }
    out
}

/// Format a `KeyEvent` as the short label shown in the footer.
/// `Ctrl+c` collapses to `^C`; bare letters render as-is.
fn key_event_label(ev: &crossterm::event::KeyEvent) -> String {
    use crossterm::event::{KeyCode, KeyModifiers};
    let base = match ev.code {
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::BackTab => "S-Tab".to_string(),
        KeyCode::Backspace => "Bksp".to_string(),
        KeyCode::Left => "←".to_string(),
        KeyCode::Right => "→".to_string(),
        KeyCode::Up => "↑".to_string(),
        KeyCode::Down => "↓".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PgUp".to_string(),
        KeyCode::PageDown => "PgDn".to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    };
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        // Use the conventional `^X` shorthand for ctrl-modified keys so the
        // footer stays compact. Uppercase to match how users type them.
        if let KeyCode::Char(c) = ev.code {
            return format!("^{}", c.to_ascii_uppercase());
        }
        return format!("Ctrl+{base}");
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        return format!("Alt+{base}");
    }
    if ev.modifiers.contains(KeyModifiers::SHIFT) && base.len() == 1 {
        return base.to_ascii_uppercase();
    }
    base
}

/// Parse `#rrggbb` into a `ratatui::style::Color`. Returns `None` for any
/// malformed input — callers fall back to a sensible default.
fn hex_to_color(hex: &str) -> Option<Color> {
    let s = hex.strip_prefix('#').unwrap_or(hex);
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn theme() -> Theme {
        Theme::default_dark()
    }

    #[test]
    fn build_line_includes_global_quit_binding() {
        let t = theme();
        let w = ContextualFooterWidget::new(Scope::MainScreen, &t);
        let line = w.build_line();
        let joined: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(joined.contains("quit"), "expected quit label, got {joined:?}");
        assert!(joined.contains("q"), "expected q key, got {joined:?}");
    }

    #[test]
    fn build_line_dedupes_repeated_keys() {
        // Global registers both `q` and `^C` for Quit — both should appear
        // exactly once and the second `q` (if a scope re-registered it)
        // would be skipped.
        let t = theme();
        let w = ContextualFooterWidget::new(Scope::Global, &t);
        let line = w.build_line();
        let joined: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        // Two distinct keys, two `quit` labels (one per binding).
        let quit_count = joined.matches("quit").count();
        assert_eq!(quit_count, 2, "expected two quit labels, got {joined:?}");
    }

    #[test]
    fn read_only_indicator_appears_when_flag_set() {
        let t = theme();
        let w = ContextualFooterWidget::new(Scope::MainScreen, &t).read_only(true);
        let line = w.build_line();
        let joined: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(joined.contains("[read-only]"), "got {joined:?}");
    }

    #[test]
    fn status_message_replaces_bindings() {
        let t = theme();
        let w = ContextualFooterWidget::new(Scope::MainScreen, &t).status(Some("saved bookmark"));
        let line = w.build_line();
        let joined: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(joined.contains("saved bookmark"), "got {joined:?}");
        // Status takes priority — the binding labels must be gone.
        assert!(!joined.contains("quit"), "status should replace quit hint, got {joined:?}");
    }

    #[test]
    fn status_keeps_read_only_indicator() {
        let t = theme();
        let w = ContextualFooterWidget::new(Scope::MainScreen, &t)
            .status(Some("hello"))
            .read_only(true);
        let line = w.build_line();
        let joined: String = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(joined.contains("hello"));
        assert!(joined.contains("[read-only]"));
    }

    #[test]
    fn hex_to_color_parses_six_digit_hex() {
        assert_eq!(hex_to_color("#ff8800"), Some(Color::Rgb(0xff, 0x88, 0x00)));
        assert_eq!(hex_to_color("aabbcc"), Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(hex_to_color("#zzz"), None);
        assert_eq!(hex_to_color(""), None);
    }

    #[test]
    fn key_event_label_handles_ctrl_and_chars() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(key_event_label(&ev), "^C");
        let ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(key_event_label(&ev), "q");
        let ev = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(key_event_label(&ev), "Esc");
    }

    #[test]
    fn widget_renders_to_buffer_without_panic() {
        // Smoke test: the Widget impl must render through ratatui's Buffer
        // for a realistic terminal size. We check the rendered text contains
        // the global `quit` label.
        let t = theme();
        let mut terminal = Terminal::new(TestBackend::new(80, 3)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 1,
                };
                let w = ContextualFooterWidget::new(Scope::MainScreen, &t);
                f.render_widget(w, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut text = String::new();
        for x in 0..80 {
            text.push_str(buf.cell((x, 0)).unwrap().symbol());
        }
        assert!(text.contains("quit"), "rendered text was {text:?}");
    }

    #[test]
    fn render_free_function_does_not_panic() {
        // Mirrors the call shape in `screens::main` (task 2.11).
        let mut terminal = Terminal::new(TestBackend::new(60, 2)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 60,
                    height: 1,
                };
                render(f, area, Scope::MainScreen);
            })
            .unwrap();
    }
}
