//! Kanban modal — sessions grouped by [`SessionStatus`], navigable as a 2D
//! grid (`h`/`l` between columns, `j`/`k` within). Phase 5 task 5.1.
//!
//! Mirrors `threadhop_core/tui/screens/kanban.py`. The Python screen is a
//! full ModalScreen with `Shift+arrow` to reassign status; here we keep the
//! interaction stripped to "navigate + jump + cycle status" so the modal
//! stays self-contained and DB-free. The App owns the actual status write
//! when [`KanbanResult::StatusChanged`] comes back.
//!
//! ## Public-API shape (per task 5.1)
//!
//! * [`KanbanItem`] — flat row carrying just what the modal needs to render
//!   and post back. Caller fills these from the App's session list.
//! * [`State`] — bucketed view + 2D cursor. Per-column row cursor (vector
//!   of length 5) so moving between columns lands on the last row visited
//!   in the destination column, matching the Python behaviour.
//! * [`handle_key`] returns `Option<KanbanResult>`. `None` keeps the modal
//!   open; `Some(_)` posts a result the App reacts to (open session, write
//!   status, or close).
//! * [`draw`] renders 5 equal columns inside a bordered block.
//! * [`centered_rect`] mirrors the helper from the search / bookmark modals.
//!
//! ## Why no DB write here?
//!
//! Same reason the bookmark browser delegates delete: the modal stays App-
//! agnostic and unit-testable. The App routes `StatusChanged` through
//! `db::set_session_status` and refreshes the kanban state on the next
//! tick — the modal posts intent, the App owns persistence.

#![allow(dead_code)] // Wave 2 wires the App integration; until then the
                     // binary doesn't reference these helpers.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        block::Padding, Block, BorderType, Borders, Clear, Paragraph, Widget,
    },
};
use threadhop_core::{
    models::SessionStatus,
    theme::{hex_to_rgb, Theme},
};

use super::label_prompt::status_label;

// ---- column order ----------------------------------------------------------

/// Status order driving the 5 columns. Kept as a `const` so tests and
/// renderer share the same source of truth. Same order as
/// `STATUS_ORDER` in `threadhop_core/tui/constants.py`.
pub const STATUS_ORDER: [SessionStatus; 5] = [
    SessionStatus::Active,
    SessionStatus::InProgress,
    SessionStatus::InReview,
    SessionStatus::Done,
    SessionStatus::Archived,
];

/// Index of a status in [`STATUS_ORDER`].
fn status_index(status: SessionStatus) -> usize {
    STATUS_ORDER.iter().position(|s| *s == status).unwrap_or(0)
}

/// Next status when the user hits `m` (cycle to next column). Wraps around
/// so `Archived` → `Active`, matching the column-list ring.
fn next_status(status: SessionStatus) -> SessionStatus {
    let i = status_index(status);
    STATUS_ORDER[(i + 1) % STATUS_ORDER.len()]
}

// ---- public types ----------------------------------------------------------

/// Flat row the modal renders. Caller fills this from the App's session
/// list — Wave 2 builds them from `db::list_sessions` + display-name logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanbanItem {
    pub session_id: String,
    pub display_name: String,
    pub status: SessionStatus,
}

/// Modal state. Items live in a flat `Vec`; [`items_in_column`] is the
/// view the renderer + nav use. Keeping items flat (vs. five Vecs) means
/// status changes are a field-flip rather than a cross-bucket move.
#[derive(Debug, Clone)]
pub struct State {
    pub items: Vec<KanbanItem>,
    /// 0..STATUS_ORDER.len()
    pub selected_column: usize,
    /// Per-column row cursor. Always length [`STATUS_ORDER`]; clamped on
    /// every render so it never points past the end of a (shrinking) column.
    pub selected_row_in_column: Vec<usize>,
}

impl State {
    pub fn new(items: Vec<KanbanItem>) -> Self {
        Self {
            items,
            selected_column: 0,
            selected_row_in_column: vec![0; STATUS_ORDER.len()],
        }
    }

    /// Borrowed view of the items in a given column, in insertion order.
    /// Out-of-range columns return an empty Vec — defensive, matches the
    /// renderer's tolerance.
    pub fn items_in_column(&self, column: usize) -> Vec<&KanbanItem> {
        if column >= STATUS_ORDER.len() {
            return Vec::new();
        }
        let status = STATUS_ORDER[column];
        self.items.iter().filter(|it| it.status == status).collect()
    }

    /// Currently-highlighted item, if any.
    fn current(&self) -> Option<&KanbanItem> {
        let col = self.items_in_column(self.selected_column);
        let row = *self
            .selected_row_in_column
            .get(self.selected_column)
            .unwrap_or(&0);
        col.get(row).copied()
    }

    /// Clamp the row cursor for the selected column to the column's bounds.
    /// Empty columns clamp to 0.
    fn clamp_row(&mut self) {
        let col_len = self.items_in_column(self.selected_column).len();
        if self.selected_row_in_column.len() < STATUS_ORDER.len() {
            self.selected_row_in_column
                .resize(STATUS_ORDER.len(), 0);
        }
        let slot = &mut self.selected_row_in_column[self.selected_column];
        if col_len == 0 {
            *slot = 0;
        } else if *slot >= col_len {
            *slot = col_len - 1;
        }
    }
}

/// Result the modal posts back to the App.
///
/// `StatusChanged` carries the *new* status (not the delta) so the App
/// can persist it directly without re-deriving from the modal's column
/// index — the modal owns the column ring; the App should not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KanbanResult {
    Cancelled,
    JumpToSession {
        session_id: String,
    },
    StatusChanged {
        session_id: String,
        new_status: SessionStatus,
    },
}

// ---- key handling ----------------------------------------------------------

/// Handle one key event. Returns `Some(KanbanResult)` if the App should
/// react (close / open session / persist status); `None` if the modal
/// stays open and absorbs the keystroke (navigation).
///
/// Recognised keys:
/// * `Esc` / `q` / `Ctrl-C` → [`KanbanResult::Cancelled`]
/// * `h` / `Left` → previous column (wraps)
/// * `l` / `Right` → next column (wraps)
/// * `j` / `Down` → next row in current column
/// * `k` / `Up` → previous row in current column
/// * `Enter` → [`KanbanResult::JumpToSession`]
/// * `m` → [`KanbanResult::StatusChanged`] cycling to the next status
///
/// Release events are skipped (Press + Repeat accepted) to keep nav from
/// double-firing on up-edges — same filter as `search` / `bookmark_browser`.
pub fn handle_key(state: &mut State, key: KeyEvent) -> Option<KanbanResult> {
    if matches!(key.kind, KeyEventKind::Release) {
        return None;
    }

    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => Some(KanbanResult::Cancelled),
        (KeyCode::Char('q'), KeyModifiers::NONE) => Some(KanbanResult::Cancelled),
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => Some(KanbanResult::Cancelled),

        (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
            move_column(state, -1);
            None
        }
        (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
            move_column(state, 1);
            None
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
            move_row(state, 1);
            None
        }
        (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
            move_row(state, -1);
            None
        }

        (KeyCode::Enter, _) => state
            .current()
            .map(|it| KanbanResult::JumpToSession {
                session_id: it.session_id.clone(),
            }),

        (KeyCode::Char('m'), KeyModifiers::NONE) => {
            // Need the session id + new status before we mutate the item,
            // because cycling the status will pull it out of the current
            // column and we'd lose the cursor anchor mid-update.
            let item = state.current().cloned()?;
            let new_status = next_status(item.status);
            // Optimistically apply the status flip in-place so the next
            // render shows the card under its new column. The App will
            // eventually re-seed state from the DB, but the local mutation
            // keeps the UI responsive without waiting for a round-trip.
            for it in &mut state.items {
                if it.session_id == item.session_id {
                    it.status = new_status;
                    break;
                }
            }
            // Cursor follows the card to the new column.
            state.selected_column = status_index(new_status);
            // Land the row cursor on the moved card so `j`/`k` continues
            // from the right anchor.
            let new_col = state.items_in_column(state.selected_column);
            if let Some(idx) = new_col
                .iter()
                .position(|it| it.session_id == item.session_id)
            {
                state.selected_row_in_column[state.selected_column] = idx;
            } else {
                state.clamp_row();
            }
            Some(KanbanResult::StatusChanged {
                session_id: item.session_id,
                new_status,
            })
        }

        _ => None,
    }
}

fn move_column(state: &mut State, delta: i32) {
    let n = STATUS_ORDER.len() as i32;
    let cur = state.selected_column as i32;
    // Wrap with euclidean rem so -1 from column 0 lands on column n-1.
    let next = ((cur + delta).rem_euclid(n)) as usize;
    state.selected_column = next;
    state.clamp_row();
}

fn move_row(state: &mut State, delta: i32) {
    let col_len = state.items_in_column(state.selected_column).len() as i32;
    if col_len == 0 {
        return;
    }
    let cur = state.selected_row_in_column[state.selected_column] as i32;
    let next = (cur + delta).clamp(0, col_len - 1);
    state.selected_row_in_column[state.selected_column] = next as usize;
}

// ---- rendering -------------------------------------------------------------

/// Render the kanban modal. Caller is expected to have computed a
/// centered popup rect (see [`centered_rect`]); we call [`Clear`] for
/// safety so the modal is opaque over the underlying screen.
///
/// Layout:
/// ```text
/// ╭──────────────── Kanban ────────────────╮
/// │ active   in_prog   in_rev   done   arch │  ← header row
/// ├────────┬────────┬────────┬────────┬─────┤
/// │  ...   │  ...   │  ...   │  ...   │ ... │  ← column lists
/// ├─────────────────────────────────────────┤
/// │  h/l col · j/k row · enter open · ...   │  ← help row
/// ╰─────────────────────────────────────────╯
/// ```
pub fn draw(state: &State, theme: &Theme, area: Rect, buf: &mut Buffer) {
    Clear.render(area, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style(theme))
        .padding(Padding::new(2, 2, 1, 1))
        .style(modal_bg_style(theme))
        .title(Span::styled(" Kanban ", title_style(theme)));
    let inner = block.inner(area);
    block.render(area, buf);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    // Vertical split: column-header row, columns, help row.
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    // Horizontal split: 5 equal columns, used by both the header and body.
    let header_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            STATUS_ORDER
                .iter()
                .map(|_| Constraint::Ratio(1, STATUS_ORDER.len() as u32))
                .collect::<Vec<_>>(),
        )
        .split(v[0]);
    let body_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            STATUS_ORDER
                .iter()
                .map(|_| Constraint::Ratio(1, STATUS_ORDER.len() as u32))
                .collect::<Vec<_>>(),
        )
        .split(v[1]);

    for (i, status) in STATUS_ORDER.iter().enumerate() {
        render_header(state, *status, i, theme, header_cols[i], buf);
        render_column(state, i, theme, body_cols[i], buf);
    }

    render_help(theme, v[2], buf);
}

fn render_header(
    state: &State,
    status: SessionStatus,
    col_idx: usize,
    theme: &Theme,
    area: Rect,
    buf: &mut Buffer,
) {
    let count = state.items_in_column(col_idx).len();
    let is_selected = col_idx == state.selected_column;
    let style = if is_selected {
        Style::default()
            .fg(theme_color(&theme.background, Color::Black))
            .bg(theme_color(&theme.accent, Color::Magenta))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(theme_color(&theme.primary, Color::Yellow))
            .add_modifier(Modifier::BOLD)
    };
    let label = format!(" {} ({}) ", status_label(status), count);
    Paragraph::new(Line::from(Span::styled(label, style))).render(area, buf);
}

/// Height (in rows) of a single rendered card: top border + content row +
/// bottom border. Kept as a `const` so the scroll math and the renderer
/// agree without a runtime parameter.
const CARD_HEIGHT: u16 = 3;

fn render_column(
    state: &State,
    col_idx: usize,
    theme: &Theme,
    area: Rect,
    buf: &mut Buffer,
) {
    let items = state.items_in_column(col_idx);

    if items.is_empty() {
        let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
        let msg = "— empty —";
        let centered_x = area.x
            + area
                .width
                .saturating_sub(msg.chars().count() as u16)
                / 2;
        let target = Rect {
            x: centered_x,
            y: area.y + area.height / 2,
            width: area.width.saturating_sub(centered_x - area.x),
            height: 1,
        };
        Paragraph::new(Span::styled(msg.to_string(), muted)).render(target, buf);
        return;
    }

    let is_selected_column = col_idx == state.selected_column;
    let selected_row = if is_selected_column {
        let row = *state.selected_row_in_column.get(col_idx).unwrap_or(&0);
        Some(row.min(items.len().saturating_sub(1)))
    } else {
        None
    };

    // How many full cards fit vertically? At least 1 if there's any height
    // at all, so a too-tight area still draws the selected card (clipped).
    let visible = (area.height / CARD_HEIGHT).max(1) as usize;

    // Scroll: keep the selected card in view (only matters for the selected
    // column; other columns anchor at the top, mirroring the old List).
    let first_visible = match selected_row {
        Some(sel) if sel >= visible => sel + 1 - visible,
        _ => 0,
    };

    // Phase E: selected-card tint. 25% blend of warning into panel bg —
    // applied to the selected card's Block style so it paints the inner
    // area as well as the border-row gaps between glyphs.
    let selected_bg_hex = threadhop_core::theme::blend(
        &theme.warning,
        &theme.background_panel,
        0.25,
    );
    let selected_bg = threadhop_core::theme::hex_to_rgb(&selected_bg_hex)
        .map(|(r, g, b)| Color::Rgb(r, g, b))
        .unwrap_or_else(|| theme_color(&theme.warning, Color::Yellow));

    let border_fg = theme_color(&theme.border, Color::DarkGray);
    let border_selected_fg = theme_color(&theme.border_active, Color::White);

    for (visible_idx, item_idx) in (first_visible..items.len().min(first_visible + visible))
        .enumerate()
    {
        let card_y = area.y + (visible_idx as u16) * CARD_HEIGHT;
        // Guard against overflow on the last partial card slot.
        if card_y >= area.y + area.height {
            break;
        }
        let card_height = CARD_HEIGHT.min(area.y + area.height - card_y);
        let card_rect = Rect {
            x: area.x,
            y: card_y,
            width: area.width,
            height: card_height,
        };

        let is_selected = selected_row == Some(item_idx);
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(if is_selected {
                BorderType::Thick
            } else {
                BorderType::Plain
            })
            .border_style(Style::default().fg(if is_selected {
                border_selected_fg
            } else {
                border_fg
            }));

        if is_selected {
            // Tint the entire card (inner + the bg of the border row
            // between glyphs) so the selection reads as a coherent chip.
            // Border glyphs themselves still pick up the border fg.
            block = block.style(Style::default().bg(selected_bg));
        }

        let inner = block.inner(card_rect);
        block.render(card_rect, buf);

        if inner.height == 0 || inner.width == 0 {
            continue;
        }

        let it = items[item_idx];
        let sid_short: String = it.session_id.chars().take(8).collect();
        let name_max = (inner.width as usize).saturating_sub(9); // 8 sid + 1 space
        let mut content_style = Style::default();
        if is_selected {
            content_style = content_style.bg(selected_bg).add_modifier(Modifier::BOLD);
        }
        let line = Line::from(vec![
            Span::styled(
                format!("{:<8}", sid_short),
                Style::default()
                    .fg(theme_color(&theme.secondary, Color::Cyan))
                    .add_modifier(Modifier::BOLD)
                    .bg(if is_selected {
                        selected_bg
                    } else {
                        Color::Reset
                    }),
            ),
            Span::styled(" ", content_style),
            Span::styled(
                truncate_display(&it.display_name, name_max),
                content_style.fg(theme_color(&theme.foreground, Color::White)),
            ),
        ]);
        Paragraph::new(line).render(inner, buf);
    }
}

fn render_help(theme: &Theme, area: Rect, buf: &mut Buffer) {
    let muted = Style::default().fg(theme_color(&theme.text_muted, Color::DarkGray));
    Paragraph::new(Line::from(Span::styled(
        " h/l column · j/k row · Enter open · m cycle status · Esc close ".to_string(),
        muted,
    )))
    .render(area, buf);
}

// ---- helpers ---------------------------------------------------------------

/// Truncate to `max` chars with an ellipsis. Tiny columns can degenerate
/// to width 0; in that case we return the empty string rather than
/// rendering a lone `…`.
fn truncate_display(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let count = s.chars().count();
    if count <= max {
        s.to_string()
    } else if max == 1 {
        "…".to_string()
    } else {
        let head: String = s.chars().take(max - 1).collect();
        format!("{}…", head)
    }
}

fn theme_color(hex: &str, fallback: Color) -> Color {
    match hex_to_rgb(hex) {
        Some((r, g, b)) => Color::Rgb(r, g, b),
        None => fallback,
    }
}

fn border_style(theme: &Theme) -> Style {
    Style::default().fg(theme_color(&theme.border_active, Color::White))
}

fn title_style(theme: &Theme) -> Style {
    Style::default()
        .fg(theme_color(&theme.accent, Color::Magenta))
        .add_modifier(Modifier::BOLD)
}

fn modal_bg_style(theme: &Theme) -> Style {
    Style::default().bg(theme_color(&theme.background_panel, Color::Reset))
}

/// Centered popup rect — mirrors the helper from `search` / `bookmark`.
pub fn centered_rect(pct_w: u16, pct_h: u16, parent: Rect) -> Rect {
    let h = parent.height.saturating_mul(pct_h) / 100;
    let w = parent.width.saturating_mul(pct_w) / 100;
    let x = parent.x + (parent.width.saturating_sub(w)) / 2;
    let y = parent.y + (parent.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

// ---- tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn item(sid: &str, name: &str, status: SessionStatus) -> KanbanItem {
        KanbanItem {
            session_id: sid.into(),
            display_name: name.into(),
            status,
        }
    }

    fn press(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn press_char(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    // ---- bucketing --------------------------------------------------

    #[test]
    fn items_in_column_filters_by_status() {
        let st = State::new(vec![
            item("a", "a", SessionStatus::Active),
            item("b", "b", SessionStatus::InProgress),
            item("c", "c", SessionStatus::Active),
            item("d", "d", SessionStatus::Done),
        ]);
        let active = st.items_in_column(0); // Active
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].session_id, "a");
        assert_eq!(active[1].session_id, "c");
        assert_eq!(st.items_in_column(1).len(), 1); // InProgress
        assert_eq!(st.items_in_column(3).len(), 1); // Done
        assert_eq!(st.items_in_column(4).len(), 0); // Archived
        // Out-of-range columns return empty rather than panic.
        assert_eq!(st.items_in_column(99).len(), 0);
    }

    // ---- column nav -------------------------------------------------

    #[test]
    fn l_and_right_move_column_forward_with_wrap() {
        let mut st = State::new(Vec::new());
        handle_key(&mut st, press_char('l'));
        assert_eq!(st.selected_column, 1);
        handle_key(&mut st, press(KeyCode::Right));
        assert_eq!(st.selected_column, 2);
        // Past the last column wraps.
        for _ in 0..4 {
            handle_key(&mut st, press_char('l'));
        }
        assert_eq!(st.selected_column, 1);
    }

    #[test]
    fn h_and_left_move_column_back_with_wrap() {
        let mut st = State::new(Vec::new());
        st.selected_column = 2;
        handle_key(&mut st, press_char('h'));
        assert_eq!(st.selected_column, 1);
        handle_key(&mut st, press(KeyCode::Left));
        assert_eq!(st.selected_column, 0);
        // Wrap below zero to the last column.
        handle_key(&mut st, press_char('h'));
        assert_eq!(st.selected_column, STATUS_ORDER.len() - 1);
    }

    // ---- row nav ----------------------------------------------------

    #[test]
    fn j_and_k_move_row_within_column() {
        let mut st = State::new(vec![
            item("a", "a", SessionStatus::Active),
            item("b", "b", SessionStatus::Active),
            item("c", "c", SessionStatus::Active),
        ]);
        // Cursor starts at column 0, row 0.
        handle_key(&mut st, press_char('j'));
        assert_eq!(st.selected_row_in_column[0], 1);
        handle_key(&mut st, press(KeyCode::Down));
        assert_eq!(st.selected_row_in_column[0], 2);
        // Past the end clamps.
        handle_key(&mut st, press_char('j'));
        assert_eq!(st.selected_row_in_column[0], 2);
        // Back up.
        handle_key(&mut st, press_char('k'));
        assert_eq!(st.selected_row_in_column[0], 1);
        handle_key(&mut st, press(KeyCode::Up));
        assert_eq!(st.selected_row_in_column[0], 0);
        // Below zero clamps.
        handle_key(&mut st, press_char('k'));
        assert_eq!(st.selected_row_in_column[0], 0);
    }

    #[test]
    fn row_cursor_is_per_column() {
        let mut st = State::new(vec![
            item("a", "a", SessionStatus::Active),
            item("b", "b", SessionStatus::Active),
            item("p", "p", SessionStatus::InProgress),
            item("q", "q", SessionStatus::InProgress),
            item("r", "r", SessionStatus::InProgress),
        ]);
        // Move to row 1 in Active.
        handle_key(&mut st, press_char('j'));
        // Hop to InProgress.
        handle_key(&mut st, press_char('l'));
        // Row in new column starts at 0 (default), not inherited.
        assert_eq!(st.selected_row_in_column[1], 0);
        // Move down once in InProgress.
        handle_key(&mut st, press_char('j'));
        assert_eq!(st.selected_row_in_column[1], 1);
        // Hop back to Active — original cursor preserved.
        handle_key(&mut st, press_char('h'));
        assert_eq!(st.selected_column, 0);
        assert_eq!(st.selected_row_in_column[0], 1);
    }

    // ---- empty-column nav ------------------------------------------

    #[test]
    fn empty_column_nav_is_noop() {
        let mut st = State::new(vec![item("a", "a", SessionStatus::Done)]);
        // Cursor starts on column 0 (Active) which is empty.
        assert_eq!(st.items_in_column(0).len(), 0);
        handle_key(&mut st, press_char('j'));
        handle_key(&mut st, press_char('k'));
        assert_eq!(st.selected_row_in_column[0], 0);
        // Enter on an empty column returns None — no item to jump to.
        assert!(handle_key(&mut st, press(KeyCode::Enter)).is_none());
    }

    // ---- enter / esc / cancel --------------------------------------

    #[test]
    fn enter_posts_jump_to_session_for_selected_item() {
        let mut st = State::new(vec![
            item("alpha", "Alpha", SessionStatus::Active),
            item("beta", "Beta", SessionStatus::Active),
        ]);
        handle_key(&mut st, press_char('j')); // row 1
        let got = handle_key(&mut st, press(KeyCode::Enter));
        assert_eq!(
            got,
            Some(KanbanResult::JumpToSession {
                session_id: "beta".into()
            })
        );
    }

    #[test]
    fn esc_posts_cancelled() {
        let mut st = State::new(Vec::new());
        assert_eq!(
            handle_key(&mut st, press(KeyCode::Esc)),
            Some(KanbanResult::Cancelled)
        );
    }

    #[test]
    fn ctrl_c_posts_cancelled() {
        let mut st = State::new(Vec::new());
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut st, ev), Some(KanbanResult::Cancelled));
    }

    // ---- m: status cycle -------------------------------------------

    #[test]
    fn m_cycles_status_to_next_column_and_posts_result() {
        let mut st = State::new(vec![
            item("a", "a", SessionStatus::Active),
            item("b", "b", SessionStatus::InProgress),
        ]);
        // Cursor on column 0, row 0 → "a" (Active). Cycle → InProgress.
        let got = handle_key(&mut st, press_char('m'));
        assert_eq!(
            got,
            Some(KanbanResult::StatusChanged {
                session_id: "a".into(),
                new_status: SessionStatus::InProgress,
            })
        );
        // State mutated in place + cursor followed the card.
        assert_eq!(st.items[0].status, SessionStatus::InProgress);
        assert_eq!(st.selected_column, 1);
        // "a" is now the second item in InProgress (after "b"); cursor on it.
        let col = st.items_in_column(1);
        let row = st.selected_row_in_column[1];
        assert_eq!(col[row].session_id, "a");
    }

    #[test]
    fn m_wraps_from_archived_back_to_active() {
        let mut st = State::new(vec![item("a", "a", SessionStatus::Archived)]);
        st.selected_column = 4; // Archived
        let got = handle_key(&mut st, press_char('m'));
        assert_eq!(
            got,
            Some(KanbanResult::StatusChanged {
                session_id: "a".into(),
                new_status: SessionStatus::Active,
            })
        );
        assert_eq!(st.selected_column, 0);
    }

    #[test]
    fn m_on_empty_column_is_noop() {
        let mut st = State::new(vec![item("a", "a", SessionStatus::Done)]);
        // Column 0 is empty.
        assert!(handle_key(&mut st, press_char('m')).is_none());
    }

    // ---- release filter --------------------------------------------

    #[test]
    fn release_events_are_ignored() {
        let mut st = State::new(vec![
            item("a", "a", SessionStatus::Active),
            item("b", "b", SessionStatus::Active),
        ]);
        let ev = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert!(handle_key(&mut st, ev).is_none());
        assert_eq!(st.selected_row_in_column[0], 0);
    }

    #[test]
    fn repeat_events_are_accepted() {
        let mut st = State::new(vec![
            item("a", "a", SessionStatus::Active),
            item("b", "b", SessionStatus::Active),
        ]);
        let ev = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Repeat,
            state: crossterm::event::KeyEventState::NONE,
        };
        handle_key(&mut st, ev);
        assert_eq!(st.selected_row_in_column[0], 1);
    }

    // ---- helpers ----------------------------------------------------

    #[test]
    fn truncate_display_respects_max() {
        assert_eq!(truncate_display("hello", 10), "hello");
        assert_eq!(truncate_display("hello world", 5), "hell…");
        assert_eq!(truncate_display("abc", 0), "");
        assert_eq!(truncate_display("abc", 1), "…");
    }

    #[test]
    fn next_status_cycles_through_order() {
        assert_eq!(next_status(SessionStatus::Active), SessionStatus::InProgress);
        assert_eq!(next_status(SessionStatus::InProgress), SessionStatus::InReview);
        assert_eq!(next_status(SessionStatus::InReview), SessionStatus::Done);
        assert_eq!(next_status(SessionStatus::Done), SessionStatus::Archived);
        assert_eq!(next_status(SessionStatus::Archived), SessionStatus::Active);
    }

    // ---- draw -------------------------------------------------------

    fn snapshot_buffer_styles(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                let cell = &buf[(x, y)];
                out.push_str(cell.symbol());
                out.push_str(&format!(
                    "[fg={:?},bg={:?},m={:?}]",
                    cell.fg, cell.bg, cell.modifier
                ));
            }
            out.push('\n');
        }
        out
    }

    fn snapshot_buffer_text(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn draw_empty_state_does_not_panic() {
        let st = State::new(Vec::new());
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(90, 80, f.area());
            draw(&st, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let all = snapshot_buffer_text(term.backend().buffer());
        assert!(all.contains("Kanban"), "title missing:\n{all}");
        // All five column headers visible.
        for s in STATUS_ORDER.iter() {
            assert!(
                all.contains(status_label(*s)),
                "missing header {} in:\n{}",
                status_label(*s),
                all
            );
        }
    }

    #[test]
    fn draw_with_items_renders_session_ids() {
        let st = State::new(vec![
            item("alpha-id-1", "Card A", SessionStatus::Active),
            item("beta-id-2", "Card B", SessionStatus::InProgress),
        ]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(95, 90, f.area());
            draw(&st, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let all = snapshot_buffer_text(term.backend().buffer());
        // First 8 chars of each session id appear.
        assert!(all.contains("alpha-id"), "session a missing:\n{all}");
        assert!(all.contains("beta-id-"), "session b missing:\n{all}");
    }

    #[test]
    fn draw_tiny_area_does_not_panic() {
        let st = State::new(vec![item("a", "a", SessionStatus::Active)]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(20, 6)).unwrap();
        term.draw(|f| {
            let area = centered_rect(70, 60, f.area());
            draw(&st, &theme, area, f.buffer_mut());
        })
        .unwrap();
    }

    #[test]
    fn kanban_selected_card_renders_warning_tint_bg() {
        // Phase E E6: the selected card row carries a bg that's a 25% blend
        // of theme.warning into theme.background_panel. Two stacked items in
        // the Active column; cursor on row 0 by default.
        let st = State::new(vec![
            item("alpha-1", "Card A", SessionStatus::Active),
            item("beta-2", "Card B", SessionStatus::Active),
        ]);
        let theme = Theme::default_dark();
        let want_hex = threadhop_core::theme::blend(
            &theme.warning,
            &theme.background_panel,
            0.25,
        );
        let want = threadhop_core::theme::hex_to_rgb(&want_hex)
            .map(|(r, g, b)| Color::Rgb(r, g, b))
            .unwrap();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(95, 90, f.area());
            draw(&st, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer();
        // Scan: some cell should be painted with the warning tint bg.
        let mut found = false;
        'rows: for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                if buf[(x, y)].bg == want {
                    found = true;
                    break 'rows;
                }
            }
        }
        assert!(
            found,
            "expected at least one cell with the warning-tint bg for the selected card"
        );
    }

    #[test]
    fn draw_buffer_changes_after_navigation() {
        // Frame-buffer test: render, navigate columns + rows, render again,
        // assert the buffer differs (highlight follows the cursor across
        // both axes).
        let mut st = State::new(vec![
            item("a", "Card A", SessionStatus::Active),
            item("b", "Card B", SessionStatus::Active),
            item("c", "Card C", SessionStatus::InProgress),
            item("d", "Card D", SessionStatus::InProgress),
        ]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();

        term.draw(|f| {
            let area = centered_rect(95, 90, f.area());
            draw(&st, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let before = snapshot_buffer_styles(term.backend().buffer());

        // Move within column, then across columns.
        handle_key(&mut st, press_char('j'));
        handle_key(&mut st, press_char('l'));

        term.draw(|f| {
            let area = centered_rect(95, 90, f.area());
            draw(&st, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let after = snapshot_buffer_styles(term.backend().buffer());

        assert_ne!(
            before, after,
            "buffer should differ after column + row navigation"
        );
    }

    // ---- Wave 1 / E6: heavy outer card border ----------------------

    /// Glyphs ratatui paints for `BorderType::Thick`.
    const THICK_GLYPHS: &[&str] = &["┏", "┓", "┗", "┛", "━", "┃"];

    /// Glyphs ratatui paints for `BorderType::Plain`.
    const PLAIN_GLYPHS: &[&str] = &["┌", "┐", "└", "┘", "─", "│"];

    fn buffer_contains_any(buf: &Buffer, needles: &[&str]) -> bool {
        for y in 0..buf.area().height {
            for x in 0..buf.area().width {
                let sym = buf[(x, y)].symbol();
                if needles.contains(&sym) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn kanban_selected_card_renders_thick_border() {
        // Two stacked cards in the Active column; cursor on row 0 by default.
        // The selected card must be drawn with `BorderType::Thick`, whose
        // glyph set is distinct from `Plain` (heavy weight strokes + corners).
        let st = State::new(vec![
            item("alpha-1", "Card A", SessionStatus::Active),
            item("beta-2", "Card B", SessionStatus::Active),
        ]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(95, 90, f.area());
            draw(&st, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert!(
            buffer_contains_any(buf, THICK_GLYPHS),
            "expected at least one heavy border glyph from {:?} in the rendered buffer; \
             the selected card should use BorderType::Thick",
            THICK_GLYPHS
        );
    }

    #[test]
    fn kanban_unselected_card_renders_plain_border() {
        // Two cards in the same column: row 0 is selected (Thick), row 1
        // un-selected (Plain). Assert that Plain glyphs are present so we
        // know the un-selected card kept its lighter weight border.
        let st = State::new(vec![
            item("alpha-1", "Card A", SessionStatus::Active),
            item("beta-2", "Card B", SessionStatus::Active),
        ]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(95, 90, f.area());
            draw(&st, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer();
        assert!(
            buffer_contains_any(buf, PLAIN_GLYPHS),
            "expected at least one plain border glyph from {:?} for the un-selected card",
            PLAIN_GLYPHS
        );
    }

    #[test]
    fn kanban_unselected_column_cards_all_use_plain_border() {
        // Cards in a non-selected column should never show a Thick border.
        // Cursor stays on column 0 (Active) here; the InProgress column
        // should draw only Plain card borders, while column 0's selected
        // card is Thick.
        let st = State::new(vec![
            item("a", "Card A", SessionStatus::Active),
            item("c", "Card C", SessionStatus::InProgress),
            item("d", "Card D", SessionStatus::InProgress),
        ]);
        let theme = Theme::default_dark();
        let mut term = Terminal::new(TestBackend::new(120, 24)).unwrap();
        term.draw(|f| {
            let area = centered_rect(95, 90, f.area());
            draw(&st, &theme, area, f.buffer_mut());
        })
        .unwrap();
        let buf = term.backend().buffer();
        // Plain glyphs must exist (the un-selected column's cards draw them).
        assert!(buffer_contains_any(buf, PLAIN_GLYPHS));
        // Thick glyphs must also exist (the selected card in column 0).
        assert!(buffer_contains_any(buf, THICK_GLYPHS));
    }
}
