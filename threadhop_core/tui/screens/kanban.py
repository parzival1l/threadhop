"""Full-screen Kanban board (sessions bucketed by status, ADR-004).

Columns reflect ``STATUS_ORDER``; cards are ``KanbanCard`` widgets and
the screen owns a 2D cursor (col_idx + row_idx). Layout reset is
deliberate — the App's grid layout would otherwise squeeze the board
into the 36-col sidebar slot.
"""

from __future__ import annotations

import re
from rich.console import Group
from rich.markup import escape
from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widgets import Static

from threadhop_core.storage import db

from ..constants import STATUS_LABELS, STATUS_ORDER
from ..utils import format_age


def _kanban_card_title(session: dict, custom_name: str | None) -> str:
    """Title line for a Kanban card.

    Priority chain mirrors what ``claude -r`` shows in Claude Code's
    own session picker — the invariant the user wants is "a card's
    title is the same thing Claude surfaces when you resume the
    session":

      1. ``custom_name``   — ThreadHop's local rename (SQLite).
      2. ``custom_title``  — /rename written into JSONL as
         ``{"type": "custom-title", ...}`` (either side writes this).
      3. ``ai_title``      — Claude's auto-generated title.
      4. ``first_user_msg`` — last-resort.
      5. ``Session <short_id>`` — guarantees a unique label.

    The ``project`` fallback the sidebar uses is still skipped — on a
    board scoped to one project every untitled card would otherwise
    show the same directory name.
    """
    if custom_name:
        return custom_name
    custom_title = (session.get("custom_title") or "").strip()
    if custom_title:
        return custom_title
    ai_title = (session.get("ai_title") or "").strip()
    if ai_title:
        return ai_title
    msg = (session.get("first_user_msg") or "").strip()
    if msg:
        return msg
    sid = session.get("session_id") or ""
    return f"Session {sid[:8]}" if sid else "(untitled)"


class KanbanCard(Vertical):
    """A session card on the Kanban board.

    Structured as a ``Vertical`` container with **two** children — a
    title Static that flexes, and a meta Static pinned to a single row
    at the bottom — rather than a monolithic Static. The two-widget
    layout is load-bearing: when the title wraps to multiple lines the
    meta row stays visible because it has its own guaranteed row in
    the layout rather than flowing after the title inside a single
    Rich Group (which was pushing meta off the bottom of the fixed-
    height card on long titles). Pattern is the same as docking a
    footer inside a panel.
    """

    def __init__(
        self,
        session: dict,
        *,
        custom_name: str | None,
        selected: bool,
    ) -> None:
        title_text = _kanban_card_title(session, custom_name)
        turns = session.get("turn_count", 0)
        age = format_age(session["modified"])

        # Defensive cap against pathologically long custom renames.
        # The layout can handle wrap naturally, but a 200-char title
        # at column width 22 would still wrap to 10 rows and clip
        # most of itself — truncating keeps the first meaningful
        # portion visible.
        if len(title_text) > 80:
            title_text = title_text[:79] + "…"
        title = Text(title_text, style="bold")

        meta = Text()
        meta.append(age, style="dim")
        meta.append("  ·  ", style="dim")
        meta.append(f"{turns}t", style="dim")
        if session.get("is_working"):
            meta.append("  ·  ", style="dim")
            meta.append("◐ working", style="bold yellow")
        elif session.get("is_active"):
            meta.append("  ·  ", style="dim")
            meta.append("● active", style="green")

        classes = "kanban-card"
        if selected:
            classes += " -selected"
        super().__init__(classes=classes)
        self._title_text = title
        self._meta_text = meta
        self.session_id: str = session.get("session_id", "")

    def compose(self) -> ComposeResult:
        yield Static(self._title_text, classes="kanban-card-title")
        yield Static(self._meta_text, classes="kanban-card-meta")

    def on_click(self) -> None:
        """Single-click selects and opens — matches enter on the keyboard
        cursor. We route through the screen so the cursor lands on this
        card first (keeps the 2D cursor state coherent for any live
        refresh that happens mid-dismiss) and then dismiss. Click events
        from the two child Statics bubble up to us here because Textual
        propagates events unless a handler calls ``event.stop()``."""
        screen = self.screen
        if isinstance(screen, KanbanScreen) and self.session_id:
            screen._focus_session_id(self.session_id)
            screen._render_board()
            screen.dismiss(self.session_id)


class KanbanScreen(ModalScreen):
    """Full-screen Kanban view: sessions bucketed by status with 2D nav.

    The CSS layout reset is deliberate — the app's top-level
    ``Screen { layout: grid; grid-columns: 36 1fr; ... }`` rule
    cascades to ``ModalScreen`` subclasses and would otherwise squeeze
    the board into the 36-col sidebar slot. Same workaround as
    ``SearchScreen``.

    Navigation model: the screen owns a 2D cursor (col_idx +
    row_idx_per_col). Arrow keys move the cursor; shift+arrows reassign
    the selected session's status via ``db.set_session_status`` and
    follow the card to its new column. Enter dismisses with the
    selected ``session_id`` so the host app can load that transcript.

    Live updates: the host app calls ``update_sessions`` from
    ``_apply_session_data`` on each 5-second refresh. Selection is
    preserved across rebuilds by session_id — a card whose status
    changed elsewhere pulls the cursor with it to the new column.
    """


    # priority=True is load-bearing for the arrow keys: without it the
    # focused VerticalScroll column consumes up/down to scroll itself
    # before the Screen binding ever sees the key, which made the 2D
    # cursor feel unresponsive. Priority bindings fire before
    # widget-local ones regardless of focus.
    BINDINGS = [
        Binding("escape", "close", "Close", show=False),
        Binding("left", "cursor_col(-1)", "Prev column", show=False, priority=True),
        Binding("right", "cursor_col(1)", "Next column", show=False, priority=True),
        Binding("up", "cursor_row(-1)", "Prev card", show=False, priority=True),
        Binding("down", "cursor_row(1)", "Next card", show=False, priority=True),
        Binding("shift+left", "move_card(-1)", "Move card left",
                show=False, priority=True),
        Binding("shift+right", "move_card(1)", "Move card right",
                show=False, priority=True),
        Binding("enter", "open_card", "Open session", show=False),
    ]

    def __init__(self, sessions: list[dict], custom_names: dict[str, str]) -> None:
        super().__init__()
        self._sessions: list[dict] = list(sessions)
        self._custom_names: dict[str, str] = dict(custom_names)
        self._columns: dict[str, list[dict]] = {s: [] for s in STATUS_ORDER}
        self._col_idx: int = 0
        # Per-column row cursor — moving between columns lands on the
        # last row you visited in the destination column, which feels
        # more natural than always snapping back to row 0.
        self._row_idx_per_col: dict[int, int] = {
            i: 0 for i in range(len(STATUS_ORDER))
        }
        # Signature of the last rendered board — structural fields
        # only, not the modified mtime (which ticks every write on
        # an active session and would cause a rebuild every refresh).
        # update_sessions skips rebuild when this hasn't changed,
        # which is the 5s-refresh flicker fix.
        self._last_sig: tuple | None = None

    def compose(self) -> ComposeResult:
        with Vertical(id="kanban-container"):
            yield Static("ThreadHop — Kanban", id="kanban-title")
            with Horizontal(id="kanban-columns"):
                for status in STATUS_ORDER:
                    with Vertical(classes="kanban-column"):
                        yield Static(
                            "",
                            classes="kanban-column-header",
                            id=f"khdr-{status}",
                        )
                        # can_focus=False so the Screen keeps key focus —
                        # otherwise the scroll container grabs up/down
                        # and the 2D cursor never moves. Mouse wheel
                        # still works because that's a different event.
                        scroll = VerticalScroll(
                            id=f"kscroll-{status}",
                            classes="kanban-column-scroll",
                        )
                        scroll.can_focus = False
                        yield scroll
            yield Static(
                "←/→ column   ↑/↓ card   ⇧←/⇧→ move card   "
                "enter/click open   esc close",
                id="kanban-hint",
            )

    def on_mount(self) -> None:
        self._bucket()
        # Seed the signature so the very first 5s tick can early-return
        # when nothing has actually changed since open.
        self._last_sig = tuple(
            self._session_signature(s) for s in self._sessions
        )
        self._render_board()

    # --- data -----------------------------------------------------------

    def _bucket(self) -> None:
        """Group sessions by status, preserving input order within each bucket.

        The host app's ``_apply_stable_ordering`` already sorts sessions
        by (status_rank, manual_sort_order), so iterating in order here
        gives us columns that match the sidebar's within-status ordering
        — no surprise jumps when a user toggles between the two views.
        """
        self._columns = {s: [] for s in STATUS_ORDER}
        for s in self._sessions:
            status = s.get("status", "active")
            if status not in self._columns:
                status = "active"
            self._columns[status].append(s)

    @staticmethod
    def _session_signature(session: dict) -> tuple:
        """Structural fingerprint of a single session for rebuild-dedup.

        Deliberately **excludes** ``modified`` (file mtime) — it ticks
        on every write during an active Claude session, which would
        flip the signature every refresh and force a rebuild every 5s
        even when nothing visible to the user changed. Age displays
        drift between structural changes; that's the tradeoff for not
        flickering.
        """
        return (
            session.get("session_id") or "",
            session.get("status") or "active",
            session.get("turn_count", 0),
            bool(session.get("is_working")),
            bool(session.get("is_active")),
            session.get("custom_title") or "",
            session.get("ai_title") or "",
            (session.get("first_user_msg") or "")[:60],
        )

    def update_sessions(
        self,
        sessions: list[dict],
        custom_names: dict[str, str],
    ) -> None:
        """Called by the host app every 5s with fresh session data.

        Early-returns when the board signature is unchanged so the
        refresh tick doesn't flicker the cards. Preserves the user's
        current selection by session_id — if the selected session's
        status changed (e.g. another process updated the DB), the
        cursor follows the card to its new column.
        """
        new_sig = tuple(self._session_signature(s) for s in sessions)
        new_names = dict(custom_names)
        if new_sig == self._last_sig and new_names == self._custom_names:
            # Nothing structurally changed — skip the tear-down /
            # remount that would otherwise fire every 5 seconds.
            # Always-visible in-session edits (age ticks, new messages
            # landing) are not visible on cards anyway.
            return
        current = self._current_session()
        selected_id = current.get("session_id") if current else None
        self._sessions = list(sessions)
        self._custom_names = new_names
        self._last_sig = new_sig
        self._bucket()
        if selected_id is not None:
            self._focus_session_id(selected_id)
        self._render_board()

    def _current_status(self) -> str:
        return STATUS_ORDER[self._col_idx]

    def _current_column(self) -> list[dict]:
        return self._columns[self._current_status()]

    def _current_session(self) -> dict | None:
        col = self._current_column()
        if not col:
            return None
        row = min(self._row_idx_per_col[self._col_idx], len(col) - 1)
        return col[row]

    def _focus_session_id(self, session_id: str) -> bool:
        for i, status in enumerate(STATUS_ORDER):
            for r, s in enumerate(self._columns[status]):
                if s.get("session_id") == session_id:
                    self._col_idx = i
                    self._row_idx_per_col[i] = r
                    return True
        return False

    # --- render ---------------------------------------------------------

    def _render_board(self) -> None:
        for i, status in enumerate(STATUS_ORDER):
            cards = self._columns[status]
            header = self.query_one(f"#khdr-{status}", Static)
            header.update(f"{STATUS_LABELS[status]}  ({len(cards)})")

            scroll = self.query_one(f"#kscroll-{status}", VerticalScroll)
            scroll.remove_children()
            if not cards:
                scroll.mount(Static("— empty —", classes="kanban-empty"))
                continue

            row_sel = min(self._row_idx_per_col[i], len(cards) - 1)
            self._row_idx_per_col[i] = row_sel
            for r, s in enumerate(cards):
                is_selected = (i == self._col_idx and r == row_sel)
                card = KanbanCard(
                    s,
                    custom_name=self._custom_names.get(s.get("session_id")),
                    selected=is_selected,
                )
                scroll.mount(card)

        # Defer scroll-into-view so the mounted cards exist before we
        # try to address them — mount is async from the renderer's
        # perspective and scroll_to_widget needs a real widget handle.
        self.call_after_refresh(self._scroll_selected_into_view)

    def _scroll_selected_into_view(self) -> None:
        status = self._current_status()
        row = self._row_idx_per_col[self._col_idx]
        try:
            scroll = self.query_one(f"#kscroll-{status}", VerticalScroll)
        except Exception:
            return
        cards = [c for c in scroll.children if isinstance(c, KanbanCard)]
        if 0 <= row < len(cards):
            scroll.scroll_to_widget(cards[row], animate=False)

    def _update_selection(self) -> None:
        """Toggle the ``-selected`` CSS class on whichever card the
        cursor is on — **without** re-mounting any cards. This is the
        cursor-move flicker fix: arrow keys used to call
        ``_render_board``, which tears down and re-creates every card
        in every column on every keystroke. Now cursor nav is a pure
        class-flip, which Textual repaints in place.
        """
        cur_status = STATUS_ORDER[self._col_idx]
        cur_row = self._row_idx_per_col[self._col_idx]
        for status in STATUS_ORDER:
            try:
                scroll = self.query_one(f"#kscroll-{status}", VerticalScroll)
            except Exception:
                continue
            cards = [c for c in scroll.children if isinstance(c, KanbanCard)]
            for r, card in enumerate(cards):
                should = (status == cur_status and r == cur_row)
                has = card.has_class("-selected")
                if should and not has:
                    card.add_class("-selected")
                elif not should and has:
                    card.remove_class("-selected")
        self.call_after_refresh(self._scroll_selected_into_view)

    # --- actions --------------------------------------------------------

    def action_close(self) -> None:
        self.dismiss(None)

    def action_cursor_col(self, delta: int) -> None:
        self._col_idx = (self._col_idx + delta) % len(STATUS_ORDER)
        # Class-flip only — no tear-down / remount. See _update_selection.
        self._update_selection()

    def action_cursor_row(self, delta: int) -> None:
        col = self._current_column()
        if not col:
            return
        cur = self._row_idx_per_col[self._col_idx]
        self._row_idx_per_col[self._col_idx] = max(
            0, min(cur + delta, len(col) - 1)
        )
        self._update_selection()

    def action_move_card(self, delta: int) -> None:
        """Reassign the selected session's status by ``delta`` columns.

        Writes to the DB first so the next refresh won't resurrect the
        old status; updates the in-memory dict so the re-render moves
        the card immediately (optimistic, but the write is synchronous
        SQLite so latency is sub-ms).
        """
        session = self._current_session()
        if session is None:
            return
        new_col_idx = self._col_idx + delta
        if not (0 <= new_col_idx < len(STATUS_ORDER)):
            return
        new_status = STATUS_ORDER[new_col_idx]
        if new_status == session.get("status"):
            return
        session_id = session["session_id"]
        try:
            db.set_session_status(self.app.conn, session_id, new_status)
        except Exception:
            # A failed write shouldn't crash the view; the next live
            # refresh will reconcile the display with the real DB state.
            return
        session["status"] = new_status
        self._bucket()
        self._col_idx = new_col_idx
        for r, s in enumerate(self._columns[new_status]):
            if s.get("session_id") == session_id:
                self._row_idx_per_col[new_col_idx] = r
                break
        self._render_board()
        # Refresh the dedup signature so the next 5s refresh tick
        # (which will see the same post-move state from the DB via
        # _gather_session_data) early-returns instead of triggering
        # a second, wasted rebuild.
        self._last_sig = tuple(
            self._session_signature(s) for s in self._sessions
        )

    def action_open_card(self) -> None:
        session = self._current_session()
        if session is None:
            return
        self.dismiss(session.get("session_id"))
