"""Bookmark browser modal (task #18).

Lists all bookmarks across sessions, with a substring filter Input on
top and the same SearchScreen-shaped layout so muscle memory carries
over. Enter dismisses with ``(session_id, uuid)`` so the App can jump
to the source message.
"""

from __future__ import annotations

import sqlite3
from datetime import datetime

from rich.console import Group
from rich.markup import escape
from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.screen import ModalScreen
from textual.widgets import Input, ListItem, ListView, Static

from threadhop_core.storage import db


class BookmarkItem(ListItem):
    """One row in the bookmark browser.

    Layout mirrors SearchResultItem so the two modals feel like siblings:
    note / kind / role marker + snippet, then session · project · timestamp.
    Bookmarks without a note fall back to the kind + role marker alone.
    """

    def __init__(self, row: dict):
        self.row = row
        super().__init__()

    def compose(self) -> ComposeResult:
        role = self.row.get("role", "")
        role_icon = "▶" if role == "user" else "●"
        role_style = "bold cyan" if role == "user" else "bold green"
        note = (self.row.get("note") or "").strip()
        kind = self.row.get("kind") or "bookmark"
        kind_style = "bold magenta" if kind == "research" else "bold yellow"

        first_line = Text()
        first_line.append("★ ", style="bold yellow")
        first_line.append(f"[{kind}] ", style=kind_style)
        if note:
            first_line.append(note, style="bold")
            first_line.append("  ", style="dim")
        first_line.append(f"{role_icon} ", style=role_style)
        # Trim the stored text to a single-line snippet — the full body
        # is reachable via Enter → jump.
        raw_text = (self.row.get("text") or "").strip().replace("\n", " ")
        snippet = raw_text[:160] + ("…" if len(raw_text) > 160 else "")
        first_line.append(snippet)

        session_name = (
            self.row.get("custom_name")
            or self.row.get("project")
            or (self.row.get("session_id", "") or "")[:8]
        )
        project = self.row.get("project") or ""
        ts_raw = self.row.get("timestamp") or ""
        ts_str = ts_raw[:16]
        try:
            dt = datetime.fromisoformat(ts_raw.replace("Z", "+00:00"))
            ts_str = dt.strftime("%Y-%m-%d %H:%M")
        except (ValueError, AttributeError):
            pass

        meta = Text()
        meta.append("  ", style="dim")
        meta.append(str(session_name), style="bold")
        if project and project != session_name:
            meta.append("  ·  ", style="dim")
            meta.append(str(project), style="cyan")
        meta.append("  ·  ", style="dim")
        meta.append(ts_str, style="dim")

        yield Static(Group(first_line, meta), classes="bookmark-item")


class BookmarkBrowserScreen(ModalScreen):
    """Modal bookmark browser (task #18).

    Shares the SearchScreen layout — filter Input on top, ListView of
    results, status + hint lines at the bottom — so keyboard muscle
    memory carries over. Filter runs case-insensitive substring matches
    over note / kind / message body / project / custom name via
    ``db.list_bookmarks``.

    Dismiss payload:
      - ``(session_id, message_uuid)`` when the user hits Enter on a row
      - ``None`` on Escape

    The caller routes the payload through ``_jump_to_search_result``
    which handles all three cases (same session, sidebar session,
    cross-project/out-of-view session).
    """

    DEBOUNCE_SECONDS = 0.08

    BINDINGS = [
        Binding("enter", "open_result", "Open", priority=True, show=False),
    ]


    def __init__(self, conn: sqlite3.Connection):
        super().__init__()
        self.conn = conn
        self._filter_timer = None

    def compose(self) -> ComposeResult:
        with Vertical(id="bookmark-container") as container:
            container.border_title = "Bookmarks"
            yield Input(
                placeholder="Filter bookmarks by note, kind, text, or session…",
                id="bookmark-input",
            )
            yield Static("", id="bookmark-status")
            yield ListView(id="bookmark-results")
            yield Static(
                "↑/↓ navigate • Enter jump • L note • d delete • Esc close",
                id="bookmark-help",
            )

    def on_mount(self) -> None:
        self._refresh_results("")
        self.query_one("#bookmark-input", Input).focus()

    def on_input_changed(self, event) -> None:
        try:
            if event.input.id != "bookmark-input":
                return
        except AttributeError:
            return
        if self._filter_timer is not None:
            try:
                self._filter_timer.stop()
            except Exception:
                pass
        value = event.value
        self._filter_timer = self.set_timer(
            self.DEBOUNCE_SECONDS, lambda v=value: self._refresh_results(v)
        )

    def _refresh_results(self, raw_query: str) -> None:
        """Reload the results list from the DB. Preserves cursor position
        when possible so in-place edits (note / delete) don't jump the
        user back to the top."""
        results_list = self.query_one("#bookmark-results", ListView)
        status = self.query_one("#bookmark-status", Static)

        prev_idx = results_list.index
        results_list.clear()

        query = raw_query.strip() or None
        try:
            rows = db.list_bookmarks(self.conn, query=query, limit=200)
        except Exception as e:  # noqa: BLE001
            status.update(f"Bookmark query error: {e}")
            return

        for row in rows:
            results_list.append(BookmarkItem(row))

        count = len(rows)
        if count == 0:
            status.update("No bookmarks" if query is None else "No bookmarks match")
        else:
            noun = "bookmark" if count == 1 else "bookmarks"
            status.update(
                f"{count} {noun}" + (" (showing first 200)" if count >= 200 else "")
            )

        # Restore cursor if still in range — otherwise clamp.
        if count:
            if prev_idx is None:
                results_list.index = 0
            else:
                results_list.index = min(prev_idx, count - 1)

    def on_key(self, event) -> None:
        key = event.key
        if key == "escape":
            event.stop()
            event.prevent_default()
            self.dismiss(None)
        elif key in ("down", "ctrl+n"):
            event.stop()
            event.prevent_default()
            self._move_selection(1)
        elif key in ("up", "ctrl+p"):
            event.stop()
            event.prevent_default()
            self._move_selection(-1)
        elif key == "L":
            event.stop()
            event.prevent_default()
            self._edit_note_selected()
        elif key == "d":
            event.stop()
            event.prevent_default()
            self._delete_selected()

    def action_open_result(self) -> None:
        self._open_selected()

    def on_input_submitted(self, event) -> None:
        try:
            if event.input.id != "bookmark-input":
                return
        except AttributeError:
            return
        event.stop()
        self._open_selected()

    def _move_selection(self, delta: int) -> None:
        lv = self.query_one("#bookmark-results", ListView)
        count = len(lv.children)
        if count == 0:
            return
        if lv.index is None:
            lv.index = 0 if delta > 0 else count - 1
            return
        lv.index = max(0, min(count - 1, lv.index + delta))

    def _current_row(self) -> dict | None:
        lv = self.query_one("#bookmark-results", ListView)
        if not lv.children:
            return None
        idx = lv.index if lv.index is not None else 0
        if idx < 0 or idx >= len(lv.children):
            idx = 0
        item = lv.children[idx]
        if not isinstance(item, BookmarkItem):
            return None
        return item.row

    def _open_selected(self) -> None:
        row = self._current_row()
        if row is None:
            return
        self.dismiss((row["session_id"], row["message_uuid"]))

    def _edit_note_selected(self) -> None:
        row = self._current_row()
        if row is None:
            return
        bookmark_id = row["id"]
        current = row.get("note") or ""

        def _apply(new_note: str | None) -> None:
            if new_note is None:
                return
            try:
                db.set_bookmark_note(self.conn, bookmark_id, new_note)
            except Exception as e:  # noqa: BLE001
                self.app.notify(f"Note update failed: {e}", severity="error")
                return
            # Refresh in place so the edited note renders immediately.
            raw = self.query_one("#bookmark-input", Input).value or ""
            self._refresh_results(raw)
            shown = new_note.strip() or "(no note)"
            self.app.notify(f"Bookmark note: {shown}")

        self.app.push_screen(LabelPromptScreen(current), _apply)

    def _delete_selected(self) -> None:
        row = self._current_row()
        if row is None:
            return
        try:
            db.delete_bookmark(self.conn, row["id"])
        except Exception as e:  # noqa: BLE001
            self.app.notify(f"Delete failed: {e}", severity="error")
            return
        raw = self.query_one("#bookmark-input", Input).value or ""
        self._refresh_results(raw)
        self.app.notify("Bookmark removed")

