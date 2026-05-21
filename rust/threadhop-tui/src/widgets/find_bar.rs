// Wave B widget — wired into the App in a later wave. Until then the binary
// doesn't reference these helpers, so silence dead-code warnings for the
// module (same convention `transcript.rs` uses for its scaffolding fields).
#![allow(dead_code)]

//! Find-in-transcript bar — a single-line search-within-current-pane UI.
//!
//! Distinct from the FTS search modal which searches across *all* indexed
//! sessions: this widget only inspects the currently-loaded transcript and
//! highlights character ranges inside individual messages. Mirrors
//! `threadhop_core/tui/widgets/find_bar.py`.
//!
//! The widget is split into three layers so the Wave 2 App integration only
//! has to wire state + key events:
//!
//! 1. [`FindState`] — owns the query, the computed match positions, and the
//!    "current match" cursor. Recomputation is a free method so the App can
//!    call it whenever the transcript reloads.
//! 2. [`FindBarWidget`] — stateless renderer. Borrows the state and renders
//!    a single-line bar at the supplied `Rect`. If `active = false` it
//!    renders nothing, letting the App treat the find bar's height as 0.
//! 3. [`handle_key`] — input glue. Mutates the state in place and returns
//!    `Some(FindResult)` when the bar should close.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use threadhop_core::{
    jsonl::CleanedMessage,
    theme::{hex_to_rgb, Theme},
};

/// One occurrence of the query inside the transcript.
///
/// `char_offset` and `length` are **byte** offsets into the message's `text`
/// field. Using bytes (not chars) keeps the highlight overlay logic O(1)
/// when slicing into the `&str` body and matches the `str::find` return
/// type the matcher uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchPosition {
    /// Index into the transcript `Vec` the matcher was given.
    pub message_index: usize,
    /// Byte offset of the match start within `messages[message_index].text`.
    pub char_offset: usize,
    /// Byte length of the matched substring (== `query.len()` for plain
    /// ASCII matches; stored explicitly so callers don't have to re-derive
    /// it from the query).
    pub length: usize,
}

/// Mutable state owned by the App while the find bar is open.
#[derive(Debug, Default, Clone)]
pub struct FindState {
    /// Current search query — what the user has typed so far. Empty string
    /// means "no query"; matches will be empty.
    pub query: String,
    /// All match positions across the transcript, in document order. Filled
    /// by [`FindState::recompute_matches`].
    pub matches: Vec<MatchPosition>,
    /// Index into `matches` of the currently-selected match. Wraps via
    /// `next_match` / `prev_match`. Clamped to `< matches.len()` after a
    /// recompute (or reset to 0 if matches is empty).
    pub current_match_index: usize,
    /// Future toggle (Ctrl-S in Wave 2) — when false, matching is done
    /// against lowercase versions of both query and text.
    pub case_sensitive: bool,
}

/// Outcome the App acts on when [`handle_key`] returns `Some`.
#[derive(Debug, PartialEq, Eq)]
pub enum FindResult {
    /// Esc — close the bar and clear highlights.
    Closed,
    /// Enter — accept the current match and close. The App should scroll
    /// the transcript so this message is visible.
    JumpedToMatch {
        /// Index into the transcript slice that was passed to `handle_key`.
        message_index: usize,
    },
}

impl FindState {
    /// Recompute `matches` against the supplied transcript text. Should be
    /// called whenever:
    ///   - the query changes (typed character / backspace), or
    ///   - the loaded transcript changes (session switched, new turn).
    ///
    /// Empty query short-circuits to an empty match list. Case-insensitive
    /// matching is the default — see [`FindState::case_sensitive`] for the
    /// future toggle.
    pub fn recompute_matches(&mut self, transcript: &[CleanedMessage]) {
        self.matches.clear();
        if self.query.is_empty() {
            self.current_match_index = 0;
            return;
        }

        if self.case_sensitive {
            for (idx, msg) in transcript.iter().enumerate() {
                find_all_in(&msg.text, &self.query, idx, &mut self.matches);
            }
        } else {
            let needle = self.query.to_lowercase();
            for (idx, msg) in transcript.iter().enumerate() {
                // Lowercase the haystack to match the lowercased needle. We
                // assume the byte offsets in the lowercased string track
                // the originals — for ASCII (the common case) this is
                // exact; for code points whose lowercase form is a
                // different byte length (e.g. ß → ss) it can drift, but
                // that's an acceptable trade-off for a simple "find in
                // page" feature and matches the Python widget's behavior.
                let haystack_lower = msg.text.to_lowercase();
                find_all_in(&haystack_lower, &needle, idx, &mut self.matches);
            }
        }

        // Keep the cursor in-bounds after a recompute. If matches shrank
        // below the previous cursor (e.g. user typed another char and
        // dropped the count), snap back to the first match.
        if self.current_match_index >= self.matches.len() {
            self.current_match_index = 0;
        }
    }

    /// Cycle to the next match, wrapping around. No-op if there are no
    /// matches.
    pub fn next_match(&mut self) {
        if self.matches.is_empty() {
            self.current_match_index = 0;
            return;
        }
        self.current_match_index = (self.current_match_index + 1) % self.matches.len();
    }

    /// Cycle to the previous match, wrapping around. No-op if there are no
    /// matches.
    pub fn prev_match(&mut self) {
        if self.matches.is_empty() {
            self.current_match_index = 0;
            return;
        }
        if self.current_match_index == 0 {
            self.current_match_index = self.matches.len() - 1;
        } else {
            self.current_match_index -= 1;
        }
    }

    /// Returns the currently-highlighted match (if any). Convenience for
    /// the transcript widget — Wave 2 will call this to render the
    /// bg-yellow / fg-black overlay.
    pub fn current_match(&self) -> Option<MatchPosition> {
        self.matches.get(self.current_match_index).copied()
    }

    /// Total number of matches. Pulled out for the status line render so
    /// callers don't have to reach into the `matches` field.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// 1-based index of the current match for the status counter, or 0 when
    /// there are no matches. Centralizes the off-by-one between the
    /// 0-indexed cursor and the human-facing "3 of 7" display.
    pub fn current_display_index(&self) -> usize {
        if self.matches.is_empty() {
            0
        } else {
            self.current_match_index + 1
        }
    }
}

/// Find every occurrence of `needle` in `haystack` and push a
/// `MatchPosition` for each. Used by both case-sensitive and -insensitive
/// branches with pre-lowercased strings.
fn find_all_in(haystack: &str, needle: &str, message_index: usize, out: &mut Vec<MatchPosition>) {
    if needle.is_empty() {
        return;
    }
    let mut start = 0usize;
    while start <= haystack.len() {
        let rest = &haystack[start..];
        match rest.find(needle) {
            Some(rel) => {
                let abs = start + rel;
                out.push(MatchPosition {
                    message_index,
                    char_offset: abs,
                    length: needle.len(),
                });
                // Advance past this match to find non-overlapping
                // occurrences. `needle.len()` is always >= 1 here so we
                // can't infinite-loop.
                start = abs + needle.len();
            }
            None => break,
        }
    }
}

/// Stateless renderer for the find bar. Construct per frame.
///
/// When `active` is `false` this widget renders nothing, so the screen
/// layer can treat the find-bar's row as zero-height without branching at
/// the call site.
pub struct FindBarWidget<'a> {
    pub state: &'a FindState,
    pub theme: &'a Theme,
    pub active: bool,
}

impl<'a> FindBarWidget<'a> {
    pub fn new(state: &'a FindState, theme: &'a Theme) -> Self {
        Self {
            state,
            theme,
            active: true,
        }
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Build the styled `Line` the widget will render. Split out so unit
    /// tests can assert on span shape without going through a `Buffer`.
    fn build_line(&self) -> Line<'static> {
        let muted = hex_to_color(&self.theme.text_muted).unwrap_or(Color::DarkGray);
        let fg = hex_to_color(&self.theme.foreground).unwrap_or(Color::White);

        // Leading label — mirrors the Textual placeholder style. The find
        // bar is always single-line so we don't bother with wrapping.
        let mut spans: Vec<Span<'static>> = Vec::with_capacity(6);
        spans.push(Span::styled(
            " find: ",
            Style::default().fg(muted).add_modifier(Modifier::BOLD),
        ));

        // The query echo. Render an empty placeholder when nothing has
        // been typed so the bar still occupies its row visibly.
        if self.state.query.is_empty() {
            spans.push(Span::styled(
                "(type to search)".to_string(),
                Style::default().fg(muted).add_modifier(Modifier::DIM),
            ));
        } else {
            spans.push(Span::styled(
                self.state.query.clone(),
                Style::default().fg(fg).add_modifier(Modifier::BOLD),
            ));
        }

        // Trailing match counter. "" when no query, "No matches" when
        // query but zero hits, "N of M" otherwise. Mirrors the Python
        // `update_status` method one-to-one.
        let status = render_status(self.state);
        if !status.is_empty() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                status,
                Style::default().fg(muted).add_modifier(Modifier::DIM),
            ));
        }

        // Hint: how to navigate / dismiss. Kept dim so it doesn't compete
        // with the query.
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "(n/N step · Esc close · Enter jump)".to_string(),
            Style::default().fg(muted).add_modifier(Modifier::DIM),
        ));

        Line::from(spans)
    }
}

impl<'a> Widget for FindBarWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if !self.active {
            return;
        }
        let line = self.build_line();
        Paragraph::new(line).render(area, buf);
    }
}

/// Format the trailing match counter the same way `FindBar.update_status`
/// does in Python. Empty string when there's no query.
fn render_status(state: &FindState) -> String {
    if state.query.is_empty() {
        return String::new();
    }
    match state.matches.len() {
        0 => "No matches".to_string(),
        1 => "1 match".to_string(),
        n => format!("{} of {} matches", state.current_display_index(), n),
    }
}

/// Handle a single key event while the find bar is focused.
///
/// Returns:
///   * `None` — the state was mutated (or ignored) but the bar stays open.
///   * `Some(FindResult::Closed)` — Esc; the App should drop `find_state`
///     to `None` and stop overlaying highlights.
///   * `Some(FindResult::JumpedToMatch { message_index })` — Enter; the App
///     should scroll to `message_index` *and* close the bar.
///
/// Key bindings:
///   * Esc → `Closed`
///   * Enter → `JumpedToMatch` (or `Closed` if no current match)
///   * Backspace → drop last char, recompute
///   * `n` → next match (no close)
///   * `N` (shift+n) → prev match (no close)
///   * Any other printable char → append, recompute
pub fn handle_key(
    state: &mut FindState,
    key: crossterm::event::KeyEvent,
    transcript: &[CleanedMessage],
) -> Option<FindResult> {
    use crossterm::event::{KeyCode, KeyModifiers};

    match key.code {
        KeyCode::Esc => Some(FindResult::Closed),
        KeyCode::Enter => match state.current_match() {
            Some(m) => Some(FindResult::JumpedToMatch {
                message_index: m.message_index,
            }),
            // No matches — Enter just dismisses, same as Esc. Saves the
            // user a second keypress when their query found nothing.
            None => Some(FindResult::Closed),
        },
        KeyCode::Backspace => {
            state.query.pop();
            state.recompute_matches(transcript);
            None
        }
        // `n` / `N` are step keys. Shift discriminates direction — match
        // the documented "n forward, N backward" contract. We check the
        // raw character first so `N` on a layout that doesn't send SHIFT
        // (rare, but caps-lock) still steps backwards.
        KeyCode::Char('n') if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            state.next_match();
            None
        }
        KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::SHIFT) => {
            state.prev_match();
            None
        }
        KeyCode::Char('N') => {
            state.prev_match();
            None
        }
        KeyCode::Char(c) => {
            // Ignore Ctrl-* chords so the App can intercept things like
            // Ctrl-F (toggle find bar) before they reach the input.
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                return None;
            }
            state.query.push(c);
            state.recompute_matches(transcript);
            None
        }
        // Up/Down arrows mirror the Python widget's bubble-up of arrow
        // keys to the App's next/prev match actions.
        KeyCode::Down => {
            state.next_match();
            None
        }
        KeyCode::Up => {
            state.prev_match();
            None
        }
        _ => None,
    }
}

/// Parse `#rrggbb` into a `ratatui::style::Color`. Returns `None` for any
/// malformed input — callers fall back to a sensible default.
fn hex_to_color(hex: &str) -> Option<Color> {
    hex_to_rgb(hex).map(|(r, g, b)| Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn msg(text: &str) -> CleanedMessage {
        CleanedMessage {
            uuid: "u".to_string(),
            session_id: None,
            role: "user".to_string(),
            text: text.to_string(),
            timestamp: None,
            cwd: None,
            parent_uuid: None,
            is_sidechain: 0,
            message_id: None,
        }
    }

    fn theme() -> Theme {
        Theme::default_dark()
    }

    // ---- recompute_matches --------------------------------------------------

    #[test]
    fn recompute_finds_all_occurrences_across_messages() {
        let transcript = vec![
            msg("hello world"),
            msg("the world is large"),
            msg("worldview"),
        ];
        let mut s = FindState {
            query: "world".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        assert_eq!(s.matches.len(), 3);
        assert_eq!(s.matches[0].message_index, 0);
        assert_eq!(s.matches[0].char_offset, 6);
        assert_eq!(s.matches[0].length, 5);
        assert_eq!(s.matches[1].message_index, 1);
        assert_eq!(s.matches[1].char_offset, 4);
        assert_eq!(s.matches[2].message_index, 2);
        assert_eq!(s.matches[2].char_offset, 0);
    }

    #[test]
    fn recompute_handles_multiple_matches_in_one_message() {
        let transcript = vec![msg("abab")];
        let mut s = FindState {
            query: "ab".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        assert_eq!(s.matches.len(), 2);
        assert_eq!(s.matches[0].char_offset, 0);
        assert_eq!(s.matches[1].char_offset, 2);
    }

    #[test]
    fn recompute_is_case_insensitive_by_default() {
        let transcript = vec![msg("Hello WORLD")];
        let mut s = FindState {
            query: "world".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        assert_eq!(s.matches.len(), 1);
        assert_eq!(s.matches[0].char_offset, 6);
    }

    #[test]
    fn recompute_case_sensitive_flag_respected() {
        let transcript = vec![msg("Hello WORLD world")];
        let mut s = FindState {
            query: "world".to_string(),
            case_sensitive: true,
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        assert_eq!(s.matches.len(), 1, "case-sensitive should match only 'world', not 'WORLD'");
        assert_eq!(s.matches[0].char_offset, 12);
    }

    #[test]
    fn recompute_empty_query_clears_matches() {
        let transcript = vec![msg("hello")];
        let mut s = FindState {
            query: "hello".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        assert_eq!(s.matches.len(), 1);
        s.query.clear();
        s.recompute_matches(&transcript);
        assert!(s.matches.is_empty());
        assert_eq!(s.current_match_index, 0);
    }

    #[test]
    fn recompute_no_matches_resets_cursor() {
        let transcript = vec![msg("nothing here")];
        let mut s = FindState {
            query: "zzz".to_string(),
            current_match_index: 5,
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        assert!(s.matches.is_empty());
        assert_eq!(s.current_match_index, 0);
    }

    #[test]
    fn recompute_clamps_cursor_when_matches_shrink() {
        let transcript = vec![msg("aaaa")];
        let mut s = FindState {
            query: "a".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        assert_eq!(s.matches.len(), 4);
        s.current_match_index = 3;
        // Now shrink: query that matches fewer
        s.query = "aaaa".to_string();
        s.recompute_matches(&transcript);
        assert_eq!(s.matches.len(), 1);
        assert_eq!(s.current_match_index, 0, "cursor should clamp into-range");
    }

    // ---- next/prev wrap -----------------------------------------------------

    #[test]
    fn next_match_wraps_at_end() {
        let transcript = vec![msg("a a a")];
        let mut s = FindState {
            query: "a".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        assert_eq!(s.matches.len(), 3);
        assert_eq!(s.current_match_index, 0);
        s.next_match();
        assert_eq!(s.current_match_index, 1);
        s.next_match();
        assert_eq!(s.current_match_index, 2);
        s.next_match();
        assert_eq!(s.current_match_index, 0, "should wrap");
    }

    #[test]
    fn prev_match_wraps_at_start() {
        let transcript = vec![msg("a a a")];
        let mut s = FindState {
            query: "a".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        s.prev_match();
        assert_eq!(s.current_match_index, 2, "should wrap from 0 to last");
        s.prev_match();
        assert_eq!(s.current_match_index, 1);
    }

    #[test]
    fn next_prev_noop_when_no_matches() {
        let mut s = FindState::default();
        s.next_match();
        assert_eq!(s.current_match_index, 0);
        s.prev_match();
        assert_eq!(s.current_match_index, 0);
    }

    #[test]
    fn current_match_returns_position() {
        let transcript = vec![msg("xy xy")];
        let mut s = FindState {
            query: "xy".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        let cur = s.current_match().unwrap();
        assert_eq!(cur.char_offset, 0);
        s.next_match();
        let cur = s.current_match().unwrap();
        assert_eq!(cur.char_offset, 3);
    }

    // ---- handle_key ---------------------------------------------------------

    #[test]
    fn handle_key_typing_appends_and_recomputes() {
        let transcript = vec![msg("hello world")];
        let mut s = FindState::default();
        for c in "hello".chars() {
            let r = handle_key(
                &mut s,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                &transcript,
            );
            assert!(r.is_none(), "typing should not close the bar");
        }
        assert_eq!(s.query, "hello");
        assert_eq!(s.matches.len(), 1);
    }

    #[test]
    fn handle_key_backspace_removes_last_char() {
        let transcript = vec![msg("hello")];
        let mut s = FindState {
            query: "hell".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        let r = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &transcript,
        );
        assert!(r.is_none());
        assert_eq!(s.query, "hel");
        // Should have recomputed: still matches "hel"
        assert_eq!(s.matches.len(), 1);
    }

    #[test]
    fn handle_key_esc_returns_closed() {
        let mut s = FindState::default();
        let r = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &[],
        );
        assert_eq!(r, Some(FindResult::Closed));
    }

    #[test]
    fn handle_key_enter_returns_jump_when_match_exists() {
        let transcript = vec![msg("zero"), msg("found here")];
        let mut s = FindState {
            query: "found".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        let r = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &transcript,
        );
        assert_eq!(r, Some(FindResult::JumpedToMatch { message_index: 1 }));
    }

    #[test]
    fn handle_key_enter_closes_when_no_match() {
        let transcript = vec![msg("zero")];
        let mut s = FindState {
            query: "zzz".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        let r = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &transcript,
        );
        assert_eq!(r, Some(FindResult::Closed));
    }

    #[test]
    fn handle_key_n_steps_forward() {
        let transcript = vec![msg("a a a")];
        let mut s = FindState {
            query: "a".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            &transcript,
        );
        assert_eq!(s.current_match_index, 1);
    }

    #[test]
    fn handle_key_shift_n_steps_backward() {
        let transcript = vec![msg("a a a")];
        let mut s = FindState {
            query: "a".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        // Capital N or n+SHIFT both step backward — wraps from 0 to last.
        handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT),
            &transcript,
        );
        assert_eq!(s.current_match_index, 2);
    }

    #[test]
    fn handle_key_ignores_ctrl_chords() {
        let transcript = vec![msg("hi")];
        let mut s = FindState::default();
        let r = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
            &transcript,
        );
        assert_eq!(r, None);
        assert_eq!(s.query, "", "Ctrl-F should not type 'f' into the query");
    }

    #[test]
    fn handle_key_down_up_cycle_matches() {
        let transcript = vec![msg("a a a")];
        let mut s = FindState {
            query: "a".to_string(),
            ..Default::default()
        };
        s.recompute_matches(&transcript);
        handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &transcript,
        );
        assert_eq!(s.current_match_index, 1);
        handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &transcript,
        );
        assert_eq!(s.current_match_index, 0);
    }

    // ---- rendering ----------------------------------------------------------

    #[test]
    fn render_status_blank_for_empty_query() {
        let s = FindState::default();
        assert_eq!(render_status(&s), "");
    }

    #[test]
    fn render_status_no_matches() {
        let s = FindState {
            query: "x".to_string(),
            ..Default::default()
        };
        assert_eq!(render_status(&s), "No matches");
    }

    #[test]
    fn render_status_one_match() {
        let mut s = FindState {
            query: "x".to_string(),
            ..Default::default()
        };
        s.matches.push(MatchPosition {
            message_index: 0,
            char_offset: 0,
            length: 1,
        });
        assert_eq!(render_status(&s), "1 match");
    }

    #[test]
    fn render_status_multi_match_with_index() {
        let mut s = FindState {
            query: "x".to_string(),
            ..Default::default()
        };
        for i in 0..3 {
            s.matches.push(MatchPosition {
                message_index: i,
                char_offset: 0,
                length: 1,
            });
        }
        s.current_match_index = 1;
        assert_eq!(render_status(&s), "2 of 3 matches");
    }

    #[test]
    fn widget_inactive_renders_nothing() {
        let t = theme();
        let s = FindState::default();
        let mut terminal = Terminal::new(TestBackend::new(40, 1)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 40,
                    height: 1,
                };
                let w = FindBarWidget::new(&s, &t).active(false);
                f.render_widget(w, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut text = String::new();
        for x in 0..40 {
            text.push_str(buf.cell((x, 0)).unwrap().symbol());
        }
        assert!(
            text.trim().is_empty(),
            "inactive widget should leave blank cells, got {text:?}"
        );
    }

    #[test]
    fn widget_active_renders_label_and_query() {
        let t = theme();
        let s = FindState {
            query: "hello".to_string(),
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
                let w = FindBarWidget::new(&s, &t);
                f.render_widget(w, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut text = String::new();
        for x in 0..80 {
            text.push_str(buf.cell((x, 0)).unwrap().symbol());
        }
        assert!(text.contains("find:"), "expected 'find:' label, got {text:?}");
        assert!(text.contains("hello"), "expected query echo, got {text:?}");
    }

    #[test]
    fn widget_placeholder_when_query_empty() {
        let t = theme();
        let s = FindState::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        terminal
            .draw(|f| {
                let area = Rect {
                    x: 0,
                    y: 0,
                    width: 80,
                    height: 1,
                };
                let w = FindBarWidget::new(&s, &t);
                f.render_widget(w, area);
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let mut text = String::new();
        for x in 0..80 {
            text.push_str(buf.cell((x, 0)).unwrap().symbol());
        }
        assert!(text.contains("type to search"), "expected placeholder, got {text:?}");
    }
}
