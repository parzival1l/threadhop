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

    // Body rows: prefer the markdown renderer for prose roles (user /
    // assistant) so the user sees `**bold**`, `# headers`, fenced code, etc.
    // as styled output. Tool / tool_result bodies skip markdown — they're
    // usually structured output (paths, command excerpts) where literal
    // formatting would corrupt the content.
    if msg.text.is_empty() {
        let mut l = Line::from(vec![gutter_span_bg(role_style, row_bg)]);
        if let Some(bg) = row_bg {
            l = l.style(Style::default().bg(bg));
        }
        out.push(l);
        return;
    }
    let use_md = matches!(msg.role.as_str(), "user" | "assistant");
    if use_md {
        let base_text = base_row_style(row_bg);
        let md_lines = md::render(&msg.text, theme, base_text, row_bg);
        for ml in md_lines {
            // Prepend the role gutter to every body row so the colored
            // accent reads all the way down the message — matching the
            // Python `border-left: thick` styling on the message widget,
            // not just on the first row.
            let mut spans: Vec<Span<'a>> = Vec::with_capacity(ml.spans.len() + 2);
            spans.push(gutter_span_bg(role_style, row_bg));
            spans.push(Span::styled(" ", base_row_style(row_bg)));
            spans.extend(ml.spans);
            let mut line = Line::from(spans);
            if let Some(bg) = row_bg {
                line = line.style(Style::default().bg(bg));
            }
            out.push(line);
        }
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
        // Mirror the Python TCSS: `UserMessage { border-left: thick $accent }`
        // and `AssistantMessage { border-left: thick $success }`. The user
        // turn gets the violet/accent gutter, the assistant gets the green
        // success gutter, so the two roles read as different turn boundaries
        // at a glance even without color (the labels also differ).
        "user" => theme_color(&theme.accent, Color::Cyan),
        "assistant" => theme_color(&theme.success, Color::Green),
        // tool / tool_result / anything else — muted gutter so they read as
        // secondary, matching the Python `ToolMessage { border: round
        // $panel-lighten-2 }` recessed surface.
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
        // User turn sits on the elevated panel surface — same elevation as
        // the session-list card, so the user's input reads as "what I said"
        // rather than blending into the canvas.
        "user" => &theme.background_panel,
        // Assistant uses the open canvas. Keeps the chat-like feel and lets
        // long bodies (which dominate the pane) breathe.
        "assistant" => return None,
        // Tool / tool_result / unknown roles recede on the darker element
        // surface — matches the Python `ToolMessage { background: $surface }`
        // styling: secondary content visually retreats.
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

// ---- markdown --------------------------------------------------------------

/// Hand-rolled minimal markdown renderer.
///
/// We intentionally do NOT depend on `pulldown-cmark` or `tui-markdown`. The
/// surface we need is small (bold, italic, inline code, fenced code blocks,
/// h1-h3 headers, bullets, numbered lists, links) and the trade-off is
/// favourable: a focused ~150-line parser that fits the rest of the
/// transcript pipeline (per-line `Vec<Line>` output styled with the role's
/// row background) beats wiring an external renderer's `Text` shape to our
/// row-bg invariant.
///
/// The contract: `render` returns one `Line` per visible row, with `row_bg`
/// applied to the line style so soft-wrapped continuations inherit the same
/// card tint. Spans inside each line carry their own foreground styling
/// (bold/italic/code/etc.) layered on top of `base_text` — which itself
/// already carries `row_bg` so partial spans don't reset the background.
///
/// Limitations (intentional MVP cut):
///   * Nested emphasis is not parsed — `**foo *bar* baz**` is rendered as a
///     single bold span with literal `*bar*` inside. A dedicated test
///     documents this.
///   * No tables, blockquotes, horizontal rules, footnotes, images, or
///     nested lists beyond a single level.
///   * Inline code wins over emphasis: the contents of a backtick span are
///     emitted verbatim even if they look like `**bold**`.
pub mod md {
    use super::{theme_color, Theme};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    /// Render `body` into a `Vec<Line>` styled for the transcript pane.
    ///
    /// * `base_text` — the row's "plain text" style (already carries any
    ///   role-tinted background). Inline spans layer modifiers on top.
    /// * `row_bg` — applied via `Line::style` so soft-wraps inherit the bg.
    pub fn render<'a>(
        body: &str,
        theme: &Theme,
        base_text: Style,
        row_bg: Option<Color>,
    ) -> Vec<Line<'a>> {
        let mut out: Vec<Line<'a>> = Vec::new();
        let mut in_fence = false;
        let code_bg = theme_color(&theme.background_element, Color::DarkGray);
        let code_fg = theme_color(&theme.foreground, Color::White);
        let header_color = theme_color(&theme.accent, Color::Magenta);
        let bullet_color = theme_color(&theme.text_muted, Color::Gray);
        let info_color = theme_color(&theme.info, Color::Blue);

        for raw_line in body.split('\n') {
            // Fence open/close: a line whose trimmed start is ``` toggles
            // the state. We don't bother parsing the language tag.
            let trimmed = raw_line.trim_start();
            if trimmed.starts_with("```") {
                in_fence = !in_fence;
                // Render the fence line itself as an empty code-styled row so
                // the user sees the block boundary visually (rather than the
                // literal backticks bleeding through).
                let span = Span::styled(
                    " ".repeat(raw_line.len().max(1)),
                    base_text.bg(code_bg).fg(code_fg).add_modifier(Modifier::DIM),
                );
                out.push(apply_bg(Line::from(vec![span]), row_bg));
                continue;
            }
            if in_fence {
                // Inside a fence: emit verbatim with the code background.
                let span = Span::styled(
                    raw_line.to_string(),
                    base_text.bg(code_bg).fg(code_fg),
                );
                out.push(apply_bg(Line::from(vec![span]), row_bg));
                continue;
            }

            // Headers (h1..h3): `# foo`, `## foo`, `### foo`. Lower levels
            // collapse to h3 styling (still bold + accent).
            if let Some(rest) = header_body(raw_line) {
                let line = Line::from(vec![Span::styled(
                    rest.to_string(),
                    base_text
                        .fg(header_color)
                        .add_modifier(Modifier::BOLD),
                )]);
                out.push(apply_bg(line, row_bg));
                continue;
            }

            // Bullet: `- ` or `* ` (with optional leading whitespace).
            if let Some((indent, rest)) = bullet_body(raw_line) {
                let mut spans = Vec::with_capacity(4);
                if !indent.is_empty() {
                    spans.push(Span::styled(indent.to_string(), base_text));
                }
                spans.push(Span::styled(
                    "• ".to_string(),
                    base_text.fg(bullet_color).add_modifier(Modifier::BOLD),
                ));
                spans.extend(render_inline(rest, theme, base_text, info_color, code_bg, code_fg));
                out.push(apply_bg(Line::from(spans), row_bg));
                continue;
            }

            // Numbered list: `1. text`, `12. text`, etc.
            if let Some((indent, marker, rest)) = numbered_body(raw_line) {
                let mut spans = Vec::with_capacity(4);
                if !indent.is_empty() {
                    spans.push(Span::styled(indent.to_string(), base_text));
                }
                spans.push(Span::styled(
                    format!("{marker} "),
                    base_text.add_modifier(Modifier::BOLD),
                ));
                spans.extend(render_inline(rest, theme, base_text, info_color, code_bg, code_fg));
                out.push(apply_bg(Line::from(spans), row_bg));
                continue;
            }

            // Plain paragraph line — inline processing only.
            let spans = render_inline(raw_line, theme, base_text, info_color, code_bg, code_fg);
            let line = if spans.is_empty() {
                Line::from(vec![Span::styled(String::new(), base_text)])
            } else {
                Line::from(spans)
            };
            out.push(apply_bg(line, row_bg));
        }

        out
    }

    fn apply_bg<'a>(line: Line<'a>, row_bg: Option<Color>) -> Line<'a> {
        match row_bg {
            Some(bg) => line.style(Style::default().bg(bg)),
            None => line,
        }
    }

    /// If `line` starts with one or more `#` followed by a space, return the
    /// post-prefix body. Up to 3 `#`s recognised; anything deeper falls back
    /// to the h3 path (still styled, since the user clearly meant a heading).
    fn header_body(line: &str) -> Option<&str> {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            return None;
        }
        let mut chars = trimmed.chars();
        let mut hashes = 0;
        for c in chars.by_ref() {
            if c == '#' {
                hashes += 1;
                if hashes > 6 {
                    return None;
                }
            } else if c == ' ' {
                let rest_start = hashes + 1;
                return Some(&trimmed[rest_start..]);
            } else {
                return None;
            }
        }
        None
    }

    fn bullet_body(line: &str) -> Option<(&str, &str)> {
        let leading = line.len() - line.trim_start().len();
        let indent = &line[..leading];
        let after = &line[leading..];
        if let Some(rest) = after.strip_prefix("- ") {
            return Some((indent, rest));
        }
        if let Some(rest) = after.strip_prefix("* ") {
            // Disambiguate from `*italic*` paragraphs: an italic span starts
            // with `*` immediately followed by a non-space char, so the `* `
            // prefix is unambiguous.
            return Some((indent, rest));
        }
        None
    }

    fn numbered_body(line: &str) -> Option<(&str, String, &str)> {
        let leading = line.len() - line.trim_start().len();
        let indent = &line[..leading];
        let after = &line[leading..];
        let bytes = after.as_bytes();
        let mut i = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == 0 || i >= bytes.len() {
            return None;
        }
        if bytes[i] != b'.' {
            return None;
        }
        if i + 1 >= bytes.len() || bytes[i + 1] != b' ' {
            return None;
        }
        let marker = format!("{}.", &after[..i]);
        let rest = &after[i + 2..];
        Some((indent, marker, rest))
    }

    /// Scan a single paragraph fragment for inline markdown. Recognises:
    ///   * `` `code` `` — emitted verbatim with the code background.
    ///   * `**bold**` / `__bold__` — `Modifier::BOLD`.
    ///   * `*italic*` / `_italic_` — `Modifier::ITALIC`.
    ///   * `[label](url)` — `label` underlined in `info` color; url dropped.
    ///
    /// Unclosed delimiters fall back to literal text — robustness over
    /// strictness, since transcript bodies frequently contain stray
    /// asterisks (e.g. shell glob patterns, prose emphasis the user didn't
    /// intend to close).
    fn render_inline<'a>(
        s: &str,
        _theme: &Theme,
        base: Style,
        link_color: Color,
        code_bg: Color,
        code_fg: Color,
    ) -> Vec<Span<'a>> {
        let mut out: Vec<Span<'a>> = Vec::new();
        let bytes = s.as_bytes();
        let mut i = 0;
        let mut buf = String::new();
        while i < bytes.len() {
            let c = bytes[i];
            // Inline code — highest precedence.
            if c == b'`' {
                if let Some(end) = find_byte(bytes, i + 1, b'`') {
                    flush_buf(&mut out, &mut buf, base);
                    let inner = &s[i + 1..end];
                    out.push(Span::styled(
                        inner.to_string(),
                        base.bg(code_bg).fg(code_fg),
                    ));
                    i = end + 1;
                    continue;
                }
            }
            // Bold via `**...**` or `__...__`.
            if (c == b'*' || c == b'_')
                && i + 1 < bytes.len()
                && bytes[i + 1] == c
            {
                let delim = [c, c];
                if let Some(end) = find_subsequence(bytes, i + 2, &delim) {
                    flush_buf(&mut out, &mut buf, base);
                    let inner = &s[i + 2..end];
                    out.push(Span::styled(
                        inner.to_string(),
                        base.add_modifier(Modifier::BOLD),
                    ));
                    i = end + 2;
                    continue;
                }
            }
            // Italic via single `*` or `_`. Don't fire on a bare `*` followed
            // by whitespace (that's almost always literal text).
            if (c == b'*' || c == b'_')
                && i + 1 < bytes.len()
                && bytes[i + 1] != b' '
                && bytes[i + 1] != c
            {
                if let Some(end) = find_byte(bytes, i + 1, c) {
                    let inner = &s[i + 1..end];
                    if !inner.is_empty() && !inner.starts_with(' ') {
                        flush_buf(&mut out, &mut buf, base);
                        out.push(Span::styled(
                            inner.to_string(),
                            base.add_modifier(Modifier::ITALIC),
                        ));
                        i = end + 1;
                        continue;
                    }
                }
            }
            // Link `[label](url)`.
            if c == b'[' {
                if let Some(close_bracket) = find_byte(bytes, i + 1, b']') {
                    if close_bracket + 1 < bytes.len()
                        && bytes[close_bracket + 1] == b'('
                    {
                        if let Some(close_paren) =
                            find_byte(bytes, close_bracket + 2, b')')
                        {
                            flush_buf(&mut out, &mut buf, base);
                            let label = &s[i + 1..close_bracket];
                            out.push(Span::styled(
                                label.to_string(),
                                base.fg(link_color).add_modifier(Modifier::UNDERLINED),
                            ));
                            i = close_paren + 1;
                            continue;
                        }
                    }
                }
            }
            // Default: accumulate into the plain-text buffer.
            buf.push(c as char);
            i += 1;
        }
        flush_buf(&mut out, &mut buf, base);
        out
    }

    fn flush_buf<'a>(out: &mut Vec<Span<'a>>, buf: &mut String, base: Style) {
        if !buf.is_empty() {
            out.push(Span::styled(std::mem::take(buf), base));
        }
    }

    fn find_byte(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
        let mut i = from;
        while i < bytes.len() {
            if bytes[i] == needle {
                return Some(i);
            }
            i += 1;
        }
        None
    }

    fn find_subsequence(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || from + needle.len() > bytes.len() {
            return None;
        }
        let mut i = from;
        while i + needle.len() <= bytes.len() {
            if &bytes[i..i + needle.len()] == needle {
                return Some(i);
            }
            i += 1;
        }
        None
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

    // ---- markdown -------------------------------------------------------

    /// Helper: collect a single rendered span line into (text, modifiers per
    /// span) tuples. Drops bg/fg colors since those vary by theme; the tests
    /// care about modifier presence (bold/italic/underline) and text content.
    fn span_shapes(line: &Line<'_>) -> Vec<(String, Modifier)> {
        line.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style.add_modifier))
            .collect()
    }

    #[test]
    fn md_bold_wraps_simple_text() {
        let theme = Theme::default_dark();
        let lines = md::render("hello **world** end", &theme, Style::default(), None);
        assert_eq!(lines.len(), 1);
        let shapes = span_shapes(&lines[0]);
        // `hello `, `world` (bold), ` end`.
        let bolded: Vec<_> = shapes
            .iter()
            .filter(|(_, m)| m.contains(Modifier::BOLD))
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(bolded, vec!["world"]);
    }

    #[test]
    fn md_italic_in_middle_of_paragraph() {
        let theme = Theme::default_dark();
        let lines = md::render("a *quick* fox", &theme, Style::default(), None);
        let shapes = span_shapes(&lines[0]);
        let italic: Vec<_> = shapes
            .iter()
            .filter(|(_, m)| m.contains(Modifier::ITALIC))
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(italic, vec!["quick"]);
    }

    #[test]
    fn md_inline_code_takes_precedence_over_emphasis() {
        // The contents of an inline-code span should be emitted verbatim,
        // even when they look like emphasis. `**foo**` stays literal inside
        // the backticks.
        let theme = Theme::default_dark();
        let lines = md::render("`**foo**` rest", &theme, Style::default(), None);
        let shapes = span_shapes(&lines[0]);
        let code: Vec<_> = shapes
            .iter()
            .filter(|(t, _)| t == "**foo**")
            .collect();
        assert_eq!(code.len(), 1, "expected verbatim code span, got {shapes:?}");
        // And nothing should be flagged BOLD in this line.
        let bolded: Vec<_> = shapes
            .iter()
            .filter(|(_, m)| m.contains(Modifier::BOLD))
            .collect();
        assert!(bolded.is_empty(), "code body must not be bolded: {shapes:?}");
    }

    #[test]
    fn md_unclosed_bold_falls_back_to_literal() {
        // `**foo` (no closing `**`) → render the asterisks as plain text,
        // not as a broken bold span. Robustness over strictness.
        let theme = Theme::default_dark();
        let lines = md::render("hello **world", &theme, Style::default(), None);
        let shapes = span_shapes(&lines[0]);
        let bolded: Vec<_> = shapes
            .iter()
            .filter(|(_, m)| m.contains(Modifier::BOLD))
            .collect();
        assert!(
            bolded.is_empty(),
            "unclosed ** must not produce a bold span: {shapes:?}"
        );
        let joined: String = shapes.iter().map(|(t, _)| t.as_str()).collect();
        assert!(joined.contains("**world"), "literal text preserved: {joined:?}");
    }

    #[test]
    fn md_code_fence_preserves_content_verbatim() {
        let theme = Theme::default_dark();
        let body = "before\n```\nlet x = **not bold**;\n```\nafter";
        let lines = md::render(body, &theme, Style::default(), None);
        // 5 source lines → 5 rendered lines (fence markers occupy a row each).
        assert_eq!(lines.len(), 5, "got {} lines", lines.len());
        // The code-block content line should contain the raw text including
        // the `**` markers (not turned into a bold span).
        let code_row = &lines[2];
        let joined: String = code_row.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            joined.contains("**not bold**"),
            "verbatim fence content expected, got {joined:?}"
        );
        for span in &code_row.spans {
            assert!(
                !span.style.add_modifier.contains(Modifier::BOLD),
                "fence body must not bold inline `**`: {:?}",
                span
            );
        }
    }

    #[test]
    fn md_header_h1_is_bold() {
        let theme = Theme::default_dark();
        let lines = md::render("# A Header", &theme, Style::default(), None);
        assert_eq!(lines.len(), 1);
        let shapes = span_shapes(&lines[0]);
        assert!(shapes.iter().all(|(_, m)| m.contains(Modifier::BOLD)));
        // The `# ` prefix should be stripped from the rendered text.
        let joined: String = shapes.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(joined, "A Header");
    }

    #[test]
    fn md_bullet_replaces_dash_with_bullet_glyph() {
        let theme = Theme::default_dark();
        let lines = md::render("- one\n- two", &theme, Style::default(), None);
        assert_eq!(lines.len(), 2);
        let joined: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(joined.starts_with("• "), "got {joined:?}");
        assert!(joined.contains("one"));
        // Original `- ` prefix must be gone.
        assert!(!joined.contains("- one"), "raw `-` leaked through: {joined:?}");
    }

    #[test]
    fn md_numbered_list_bolds_number() {
        let theme = Theme::default_dark();
        let lines = md::render("1. first\n2. second", &theme, Style::default(), None);
        assert_eq!(lines.len(), 2);
        let marker_span = &lines[0].spans[0];
        assert_eq!(marker_span.content, "1. ");
        assert!(
            marker_span.style.add_modifier.contains(Modifier::BOLD),
            "marker should be bold: {marker_span:?}"
        );
    }

    #[test]
    fn md_link_drops_url_and_underlines_text() {
        let theme = Theme::default_dark();
        let lines = md::render(
            "see [docs](https://example.com) please",
            &theme,
            Style::default(),
            None,
        );
        let shapes = span_shapes(&lines[0]);
        let underlined: Vec<_> = shapes
            .iter()
            .filter(|(_, m)| m.contains(Modifier::UNDERLINED))
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(underlined, vec!["docs"]);
        // URL must not be rendered.
        let joined: String = shapes.iter().map(|(t, _)| t.as_str()).collect();
        assert!(
            !joined.contains("https://example.com"),
            "url leaked through: {joined:?}"
        );
        assert!(
            !joined.contains("("),
            "link parens leaked through: {joined:?}"
        );
    }

    #[test]
    fn md_empty_body_returns_empty_lines() {
        let theme = Theme::default_dark();
        let lines = md::render("", &theme, Style::default(), None);
        // `"".split('\n')` yields one empty fragment → one (empty) line.
        // The renderer still emits a Line so the cursor math stays simple.
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn md_nested_emphasis_not_supported() {
        // Documented limitation: `**foo *bar* baz**` is parsed as a single
        // bold span containing literal `*bar*`. We choose this so the
        // parser stays a one-pass scan rather than a recursive descent.
        let theme = Theme::default_dark();
        let lines = md::render("**foo *bar* baz**", &theme, Style::default(), None);
        let shapes = span_shapes(&lines[0]);
        // Exactly one bold span, and its body still contains the inner `*bar*`.
        let bold_bodies: Vec<_> = shapes
            .iter()
            .filter(|(_, m)| m.contains(Modifier::BOLD))
            .map(|(t, _)| t.as_str())
            .collect();
        assert_eq!(bold_bodies, vec!["foo *bar* baz"]);
        // No ITALIC modifier was applied — the inner emphasis is literal.
        assert!(
            shapes
                .iter()
                .all(|(_, m)| !m.contains(Modifier::ITALIC)),
            "nested italic was unexpectedly parsed: {shapes:?}"
        );
    }

    #[test]
    fn widget_buffer_contains_bold_modifier_for_markdown_body() {
        // Frame-buffer test: render a message whose body has `**bold**` and
        // assert at least one cell in the body region carries the BOLD
        // modifier. Sanity-checks the wiring from `md::render` through
        // `push_message` into the `Buffer`.
        let theme = Theme::default_dark();
        let msgs = vec![cm("assistant", "say **hi** there")];
        let mut term = Terminal::new(TestBackend::new(40, 4)).unwrap();
        term.draw(|f| {
            let w = TranscriptWidget::new(&msgs, 0, &theme);
            f.render_widget(w, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer();
        // Scan the body row (y=1) for any bold cell whose symbol is one of
        // the letters of `hi`.
        let mut found_bold_h = false;
        for x in 0..buf.area().width {
            let cell = &buf[(x, 1)];
            if cell.symbol() == "h" && cell.modifier.contains(Modifier::BOLD) {
                found_bold_h = true;
                break;
            }
        }
        assert!(
            found_bold_h,
            "expected at least one BOLD `h` cell in the body row of the buffer"
        );
    }

    #[test]
    fn widget_body_keeps_gutter_on_every_line() {
        // Phase 4 Wave 2 deferred item: gutter on every body row, not just
        // hard newlines. With markdown rendering preserving line count 1:1
        // for plain paragraphs, every body row should start with the
        // GUTTER_GLYPH.
        let theme = Theme::default_dark();
        let msgs = vec![cm("user", "line one\nline two\nline three")];
        let mut term = Terminal::new(TestBackend::new(40, 6)).unwrap();
        term.draw(|f| {
            let w = TranscriptWidget::new(&msgs, 0, &theme);
            f.render_widget(w, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer();
        // Rows 0 (header), 1, 2, 3 (body) all start with the gutter.
        for y in 0..4u16 {
            assert_eq!(
                buf[(0, y)].symbol(),
                GUTTER_GLYPH,
                "missing gutter at row {y}"
            );
        }
    }
}
