//! Top-of-screen digest bar for the currently-selected session.
//!
//! Single-line, stateless widget constructed per frame. Shows the session
//! metadata digest: status glyph, session name, last-active age, and the
//! bookmark marker.
//!
//! ADR-029 removed the observation/reflector layer, and with it the
//! observation-fed sections this bar used to carry (open-TODO count,
//! unresolved-conflict count, newest decision, "run threadhop observe"
//! CTA). What survives is the session-metadata digest — the parts that
//! never depended on observation data.
//!
//! Layout (single line, left-aligned):
//!
//! ```text
//!  ● <session_name> · last 2h · ★ bookmarked
//! ```
//!
//! ## Rendering rules
//!
//! * The session name renders in `accent + bold` so row 0 always carries at
//!   least one high-contrast cell (Phase A.5 visibility fix).
//! * The leading status glyph mirrors the sidebar: `◐` working (warning),
//!   `●` active (success), `○` inactive (muted), `·` no context.
//! * `last_active_at` renders as a human age (`Ns / Nm / Nh / Nd`) computed
//!   against `SystemTime::now()` so the bar stays current across refreshes.
//! * The bookmark marker appears only when the session has bookmarks.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use threadhop_core::theme::Theme;

/// Optional sidebar item context fed into the digest bar so the row can
/// always carry something useful — session age + status icon.
#[derive(Debug, Clone, Copy, Default)]
pub struct DigestBarContext {
    /// Last-active timestamp (epoch seconds). Drives a `last 2h` suffix.
    pub last_active_at: Option<f64>,
    /// True when a `claude` process is bound to the session.
    pub is_active: bool,
    /// True when the session is mid-tool-call.
    pub is_working: bool,
}

/// Single-line digest bar.
///
/// Lifetime `'a` borrows everything from the App's frame-local state — the
/// widget is constructed per frame and dropped after `render`.
pub struct DigestBarWidget<'a> {
    /// Theme palette for semantic colors (warning / success / muted / accent).
    pub theme: &'a Theme,
    /// Human-readable session label (e.g. the session's display name). When
    /// `None`, a muted "no session selected" placeholder renders instead.
    pub session_display_name: Option<&'a str>,
    /// Whether the current session has at least one bookmark.
    pub has_bookmarks: bool,
    /// Optional sidebar context (age, active/working flags).
    pub context: Option<DigestBarContext>,
}

impl<'a> DigestBarWidget<'a> {
    /// Build the styled `Line` the widget will render. Split out so unit
    /// tests can assert on span shape without going through a `Buffer`.
    fn build_line(&self) -> Line<'static> {
        self.build_line_with_now(now_epoch_seconds())
    }

    /// Same as [`build_line`] but with an injected `now` so tests can pin
    /// the age formatting.
    fn build_line_with_now(&self, now: f64) -> Line<'static> {
        let muted = hex_to_color(&self.theme.text_muted).unwrap_or(Color::DarkGray);
        let warn = hex_to_color(&self.theme.warning).unwrap_or(Color::Yellow);
        let accent = hex_to_color(&self.theme.accent).unwrap_or(Color::Cyan);
        let success = hex_to_color(&self.theme.success).unwrap_or(Color::Green);

        // Leading status glyph derived from the optional sidebar context.
        // Working > Active > Inactive. Even without context (e.g. no session
        // selected) we still want the bar to start with a visible cue.
        let (glyph, glyph_color) = match self.context {
            Some(ctx) if ctx.is_working => ("◐", warn),
            Some(ctx) if ctx.is_active => ("●", success),
            Some(_) => ("○", muted),
            None => ("·", muted),
        };

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(8);
        spans.push(Span::styled(
            format!(" {glyph} "),
            Style::default().fg(glyph_color).add_modifier(Modifier::BOLD),
        ));

        // Session name in accent + bold so the row always pops regardless of
        // the panel bg's contrast with the canvas (Phase A.5).
        if let Some(name) = self.session_display_name {
            spans.push(Span::styled(
                name.to_string(),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                "no session selected",
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ));
        }

        // Age suffix from context, if available.
        if let Some(ctx) = self.context {
            if let Some(ts) = ctx.last_active_at {
                spans.push(separator(muted));
                spans.push(Span::styled(
                    format!("last {}", format_age(ts, now)),
                    Style::default().fg(muted),
                ));
            }
        }

        if self.has_bookmarks {
            spans.push(separator(muted));
            spans.push(Span::styled(
                "★ bookmarked",
                Style::default().fg(accent),
            ));
        }

        Line::from(spans)
    }
}

impl<'a> Widget for DigestBarWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Give the bar its own panel background so it reads as a distinct
        // status stripe. Falls back to the canvas background when the theme
        // is missing the field.
        let panel_bg = hex_to_color(&self.theme.background_panel);
        // Paint every cell in the row with the panel bg so the background
        // tint extends across the whole row — Paragraph alone only styles
        // cells it writes a glyph into.
        if let Some(bg) = panel_bg {
            for y in area.y..area.y.saturating_add(area.height) {
                for x in area.x..area.x.saturating_add(area.width) {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_bg(bg);
                    }
                }
            }
        }
        let line = self.build_line();
        let mut p = Paragraph::new(line);
        if let Some(bg) = panel_bg {
            p = p.style(Style::default().bg(bg));
        }
        p.render(area, buf);
    }
}

// ---------------------------------------------------------------- helpers

/// Build the standard ` · ` dim separator span.
fn separator(muted: Color) -> Span<'static> {
    Span::styled(
        " · ",
        Style::default().fg(muted).add_modifier(Modifier::DIM),
    )
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

/// Wall-clock seconds since the UNIX epoch. Safe on all hosts we target.
fn now_epoch_seconds() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Format an epoch-seconds timestamp as a human-readable age (`Ns / Nm /
/// Nh / Nd`). Mirrors `widgets::session_list::format_age` — kept local to
/// avoid a cross-widget dep just for one helper.
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

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn theme() -> Theme {
        Theme::default_dark()
    }

    fn joined(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn renders_session_name() {
        let t = theme();
        let w = DigestBarWidget {
            theme: &t,
            session_display_name: Some("my-session"),
            has_bookmarks: false,
            context: None,
        };
        let text = joined(&w.build_line());
        assert!(text.contains("my-session"), "expected name, got {text:?}");
    }

    #[test]
    fn renders_placeholder_without_name() {
        let t = theme();
        let w = DigestBarWidget {
            theme: &t,
            session_display_name: None,
            has_bookmarks: false,
            context: None,
        };
        assert!(joined(&w.build_line()).contains("no session selected"));
    }

    #[test]
    fn observation_sections_are_gone_post_adr_029() {
        // The bar must not advertise the removed observation layer.
        let t = theme();
        let w = DigestBarWidget {
            theme: &t,
            session_display_name: Some("sess"),
            has_bookmarks: false,
            context: None,
        };
        let text = joined(&w.build_line());
        assert!(!text.contains("observation"), "got {text:?}");
        assert!(!text.contains("todo"), "got {text:?}");
        assert!(!text.contains("conflict"), "got {text:?}");
        assert!(!text.contains("threadhop observe"), "got {text:?}");
    }

    #[test]
    fn bookmark_marker_appears_when_flag_set() {
        let t = theme();
        let w = DigestBarWidget {
            theme: &t,
            session_display_name: Some("sess"),
            has_bookmarks: true,
            context: None,
        };
        let text = joined(&w.build_line());
        assert!(text.contains("★ bookmarked"), "got {text:?}");
    }

    #[test]
    fn last_active_age_renders_from_context() {
        let t = theme();
        // 7200s = 2h before "now".
        let now = 10_000.0;
        let w = DigestBarWidget {
            theme: &t,
            session_display_name: Some("sess"),
            has_bookmarks: false,
            context: Some(DigestBarContext {
                last_active_at: Some(now - 7200.0),
                is_active: true,
                is_working: false,
            }),
        };
        let text = joined(&w.build_line_with_now(now));
        assert!(text.contains("last 2h"), "got {text:?}");
    }

    #[test]
    fn status_glyph_prefers_working_over_active() {
        let t = theme();
        let working = DigestBarWidget {
            theme: &t,
            session_display_name: Some("s"),
            has_bookmarks: false,
            context: Some(DigestBarContext {
                last_active_at: None,
                is_active: true,
                is_working: true,
            }),
        };
        assert!(joined(&working.build_line()).contains('◐'));
        let active = DigestBarWidget {
            theme: &t,
            session_display_name: Some("s"),
            has_bookmarks: false,
            context: Some(DigestBarContext {
                last_active_at: None,
                is_active: true,
                is_working: false,
            }),
        };
        assert!(joined(&active.build_line()).contains('●'));
    }

    #[test]
    fn widget_renders_to_buffer_with_expected_text() {
        let t = theme();
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 1,
                };
                let w = DigestBarWidget {
                    theme: &t,
                    session_display_name: Some("my-session"),
                    has_bookmarks: true,
                    context: None,
                };
                f.render_widget(w, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut text = String::new();
        for x in 0..80 {
            text.push_str(buf.cell((x, 0)).unwrap().symbol());
        }
        assert!(text.contains("my-session"), "rendered: {text:?}");
        assert!(text.contains("bookmarked"), "rendered: {text:?}");
    }

    // Phase A.5 visibility: at least one cell in row 0 carries the accent
    // foreground so the row pops regardless of panel-bg luminance.
    #[test]
    fn session_name_emits_accent_fg_cell_on_row_zero() {
        let t = theme();
        let mut terminal = Terminal::new(TestBackend::new(120, 1)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 120,
                    height: 1,
                };
                let w = DigestBarWidget {
                    theme: &t,
                    session_display_name: Some("my-session"),
                    has_bookmarks: false,
                    context: None,
                };
                f.render_widget(w, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let accent = hex_to_color(&t.accent).unwrap();
        let has_accent = (0..buf.area.width)
            .any(|x| buf.cell((x, 0)).map(|c| c.fg == accent).unwrap_or(false));
        assert!(
            has_accent,
            "digest bar row 0 has no accent-color cell — name invisible"
        );
    }

    #[test]
    fn hex_to_color_parses_six_digit_hex() {
        assert_eq!(hex_to_color("#ff8800"), Some(Color::Rgb(0xff, 0x88, 0x00)));
        assert_eq!(hex_to_color("aabbcc"), Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(hex_to_color("#zzz"), None);
        assert_eq!(hex_to_color(""), None);
    }
}
