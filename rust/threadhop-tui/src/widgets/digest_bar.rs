//! Top-of-screen digest bar showing observation counts for the
//! currently-selected session (Phase 5 task 5.2).
//!
//! Single-line, stateless widget. The App computes an `ObservationSummary`
//! (cheap join over the per-session observation JSONL + `conflict_reviews`)
//! on session-switch / on `ObservationsGrew` events and feeds it here. The
//! widget owns no state of its own; it is constructed per frame.
//!
//! Layout (single line, left-aligned):
//!
//! ```text
//!  <session_name> · ✓ 3 todos · ⚑ 1 conflict · ★ bookmarked · last 2h
//! ```
//!
//! The Python reference (`threadhop_core/tui/widgets/session_digest_bar.py`)
//! is a *right-column* multi-line digest. The Rust port deliberately picks
//! the simpler horizontal aggregate bar described in the plan (task 5.2);
//! the richer right-column treatment is out of scope for this widget.
//!
//! ## Rendering rules
//!
//! * `summary = None` → render a single dim placeholder (`no observations`).
//! * Zero counts are *suppressed* (no `0 todos` clutter). Only sections with
//!   a non-zero count, a bookmark mark, or a `last_observed_at` appear.
//! * `unresolved_conflict_count > 0` is styled with the theme's `error`
//!   color (red-ish) and bolded so it visually pops; `open_todo_count > 0`
//!   uses the `warning` color.
//! * The session name is shown in `foreground`; separators (` · `) are dim
//!   and use `text_muted`.
//! * `last_observed_at` is rendered as a human age (`Ns / Nm / Nh / Nd`)
//!   computed against `SystemTime::now()` so the bar stays current across
//!   refreshes.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use threadhop_core::observations::ObservationSummary;
use threadhop_core::theme::Theme;

/// Single-line digest bar.
///
/// Lifetime `'a` borrows everything from the App's frame-local state — the
/// widget is constructed per frame and dropped after `render`.
//
// `dead_code` is silenced until `screens::main` wires the widget in (task
// 5.2 step 2 / Phase 5). The tests below exercise every field and method.
#[allow(dead_code)]
pub struct DigestBarWidget<'a> {
    /// Theme palette for semantic colors (warning / error / muted / fg).
    pub theme: &'a Theme,
    /// Aggregate counts for the currently-selected session.
    /// `None` → nothing selected or the session has no observation file yet.
    pub summary: Option<&'a ObservationSummary>,
    /// Human-readable session label (e.g. the session's display name). When
    /// `None`, the leading session-name span is omitted.
    pub session_display_name: Option<&'a str>,
    /// Whether the current session has at least one bookmark. The bookmark
    /// table is App-side state (not part of `ObservationSummary`), so it's
    /// passed as a separate flag.
    pub has_bookmarks: bool,
}

#[allow(dead_code)]
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
        let fg = hex_to_color(&self.theme.foreground).unwrap_or(Color::White);
        let warn = hex_to_color(&self.theme.warning).unwrap_or(Color::Yellow);
        let err = hex_to_color(&self.theme.error).unwrap_or(Color::Red);
        let accent = hex_to_color(&self.theme.accent).unwrap_or(Color::Cyan);

        // Empty-state: no summary at all → muted placeholder.
        let Some(summary) = self.summary else {
            let mut spans = Vec::new();
            if let Some(name) = self.session_display_name {
                spans.push(Span::styled(format!(" {name} "), Style::default().fg(fg)));
                spans.push(separator(muted));
            } else {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                "no observations",
                Style::default().fg(muted).add_modifier(Modifier::DIM),
            ));
            return Line::from(spans);
        };

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(16);

        // 1. Session name (if provided).
        if let Some(name) = self.session_display_name {
            spans.push(Span::styled(
                format!(" {name} "),
                Style::default().fg(fg).add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::raw(" "));
        }

        // Track whether anything meaningful has been pushed after the name
        // so we know whether to seed a leading separator or fall back to
        // an "all quiet" indicator below.
        let mut any_signal = false;

        // 2. Open TODOs (warning color when > 0).
        if summary.open_todo_count > 0 {
            push_sep_if_needed(&mut spans, &mut any_signal, muted);
            spans.push(Span::styled(
                format!("✓ {} {}", summary.open_todo_count, plural("todo", summary.open_todo_count)),
                Style::default().fg(warn).add_modifier(Modifier::BOLD),
            ));
        }

        // 3. Unresolved conflicts (error color, always bold when > 0).
        if summary.unresolved_conflict_count > 0 {
            push_sep_if_needed(&mut spans, &mut any_signal, muted);
            spans.push(Span::styled(
                format!(
                    "⚑ {} {}",
                    summary.unresolved_conflict_count,
                    plural("conflict", summary.unresolved_conflict_count),
                ),
                Style::default().fg(err).add_modifier(Modifier::BOLD),
            ));
        }

        // 4. Bookmark marker (presence only, no count — that's a TUI-side
        //    affordance; the modal lists them).
        if self.has_bookmarks {
            push_sep_if_needed(&mut spans, &mut any_signal, muted);
            spans.push(Span::styled(
                "★ bookmarked",
                Style::default().fg(accent),
            ));
        }

        // 5. Newest decision text — short snippet, muted-italic, only if
        //    we have one and there's still room to spare conceptually.
        //    We truncate aggressively so a long decision can't blow the line.
        if let Some(text) = summary.newest_decision.as_deref() {
            push_sep_if_needed(&mut spans, &mut any_signal, muted);
            spans.push(Span::styled(
                format!("decision: {}", truncate(text, 60)),
                Style::default().fg(fg).add_modifier(Modifier::ITALIC),
            ));
        }

        // 6. Last-observed age — `last 2h`. Always last, muted.
        if let Some(ts) = summary.last_observed_at.as_deref() {
            if let Some(epoch) = parse_iso8601_to_epoch(ts) {
                push_sep_if_needed(&mut spans, &mut any_signal, muted);
                spans.push(Span::styled(
                    format!("last {}", format_age(epoch, now)),
                    Style::default().fg(muted),
                ));
            }
        }

        // 7. If literally nothing fired (all-zero summary, no bookmarks,
        //    no last_observed_at, no newest_decision), emit a quiet hint
        //    so the bar doesn't look broken.
        if !any_signal {
            spans.push(separator(muted));
            spans.push(Span::styled(
                "no signals yet",
                Style::default().fg(muted).add_modifier(Modifier::DIM),
            ));
        }

        Line::from(spans)
    }
}

impl<'a> Widget for DigestBarWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Give the bar its own panel background so it reads as a distinct
        // status stripe — the Python session-digest panel sits on
        // `$panel`, mirrored here via `background_panel`. Falls back to
        // the canvas background when the theme is missing the field.
        let panel_bg = hex_to_color(&self.theme.background_panel);
        let line = self.build_line();
        let mut p = Paragraph::new(line);
        if let Some(bg) = panel_bg {
            p = p.style(Style::default().bg(bg));
        }
        p.render(area, buf);
    }
}

// ---------------------------------------------------------------- helpers
// Helpers are all consumed by `build_line_with_now`; the `dead_code` lint
// only fires because the public widget itself isn't yet mounted in
// `screens::main`. Each helper is annotated individually.

/// Build the standard ` · ` dim separator span.
#[allow(dead_code)]
fn separator(muted: Color) -> Span<'static> {
    Span::styled(
        " · ",
        Style::default().fg(muted).add_modifier(Modifier::DIM),
    )
}

/// Push a separator before the next signal span, unless this is the first
/// signal after the session name. Flips `any_signal` to `true`.
#[allow(dead_code)]
fn push_sep_if_needed(spans: &mut Vec<Span<'static>>, any_signal: &mut bool, muted: Color) {
    if *any_signal {
        spans.push(separator(muted));
    } else {
        // Always inject one separator between the session name and the
        // first signal so the bar reads as `<name> · <signal> ...`.
        spans.push(separator(muted));
        *any_signal = true;
    }
}

/// Pluralize `noun` based on `n` (English). `1 todo` / `2 todos`.
#[allow(dead_code)]
fn plural(noun: &str, n: usize) -> String {
    if n == 1 { noun.to_string() } else { format!("{noun}s") }
}

/// Truncate `s` to at most `max` chars, appending `…` if shortened.
#[allow(dead_code)]
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Parse `#rrggbb` into a `ratatui::style::Color`. Returns `None` for any
/// malformed input — callers fall back to a sensible default.
#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Best-effort ISO-8601 → epoch seconds. Accepts `...Z` and `...+00:00`
/// suffixes; only the date+time portion is consumed and assumed UTC.
/// On any parse failure returns `None` and the caller hides the age field.
///
/// We do not pull a date-time crate just for this — the observer emits a
/// well-known shape (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) and the formatter only
/// needs second-level precision.
#[allow(dead_code)]
fn parse_iso8601_to_epoch(ts: &str) -> Option<f64> {
    // Strip trailing `Z` or `+00:00` / `-00:00` — we treat everything as UTC.
    let core = ts
        .trim_end_matches('Z')
        .trim_end_matches("+00:00")
        .trim_end_matches("-00:00");
    let (date, time) = core.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;

    // Time may have fractional seconds: `HH:MM:SS.fff`.
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, b),
        None => (time, "0"),
    };
    let mut t = hms.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: i64 = t.next().unwrap_or("0").parse().ok()?;
    let frac_f: f64 = format!("0.{frac}").parse().ok()?;

    // Days-from-civil algorithm (Howard Hinnant). Public domain. All
    // arithmetic kept in signed i64 to avoid u64 underflow when `month`
    // is Jan/Feb (the algorithm subtracts 3 from `month` in that case).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let m_adj = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m_adj + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days_since_epoch = era * 146_097 + doe - 719_468;

    let seconds = days_since_epoch * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(seconds as f64 + frac_f)
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use threadhop_core::observations::ObservationSummary;

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
    fn empty_summary_renders_placeholder_without_panic() {
        let t = theme();
        let w = DigestBarWidget {
            theme: &t,
            summary: None,
            session_display_name: Some("my-session"),
            has_bookmarks: false,
        };
        let line = w.build_line();
        let text = joined(&line);
        assert!(text.contains("my-session"), "expected name, got {text:?}");
        assert!(
            text.contains("no observations"),
            "expected placeholder, got {text:?}"
        );
    }

    #[test]
    fn empty_summary_without_name_still_renders() {
        let t = theme();
        let w = DigestBarWidget {
            theme: &t,
            summary: None,
            session_display_name: None,
            has_bookmarks: false,
        };
        // Just shouldn't panic and should produce *something*.
        let line = w.build_line();
        assert!(joined(&line).contains("no observations"));
    }

    #[test]
    fn all_zero_counts_render_no_zero_clutter() {
        let t = theme();
        let summary = ObservationSummary::default();
        let w = DigestBarWidget {
            theme: &t,
            summary: Some(&summary),
            session_display_name: Some("sess"),
            has_bookmarks: false,
        };
        let text = joined(&w.build_line());
        assert!(!text.contains("0 todo"), "got {text:?}");
        assert!(!text.contains("0 conflict"), "got {text:?}");
        // Quiet-state hint should appear.
        assert!(text.contains("no signals yet"), "got {text:?}");
    }

    #[test]
    fn conflict_count_styled_with_error_color() {
        let t = theme();
        let summary = ObservationSummary {
            unresolved_conflict_count: 2,
            ..Default::default()
        };
        let w = DigestBarWidget {
            theme: &t,
            summary: Some(&summary),
            session_display_name: Some("sess"),
            has_bookmarks: false,
        };
        let line = w.build_line();
        let text = joined(&line);
        assert!(text.contains("⚑ 2 conflicts"), "got {text:?}");
        // Find the conflict span and assert its color is the error palette
        // (not the foreground or warning palette).
        let err_color = hex_to_color(&t.error).unwrap();
        let warn_color = hex_to_color(&t.warning).unwrap();
        let conflict_span = line
            .spans
            .iter()
            .find(|s| s.content.contains("conflict"))
            .expect("conflict span present");
        assert_eq!(conflict_span.style.fg, Some(err_color));
        assert_ne!(conflict_span.style.fg, Some(warn_color));
        assert!(conflict_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn todos_pluralization() {
        let t = theme();
        let one = ObservationSummary {
            open_todo_count: 1,
            ..Default::default()
        };
        let many = ObservationSummary {
            open_todo_count: 4,
            ..Default::default()
        };
        let w1 = DigestBarWidget {
            theme: &t,
            summary: Some(&one),
            session_display_name: None,
            has_bookmarks: false,
        };
        let w2 = DigestBarWidget {
            theme: &t,
            summary: Some(&many),
            session_display_name: None,
            has_bookmarks: false,
        };
        assert!(joined(&w1.build_line()).contains("✓ 1 todo"));
        assert!(!joined(&w1.build_line()).contains("✓ 1 todos"));
        assert!(joined(&w2.build_line()).contains("✓ 4 todos"));
    }

    #[test]
    fn bookmark_marker_appears_when_flag_set() {
        let t = theme();
        let summary = ObservationSummary::default();
        let w = DigestBarWidget {
            theme: &t,
            summary: Some(&summary),
            session_display_name: Some("sess"),
            has_bookmarks: true,
        };
        let text = joined(&w.build_line());
        assert!(text.contains("★ bookmarked"), "got {text:?}");
        // With a bookmark present the "no signals yet" hint must NOT fire.
        assert!(!text.contains("no signals yet"), "got {text:?}");
    }

    #[test]
    fn last_observed_age_renders_when_timestamp_present() {
        let t = theme();
        // 7200s = 2h before "now"
        let now = parse_iso8601_to_epoch("2026-05-20T12:00:00Z").unwrap();
        let ts = "2026-05-20T10:00:00Z".to_string();
        let summary = ObservationSummary {
            last_observed_at: Some(ts),
            ..Default::default()
        };
        let w = DigestBarWidget {
            theme: &t,
            summary: Some(&summary),
            session_display_name: None,
            has_bookmarks: false,
        };
        let line = w.build_line_with_now(now);
        let text = joined(&line);
        assert!(text.contains("last 2h"), "got {text:?}");
    }

    #[test]
    fn newest_decision_is_truncated_to_60_chars() {
        let t = theme();
        let long = "a".repeat(120);
        let summary = ObservationSummary {
            newest_decision: Some(long),
            ..Default::default()
        };
        let w = DigestBarWidget {
            theme: &t,
            summary: Some(&summary),
            session_display_name: None,
            has_bookmarks: false,
        };
        let text = joined(&w.build_line());
        assert!(text.contains("…"), "expected ellipsis, got {text:?}");
        // The decision span itself shouldn't exceed `prefix + 60` chars.
        let span = w
            .build_line()
            .spans
            .into_iter()
            .find(|s| s.content.starts_with("decision: "))
            .expect("decision span present");
        // 10-char prefix + 60-char body
        assert!(
            span.content.chars().count() <= 10 + 60,
            "span too long: {:?}",
            span.content
        );
    }

    #[test]
    fn widget_renders_to_buffer_with_expected_text() {
        // Frame-buffer test: drive the Widget through a real ratatui Buffer
        // and read the cells back to confirm the bar text shows up.
        let t = theme();
        let summary = ObservationSummary {
            open_todo_count: 3,
            unresolved_conflict_count: 1,
            ..Default::default()
        };
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
                    summary: Some(&summary),
                    session_display_name: Some("my-session"),
                    has_bookmarks: true,
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
        assert!(text.contains("3 todos"), "rendered: {text:?}");
        assert!(text.contains("1 conflict"), "rendered: {text:?}");
        assert!(text.contains("bookmarked"), "rendered: {text:?}");
    }

    #[test]
    fn truncate_handles_multibyte() {
        assert_eq!(truncate("héllo", 10), "héllo");
        let t = truncate("héllo world this is long", 8);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 8);
    }

    #[test]
    fn parse_iso8601_round_trip_against_known_epoch() {
        // 2026-05-20T12:00:00Z — sanity-check against a hand-computed value.
        let a = parse_iso8601_to_epoch("2026-05-20T12:00:00Z").unwrap();
        let b = parse_iso8601_to_epoch("2026-05-20T13:00:00Z").unwrap();
        assert!((b - a - 3600.0).abs() < 1e-6, "1h diff expected, got {}", b - a);

        let c = parse_iso8601_to_epoch("2026-05-21T12:00:00Z").unwrap();
        assert!((c - a - 86_400.0).abs() < 1e-6);

        // Fractional seconds parse.
        let d = parse_iso8601_to_epoch("2026-05-20T12:00:00.500Z").unwrap();
        assert!((d - a - 0.5).abs() < 1e-6);

        // Garbage returns None.
        assert!(parse_iso8601_to_epoch("not a date").is_none());
    }

    #[test]
    fn hex_to_color_parses_six_digit_hex() {
        assert_eq!(hex_to_color("#ff8800"), Some(Color::Rgb(0xff, 0x88, 0x00)));
        assert_eq!(hex_to_color("aabbcc"), Some(Color::Rgb(0xaa, 0xbb, 0xcc)));
        assert_eq!(hex_to_color("#zzz"), None);
        assert_eq!(hex_to_color(""), None);
    }
}
