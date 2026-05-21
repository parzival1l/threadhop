//! Sidebar session list (Phase 2 Wave B — task 2.8).
//!
//! Stateless render of one row per session. Mirrors the Python
//! `threadhop_core/tui/widgets/session_list.py` behaviour:
//!
//! - Status icon: `◐` working / `●` active / `○` inactive (working frame
//!   advances on a caller-supplied spinner index — Wave C ticks it).
//! - Observation marker: `🗒` (UTF-8) or `≡` (ASCII fallback) per
//!   ADR-021. Picked up from the `THREADHOP_ASCII_OBSERVATION_MARKER`
//!   env var to mirror `_supports_observation_emoji` in Python.
//! - Display name: custom_name → JSONL title → project, truncated to
//!   `DISPLAY_NAME_WIDTH` (22 cols), padded to fixed width so ages align.
//! - Age suffix: human-readable `Ns / Nm / Nh / Nd` from `last_active_at`.
//!
//! The widget borrows app state by reference (`SessionListWidget<'a>`) and
//! implements `ratatui::widgets::Widget` so it can be passed directly to
//! `Frame::render_widget`. Pure helpers (`status_icon`, `format_age`,
//! `render_session_label_text`) are free functions so they can be unit-tested
//! without standing up a buffer.

// Wave B scaffolding — the public surface is consumed by a screen in a
// later wave. Same pattern as `app.rs::App`.
#![allow(dead_code)]

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, StatefulWidget, Widget},
};
use threadhop_core::{models::SessionStatus, theme::hex_to_rgb};

/// Width reserved for the display name column. Mirrors
/// `threadhop_core.tui.constants.DISPLAY_NAME_WIDTH`.
pub const DISPLAY_NAME_WIDTH: usize = 22;

/// Working-state spinner frames. Mirrors
/// `threadhop_core.tui.constants.SPINNER_FRAMES`.
pub const SPINNER_FRAMES: &[&str] = &["◐", "◓", "◑", "◒"];

/// UTF-8 observation marker (ADR-021).
pub const OBSERVATION_MARKER: &str = "🗒";

/// ASCII fallback for terminals that can't encode the emoji marker.
pub const OBSERVATION_MARKER_FALLBACK: &str = "≡";

/// View-model for one sidebar row.
///
/// Wave A's `threadhop_core::models::Session` lacks the runtime fields
/// (`is_active`, `is_working`, `has_observations`, `last_active_at`) that
/// drive the sidebar; those are derived by Wave C workers (active detector
/// and session scanner). This struct is the per-row contract the widget
/// needs. The worker layer fills it in, the App holds the resulting `Vec`.
#[derive(Debug, Clone, Default)]
pub struct SessionListItem {
    /// Stable session id — matches `Session::session_id`. Used by callers to
    /// resolve selection back to a session.
    pub session_id: String,

    /// Resolved display name. Pre-truncation; the widget handles width.
    /// Order of preference: custom_name → JSONL title → project name.
    pub display_name: String,

    /// True when a `claude` process is bound to this session.
    pub is_active: bool,

    /// True when active + has a pending tool call / user-final message.
    /// Drives the spinner instead of the static active dot.
    pub is_working: bool,

    /// True when the session has observation entries (drives the marker).
    pub has_observations: bool,

    /// JSONL `modified` timestamp (epoch seconds) — drives the age column.
    /// None renders as empty.
    pub last_active_at: Option<f64>,

    /// Tag status from `sessions.status`. Wave 2 (Phase 5) populates this when
    /// opening the kanban modal so status changes can update sidebar items in
    /// place without a worker round-trip. Default `Active` keeps existing
    /// scanner code path untouched.
    pub status: SessionStatus,

    /// Count of unresolved cross-session conflicts whose origin is this
    /// session. Populated by Phase 5 Wave 2 from `app.conflict_counts`; the
    /// session_scanner worker leaves it at 0.
    pub unresolved_conflict_count: u32,

    /// Project name (the `<encoded-project>` directory under
    /// `~/.claude/projects/`). Populated by the session_scanner so the App
    /// can apply a `--project` filter without re-scanning the filesystem.
    pub project: Option<String>,
}

/// Stateless sidebar renderer. Holds references to App state so a fresh
/// instance can be built each frame without allocation churn.
pub struct SessionListWidget<'a> {
    pub items: &'a [SessionListItem],
    pub selected_session_id: Option<&'a str>,
    pub spinner_frame: usize,
    /// Current unix timestamp for age calculation. Caller passes
    /// `SystemTime::now()` in production; tests pass a fixed value so
    /// snapshots are stable.
    pub now: f64,
    /// Optional theme — Phase 5 Wave 2 uses `theme.error` to color the
    /// unresolved-conflict `!` marker. `None` keeps the marker uncolored.
    pub theme: Option<&'a threadhop_core::theme::Theme>,
}

impl<'a> SessionListWidget<'a> {
    /// Resolve `selected_session_id` to an index into `items`. None when
    /// nothing is selected or the selection no longer matches a row.
    fn selected_index(&self) -> Option<usize> {
        let sid = self.selected_session_id?;
        self.items.iter().position(|it| it.session_id == sid)
    }
}

impl<'a> Widget for SessionListWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let selected = self.selected_index();
        let supports_emoji = supports_observation_emoji();
        let error_color = self.theme.and_then(|t| {
            hex_to_rgb(&t.error).map(|(r, g, b)| Color::Rgb(r, g, b))
        });
        let list_items: Vec<ListItem> = self
            .items
            .iter()
            .map(|it| {
                ListItem::new(render_session_label_line_themed(
                    it,
                    self.spinner_frame,
                    self.now,
                    supports_emoji,
                    error_color,
                ))
            })
            .collect();

        let mut state = ListState::default();
        state.select(selected);

        let list = List::new(list_items)
            .block(Block::default().borders(Borders::RIGHT))
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        StatefulWidget::render(list, area, buf, &mut state);
    }
}

// --- Pure helpers --------------------------------------------------------

/// Resolve the leading status glyph for a row. Mirrors the Python
/// `render_session_label_text` status branch.
pub fn status_icon(item: &SessionListItem, spinner_frame: usize) -> &'static str {
    if item.is_working {
        SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()]
    } else if item.is_active {
        "●"
    } else {
        "○"
    }
}

/// Whether the current terminal can encode the UTF-8 observation marker.
/// Mirrors `_supports_observation_emoji` — opts out when the env override
/// is set. The string-level check is implicit: we always know we have UTF-8
/// in a Rust string, so the only signal is the env override.
pub fn supports_observation_emoji() -> bool {
    std::env::var("THREADHOP_ASCII_OBSERVATION_MARKER").as_deref() != Ok("1")
}

/// Resolve the observation marker glyph. Pure for testability — callers
/// pass the `supports_emoji` flag rather than reading env state per row.
pub fn observation_marker(supports_emoji: bool) -> &'static str {
    if supports_emoji {
        OBSERVATION_MARKER
    } else {
        OBSERVATION_MARKER_FALLBACK
    }
}

/// Format an epoch-seconds timestamp as a human-readable age (`Ns / Nm /
/// Nh / Nd`). Mirrors `threadhop_core.tui.utils.format_age`.
///
/// `now` is taken as a parameter so tests are deterministic.
pub fn format_age(timestamp: f64, now: f64) -> String {
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

/// Truncate-and-pad the display name so the age column stays aligned, with
/// optional observation marker appended. The marker steals one column of
/// width for the leading space plus its own cell width (1 for ASCII, 2 for
/// the emoji — terminals render `🗒` as a wide glyph, but ratatui's grid
/// charges us 1 byte-display column; we conservatively reserve 2 in the
/// emoji case to match the Python `cell_len` of 2).
fn render_display_segment(item: &SessionListItem, supports_emoji: bool) -> String {
    let marker = if item.has_observations {
        Some(observation_marker(supports_emoji))
    } else {
        None
    };

    let reserved = match marker {
        // " " + marker width (emoji counts as 2 cells, ascii as 1).
        Some(m) if m == OBSERVATION_MARKER => 1 + 2,
        Some(_) => 1 + 1,
        None => 0,
    };
    let name_width = DISPLAY_NAME_WIDTH.saturating_sub(reserved);

    // Truncate by char count — close enough to Rich's `cell_len` for ASCII
    // session names, which is the overwhelming common case for Claude
    // sessions. Wider grapheme handling can come with the wider unicode
    // pass.
    let truncated: String = item.display_name.chars().take(name_width).collect();
    let truncated_cells = truncated.chars().count();

    let mut out = String::with_capacity(DISPLAY_NAME_WIDTH + 2);
    out.push_str(&truncated);
    if let Some(m) = marker {
        out.push(' ');
        out.push_str(m);
    }
    let used = truncated_cells + reserved;
    if used < DISPLAY_NAME_WIDTH {
        for _ in 0..(DISPLAY_NAME_WIDTH - used) {
            out.push(' ');
        }
    }
    out
}

/// Build the styled `Line` for one sidebar row. Public so test
/// snapshots can pin the exact text without going through a buffer.
pub fn render_session_label_line<'a>(
    item: &SessionListItem,
    spinner_frame: usize,
    now: f64,
) -> Line<'a> {
    render_session_label_line_with(item, spinner_frame, now, supports_observation_emoji())
}

/// Pure variant that takes the emoji-support flag explicitly. Used in
/// tests to lock the rendered shape regardless of host terminal env.
pub fn render_session_label_line_with<'a>(
    item: &SessionListItem,
    spinner_frame: usize,
    now: f64,
    supports_emoji: bool,
) -> Line<'a> {
    render_session_label_line_themed(item, spinner_frame, now, supports_emoji, None)
}

/// Variant that optionally styles a Phase-5 unresolved-conflict marker (`!`)
/// in the supplied theme's `error` color. Pass `None` to render without
/// styling (used by the text-only convenience helper and existing tests).
pub fn render_session_label_line_themed<'a>(
    item: &SessionListItem,
    spinner_frame: usize,
    now: f64,
    supports_emoji: bool,
    error_color: Option<Color>,
) -> Line<'a> {
    let icon = status_icon(item, spinner_frame);
    let middle = render_display_segment(item, supports_emoji);
    let age = item
        .last_active_at
        .map(|ts| format_age(ts, now))
        .unwrap_or_default();
    // Right-align age to 4 cols, matching the Python `f" {age_str:>4}"`.
    let age_padded = format!("{age:>4}");

    let mut spans = vec![
        Span::raw(format!("{icon} ")),
        Span::raw(middle),
        Span::raw(format!(" {age_padded}")),
    ];
    if item.unresolved_conflict_count > 0 {
        let marker = " !";
        let style = match error_color {
            Some(c) => Style::default().fg(c).add_modifier(Modifier::BOLD),
            None => Style::default().add_modifier(Modifier::BOLD),
        };
        spans.push(Span::styled(marker.to_string(), style));
    }
    Line::from(spans)
}

/// Render text for one row — convenience that drops styling for callers
/// that only need the string form (e.g. accessibility, CLI export).
pub fn render_session_label_text(
    item: &SessionListItem,
    spinner_frame: usize,
    now: f64,
    supports_emoji: bool,
) -> String {
    render_session_label_line_with(item, spinner_frame, now, supports_emoji)
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn item(id: &str, name: &str, active: bool, working: bool) -> SessionListItem {
        SessionListItem {
            session_id: id.to_string(),
            display_name: name.to_string(),
            is_active: active,
            is_working: working,
            last_active_at: Some(0.0),
            ..Default::default()
        }
    }

    #[test]
    fn format_age_buckets_match_python() {
        assert_eq!(format_age(0.0, 30.0), "30s");
        assert_eq!(format_age(0.0, 120.0), "2m");
        assert_eq!(format_age(0.0, 7200.0), "2h");
        assert_eq!(format_age(0.0, 2.0 * 86_400.0), "2d");
    }

    #[test]
    fn format_age_clamps_negative_drift_to_zero() {
        // Clock skew shouldn't produce "-3s" — the Python version doesn't
        // either (int(negative) collapses to 0).
        assert_eq!(format_age(100.0, 50.0), "0s");
    }

    #[test]
    fn format_age_handles_28d_edge_case() {
        // Phase 6 verifier regression: a sidebar row was observed showing
        // "28" instead of "28d". Pin the day bucket so any future refactor
        // that drops the unit suffix at the exact-day boundary trips this
        // test. Covers both the exact-multiple-of-86400 case and a value
        // a hair under 29 days (which must still bucket as "28d").
        assert_eq!(format_age(0.0, 28.0 * 86_400.0), "28d");
        assert_eq!(
            format_age(0.0, 28.0 * 86_400.0 + 3600.0 * 23.0),
            "28d",
            "23 hours past the 28-day mark must still read as 28d"
        );
        // Exactly the 24h → 1d transition: 86_400 seconds exactly bucket as
        // "1d", not "24h" (the `< 86_400.0` guard is strict-less-than).
        assert_eq!(format_age(0.0, 86_400.0), "1d");
        // And confirm a 3-digit day-count keeps its suffix.
        assert_eq!(format_age(0.0, 100.0 * 86_400.0), "100d");
    }

    #[test]
    fn status_icon_picks_inactive_active_working() {
        let inactive = item("s", "n", false, false);
        let active = item("s", "n", true, false);
        let working = item("s", "n", true, true);
        assert_eq!(status_icon(&inactive, 0), "○");
        assert_eq!(status_icon(&active, 0), "●");
        assert_eq!(status_icon(&working, 0), SPINNER_FRAMES[0]);
        assert_eq!(status_icon(&working, 2), SPINNER_FRAMES[2]);
    }

    #[test]
    fn status_icon_spinner_wraps_modulo() {
        let working = item("s", "n", true, true);
        assert_eq!(
            status_icon(&working, SPINNER_FRAMES.len() + 1),
            SPINNER_FRAMES[1]
        );
    }

    #[test]
    fn observation_marker_picks_glyph() {
        assert_eq!(observation_marker(true), OBSERVATION_MARKER);
        assert_eq!(observation_marker(false), OBSERVATION_MARKER_FALLBACK);
    }

    #[test]
    fn render_session_label_text_layout() {
        let it = SessionListItem {
            session_id: "s1".into(),
            display_name: "Refactor parser".into(),
            is_active: true,
            last_active_at: Some(0.0),
            ..Default::default()
        };
        // 30s ago → "30s" age.
        let text = render_session_label_text(&it, 0, 30.0, true);
        // status + space + 22-col name + space + 4-col right-aligned age.
        assert!(text.starts_with("● "));
        assert!(text.ends_with(" 30s"));
        // Name segment + leading "● " + trailing " AGE" → total width is
        // 2 + DISPLAY_NAME_WIDTH + 1 + 4 = 29 columns.
        assert_eq!(text.chars().count(), 2 + DISPLAY_NAME_WIDTH + 1 + 4);
    }

    #[test]
    fn render_session_label_truncates_long_name() {
        let it = SessionListItem {
            session_id: "s1".into(),
            display_name: "a".repeat(50),
            last_active_at: Some(0.0),
            ..Default::default()
        };
        let text = render_session_label_text(&it, 0, 0.0, true);
        // The middle segment is exactly DISPLAY_NAME_WIDTH cells wide,
        // even when the source is far longer.
        let middle: String = text.chars().skip(2).take(DISPLAY_NAME_WIDTH).collect();
        assert_eq!(middle.chars().count(), DISPLAY_NAME_WIDTH);
        assert!(middle.chars().all(|c| c == 'a'));
    }

    #[test]
    fn render_session_label_reserves_space_for_marker() {
        let it = SessionListItem {
            session_id: "s1".into(),
            display_name: "abc".into(),
            has_observations: true,
            last_active_at: Some(0.0),
            ..Default::default()
        };
        let text = render_session_label_text(&it, 0, 0.0, false);
        // ASCII marker takes 1 + 1 = 2 cols → name is "abc" + padding.
        assert!(text.contains(OBSERVATION_MARKER_FALLBACK));
    }

    #[test]
    fn supports_observation_emoji_respects_env() {
        // Clear, then assert UTF-8 mode.
        std::env::remove_var("THREADHOP_ASCII_OBSERVATION_MARKER");
        assert!(supports_observation_emoji());
        std::env::set_var("THREADHOP_ASCII_OBSERVATION_MARKER", "1");
        assert!(!supports_observation_emoji());
        // Cleanup so we don't pollute other tests in the same process.
        std::env::remove_var("THREADHOP_ASCII_OBSERVATION_MARKER");
    }

    #[test]
    fn widget_renders_into_buffer_without_panic() {
        let items = vec![
            item("s1", "Refactor parser", true, false),
            item("s2", "Bug investigation", false, true),
            item("s3", "Random thoughts", false, false),
        ];
        let widget = SessionListWidget {
            items: &items,
            selected_session_id: Some("s2"),
            spinner_frame: 0,
            now: 30.0,
            theme: None,
        };
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        term.draw(|f| {
            f.render_widget(widget, f.area());
        })
        .unwrap();
    }

    #[test]
    fn widget_handles_empty_session_list() {
        let widget = SessionListWidget {
            items: &[],
            selected_session_id: None,
            spinner_frame: 0,
            now: 0.0,
            theme: None,
        };
        let mut term = Terminal::new(TestBackend::new(20, 5)).unwrap();
        term.draw(|f| {
            f.render_widget(widget, f.area());
        })
        .unwrap();
    }

    #[test]
    fn widget_handles_stale_selection_gracefully() {
        // selected_session_id points at a row that no longer exists →
        // selected_index() returns None; the list still renders.
        let items = vec![item("s1", "Only row", false, false)];
        let widget = SessionListWidget {
            items: &items,
            selected_session_id: Some("missing"),
            spinner_frame: 0,
            now: 0.0,
            theme: None,
        };
        let mut term = Terminal::new(TestBackend::new(30, 5)).unwrap();
        term.draw(|f| {
            f.render_widget(widget, f.area());
        })
        .unwrap();
    }

    #[test]
    fn widget_renders_at_sidebar_width() {
        // Spec §4 calls for a 36-char sidebar — sanity-check that the
        // 29-cell content fits with room for the right border.
        let items = vec![item("s1", "Refactor parser", true, false)];
        let widget = SessionListWidget {
            items: &items,
            selected_session_id: Some("s1"),
            spinner_frame: 0,
            now: 30.0,
            theme: None,
        };
        let mut term = Terminal::new(TestBackend::new(36, 8)).unwrap();
        term.draw(|f| {
            f.render_widget(widget, f.area());
        })
        .unwrap();
        let buf = term.backend().buffer();
        // First row should start with the status dot.
        let cell = &buf[(0, 0)];
        assert_eq!(cell.symbol(), "●");
    }
}
