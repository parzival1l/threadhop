//! Right-column session digest panel — Phase C (minimal).
//!
//! Mirrors `threadhop_core/tui/widgets/session_digest_bar.py` in structure:
//! a passive, non-focusable column on the right showing the focused
//! session's identity, a recap snippet, stub Outputs / Context rows, and a
//! footer with a `claude -r <sid>…` resume command.
//!
//! This is the **minimal Phase C** landing — multi-entry recap, real
//! outputs (PR / files), real context (token usage / models), and the
//! permission / version footer chips are deferred until `ObservationSummary`
//! grows a `Vec<RecapEntry>` and the App carries a `SessionDigest`. See
//! `docs/superpowers/plans/2026-05-21-rust-tui-parity-plan.md` § 4 Phase C.
//!
//! Layout (top → bottom inside the panel rect):
//!
//! ```text
//!  ┌ digest ─────────────────────────┐
//!  │ <title (bold fg)>               │
//!  │ <slug (muted)>                  │
//!  │                                 │
//!  │ Recap                           │
//!  │ ▌ <newest_decision or stub>     │
//!  │                                 │
//!  │ Outputs                         │
//!  │ —                               │
//!  │                                 │
//!  │ Context                         │
//!  │ —                               │
//!  │                                 │
//!  │ ────────                        │
//!  │ claude -r abcdef12…             │
//!  └─────────────────────────────────┘
//! ```
//!
//! Render rules:
//! - Empty state (no `selected_session_id`): single muted line
//!   "Select a session to see its digest." inside the bordered block.
//! - Otherwise: render the five blocks. Each section header uses
//!   `theme.foreground + bold`. The recap band has a thick left-edge
//!   gutter (`▌`) in `theme.border_active`; band body uses `theme.foreground`.
//!   Outputs / Context bodies stub to `—` in `theme.text_muted` until
//!   real data lands.
//! - Footer: 8-cell `─` divider in `theme.border_active`, then the
//!   resume command in `theme.text_muted + italic`. The footer floats
//!   pinned to the bottom of the inner area when the panel is tall
//!   enough; otherwise it's clipped from the top with the rest of the
//!   blocks.
//!
//! The widget owns no state — like `DigestBarWidget`, all fields are
//! borrowed from the App and the struct is constructed per frame.

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};
use threadhop_core::digest::SessionDigest;
use threadhop_core::observations::ObservationSummary;
use threadhop_core::theme::Theme;

use crate::widgets::session_list::SessionListItem;

/// Right-column passive digest. Constructed per-frame from App state.
//
// `dead_code` is silenced for now because the widget is wired in
// `screens::main` directly via `frame.render_widget`; rust-analyzer can be
// pedantic about pub fields that are only constructed and never read by name.
#[allow(dead_code)]
pub struct SessionDigestPanel<'a> {
    pub theme: &'a Theme,
    /// Sidebar row for the currently-selected session, if any. Provides
    /// `display_name` (title) and `session_id` (slug).
    pub selected_item: Option<&'a SessionListItem>,
    /// Observation summary for the selected session. Used to populate the
    /// recap band's body line. `None` → stub "(no recap yet)".
    pub summary: Option<&'a ObservationSummary>,
    /// Full session digest for the selected session. Drives the Outputs
    /// (PR / files) and Context (token totals / cache / models) blocks.
    /// `None` → both blocks render their `—` stub.
    pub digest: Option<&'a SessionDigest>,
}

impl<'a> SessionDigestPanel<'a> {
    /// Inner inset used both for empty-state and populated rendering.
    fn inner(area: Rect, theme: &Theme) -> (Rect, Block<'static>) {
        let border = hex_to_color(&theme.border).unwrap_or(Color::DarkGray);
        let muted = hex_to_color(&theme.text_muted).unwrap_or(Color::DarkGray);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border))
            .title(Span::styled(" digest ", Style::default().fg(muted)));
        let inner = block.inner(area);
        (inner, block)
    }
}

impl<'a> Widget for SessionDigestPanel<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width < 4 || area.height < 2 {
            // Too small to draw anything meaningful — skip silently.
            return;
        }

        let (inner, block) = Self::inner(area, self.theme);
        block.render(area, buf);

        if self.selected_item.is_none() {
            // Empty state: single muted line, centered within the inner box.
            let muted = hex_to_color(&self.theme.text_muted).unwrap_or(Color::DarkGray);
            let p = Paragraph::new(Line::from(Span::styled(
                "Select a session to see its digest.",
                Style::default().fg(muted),
            )))
            .alignment(Alignment::Center);
            // Center vertically too — slot the line at roughly mid-height.
            let mid_y = inner.y + inner.height / 2;
            let one_row = Rect {
                x: inner.x,
                y: mid_y,
                width: inner.width,
                height: 1,
            };
            p.render(one_row, buf);
            return;
        }

        // Populated state.
        let item = self.selected_item.unwrap();
        let theme = self.theme;
        let fg = hex_to_color(&theme.foreground).unwrap_or(Color::White);
        let muted = hex_to_color(&theme.text_muted).unwrap_or(Color::DarkGray);
        let gutter_color = hex_to_color(&theme.border_active).unwrap_or(muted);

        // We render top-down using a manual row cursor inside `inner`.
        let mut y = inner.y;
        let max_y = inner.y.saturating_add(inner.height);
        let x = inner.x;
        let w = inner.width;

        // Helper closures avoid the boilerplate of constructing Rect + Paragraph
        // per row. Each emits one row and bumps `y`.
        let mut render_row = |line: Line<'static>, y: &mut u16| {
            if *y >= max_y {
                return;
            }
            let rect = Rect {
                x,
                y: *y,
                width: w,
                height: 1,
            };
            Paragraph::new(line).render(rect, buf);
            *y = y.saturating_add(1);
        };

        // ---------- Identity block ----------
        // Title (display_name) — bold foreground.
        render_row(
            Line::from(Span::styled(
                truncate(&item.display_name, w as usize),
                Style::default().fg(fg).add_modifier(Modifier::BOLD),
            )),
            &mut y,
        );
        // Slug — first 8 chars of session id, muted.
        let slug = short_id(&item.session_id);
        render_row(
            Line::from(Span::styled(
                slug.clone(),
                Style::default().fg(muted),
            )),
            &mut y,
        );

        // ---------- Reserve footer height before subsequent blocks so the
        // resume command always fits. Footer is 2 rows (divider + command).
        let footer_reserve: u16 = 2;
        let body_max_y = max_y.saturating_sub(footer_reserve);

        // 1-row gap.
        if y < body_max_y {
            y = y.saturating_add(1);
        }

        // ---------- Recap block ----------
        if y < body_max_y {
            render_row(
                Line::from(Span::styled(
                    "Recap",
                    Style::default().fg(fg).add_modifier(Modifier::BOLD),
                )),
                &mut y,
            );
        }
        if y < body_max_y {
            // Band: gutter `▌` + space + body text. Body is the newest_decision
            // or a muted-italic placeholder.
            let (body_text, body_style) = match self.summary.and_then(|s| s.newest_decision.as_deref()) {
                Some(text) => (
                    truncate(text, w.saturating_sub(2) as usize),
                    Style::default().fg(fg),
                ),
                None => (
                    "(no recap yet)".to_string(),
                    Style::default().fg(muted).add_modifier(Modifier::ITALIC),
                ),
            };
            render_row(
                Line::from(vec![
                    Span::styled("▌", Style::default().fg(gutter_color)),
                    Span::raw(" "),
                    Span::styled(body_text, body_style),
                ]),
                &mut y,
            );
        }

        // 1-row gap.
        if y < body_max_y {
            y = y.saturating_add(1);
        }

        // ---------- Outputs block ----------
        if y < body_max_y {
            render_row(
                Line::from(Span::styled(
                    "Outputs",
                    Style::default().fg(fg).add_modifier(Modifier::BOLD),
                )),
                &mut y,
            );
        }
        // Collect populated output rows from the digest. Empty → single `—`.
        let mut output_rows: Vec<String> = Vec::new();
        if let Some(d) = self.digest {
            if let Some(pr) = d.pr_number {
                let mut line = format!("↗ PR #{pr}");
                if let Some(repo) = d.pr_repository.as_deref() {
                    line.push_str("  ");
                    line.push_str(repo);
                }
                output_rows.push(line);
            }
            if d.files_touched_count > 0 {
                let suffix = if d.files_touched_count == 1 { "" } else { "s" };
                output_rows.push(format!("✎ {} file{suffix}", d.files_touched_count));
            }
        }
        if output_rows.is_empty() {
            if y < body_max_y {
                render_row(
                    Line::from(Span::styled("—", Style::default().fg(muted))),
                    &mut y,
                );
            }
        } else {
            for row in output_rows {
                if y >= body_max_y {
                    break;
                }
                render_row(
                    Line::from(Span::styled(
                        truncate(&row, w as usize),
                        Style::default().fg(fg),
                    )),
                    &mut y,
                );
            }
        }

        // 1-row gap.
        if y < body_max_y {
            y = y.saturating_add(1);
        }

        // ---------- Context block ----------
        if y < body_max_y {
            render_row(
                Line::from(Span::styled(
                    "Context",
                    Style::default().fg(fg).add_modifier(Modifier::BOLD),
                )),
                &mut y,
            );
        }
        let mut context_rows: Vec<String> = Vec::new();
        if let Some(d) = self.digest {
            // Line 1: `<latest> / <window>  ·  NN% used`
            if let (Some(window), Some(ratio)) = (d.context_window, d.context_fill_ratio)
            {
                if d.latest_turn_input_tokens > 0 {
                    let pct = (ratio * 100.0).round() as u32;
                    context_rows.push(format!(
                        "{} / {}  ·  {pct}% used",
                        fmt_tokens(d.latest_turn_input_tokens),
                        fmt_tokens(window),
                    ));
                }
            }
            // Line 2: `input <in_total>  ·  output <out_total>`
            if d.total_input_tokens_billed > 0 || d.total_output_tokens > 0 {
                context_rows.push(format!(
                    "input {}  ·  output {}",
                    fmt_tokens(d.total_input_tokens_billed),
                    fmt_tokens(d.total_output_tokens),
                ));
            }
            // Line 3: `cache NN% hit`
            if let Some(hit) = d.cache_hit_ratio {
                let pct = (hit * 100.0).round() as u32;
                context_rows.push(format!("cache {pct}% hit"));
            }
            // Line 4: `model1 · model2`
            if !d.models_used.is_empty() {
                let labels: Vec<String> = d
                    .models_used
                    .iter()
                    .filter(|m| !m.is_empty() && m.as_str() != "<synthetic>")
                    .map(|m| short_model(m))
                    .collect();
                if !labels.is_empty() {
                    context_rows.push(labels.join(" · "));
                }
            }
        }
        if context_rows.is_empty() {
            if y < body_max_y {
                render_row(
                    Line::from(Span::styled("—", Style::default().fg(muted))),
                    &mut y,
                );
            }
        } else {
            for row in context_rows {
                if y >= body_max_y {
                    break;
                }
                render_row(
                    Line::from(Span::styled(
                        truncate(&row, w as usize),
                        Style::default().fg(fg),
                    )),
                    &mut y,
                );
            }
        }

        // ---------- Footer (pinned to bottom) ----------
        // Divider (8 `─` cells in border_active) + resume command, anchored
        // at the bottom of `inner`. We compute the y rather than continuing
        // the cursor so the footer stays glued to the bottom edge — Python's
        // bar achieves this via `VerticalScroll` flow + the resume row being
        // the last mounted child.
        let footer_top = max_y.saturating_sub(footer_reserve);
        if footer_top >= inner.y {
            let divider_rect = Rect {
                x,
                y: footer_top,
                width: w,
                height: 1,
            };
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(8.min(w as usize)),
                Style::default().fg(gutter_color),
            )))
            .render(divider_rect, buf);

            let cmd_rect = Rect {
                x,
                y: footer_top.saturating_add(1),
                width: w,
                height: 1,
            };
            if cmd_rect.y < max_y {
                let resume = format!("claude -r {}…", slug);
                Paragraph::new(Line::from(Span::styled(
                    resume,
                    Style::default()
                        .fg(muted)
                        .add_modifier(Modifier::ITALIC),
                )))
                .render(cmd_rect, buf);
            }
        }
    }
}

// ---------------------------------------------------------------- helpers

/// First 8 chars of a session id, mirroring Python's `sid[:8]` slug.
#[allow(dead_code)]
fn short_id(sid: &str) -> String {
    sid.chars().take(8).collect()
}

/// Truncate `s` to at most `max` chars, appending `…` if shortened. Mirrors
/// `digest_bar::truncate` — kept local so the panel widget has no cross-file
/// helper dep.
#[allow(dead_code)]
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Format a token count for the Context block:
/// * < 1,000 → "<n>"
/// * < 1,000,000 → "<n.n>k"
/// * ≥ 1,000,000 → "<n.n>M"
///
/// Mirrors the Python `_fmt_tokens` in `session_digest_bar.py`.
#[allow(dead_code)]
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Compact model label — `claude-opus-4-7[1m]` → `Opus 4.7 1m`.
/// Mirrors Python's `_short_model` heuristic; unknown ids fall through
/// with a 18-char cap so a stray model name can't blow the column.
#[allow(dead_code)]
fn short_model(model: &str) -> String {
    let raw = model.trim();
    if raw.is_empty() {
        return String::new();
    }
    let lower = raw.to_ascii_lowercase();

    // Split off a `[<suffix>]` tail if present (e.g. `[1m]`).
    let (body_lower, body_orig, suffix) =
        if let Some(start) = lower.rfind('[') {
            if lower.ends_with(']') {
                let tail = &raw[start + 1..raw.len() - 1];
                let body = raw[..start].trim();
                let body_lower = body.to_ascii_lowercase();
                (body_lower, body.to_string(), format!(" {tail}"))
            } else {
                (lower.clone(), raw.to_string(), String::new())
            }
        } else {
            (lower.clone(), raw.to_string(), String::new())
        };
    let _ = body_orig; // currently unused after split; kept for clarity.

    for (family, label) in &[
        ("opus", "Opus"),
        ("sonnet", "Sonnet"),
        ("haiku", "Haiku"),
    ] {
        if let Some(idx) = body_lower.find(family) {
            let after = &body_lower[idx + family.len()..];
            let digits: String = after
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '-')
                .collect();
            let cleaned = digits.trim_matches('-').replace("--", "-");
            let version = if cleaned.is_empty() {
                String::new()
            } else {
                let parts: Vec<&str> = cleaned.split('-').collect();
                let mut v = parts.first().copied().unwrap_or("").to_string();
                if parts.len() > 1 && !parts[1].is_empty() {
                    v.push('.');
                    v.push_str(parts[1]);
                }
                v
            };
            let mut out = label.to_string();
            if !version.is_empty() {
                out.push(' ');
                out.push_str(&version);
            }
            out.push_str(&suffix);
            return out;
        }
    }
    if raw.chars().count() > 18 {
        let cut: String = raw.chars().take(17).collect();
        return format!("{cut}…");
    }
    raw.to_string()
}

/// Parse `#rrggbb` into a `ratatui::style::Color`. Returns `None` for any
/// malformed input — callers fall back to a sensible default.
///
/// Duplicated from `digest_bar` per the parity-plan convention: each widget
/// keeps its own copy so widget files stay self-contained and don't grow a
/// cross-cutting `theme::color` module just for a 10-line helper.
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

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use threadhop_core::observations::ObservationSummary;

    fn theme() -> Theme {
        Theme::default_dark()
    }

    fn buffer_to_string(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn row_contains_fg(buf: &Buffer, y: u16, color: Color) -> bool {
        (0..buf.area.width)
            .any(|x| buf.cell((x, y)).map(|c| c.fg == color).unwrap_or(false))
    }

    /// Helper: render a panel directly (no leaking) — preferred when the
    /// borrowed refs live for the duration of the test scope.
    fn render_panel_into<'a>(
        buf_w: u16,
        buf_h: u16,
        theme: &'a Theme,
        item: Option<&'a SessionListItem>,
        summary: Option<&'a ObservationSummary>,
    ) -> Buffer {
        render_panel_with_digest(buf_w, buf_h, theme, item, summary, None)
    }

    fn render_panel_with_digest<'a>(
        buf_w: u16,
        buf_h: u16,
        theme: &'a Theme,
        item: Option<&'a SessionListItem>,
        summary: Option<&'a ObservationSummary>,
        digest: Option<&'a SessionDigest>,
    ) -> Buffer {
        let mut term = Terminal::new(TestBackend::new(buf_w, buf_h)).unwrap();
        term.draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: buf_w,
                height: buf_h,
            };
            let panel = SessionDigestPanel {
                theme,
                selected_item: item,
                summary,
                digest,
            };
            f.render_widget(panel, area);
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    fn sample_item() -> SessionListItem {
        SessionListItem {
            session_id: "abcdef1234567890".into(),
            display_name: "my-cool-session".into(),
            is_active: true,
            ..Default::default()
        }
    }

    // ----- frame-buffer tests -----

    #[test]
    fn panel_renders_title_in_foreground_when_session_selected() {
        let t = theme();
        let item = sample_item();
        let buf = render_panel_into(36, 40, &t, Some(&item), None);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("my-cool-session"),
            "expected display_name in panel; got:\n{dump}"
        );
        // At least one cell in the panel must carry the theme foreground
        // color (the title row renders the name in `fg + bold`).
        let fg = hex_to_color(&t.foreground).unwrap();
        let has_fg_cell = (0..buf.area.height).any(|y| row_contains_fg(&buf, y, fg));
        assert!(
            has_fg_cell,
            "no foreground-colored cell found in panel — title invisible"
        );
    }

    #[test]
    fn panel_renders_recap_placeholder_when_no_summary() {
        let t = theme();
        let item = sample_item();
        let buf = render_panel_into(36, 40, &t, Some(&item), None);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("(no recap yet)"),
            "expected recap placeholder; got:\n{dump}"
        );
        assert!(
            dump.contains("Recap"),
            "expected Recap section header; got:\n{dump}"
        );
    }

    #[test]
    fn panel_renders_recap_text_when_summary_has_decision() {
        let t = theme();
        let item = sample_item();
        let summary = ObservationSummary {
            newest_decision: Some("switch to fts5 for snippet search".into()),
            ..Default::default()
        };
        let buf = render_panel_into(50, 40, &t, Some(&item), Some(&summary));
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("switch to fts5"),
            "expected decision text in recap; got:\n{dump}"
        );
        // Placeholder must NOT appear when a decision is present.
        assert!(
            !dump.contains("(no recap yet)"),
            "placeholder leaked through with decision present;\n{dump}"
        );
    }

    #[test]
    fn panel_renders_resume_command_in_footer() {
        let t = theme();
        let item = sample_item();
        let buf = render_panel_into(36, 40, &t, Some(&item), None);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("claude -r"),
            "expected resume command; got:\n{dump}"
        );
        // The 8-char slug should also appear.
        assert!(
            dump.contains("abcdef12"),
            "expected sid[:8] slug; got:\n{dump}"
        );
    }

    #[test]
    fn panel_renders_empty_state_when_no_session_selected() {
        let t = theme();
        let buf = render_panel_into(36, 40, &t, None, None);
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("Select a session"),
            "expected empty-state copy; got:\n{dump}"
        );
        // No identity / recap / footer should appear in the empty state.
        assert!(
            !dump.contains("Recap"),
            "empty state must not render section headers; got:\n{dump}"
        );
        assert!(
            !dump.contains("claude -r"),
            "empty state must not render resume cmd; got:\n{dump}"
        );
    }

    #[test]
    fn panel_renders_outputs_and_context_stubs() {
        let t = theme();
        let item = sample_item();
        let buf = render_panel_into(36, 40, &t, Some(&item), None);
        let dump = buffer_to_string(&buf);
        assert!(dump.contains("Outputs"), "expected Outputs header; {dump}");
        assert!(dump.contains("Context"), "expected Context header; {dump}");
        // The em-dash stub must appear (we don't have real data yet).
        assert!(dump.contains("—"), "expected stub em-dash; {dump}");
    }

    #[test]
    fn outputs_block_renders_pr_number_when_present() {
        let t = theme();
        let item = sample_item();
        let digest = SessionDigest {
            session_id: item.session_id.clone(),
            pr_number: Some(42),
            pr_repository: Some("me/proj".into()),
            ..Default::default()
        };
        let buf = render_panel_with_digest(50, 40, &t, Some(&item), None, Some(&digest));
        let dump = buffer_to_string(&buf);
        assert!(
            dump.contains("PR #42"),
            "expected PR # row; got:\n{dump}"
        );
        assert!(
            dump.contains("me/proj"),
            "expected repo suffix; got:\n{dump}"
        );
    }

    #[test]
    fn context_block_renders_token_totals_when_present() {
        let t = theme();
        let item = sample_item();
        let digest = SessionDigest {
            session_id: item.session_id.clone(),
            context_window: Some(200_000),
            latest_turn_input_tokens: 50_000,
            context_fill_ratio: Some(0.25),
            total_input_tokens_billed: 120_000,
            total_output_tokens: 5_000,
            cache_hit_ratio: Some(0.8),
            models_used: vec!["claude-opus-4-7".into()],
            ..Default::default()
        };
        let buf = render_panel_with_digest(50, 40, &t, Some(&item), None, Some(&digest));
        let dump = buffer_to_string(&buf);
        // Headline tokens line: `50.0k / 200.0k  ·  25% used`
        assert!(
            dump.contains("50.0k") && dump.contains("200.0k"),
            "expected token totals; got:\n{dump}"
        );
        assert!(dump.contains("% used"), "expected fill pct; got:\n{dump}");
        assert!(dump.contains("input "), "expected input line; got:\n{dump}");
        assert!(dump.contains("output "), "expected output line; got:\n{dump}");
        assert!(dump.contains("cache "), "expected cache line; got:\n{dump}");
        assert!(dump.contains("hit"), "expected hit suffix; got:\n{dump}");
        assert!(dump.contains("Opus"), "expected short model; got:\n{dump}");
    }

    #[test]
    fn panel_renders_em_dash_when_digest_is_default() {
        // A default digest has no PR / no files / no tokens — both blocks
        // must fall back to the em-dash placeholder.
        let t = theme();
        let item = sample_item();
        let digest = SessionDigest::default();
        let buf = render_panel_with_digest(36, 40, &t, Some(&item), None, Some(&digest));
        let dump = buffer_to_string(&buf);
        assert!(dump.contains("—"), "expected em-dash; got:\n{dump}");
        // Sanity — neither populated row should appear.
        assert!(!dump.contains("PR #"), "stray PR row; got:\n{dump}");
        assert!(!dump.contains("file"), "stray files row; got:\n{dump}");
    }

    // ----- helper tests -----

    #[test]
    fn fmt_tokens_uses_k_suffix() {
        assert_eq!(fmt_tokens(0), "0");
        assert_eq!(fmt_tokens(999), "999");
        assert_eq!(fmt_tokens(1_500), "1.5k");
        assert_eq!(fmt_tokens(120_000), "120.0k");
        assert_eq!(fmt_tokens(2_500_000), "2.5M");
    }

    #[test]
    fn short_model_collapses_family_names() {
        assert_eq!(short_model("claude-opus-4-7"), "Opus 4.7");
        assert_eq!(short_model("claude-haiku-4-5"), "Haiku 4.5");
        assert_eq!(short_model("claude-opus-4-7[1m]"), "Opus 4.7 1m");
        // Unknown id capped at 18 chars.
        let long = short_model("some-truly-massive-model-id");
        assert!(long.chars().count() <= 18, "got {long}");
    }

    #[test]
    fn panel_truncates_long_decision_to_fit_width() {
        let t = theme();
        let item = sample_item();
        let long = "x".repeat(200);
        let summary = ObservationSummary {
            newest_decision: Some(long),
            ..Default::default()
        };
        let buf = render_panel_into(36, 40, &t, Some(&item), Some(&summary));
        let dump = buffer_to_string(&buf);
        // Ellipsis should appear — the decision is way wider than 36 cells.
        assert!(
            dump.contains("…"),
            "expected ellipsis on truncated recap; got:\n{dump}"
        );
    }

    #[test]
    fn panel_does_not_panic_at_tiny_size() {
        // The widget must gracefully no-op (or partially render) at sizes
        // below the inner-area threshold. Smoke test only.
        let t = theme();
        let item = sample_item();
        let _ = render_panel_into(4, 3, &t, Some(&item), None);
        let _ = render_panel_into(1, 1, &t, Some(&item), None);
    }

}
