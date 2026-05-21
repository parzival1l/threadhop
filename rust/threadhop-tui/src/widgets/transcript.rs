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
    /// Optional find-bar state. When set, the renderer splices yellow/black
    /// highlight spans into the body lines for every recorded match, with the
    /// current match rendered bold + reversed. None means "no highlights" —
    /// the widget renders exactly as it did before Wave 2.
    pub find_state: Option<&'a crate::widgets::find_bar::FindState>,
    /// Optional index of the currently-focused message — the one
    /// `ToggleBookmark` will act on. When set, the renderer bolds the
    /// matching message header so the user can see what would be bookmarked.
    pub message_cursor: Option<usize>,
}

impl<'a> TranscriptWidget<'a> {
    /// Convenience constructor; equivalent to the struct literal but reads
    /// naturally at call sites in `screens::main`.
    pub fn new(messages: &'a [CleanedMessage], scroll: u16, theme: &'a Theme) -> Self {
        Self {
            messages,
            scroll,
            theme,
            find_state: None,
            message_cursor: None,
        }
    }

    /// Attach an optional find-bar overlay. Builder-style so `screens::main`
    /// can pipe `.find_state(app.find_state.as_ref())` at the call site.
    pub fn find_state(
        mut self,
        find_state: Option<&'a crate::widgets::find_bar::FindState>,
    ) -> Self {
        self.find_state = find_state;
        self
    }

    /// Attach the message-cursor index (Phase 4 Wave 2). Builder-style for
    /// the same reason `find_state` is.
    pub fn message_cursor(mut self, idx: Option<usize>) -> Self {
        self.message_cursor = idx;
        self
    }
}

impl<'a> Widget for TranscriptWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut lines = match self.find_state {
            Some(fs) if !fs.matches.is_empty() => {
                build_lines_with_highlights(self.messages, self.theme, fs)
            }
            _ => build_lines(self.messages, self.theme),
        };
        // Phase 4 Wave 2: apply the cursor highlight to the header line of
        // the focused message. We compute the line index off the same
        // estimator the App uses to scroll-to-message, so the header lands
        // at the right row regardless of body shape.
        if let (Some(idx), false) =
            (self.message_cursor, self.messages.is_empty())
        {
            let bounded = idx.min(self.messages.len().saturating_sub(1));
            if let Some(msg) = self.messages.get(bounded) {
                if let Some(header_line) =
                    message_to_line_index(self.messages, &msg.uuid)
                {
                    if let Some(line) = lines.get_mut(header_line as usize) {
                        for span in &mut line.spans {
                            span.style = span
                                .style
                                .add_modifier(Modifier::BOLD | Modifier::REVERSED);
                        }
                    }
                }
            }
        }
        // Clamp the requested scroll so that scrolling past the end (notably
        // `G` setting scroll to `u16::MAX`) doesn't blank the pane —
        // `Paragraph::scroll` does NOT clamp on its own. The cap is
        // `lines.len().saturating_sub(1)` rather than `lines.len() - height`
        // because we don't know the post-wrap line count (depends on terminal
        // width). Overshoot for soft-wrapped content is preferable to
        // undershoot — the user can still see the last source line.
        let line_count = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let max_scroll = line_count.saturating_sub(1);
        let clamped = self.scroll.min(max_scroll);
        // Empty-state: keep the pane blank rather than dumping an "(empty)"
        // placeholder — Wave C will render a "no session selected" banner in
        // the screen itself, not the widget.
        let paragraph = Paragraph::new(lines)
            .scroll((clamped, 0))
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

/// Like `build_lines` but splices find-bar highlight spans into each body
/// line. Kept as a separate function (rather than a parameter on
/// `build_lines`) so the hot non-highlighted path stays branch-free.
pub fn build_lines_with_highlights<'a>(
    messages: &[CleanedMessage],
    theme: &Theme,
    find_state: &crate::widgets::find_bar::FindState,
) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        push_message_highlighted(&mut lines, msg, idx, theme, find_state);
        if idx + 1 < messages.len() {
            lines.push(Line::from(""));
        }
    }
    lines
}

fn push_message_highlighted<'a>(
    out: &mut Vec<Line<'a>>,
    msg: &CleanedMessage,
    msg_index: usize,
    theme: &Theme,
    find_state: &crate::widgets::find_bar::FindState,
) {
    let role_style = style_for_role(&msg.role, theme);
    let muted = muted_style(theme);
    let row_bg = role_bg(&msg.role, theme);
    let style_with_bg = |s: Style| -> Style {
        match row_bg {
            Some(bg) => s.bg(bg),
            None => s,
        }
    };

    // Header (identical to push_message — highlights don't apply here).
    let mut header_spans: Vec<Span<'a>> = Vec::with_capacity(4);
    header_spans.push(gutter_span_bg(role_style, row_bg));
    header_spans.push(Span::styled(" ", base_row_style(row_bg)));
    header_spans.push(Span::styled(
        role_label(&msg.role).to_string(),
        style_with_bg(role_style.add_modifier(Modifier::BOLD)),
    ));
    if let Some(ts) = &msg.timestamp {
        header_spans.push(Span::styled("  ", base_row_style(row_bg)));
        header_spans.push(Span::styled(ts.clone(), style_with_bg(muted)));
    }
    let mut header_line = Line::from(header_spans);
    if let Some(bg) = row_bg {
        header_line = header_line.style(Style::default().bg(bg));
    }
    out.push(header_line);

    // Pre-collect match byte ranges for this message, in document order.
    let matches: Vec<(usize, usize)> = find_state
        .matches
        .iter()
        .filter(|m| m.message_index == msg_index)
        .map(|m| (m.char_offset, m.char_offset + m.length))
        .collect();
    let current = find_state.current_match();

    if msg.text.is_empty() {
        let mut l = Line::from(vec![gutter_span_bg(role_style, row_bg)]);
        if let Some(bg) = row_bg {
            l = l.style(Style::default().bg(bg));
        }
        out.push(l);
        return;
    }

    // Walk byte offsets per body line so highlight ranges land on the right
    // line. CleanedMessage.text is a single String split on '\n'.
    let mut line_start: usize = 0;
    for (line_no, body_line) in msg.text.split('\n').enumerate() {
        let line_end = line_start + body_line.len();
        let mut spans: Vec<Span<'a>> = vec![
            gutter_span_bg(role_style, row_bg),
            Span::styled(" ", base_row_style(row_bg)),
        ];
        // Find matches that intersect [line_start, line_end].
        let mut cursor = line_start;
        for (ms, me) in matches.iter().copied() {
            if me <= line_start || ms >= line_end {
                continue;
            }
            let clamped_start = ms.max(line_start);
            let clamped_end = me.min(line_end);
            // Pre-match plain text.
            if cursor < clamped_start {
                let s = &msg.text[cursor..clamped_start];
                spans.push(Span::styled(s.to_string(), base_row_style(row_bg)));
            }
            // Match span. Bold + reversed if it's the current match
            // (matched on absolute byte offset + message index).
            let is_current = current
                .map(|c| {
                    c.message_index == msg_index
                        && c.char_offset == ms
                        && c.length == me - ms
                })
                .unwrap_or(false);
            // Phase 6: route the match highlight through the theme — warning
            // for the background (yellow-equivalent across opencode variants),
            // background for the fg so the text stays legible. Falls back to
            // Yellow/Black for terminals that can't parse the hex.
            let mut style = Style::default()
                .bg(theme_color(&theme.warning, Color::Yellow))
                .fg(theme_color(&theme.background, Color::Black));
            if is_current {
                style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
            }
            let s = &msg.text[clamped_start..clamped_end];
            spans.push(Span::styled(s.to_string(), style));
            cursor = clamped_end;
        }
        // Trailing plain text after the last match on this line.
        if cursor < line_end {
            let s = &msg.text[cursor..line_end];
            spans.push(Span::styled(s.to_string(), base_row_style(row_bg)));
        }
        let mut line = Line::from(spans);
        if let Some(bg) = row_bg {
            line = line.style(Style::default().bg(bg));
        }
        out.push(line);
        // +1 for the consumed '\n', except after the last fragment.
        line_start = line_end + 1;
        let _ = line_no; // suppress unused on debug builds
    }
}

fn push_message<'a>(out: &mut Vec<Line<'a>>, msg: &CleanedMessage, theme: &Theme) {
    let role_style = style_for_role(&msg.role, theme);
    let muted = muted_style(theme);
    let row_bg = role_bg(&msg.role, theme);
    let style_with_bg = |s: Style| -> Style {
        match row_bg {
            Some(bg) => s.bg(bg),
            None => s,
        }
    };

    // Header row: gutter + role label + timestamp (if present).
    let mut header_spans: Vec<Span<'a>> = Vec::with_capacity(4);
    header_spans.push(gutter_span_bg(role_style, row_bg));
    header_spans.push(Span::styled(" ", base_row_style(row_bg)));
    header_spans.push(Span::styled(
        role_label(&msg.role).to_string(),
        style_with_bg(role_style.add_modifier(Modifier::BOLD)),
    ));
    if let Some(ts) = &msg.timestamp {
        header_spans.push(Span::styled("  ", base_row_style(row_bg)));
        header_spans.push(Span::styled(ts.clone(), style_with_bg(muted)));
    }
    let mut header_line = Line::from(header_spans);
    if let Some(bg) = row_bg {
        header_line = header_line.style(Style::default().bg(bg));
    }
    out.push(header_line);

    // Body rows: split on '\n' so the gutter renders on every wrapped line
    // (ratatui `Wrap { trim: false }` will still soft-wrap long lines, but
    // the gutter only fires on hard newlines — same trade-off as the Python
    // widget's per-line mounting).
    if msg.text.is_empty() {
        let mut l = Line::from(vec![gutter_span_bg(role_style, row_bg)]);
        if let Some(bg) = row_bg {
            l = l.style(Style::default().bg(bg));
        }
        out.push(l);
    } else {
        for body_line in msg.text.split('\n') {
            let mut line = Line::from(vec![
                gutter_span_bg(role_style, row_bg),
                Span::styled(" ", base_row_style(row_bg)),
                Span::styled(body_line.to_string(), base_row_style(row_bg)),
            ]);
            if let Some(bg) = row_bg {
                line = line.style(Style::default().bg(bg));
            }
            out.push(line);
        }
    }
}

/// Estimate the rendered line index for the message at `target_index`. Used
/// by the App's pending-jump resolution path (Phase 3 Wave 2) to translate a
/// message index into a `scroll` value the transcript renderer can consume.
///
/// The estimate sums:
///   * 1 header row per message,
///   * `max(1, text.split('\n').count())` body rows per message,
///   * 1 blank separator between messages (so we don't count it after the
///     last message — matches what `build_lines` emits).
///
/// Soft-wraps from `Wrap { trim: false }` aren't counted — the renderer will
/// clamp `u16::MAX` to the last line on overshoot, and undershooting by a
/// soft-wrap is preferable to the alternative of needing the terminal width
/// at the time we compute the scroll target.
pub fn message_to_line_index(messages: &[CleanedMessage], uuid: &str) -> Option<u16> {
    let mut acc: u32 = 0;
    for (idx, msg) in messages.iter().enumerate() {
        if msg.uuid == uuid {
            return Some(acc.min(u16::MAX as u32) as u16);
        }
        // 1 header + N body rows (1 for empty text, else line count).
        let body_rows = if msg.text.is_empty() {
            1
        } else {
            msg.text.split('\n').count() as u32
        };
        acc += 1 + body_rows;
        // Blank separator between messages.
        if idx + 1 < messages.len() {
            acc += 1;
        }
    }
    None
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

/// Gutter span variant that paints the role-tinted row background as well as
/// the foreground accent. When `bg` is `None` (no tint configured for the
/// role) we fall back to the unaltered foreground-only span.
fn gutter_span_bg<'a>(style: Style, bg: Option<Color>) -> Span<'a> {
    let s = match bg {
        Some(b) => style.bg(b),
        None => style,
    };
    Span::styled(GUTTER_GLYPH.to_string(), s)
}

/// Base style for a row's plain text — carries only the row's background
/// tint (if any) so paragraphs land on the same card surface as the gutter
/// and header. Foreground is left at default so wrapping respects the user's
/// terminal palette.
fn base_row_style(bg: Option<Color>) -> Style {
    match bg {
        Some(b) => Style::default().bg(b),
        None => Style::default(),
    }
}

/// Per-role background tint. `None` means "no tint — let the terminal bg
/// show through." Mirrors the Python TCSS:
///   * user → `background_panel` (one step lighter than the canvas, reads
///     as a card)
///   * assistant → no tint (the open canvas — keeps the conversation feel)
///   * tool/other → `background_element` (darker, dimmer secondary surface)
pub fn role_bg(role: &str, theme: &Theme) -> Option<Color> {
    let hex = match role {
        "user" => &theme.background_panel,
        "assistant" => return None,
        _ => &theme.background_element,
    };
    hex_to_rgb(hex).map(|(r, g, b)| Color::Rgb(r, g, b))
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
    fn widget_clamps_oversized_scroll_so_pane_does_not_blank() {
        // Regression for "transcript stuck on first page": when `G` set
        // `scroll = u16::MAX` the unclamped `Paragraph::scroll` would render
        // nothing — the user saw a blank pane and read it as "nothing
        // happened." The widget must clamp so the last source line stays
        // visible.
        let theme = Theme::default_dark();
        let msgs = vec![cm("user", "alpha"), cm("assistant", "omega")];
        let mut term = Terminal::new(TestBackend::new(40, 6)).unwrap();
        term.draw(|f| {
            let w = TranscriptWidget::new(&msgs, u16::MAX, &theme);
            f.render_widget(w, f.area());
        })
        .unwrap();
        // Collect the buffer; *something* from the message bodies must render.
        let buf = term.backend().buffer();
        let mut whole = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                whole.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(
            whole.contains("alpha") || whole.contains("omega"),
            "scroll=u16::MAX must not blank the pane, got {whole:?}"
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
