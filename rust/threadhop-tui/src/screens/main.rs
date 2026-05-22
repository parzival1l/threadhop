//! Main screen — wires session_list (sidebar) + transcript (right pane) +
//! contextual_footer (bottom).
//!
//! Layout (per design spec §4):
//! - Vertical: content area (fills) + 1-line footer (fixed).
//! - Within content: horizontal 36-char sidebar + remaining transcript pane.
//!
//! Wave E wiring: the App holds `Vec<SessionListItem>` populated by the
//! `session_scanner` worker via `SessionsRefreshed`. The screen renders it
//! straight through — no derivation.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders},
    Frame,
};
use threadhop_core::theme::{blend, hex_to_rgb};

use crate::anim::Easing;
use crate::app::{App, PaneFocus, MODAL_BACKDROP_ALPHA, MODAL_FADE_DURATION};
use crate::widgets::{
    contextual_footer::ContextualFooterWidget,
    digest_bar::{DigestBarContext, DigestBarWidget},
    find_bar::FindBarWidget,
    session_digest_panel::SessionDigestPanel,
    session_list::SessionListWidget,
    transcript::TranscriptWidget,
};

/// Sidebar width in cells. Matches the Python TUI's fixed column.
const SIDEBAR_WIDTH: u16 = 36;

/// Right-column digest panel width. Same fixed value as the Python grid
/// (`grid-columns: 36 1fr 36` in `app.tcss`).
const DIGEST_PANEL_WIDTH: u16 = 36;

/// Minimum terminal width before the right-column digest panel appears.
/// Below this we collapse back to `[sidebar, transcript]` so narrow
/// terminals still get a usable transcript pane. Picked from the parity
/// plan (§5 Open Questions #7).
const DIGEST_PANEL_MIN_WIDTH: u16 = 110;

/// Render one frame of the main screen.
///
/// The function takes `&App` (no mutation) — anything that would need a
/// mutable borrow (e.g. spinner tick, selection clamping) belongs in the
/// event-loop tick, not the renderer.
pub fn draw(app: &App, frame: &mut Frame) {
    let area = frame.area();

    // Outer: digest bar (Length 1) + content (Min 1) + footer (Length 1).
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    // Phase 5 Wave 2: digest bar for the currently-selected session.
    let summary = app
        .selected_session_id
        .as_deref()
        .and_then(|sid| app.digest_summary_cache.get(sid));
    let selected_item = app
        .selected_session_id
        .as_deref()
        .and_then(|sid| app.sidebar.iter().find(|i| i.session_id == sid));
    let session_display_name = selected_item.map(|i| i.display_name.as_str());
    let context = selected_item.map(|i| DigestBarContext {
        last_active_at: i.last_active_at,
        is_active: i.is_active,
        is_working: i.is_working,
    });
    let has_bookmarks = app
        .selected_session_id
        .as_deref()
        .map(|sid| app.has_bookmarks_for_session.contains(sid))
        .unwrap_or(false);
    let digest = DigestBarWidget {
        theme: &app.theme,
        summary,
        session_display_name,
        has_bookmarks,
        context,
    };
    frame.render_widget(digest, outer[0]);

    // Content row: 36-char sidebar + transcript + optional 36-char digest panel.
    //
    // Phase C (minimal): the right-column digest panel appears whenever the
    // terminal is at least `DIGEST_PANEL_MIN_WIDTH` cells wide. Below that
    // threshold we collapse back to the original two-column layout so the
    // transcript pane keeps a usable width on narrow terminals (e.g. side
    // panes / split tmux windows).
    let show_digest_panel = frame.area().width >= DIGEST_PANEL_MIN_WIDTH;
    let content = if show_digest_panel {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(SIDEBAR_WIDTH),
                Constraint::Min(1),
                Constraint::Length(DIGEST_PANEL_WIDTH),
            ])
            .split(outer[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(1)])
            .split(outer[1])
    };

    // Sidebar — view-model already populated by the session_scanner worker.
    // Divide the 60fps render-tick counter by 5 → ~12fps spinner motion,
    // which feels fluid without being twitchy.
    const SPINNER_DIVISOR: usize = 5;
    let sidebar = SessionListWidget {
        items: &app.sidebar,
        selected_session_id: app.selected_session_id.as_deref(),
        spinner_frame: app.spinner_tick / SPINNER_DIVISOR,
        now: now_epoch(),
        theme: Some(&app.theme),
        focused: app.pane_focus == PaneFocus::Sidebar,
        row_layout: Some(&app.last_sidebar_rows),
    };
    // Phase E: remember the sidebar rect so the mouse dispatcher can
    // translate a click row into a session index.
    app.last_sidebar_rect.set(content[0]);
    frame.render_widget(sidebar, content[0]);

    // Wave 1 E4: outer Block around the transcript pane with a
    // focus-aware border color. When the transcript pane is focused the
    // border lights up in `theme.accent`; otherwise it falls back to
    // `theme.border_subtle` so it's visible but quiet. This mirrors the
    // sidebar's right-border focus indicator from Phase E.
    let transcript_focused = app.pane_focus == PaneFocus::Transcript;
    let transcript_border_color = if transcript_focused {
        match hex_to_rgb(&app.theme.accent) {
            Some((r, g, b)) => Color::Rgb(r, g, b),
            None => Color::Magenta,
        }
    } else {
        match hex_to_rgb(&app.theme.border_subtle) {
            Some((r, g, b)) => Color::Rgb(r, g, b),
            None => Color::DarkGray,
        }
    };
    let transcript_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(transcript_border_color));
    let block_outer = content[1];
    let block_inner = transcript_block.inner(block_outer);
    frame.render_widget(transcript_block, block_outer);

    // Transcript pane. When the find bar is open, reserve the bottom row of
    // the transcript pane (inside the block) for it so the bar sits flush
    // with the bottom border.
    let has_find = app.find_state.is_some();
    let transcript_area = if has_find && block_inner.height > 1 {
        ratatui::layout::Rect {
            x: block_inner.x,
            y: block_inner.y,
            width: block_inner.width,
            height: block_inner.height - 1,
        }
    } else {
        block_inner
    };
    // Phase A fix-up: record the transcript pane height so
    // `App::scroll_selection_into_view` can decide whether the selection
    // cursor needs to be scrolled into the viewport. Updated every frame —
    // resizes are picked up on the next render.
    app.last_transcript_height.set(transcript_area.height);
    let transcript = TranscriptWidget::new(&app.transcript, app.scroll, &app.theme)
        .find_state(app.find_state.as_ref())
        .message_cursor(Some(app.message_cursor))
        .selection(app.selection_state);
    // Phase E: remember the transcript rect so the mouse dispatcher can
    // route scroll-wheel events to it.
    app.last_transcript_rect.set(transcript_area);
    frame.render_widget(transcript, transcript_area);

    if has_find && block_inner.height > 1 {
        let bar_area = ratatui::layout::Rect {
            x: block_inner.x,
            y: block_inner.y + block_inner.height - 1,
            width: block_inner.width,
            height: 1,
        };
        // Phase E: stamp the find-bar rect + the mouse cursor so the bar
        // can compute the hover/click state for its `×` close glyph.
        app.last_find_bar_rect.set(bar_area);
        let bar = FindBarWidget::new(app.find_state.as_ref().unwrap(), &app.theme)
            .mouse_cursor(app.mouse_cursor);
        frame.render_widget(bar, bar_area);
    } else {
        app.last_find_bar_rect
            .set(ratatui::layout::Rect::default());
    }

    // Phase C (minimal): right-column session digest panel. Reuses the same
    // `selected_item` and `summary` references already resolved above for the
    // top digest bar — the App holds the data; both widgets just read.
    if show_digest_panel {
        let panel_area = content[2];
        let panel = SessionDigestPanel {
            theme: &app.theme,
            selected_item,
            summary,
        };
        frame.render_widget(panel, panel_area);
    }

    // Footer — scope-aware, surfaces read-only and status banner.
    let footer = ContextualFooterWidget::new(app.scope, &app.theme)
        .read_only(app.read_only)
        .status(app.status_message.as_deref());
    frame.render_widget(footer, outer[2]);

    // Phase D: modal backdrop fade-in. Identify the topmost modal's
    // `opened_at` (z-order matches the if-chain below), compute the
    // current alpha via `EaseOutCubic` over MODAL_FADE_DURATION, and paint
    // a blended overlay across the FULL frame area. Each modal's own
    // `Clear` call wipes the modal rect, leaving only the cells outside
    // the modal carrying the dim — that's exactly the backdrop tint the
    // spec calls for.
    if let Some(opened_at) = topmost_modal_opened_at(app) {
        let alpha = compute_fade_alpha(opened_at, app);
        if alpha > 0.0 {
            paint_backdrop(frame, area, alpha, app);
        }
    }

    // Modals are stacked in z-order (later = on top). `ratatui::widgets::Clear`
    // (called inside each modal's draw) wipes the background, so the
    // underlying main layout never bleeds through. The confirm modal lands
    // last because it stacks over the bookmark browser during delete flow.
    if let Some(state) = app.bookmark_browser.as_ref() {
        let modal_area =
            crate::screens::bookmark_browser::centered_rect(80, 70, frame.area());
        crate::screens::bookmark_browser::draw(
            state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
    if let Some(state) = app.label_prompt.as_ref() {
        let modal_area =
            crate::screens::label_prompt::centered_rect(60, 60, frame.area());
        crate::screens::label_prompt::draw(
            state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
    if let Some(state) = app.kanban.as_ref() {
        let modal_area =
            crate::screens::kanban::centered_rect(95, 90, frame.area());
        crate::screens::kanban::draw(
            state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
    if let Some(state) = app.conflict_viewer.as_ref() {
        let modal_area =
            crate::screens::conflict_viewer::centered_rect(85, 75, frame.area());
        crate::screens::conflict_viewer::draw(
            state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
    if let Some(search_state) = app.search.as_ref() {
        let modal_area =
            crate::screens::search::centered_rect(70, 60, frame.area());
        crate::screens::search::draw(
            search_state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
    // Confirm is the top layer — it stacks over the bookmark browser when
    // delete is pending.
    if let Some(req) = app.confirm.as_ref() {
        // Small popup — confirm only needs ~5 rows.
        let modal_area =
            crate::screens::confirm::centered_rect(50, 30, frame.area());
        crate::screens::confirm::draw(
            &req.state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
    // Help overlay sits above every other modal — the user can pop it open
    // from any scope.
    if let Some(state) = app.help.as_ref() {
        let modal_area =
            crate::screens::help::centered_rect(70, 80, frame.area());
        crate::screens::help::draw(
            state,
            &app.theme,
            modal_area,
            frame.buffer_mut(),
        );
    }
}

/// Current unix timestamp in seconds. Defined here (not in the widget) so
/// tests of the widget stay deterministic — the widget takes `now` by
/// parameter; only the live renderer reads the wall clock.
fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Phase D: when did the topmost modal open? The App stamps
/// `modal_opened_at` on every modal-open path so we don't have to
/// reach into per-modal state structs (one of which lives in a file
/// with uncommitted in-flight changes from another session). Stacking
/// a confirm over the bookmark browser re-stamps the timer so the
/// backdrop fades in again from 0 alpha — exactly what we want.
fn topmost_modal_opened_at(app: &App) -> Option<Instant> {
    app.modal_opened_at
}

/// Phase D: sample the fade-in alpha given the modal's `opened_at`.
/// Honors `app.no_anim` (env override or test-mode default) by snapping
/// straight to the peak alpha.
pub(crate) fn compute_fade_alpha(opened_at: Instant, app: &App) -> f32 {
    if app.no_anim {
        return MODAL_BACKDROP_ALPHA;
    }
    let now = app.clock.now();
    if now <= opened_at {
        return 0.0;
    }
    let elapsed = now.duration_since(opened_at);
    if elapsed >= MODAL_FADE_DURATION {
        return MODAL_BACKDROP_ALPHA;
    }
    let t = elapsed.as_secs_f32() / MODAL_FADE_DURATION.as_secs_f32();
    let eased = Easing::EaseOutCubic.apply(t);
    eased * MODAL_BACKDROP_ALPHA
}

/// Phase D: paint a uniform blended background across the entire frame
/// area. Cells outside the modal rect keep this tint; cells inside get
/// overwritten by the modal's own `Clear`. Uses `theme::blend` to mix
/// foreground into background by `alpha`, exactly the same primitive the
/// selection-mode tint uses.
fn paint_backdrop(frame: &mut Frame, area: Rect, alpha: f32, app: &App) {
    let blended_hex = blend(&app.theme.foreground, &app.theme.background, alpha);
    let bg = match hex_to_rgb(&blended_hex) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => return,
    };
    let buf = frame.buffer_mut();
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_bg(bg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn draw_renders_empty_app_without_panic() {
        // The default App has no sessions and no transcript — the screen
        // should still lay out cleanly with sidebar + transcript + footer.
        let app = App::new();
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
    }

    #[test]
    fn draw_renders_tiny_buffer_without_panic() {
        // Layouts must degrade gracefully — a 10x5 terminal still needs to
        // produce a valid frame even though only fragments of each pane fit.
        let app = App::new();
        let mut term = Terminal::new(TestBackend::new(10, 5)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
    }

    #[test]
    fn digest_bar_first_row_has_visible_content_even_without_observations() {
        // Regression: when no observation summary exists for a session, the
        // digest bar row used to collapse to invisible whitespace. Verify
        // that row 0 (the digest bar) carries the session display name and
        // a visible status hint without any observation cache.
        use crate::widgets::session_list::SessionListItem;
        let mut app = App::new();
        app.sidebar = vec![SessionListItem {
            session_id: "sess-x".into(),
            display_name: "my-cool-session".into(),
            is_active: true,
            last_active_at: Some(0.0),
            ..Default::default()
        }];
        app.selected_session_id = Some("sess-x".into());
        // No digest_summary_cache entry — the empty-state path is what we
        // expect when the Python observer hasn't run yet.
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
        let buf = term.backend().buffer();
        let mut row0 = String::new();
        for x in 0..buf.area().width {
            row0.push_str(buf[(x, 0)].symbol());
        }
        assert!(
            row0.contains("my-cool-session"),
            "digest bar row 0 must show session name; got: {row0:?}"
        );
        // The visible content must not be pure whitespace — at least one
        // non-space glyph (the status icon or name) must be present.
        assert!(
            row0.chars().any(|c| !c.is_whitespace()),
            "digest bar row 0 had only whitespace; got: {row0:?}"
        );
    }

    #[test]
    fn digest_panel_renders_on_wide_terminal() {
        // Phase C: at >= 110 cells wide, the right-column digest panel must
        // appear and carry the selected session's name. Frame-buffer test —
        // we render and read cells back, not state.
        use crate::widgets::session_list::SessionListItem;
        let mut app = App::new();
        app.sidebar = vec![SessionListItem {
            session_id: "abcdef1234".into(),
            display_name: "wide-term-session".into(),
            is_active: true,
            ..Default::default()
        }];
        app.selected_session_id = Some("abcdef1234".into());
        let mut term = Terminal::new(TestBackend::new(160, 40)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
        let buf = term.backend().buffer();
        // Sample the panel area: it occupies the rightmost 36 cells (x =
        // 160-36 = 124..160), rows 1..H-1 (between top digest + footer).
        let mut panel_dump = String::new();
        for y in 1..buf.area().height - 1 {
            for x in 124..buf.area().width {
                panel_dump.push_str(buf[(x, y)].symbol());
            }
            panel_dump.push('\n');
        }
        assert!(
            panel_dump.contains("wide-term-session"),
            "digest panel did not render session name on wide terminal; got:\n{panel_dump}"
        );
        assert!(
            panel_dump.contains("digest"),
            "digest panel title missing on wide terminal; got:\n{panel_dump}"
        );
        assert!(
            panel_dump.contains("claude -r"),
            "digest panel resume command missing on wide terminal; got:\n{panel_dump}"
        );
    }

    #[test]
    fn digest_panel_hidden_on_narrow_terminal() {
        // Phase C narrow-fallback: at < 110 cells wide the right-column
        // panel must not render. The transcript pane fills the remainder
        // after the sidebar.
        use crate::widgets::session_list::SessionListItem;
        let mut app = App::new();
        app.sidebar = vec![SessionListItem {
            session_id: "abcdef1234".into(),
            display_name: "narrow-term-session".into(),
            is_active: true,
            ..Default::default()
        }];
        app.selected_session_id = Some("abcdef1234".into());
        // 100 < 110 — fallback path.
        let mut term = Terminal::new(TestBackend::new(100, 40)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
        let buf = term.backend().buffer();
        // Scan all rows (except the top digest bar at y=0 and footer at
        // y=H-1) for the panel title " digest " — must NOT appear.
        let mut body_dump = String::new();
        for y in 1..buf.area().height - 1 {
            for x in 0..buf.area().width {
                body_dump.push_str(buf[(x, y)].symbol());
            }
            body_dump.push('\n');
        }
        assert!(
            !body_dump.contains(" digest "),
            "digest panel must be hidden below 110 cols but title appeared; got:\n{body_dump}"
        );
        assert!(
            !body_dump.contains("claude -r"),
            "digest panel must be hidden below 110 cols but resume cmd appeared"
        );
    }

    #[test]
    fn draw_renders_footer_with_quit_hint() {
        // The bottom row must show the global `quit` hint so the user knows
        // how to exit. Sanity-checks the footer wiring without coupling to
        // specific column positions.
        let mut app = App::new();
        // Clear the Phase 0 boot status_message so the footer renders the
        // binding hints (a non-empty status takes priority over hints).
        app.status_message = None;
        let mut term = Terminal::new(TestBackend::new(80, 5)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
        let buf = term.backend().buffer();
        let mut footer_row = String::new();
        let last = buf.area().height - 1;
        for x in 0..buf.area().width {
            footer_row.push_str(buf[(x, last)].symbol());
        }
        assert!(
            footer_row.contains("quit"),
            "expected quit hint on footer row, got {footer_row:?}"
        );
    }

    // ---- Phase D: modal backdrop fade-in --------------------------------
    //
    // State-only checks for alpha, plus one frame-buffer test that the
    // backdrop region carries the blended bg color after the fade has
    // completed. The frame-buffer test is the one that actually catches
    // render-pipeline regressions (Phase A taught us state alone is not
    // enough).

    use crate::anim::Clock;
    use crate::screens::bookmark_browser as bb;
    use std::time::Instant;

    #[test]
    fn modal_fade_in_at_zero_elapsed_is_transparent() {
        // Pin the clock to opened_at; alpha is 0 at t=0.
        let mut app = App::new();
        app.no_anim = false;
        let now = Instant::now();
        app.clock = Clock::Frozen(now);
        app.modal_opened_at = Some(now);
        let alpha = compute_fade_alpha(now, &app);
        assert!(alpha.abs() < 1e-5, "alpha at t=0 should be ~0; got {alpha}");
    }

    #[test]
    fn modal_fade_in_after_80ms_is_full() {
        let mut app = App::new();
        app.no_anim = false;
        let now = Instant::now();
        app.clock = Clock::Frozen(now + MODAL_FADE_DURATION);
        let alpha = compute_fade_alpha(now, &app);
        assert!(
            (alpha - MODAL_BACKDROP_ALPHA).abs() < 1e-5,
            "alpha at t=duration should be {}; got {alpha}",
            MODAL_BACKDROP_ALPHA
        );
    }

    #[test]
    fn modal_with_no_anim_renders_full_alpha_immediately() {
        let app = App::new();
        // cfg(test) default is `no_anim = true`; this is the path we expect
        // headless tests to use unless they explicitly opt in.
        assert!(app.no_anim);
        let alpha = compute_fade_alpha(Instant::now(), &app);
        assert!(
            (alpha - MODAL_BACKDROP_ALPHA).abs() < 1e-5,
            "no_anim alpha must snap to peak; got {alpha}"
        );
    }

    // ---- Wave 1 E4: transcript pane focus border ------------------------

    #[test]
    fn transcript_pane_renders_block_with_accent_border_when_focused() {
        // When the transcript pane has focus, the outer Block painted
        // around it must render its border in `theme.accent`. We sweep
        // the perimeter of the transcript area (computed the same way
        // `draw` does) and require at least one cell to match.
        let mut app = App::new();
        app.pane_focus = PaneFocus::Transcript;
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
        let buf = term.backend().buffer();

        let (r, g, b) = hex_to_rgb(&app.theme.accent)
            .expect("default theme accent is valid hex");
        let want = Color::Rgb(r, g, b);

        // The transcript block lives at content[1]: x = SIDEBAR_WIDTH = 36.
        // Width = total - sidebar - digest_panel (when shown). For 120 cols,
        // digest panel shows (120 >= 110), so transcript x ∈ [36, 120-36) =
        // [36, 84). Vertical: rows 1..(H-1) (between digest bar + footer).
        let outer = ratatui::layout::Rect {
            x: 36,
            y: 1,
            width: 120 - 36 - 36,
            height: 24 - 2,
        };

        let mut found = false;
        // Top + bottom rows.
        for x in outer.x..outer.x + outer.width {
            for y in [outer.y, outer.y + outer.height - 1] {
                if buf[(x, y)].fg == want {
                    found = true;
                }
            }
        }
        // Left + right columns.
        for y in outer.y..outer.y + outer.height {
            for x in [outer.x, outer.x + outer.width - 1] {
                if buf[(x, y)].fg == want {
                    found = true;
                }
            }
        }
        assert!(
            found,
            "focused transcript block must paint at least one perimeter cell in accent"
        );
    }

    #[test]
    fn transcript_pane_renders_block_with_default_border_when_not_focused() {
        // When the sidebar has focus, the transcript block's border must
        // NOT use the accent color — instead it should fall back to the
        // theme's subtle border color. We confirm no perimeter cell paints
        // in accent, AND that at least one perimeter cell paints in the
        // border_subtle color (so the block is in fact drawn).
        let mut app = App::new();
        app.pane_focus = PaneFocus::Sidebar;
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
        let buf = term.backend().buffer();

        let (ar, ag, ab) = hex_to_rgb(&app.theme.accent)
            .expect("default theme accent is valid hex");
        let accent = Color::Rgb(ar, ag, ab);
        let (sr, sg, sb) = hex_to_rgb(&app.theme.border_subtle)
            .expect("default theme border_subtle is valid hex");
        let subtle = Color::Rgb(sr, sg, sb);

        let outer = ratatui::layout::Rect {
            x: 36,
            y: 1,
            width: 120 - 36 - 36,
            height: 24 - 2,
        };

        let mut saw_accent = false;
        let mut saw_subtle = false;
        for x in outer.x..outer.x + outer.width {
            for y in [outer.y, outer.y + outer.height - 1] {
                let fg = buf[(x, y)].fg;
                if fg == accent {
                    saw_accent = true;
                }
                if fg == subtle {
                    saw_subtle = true;
                }
            }
        }
        for y in outer.y..outer.y + outer.height {
            for x in [outer.x, outer.x + outer.width - 1] {
                let fg = buf[(x, y)].fg;
                if fg == accent {
                    saw_accent = true;
                }
                if fg == subtle {
                    saw_subtle = true;
                }
            }
        }
        assert!(
            !saw_accent,
            "unfocused transcript block must NOT paint perimeter cells in accent"
        );
        assert!(
            saw_subtle,
            "unfocused transcript block must paint perimeter cells in border_subtle"
        );
    }

    #[test]
    fn bookmark_browser_backdrop_carries_blended_bg_after_fade_completes() {
        // Frame-buffer test: open the bookmark browser, ensure the fade has
        // run to completion (no_anim default snaps to peak), and confirm
        // at least one cell *outside* the modal rect carries the blended
        // backdrop background color.
        let mut app = App::new();
        app.bookmark_browser = Some(bb::State::new());
        // Set modal_opened_at directly to simulate the open-path stamp.
        app.modal_opened_at = Some(Instant::now());
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(&app, f)).unwrap();
        let buf = term.backend().buffer();
        let blended_hex =
            blend(&app.theme.foreground, &app.theme.background, MODAL_BACKDROP_ALPHA);
        let (r, g, b) = hex_to_rgb(&blended_hex).expect("default theme colors are valid hex");
        let want = Color::Rgb(r, g, b);
        // Sample (0, 1) — row 1 is below the digest bar and outside the
        // centered modal rect for these dimensions.
        let cell = &buf[(0u16, 1u16)];
        assert_eq!(
            cell.bg, want,
            "expected blended backdrop bg at (0,1) after fade; got {:?}",
            cell.bg
        );
    }
}
