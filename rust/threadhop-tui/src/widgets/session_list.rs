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
use threadhop_core::{
    models::SessionStatus,
    theme::{hex_to_rgb, Theme},
};
use unicode_width::UnicodeWidthChar;

/// Phase E: Y-row mapping recorded after the most recent sidebar render.
/// The mouse dispatcher reads this to translate a click row into a session
/// index. Includes both header rows (which are non-clickable) and session
/// rows. Stored as a `RefCell<Vec<RowKind>>` so the immediate-mode renderer
/// (which takes `&self`) can write into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRowKind {
    /// A status group header row — not clickable.
    Header,
    /// A session row at the given index into `App::sidebar`.
    Session(usize),
}

fn theme_color(hex: &str, fallback: Color) -> Color {
    match hex_to_rgb(hex) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => fallback,
    }
}

/// Width reserved for the display name column. Mirrors
/// `threadhop_core.tui.constants.DISPLAY_NAME_WIDTH`.
pub const DISPLAY_NAME_WIDTH: usize = 22;

/// Working-state spinner frames. Braille set — standard in modern Rust
/// TUIs (Atuin, cargo, indicatif). Replaces the older 4-frame circle set
/// which read as a hard step rather than fluid motion.
pub const SPINNER_FRAMES: &[&str] = &[
    "⠁", "⠂", "⠄", "⠆", "⠇", "⠧", "⠷", "⠿",
];

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
    /// Phase E: whether the sidebar has focus. When true the right border
    /// renders in `theme.accent` rather than `theme.border_subtle`, mirroring
    /// the Python TUI's focus highlight.
    pub focused: bool,
    /// Phase E: out-parameter for the click hit-test. After `render` runs,
    /// the dispatcher reads this back and translates a clicked Y-row into
    /// either a `Header` (no-op) or a `Session(index)` action. Rows live in
    /// render-Y order — index 0 corresponds to `area.y`.
    pub row_layout: Option<&'a std::cell::RefCell<Vec<SidebarRowKind>>>,
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
        let supports_emoji = supports_observation_emoji();
        let error_color = self.theme.and_then(|t| {
            hex_to_rgb(&t.error).map(|(r, g, b)| Color::Rgb(r, g, b))
        });

        // Phase E: Group sessions by status. Order matches Python's
        // STATUS_ORDER (Active / InProgress / InReview / Done / Archived).
        // Headers are only emitted for groups that actually contain a row,
        // and only when more than one group is non-empty (no need to
        // shout "ACTIVE" when every session is Active).
        const GROUP_ORDER: [SessionStatus; 5] = [
            SessionStatus::Active,
            SessionStatus::InProgress,
            SessionStatus::InReview,
            SessionStatus::Done,
            SessionStatus::Archived,
        ];
        let mut grouped: Vec<(SessionStatus, Vec<usize>)> = Vec::new();
        for status in GROUP_ORDER {
            let indices: Vec<usize> = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, it)| it.status == status)
                .map(|(i, _)| i)
                .collect();
            if !indices.is_empty() {
                grouped.push((status, indices));
            }
        }
        let render_headers = grouped.len() > 1;

        // Build the unified row stream — headers + sessions — and a parallel
        // `row_kinds` tracker so the mouse dispatcher can translate Y → action.
        let mut list_items: Vec<ListItem> = Vec::new();
        let mut row_kinds: Vec<SidebarRowKind> = Vec::new();
        let mut highlight_row: Option<usize> = None;
        let selected_id = self.selected_session_id;

        let header_style = self
            .theme
            .map(|t| {
                Style::default()
                    .fg(theme_color(&t.text_muted, Color::DarkGray))
                    .bg(theme_color(&t.background_panel, Color::Reset))
                    .add_modifier(Modifier::BOLD)
            })
            .unwrap_or_else(|| {
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            });

        for (status, indices) in &grouped {
            if render_headers {
                let label = group_header_label(*status);
                list_items.push(ListItem::new(Line::from(Span::styled(label, header_style))));
                row_kinds.push(SidebarRowKind::Header);
            }
            for &idx in indices {
                let it = &self.items[idx];
                let line = render_session_label_line_full(
                    it,
                    self.spinner_frame,
                    self.now,
                    supports_emoji,
                    error_color,
                    self.theme,
                );
                // Phase E row class — apply on top of the per-span theming.
                let item_style = row_class_style(it, self.theme);
                let li = ListItem::new(line).style(item_style);
                let here = list_items.len();
                if Some(it.session_id.as_str()) == selected_id {
                    highlight_row = Some(here);
                }
                list_items.push(li);
                row_kinds.push(SidebarRowKind::Session(idx));
            }
        }

        // Publish the row layout for mouse hit-testing before we render.
        if let Some(cell) = self.row_layout {
            cell.replace(row_kinds);
        }

        let mut state = ListState::default();
        state.select(highlight_row);

        // Modern selection: accent background with the canvas color as
        // foreground.
        let (sel_bg, sel_fg) = self
            .theme
            .map(|t| {
                (
                    theme_color(&t.accent, Color::Magenta),
                    theme_color(&t.background, Color::Black),
                )
            })
            .unwrap_or((Color::Magenta, Color::Black));

        // Phase E: focus-aware border color. When the sidebar has focus,
        // light up the right border in the theme accent so the user sees
        // which pane will receive their next keystroke.
        let border_color = match (self.theme, self.focused) {
            (Some(t), true) => theme_color(&t.accent, Color::Magenta),
            (Some(t), false) => theme_color(&t.border_subtle, Color::DarkGray),
            (None, true) => Color::Magenta,
            (None, false) => Color::DarkGray,
        };

        let list = List::new(list_items)
            .block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .border_style(Style::default().fg(border_color)),
            )
            .highlight_style(
                Style::default()
                    .bg(sel_bg)
                    .fg(sel_fg)
                    .add_modifier(Modifier::BOLD),
            );

        StatefulWidget::render(list, area, buf, &mut state);
    }
}

/// Status group header label — full-width sidebar row text. Mirrors the
/// Python `SessionStatusHeader` rendering: `── Backlog ──` etc.
fn group_header_label(status: SessionStatus) -> String {
    let name = match status {
        SessionStatus::Active => "Active",
        SessionStatus::InProgress => "In Progress",
        SessionStatus::InReview => "In Review",
        SessionStatus::Done => "Done",
        SessionStatus::Archived => "Archived",
    };
    format!(" ── {name} ── ")
}

/// Phase E row-class styling. Mirrors Python's CSS classes:
/// * `archived` → muted + italic
/// * `unread` (Phase 5 placeholder) → warning + bold
/// * `active && !working` → accent
/// * `working` → success
/// * otherwise → default foreground
///
/// Returns a `Style` applied to the row's `ListItem`. The inner spans keep
/// their own styling — we only set color/modifier defaults the spans haven't
/// overridden.
fn row_class_style(item: &SessionListItem, theme: Option<&Theme>) -> Style {
    let Some(t) = theme else {
        return Style::default();
    };
    if matches!(item.status, SessionStatus::Archived) {
        return Style::default()
            .fg(theme_color(&t.text_muted, Color::DarkGray))
            .add_modifier(Modifier::ITALIC);
    }
    if item.is_working {
        return Style::default().fg(theme_color(&t.success, Color::Green));
    }
    if item.is_active {
        return Style::default().fg(theme_color(&t.accent, Color::Magenta));
    }
    Style::default().fg(theme_color(&t.foreground, Color::White))
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

/// Cell-width-aware truncation: walks the string char-by-char and stops
/// when the accumulated cell width hits `budget`. Returns `(truncated,
/// width_consumed)`. CJK / emoji characters count as 2 cells, ASCII as 1.
/// When `add_ellipsis` is true and truncation occurred, reserves 1 cell
/// for the trailing `…`.
fn truncate_to_width(s: &str, budget: usize, add_ellipsis: bool) -> (String, usize) {
    // Fast-path: full string already fits.
    let full_width: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    if full_width <= budget {
        return (s.to_string(), full_width);
    }
    let cap = if add_ellipsis {
        budget.saturating_sub(1)
    } else {
        budget
    };
    let mut out = String::with_capacity(s.len());
    let mut used = 0usize;
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > cap {
            break;
        }
        out.push(c);
        used += w;
    }
    if add_ellipsis {
        out.push('…');
        used += 1;
    }
    (out, used)
}

/// Truncate-and-pad the display name so the age column stays aligned, with
/// optional observation marker appended. The marker steals one column of
/// width for the leading space plus its own cell width (1 for ASCII, 2 for
/// the emoji — terminals render `🗒` as a wide glyph).
///
/// Uses cell-width-aware truncation so CJK + emoji session names don't
/// overflow the 22-cell display column.
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
    let name_budget = DISPLAY_NAME_WIDTH.saturating_sub(reserved);

    let (truncated, used_cells) = truncate_to_width(&item.display_name, name_budget, true);

    let mut out = String::with_capacity(DISPLAY_NAME_WIDTH * 2);
    out.push_str(&truncated);
    if let Some(m) = marker {
        out.push(' ');
        out.push_str(m);
    }
    let used = used_cells + reserved;
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
    render_session_label_line_full(item, spinner_frame, now, supports_emoji, error_color, None)
}

/// Full-fat themed variant that also colors the leading status glyph
/// (success / warning / muted) when a theme is supplied. The earlier
/// `_themed` entrypoint still works — it just doesn't color the icon.
pub fn render_session_label_line_full<'a>(
    item: &SessionListItem,
    spinner_frame: usize,
    now: f64,
    supports_emoji: bool,
    error_color: Option<Color>,
    theme: Option<&Theme>,
) -> Line<'a> {
    let icon = status_icon(item, spinner_frame);
    let middle = render_display_segment(item, supports_emoji);
    let age = item
        .last_active_at
        .map(|ts| format_age(ts, now))
        .unwrap_or_default();
    // Right-align age to 4 cols, matching the Python `f" {age_str:>4}"`.
    let age_padded = format!("{age:>4}");

    let icon_style = match theme {
        Some(t) if item.is_working => Style::default()
            .fg(theme_color(&t.warning, Color::Yellow))
            .add_modifier(Modifier::BOLD),
        Some(t) if item.is_active => Style::default()
            .fg(theme_color(&t.success, Color::Green))
            .add_modifier(Modifier::BOLD),
        Some(t) => Style::default().fg(theme_color(&t.text_muted, Color::DarkGray)),
        None => Style::default(),
    };
    let age_style = match theme {
        Some(t) => Style::default().fg(theme_color(&t.text_muted, Color::DarkGray)),
        None => Style::default(),
    };

    let mut spans = vec![
        Span::styled(format!("{icon} "), icon_style),
        Span::raw(middle),
        Span::styled(format!(" {age_padded}"), age_style),
    ];
    if item.unresolved_conflict_count > 0 {
        // Tiny red pill — bg(error) fg(background) so it reads as a deliberate
        // chip rather than a stray glyph. Leading space separates it from the
        // age column.
        let bg_color = error_color.unwrap_or(Color::Red);
        let fg_color = theme
            .map(|t| theme_color(&t.background, Color::Black))
            .unwrap_or(Color::Black);
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            " ! ".to_string(),
            Style::default()
                .bg(bg_color)
                .fg(fg_color)
                .add_modifier(Modifier::BOLD),
        ));
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
    fn spinner_changes_glyph_across_frames() {
        // Regression: the rotating spinner needs distinct glyphs across
        // consecutive frames so the user perceives motion. Walk the frame
        // table and confirm we hit at least 3 unique glyphs.
        let working = item("s", "n", true, true);
        let mut seen = std::collections::HashSet::new();
        for frame in 0..SPINNER_FRAMES.len() {
            seen.insert(status_icon(&working, frame));
        }
        assert!(
            seen.len() >= 3,
            "spinner should expose multiple distinct frames, got {} unique glyphs",
            seen.len()
        );
    }

    #[test]
    fn conflict_marker_renders_as_pill_with_bg_and_fg() {
        // The unresolved-conflict marker should be a bg/fg-styled pill,
        // not just a colored character — verifies the pill upgrade lands.
        use threadhop_core::theme::Theme;
        let it = SessionListItem {
            session_id: "s1".into(),
            display_name: "n".into(),
            unresolved_conflict_count: 2,
            last_active_at: Some(0.0),
            ..Default::default()
        };
        let theme = Theme::default_dark();
        let err_color = hex_to_rgb(&theme.error).map(|(r, g, b)| Color::Rgb(r, g, b));
        let line = render_session_label_line_full(&it, 0, 0.0, true, err_color, Some(&theme));
        let pill = line
            .spans
            .iter()
            .find(|s| s.content.contains('!'))
            .expect("pill span present");
        assert!(pill.style.bg.is_some(), "pill must have bg color");
        assert!(pill.style.fg.is_some(), "pill must have fg color");
        // Pill text should be 3 cells wide (` ! `) for a chip-like look.
        assert_eq!(pill.content.as_ref(), " ! ");
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
        // Now uses cell-width-aware truncation with a trailing ellipsis.
        assert!(middle.ends_with('…'), "expected ellipsis on truncation, got {middle:?}");
        // All other chars are still the source 'a'.
        assert!(middle.chars().filter(|c| *c != '…').all(|c| c == 'a'));
    }

    #[test]
    fn render_display_segment_handles_cjk_within_budget() {
        // CJK chars are 2 cells each — 11 chars = 22 cells = exactly the
        // budget. Verify the segment doesn't overflow and fills the column.
        let it = SessionListItem {
            session_id: "s1".into(),
            display_name: "これは長い日本語のセッション名".into(),
            last_active_at: Some(0.0),
            ..Default::default()
        };
        let text = render_session_label_text(&it, 0, 0.0, true);
        // Skip "○ " (status + space) then take the cell-width worth of chars.
        // We don't pin the exact rendered form — instead check that the
        // total display column width matches DISPLAY_NAME_WIDTH.
        use unicode_width::UnicodeWidthStr;
        // status icon + space = 2 cells; then DISPLAY_NAME_WIDTH; then " AGE".
        let trimmed = text.trim_end();
        let width = UnicodeWidthStr::width(trimmed);
        // Status (1) + space (1) + name column (22) + space (1) + "0s" (2) = 27
        // Either way it must not exceed 1 + 1 + DISPLAY_NAME_WIDTH + 1 + 4.
        assert!(
            width <= 2 + DISPLAY_NAME_WIDTH + 1 + 4,
            "cjk row overflowed: width={width} text={text:?}"
        );
    }

    #[test]
    fn render_display_segment_handles_emoji() {
        let it = SessionListItem {
            session_id: "s1".into(),
            display_name: "🎯 emoji session name that is too long".into(),
            last_active_at: Some(0.0),
            ..Default::default()
        };
        let text = render_session_label_text(&it, 0, 0.0, true);
        use unicode_width::UnicodeWidthStr;
        let trimmed = text.trim_end();
        let width = UnicodeWidthStr::width(trimmed);
        assert!(
            width <= 2 + DISPLAY_NAME_WIDTH + 1 + 4,
            "emoji row overflowed: width={width} text={text:?}"
        );
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
            focused: false,
            row_layout: None,
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
            focused: false,
            row_layout: None,
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
            focused: false,
            row_layout: None,
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
            focused: false,
            row_layout: None,
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

    // ---- Phase E: status group headers + row classes -----------------------

    fn item_with_status(id: &str, name: &str, status: SessionStatus) -> SessionListItem {
        SessionListItem {
            session_id: id.into(),
            display_name: name.into(),
            last_active_at: Some(0.0),
            status,
            ..Default::default()
        }
    }

    #[test]
    fn sidebar_renders_status_group_header_when_sessions_have_mixed_status() {
        // Mixed statuses ⇒ headers must render. Frame-buffer assertion: a
        // group-header row carrying the status name appears in the buffer.
        use threadhop_core::theme::Theme;
        let items = vec![
            item_with_status("s1", "alpha", SessionStatus::Active),
            item_with_status("s2", "beta", SessionStatus::InProgress),
        ];
        let theme = Theme::default_dark();
        let widget = SessionListWidget {
            items: &items,
            selected_session_id: Some("s1"),
            spinner_frame: 0,
            now: 0.0,
            theme: Some(&theme),
            focused: false,
            row_layout: None,
        };
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        term.draw(|f| f.render_widget(widget, f.area())).unwrap();
        let buf = term.backend().buffer();
        let mut whole = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                whole.push_str(buf[(x, y)].symbol());
            }
            whole.push('\n');
        }
        assert!(
            whole.contains("Active") && whole.contains("In Progress"),
            "expected status group headers, got:\n{whole}"
        );
    }

    #[test]
    fn sidebar_omits_status_group_headers_when_only_one_group_present() {
        // Single status ⇒ headers suppressed; sessions render flat.
        use threadhop_core::theme::Theme;
        let items = vec![
            item_with_status("s1", "alpha", SessionStatus::Active),
            item_with_status("s2", "beta", SessionStatus::Active),
        ];
        let theme = Theme::default_dark();
        let widget = SessionListWidget {
            items: &items,
            selected_session_id: None,
            spinner_frame: 0,
            now: 0.0,
            theme: Some(&theme),
            focused: false,
            row_layout: None,
        };
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        term.draw(|f| f.render_widget(widget, f.area())).unwrap();
        let buf = term.backend().buffer();
        let mut whole = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                whole.push_str(buf[(x, y)].symbol());
            }
            whole.push('\n');
        }
        assert!(
            !whole.contains("── Active ──"),
            "single-group sidebar should suppress group header, got:\n{whole}"
        );
    }

    #[test]
    fn sidebar_row_for_archived_session_renders_muted_italic() {
        // Frame-buffer: an archived row must carry the text-muted color +
        // ITALIC modifier somewhere in its span run.
        use threadhop_core::theme::Theme;
        let items = vec![
            item_with_status("s1", "alive", SessionStatus::Active),
            item_with_status("s2", "old", SessionStatus::Archived),
        ];
        let theme = Theme::default_dark();
        let widget = SessionListWidget {
            items: &items,
            selected_session_id: None,
            spinner_frame: 0,
            now: 0.0,
            theme: Some(&theme),
            focused: false,
            row_layout: None,
        };
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        term.draw(|f| f.render_widget(widget, f.area())).unwrap();
        let buf = term.backend().buffer();
        // Walk rows looking for the "old" session name; whichever row it
        // sits on must carry ITALIC styling.
        let mut found_italic = false;
        'rows: for y in 0..buf.area().height {
            let mut row_text = String::new();
            for x in 0..buf.area().width {
                row_text.push_str(buf[(x, y)].symbol());
            }
            if row_text.contains("old") {
                for x in 0..buf.area().width {
                    let cell = &buf[(x, y)];
                    if cell.modifier.contains(Modifier::ITALIC) {
                        found_italic = true;
                        break 'rows;
                    }
                }
            }
        }
        assert!(found_italic, "archived row must carry ITALIC modifier");
    }

    #[test]
    fn sidebar_row_for_working_session_renders_in_success_color() {
        // Frame-buffer: a working session's row body should carry the
        // theme.success color somewhere.
        use threadhop_core::theme::Theme;
        let items = vec![SessionListItem {
            session_id: "s1".into(),
            display_name: "running".into(),
            is_active: true,
            is_working: true,
            last_active_at: Some(0.0),
            ..Default::default()
        }];
        let theme = Theme::default_dark();
        let want = hex_to_rgb(&theme.success).map(|(r, g, b)| Color::Rgb(r, g, b));
        let widget = SessionListWidget {
            items: &items,
            selected_session_id: None,
            spinner_frame: 0,
            now: 0.0,
            theme: Some(&theme),
            focused: false,
            row_layout: None,
        };
        let mut term = Terminal::new(TestBackend::new(40, 5)).unwrap();
        term.draw(|f| f.render_widget(widget, f.area())).unwrap();
        let buf = term.backend().buffer();
        let mut found = false;
        'rows: for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                if Some(buf[(x, y)].fg) == want {
                    found = true;
                    break 'rows;
                }
            }
        }
        assert!(found, "working row must paint a cell in theme.success");
    }

    #[test]
    fn sidebar_focused_border_uses_accent() {
        // The focused sidebar's right border must paint the accent color.
        use threadhop_core::theme::Theme;
        let items = vec![item_with_status("s1", "a", SessionStatus::Active)];
        let theme = Theme::default_dark();
        let want = hex_to_rgb(&theme.accent).map(|(r, g, b)| Color::Rgb(r, g, b));
        let widget = SessionListWidget {
            items: &items,
            selected_session_id: None,
            spinner_frame: 0,
            now: 0.0,
            theme: Some(&theme),
            focused: true,
            row_layout: None,
        };
        let mut term = Terminal::new(TestBackend::new(36, 5)).unwrap();
        term.draw(|f| f.render_widget(widget, f.area())).unwrap();
        let buf = term.backend().buffer();
        // The right border lives at column 35 (rightmost cell of the 36-cell
        // sidebar). Walk rows of that column; at least one cell carries the
        // accent fg.
        let mut found = false;
        for y in 0..buf.area().height {
            if Some(buf[(35, y)].fg) == want {
                found = true;
                break;
            }
        }
        assert!(found, "focused sidebar right border must paint in accent");
    }

    #[test]
    fn sidebar_row_layout_is_emitted_into_provided_cell() {
        // E1 hit-test: the widget publishes a row_kinds vector via the
        // optional RefCell cell so the mouse dispatcher can translate a
        // clicked row into a session index.
        use std::cell::RefCell;
        use threadhop_core::theme::Theme;
        let items = vec![
            item_with_status("s1", "a", SessionStatus::Active),
            item_with_status("s2", "b", SessionStatus::InProgress),
        ];
        let theme = Theme::default_dark();
        let cell: RefCell<Vec<SidebarRowKind>> = RefCell::new(Vec::new());
        let widget = SessionListWidget {
            items: &items,
            selected_session_id: None,
            spinner_frame: 0,
            now: 0.0,
            theme: Some(&theme),
            focused: false,
            row_layout: Some(&cell),
        };
        let mut term = Terminal::new(TestBackend::new(40, 10)).unwrap();
        term.draw(|f| f.render_widget(widget, f.area())).unwrap();
        let rows = cell.borrow();
        // Mixed-status path: header, session(0), header, session(1).
        assert_eq!(rows.len(), 4);
        assert!(matches!(rows[0], SidebarRowKind::Header));
        assert!(matches!(rows[1], SidebarRowKind::Session(0)));
        assert!(matches!(rows[2], SidebarRowKind::Header));
        assert!(matches!(rows[3], SidebarRowKind::Session(1)));
    }
}
