// Wave B widget — wired into `screens::main` in Wave C. Until then the
// binary doesn't reference these helpers, so silence dead-code warnings for
// the module (same convention `app.rs` uses for its Wave A scaffolding
// fields).
#![allow(dead_code)]

//! Transcript pane — the main scrolling view of the selected session.
//!
//! Consumes `threadhop_core::jsonl::CleanedMessage` slices that have already
//! been produced by `jsonl::parse_byte_range` (ADR-003: streaming chunks
//! merged by `message.id`, `<system-reminder>` blocks stripped, `tool_use`
//! blocks abbreviated inline into the assistant body). The widget never
//! re-parses raw JSONL — that loading work belongs to the worker layer
//! (Wave C/E).
//!
//! Per the CLAUDE.md convention, each role gets a colored left gutter glyph
//! (`▌`). Background tints are reserved for bookmark / find hits in later
//! waves; this widget exposes the gutter + role label + body lines now, with
//! `Style` knobs sourced from the existing `threadhop_core::theme::Theme`.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};
use threadhop_core::{
    jsonl::CleanedMessage,
    theme::{hex_to_rgb, Theme},
};

/// Glyph used for the role gutter on every rendered line of a message.
///
/// Kept as a module constant so the unit tests can assert on it without
/// duplicating the literal. Matches the Python widget's left-border styling.
pub const GUTTER_GLYPH: &str = "▌";

/// Borrowed view over the state this widget needs. Constructed per frame by
/// the screen renderer — never owns its data.
///
/// Lifetimes:
/// - `'a` ties the borrowed slice, scroll offset, and theme to the App so we
///   can't accidentally retain them across frames.
pub struct TranscriptWidget<'a> {
    pub messages: &'a [CleanedMessage],
    pub scroll: u16,
    pub theme: &'a Theme,
}

impl<'a> TranscriptWidget<'a> {
    /// Convenience constructor; equivalent to the struct literal but reads
    /// naturally at call sites in `screens::main`.
    pub fn new(messages: &'a [CleanedMessage], scroll: u16, theme: &'a Theme) -> Self {
        Self {
            messages,
            scroll,
            theme,
        }
    }
}

impl<'a> Widget for TranscriptWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines = build_lines(self.messages, self.theme);
        // Empty-state: keep the pane blank rather than dumping an "(empty)"
        // placeholder — Wave C will render a "no session selected" banner in
        // the screen itself, not the widget.
        let paragraph = Paragraph::new(lines)
            .scroll((self.scroll, 0))
            .wrap(Wrap { trim: false });
        paragraph.render(area, buf);
    }
}

// ---- pure builders ---------------------------------------------------------

/// Build the full `Vec<Line>` for a slice of cleaned messages. Public so the
/// screen layer (and tests) can re-use the line list across renders without
/// rebuilding for every frame — a key part of the perf claim in §6 of the
/// design spec.
pub fn build_lines<'a>(messages: &[CleanedMessage], theme: &Theme) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        push_message(&mut lines, msg, theme);
        // Blank separator between messages — but not after the last, so a
        // short transcript doesn't trail dead space at the bottom of the
        // pane. Wave B+ may swap this for a thin rule line.
        if idx + 1 < messages.len() {
            lines.push(Line::from(""));
        }
    }
    lines
}

fn push_message<'a>(out: &mut Vec<Line<'a>>, msg: &CleanedMessage, theme: &Theme) {
    let role_style = style_for_role(&msg.role, theme);
    let muted = muted_style(theme);

    // Header row: gutter + role label + timestamp (if present).
    let mut header_spans: Vec<Span<'a>> = Vec::with_capacity(4);
    header_spans.push(gutter_span(role_style));
    header_spans.push(Span::raw(" "));
    header_spans.push(Span::styled(
        role_label(&msg.role).to_string(),
        role_style.add_modifier(Modifier::BOLD),
    ));
    if let Some(ts) = &msg.timestamp {
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(ts.clone(), muted));
    }
    out.push(Line::from(header_spans));

    // Body rows: split on '\n' so the gutter renders on every wrapped line
    // (ratatui `Wrap { trim: false }` will still soft-wrap long lines, but
    // the gutter only fires on hard newlines — same trade-off as the Python
    // widget's per-line mounting).
    if msg.text.is_empty() {
        out.push(Line::from(vec![gutter_span(role_style)]));
    } else {
        for body_line in msg.text.split('\n') {
            out.push(Line::from(vec![
                gutter_span(role_style),
                Span::raw(" "),
                Span::raw(body_line.to_string()),
            ]));
        }
    }
}

/// Pure helper — exposed for tests. Maps `CleanedMessage.role` (always
/// `"user"` or `"assistant"` per `parse_byte_range`'s contract) to a ratatui
/// `Style` sourced from the theme. Unknown roles fall back to a muted style
/// so future tool / system rows render gracefully without a code change.
pub fn style_for_role(role: &str, theme: &Theme) -> Style {
    let color = match role {
        "user" => theme_color(&theme.primary, Color::Cyan),
        "assistant" => theme_color(&theme.accent, Color::Green),
        // tool / tool_result / anything else — muted gutter so they read as
        // secondary even though `parse_byte_range` doesn't currently emit
        // them as standalone rows.
        _ => theme_color(&theme.text_muted, Color::Gray),
    };
    Style::default().fg(color)
}

fn muted_style(theme: &Theme) -> Style {
    Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray))
}

fn gutter_span<'a>(style: Style) -> Span<'a> {
    Span::styled(GUTTER_GLYPH.to_string(), style)
}

/// Pure helper — exposed for tests. Maps a role string to the user-facing
/// label rendered in the header row.
pub fn role_label(role: &str) -> &'static str {
    match role {
        "user" => "You",
        "assistant" => "Claude",
        "tool" => "Tool",
        "tool_result" => "Tool Result",
        _ => "Message",
    }
}

/// Convert a `#rrggbb` (or `#rgb`) theme color into a ratatui `Color`. Falls
/// back to `fallback` if the theme string is malformed — TUIs should degrade
/// to a recognisable default rather than panic.
fn theme_color(hex: &str, fallback: Color) -> Color {
    match hex_to_rgb(hex) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => fallback,
    }
}

// ---- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn cm(role: &str, text: &str) -> CleanedMessage {
        CleanedMessage {
            uuid: format!("u-{role}-{}", text.len()),
            session_id: Some("s1".into()),
            role: role.into(),
            text: text.into(),
            timestamp: Some("2026-05-21T00:00:00Z".into()),
            cwd: None,
            parent_uuid: None,
            is_sidechain: 0,
            message_id: None,
        }
    }

    #[test]
    fn role_label_covers_known_roles() {
        assert_eq!(role_label("user"), "You");
        assert_eq!(role_label("assistant"), "Claude");
        assert_eq!(role_label("tool"), "Tool");
        assert_eq!(role_label("tool_result"), "Tool Result");
        assert_eq!(role_label("???"), "Message");
    }

    #[test]
    fn style_for_role_differs_per_role() {
        let theme = Theme::default_dark();
        let u = style_for_role("user", &theme);
        let a = style_for_role("assistant", &theme);
        let t = style_for_role("tool", &theme);
        assert_ne!(u.fg, a.fg, "user/assistant must read as different roles");
        assert_ne!(u.fg, t.fg);
    }

    #[test]
    fn theme_color_falls_back_on_garbage() {
        // Sanity: malformed hex yields the supplied fallback, never panics.
        assert_eq!(theme_color("not a hex", Color::Magenta), Color::Magenta);
    }

    #[test]
    fn build_lines_empty_input_yields_no_lines() {
        let theme = Theme::default_dark();
        let lines = build_lines(&[], &theme);
        assert!(lines.is_empty());
    }

    #[test]
    fn build_lines_emits_header_plus_body_lines() {
        let theme = Theme::default_dark();
        let msgs = vec![cm("user", "hello\nworld")];
        let lines = build_lines(&msgs, &theme);
        // 1 header + 2 body lines = 3.
        assert_eq!(lines.len(), 3, "got {lines:?}");
    }

    #[test]
    fn build_lines_separates_consecutive_messages_with_blank() {
        let theme = Theme::default_dark();
        let msgs = vec![cm("user", "hi"), cm("assistant", "yo")];
        let lines = build_lines(&msgs, &theme);
        // header + body + blank + header + body = 5.
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn build_lines_no_trailing_blank() {
        let theme = Theme::default_dark();
        let msgs = vec![cm("user", "hi")];
        let lines = build_lines(&msgs, &theme);
        // Just header + body — no dangling blank separator.
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn build_lines_empty_text_still_emits_gutter() {
        // An empty message body shouldn't make the gutter disappear — the
        // user still needs to see *something* indicating a turn boundary.
        let theme = Theme::default_dark();
        let msgs = vec![cm("assistant", "")];
        let lines = build_lines(&msgs, &theme);
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn widget_renders_into_buffer_without_panicking() {
        let theme = Theme::default_dark();
        let msgs = vec![cm("user", "hi there"), cm("assistant", "yo")];
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        term.draw(|f| {
            let w = TranscriptWidget::new(&msgs, 0, &theme);
            f.render_widget(w, f.area());
        })
        .unwrap();
        // Cell at column 0, row 0 should contain the gutter glyph.
        let buf = term.backend().buffer();
        assert_eq!(buf[(0, 0)].symbol(), GUTTER_GLYPH);
    }

    #[test]
    fn widget_renders_role_label_in_header() {
        let theme = Theme::default_dark();
        let msgs = vec![cm("user", "hi")];
        let mut term = Terminal::new(TestBackend::new(40, 4)).unwrap();
        term.draw(|f| {
            let w = TranscriptWidget::new(&msgs, 0, &theme);
            f.render_widget(w, f.area());
        })
        .unwrap();
        // Collect row 0 as a string and assert the label is present.
        let buf = term.backend().buffer();
        let mut row0 = String::new();
        for x in 0..buf.area().width {
            row0.push_str(buf[(x, 0)].symbol());
        }
        assert!(row0.contains("You"), "header row was: {row0:?}");
    }

    #[test]
    fn widget_scroll_offsets_visible_lines() {
        // With scroll=1 the first header row should fall above the viewport
        // and the first body row should be at the top instead.
        let theme = Theme::default_dark();
        let msgs = vec![cm("user", "alpha\nbeta")];
        let mut term = Terminal::new(TestBackend::new(40, 3)).unwrap();
        term.draw(|f| {
            let w = TranscriptWidget::new(&msgs, 1, &theme);
            f.render_widget(w, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer();
        let mut row0 = String::new();
        for x in 0..buf.area().width {
            row0.push_str(buf[(x, 0)].symbol());
        }
        assert!(
            row0.contains("alpha"),
            "scroll=1 should reveal body row, got {row0:?}"
        );
    }

    #[test]
    fn widget_handles_tiny_buffer() {
        // 1×1 buffer — must not panic or overflow.
        let theme = Theme::default_dark();
        let msgs = vec![cm("user", "x")];
        let mut term = Terminal::new(TestBackend::new(1, 1)).unwrap();
        term.draw(|f| {
            let w = TranscriptWidget::new(&msgs, 0, &theme);
            f.render_widget(w, f.area());
        })
        .unwrap();
    }
}
