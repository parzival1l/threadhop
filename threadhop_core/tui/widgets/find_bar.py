"""Persistent in-transcript find bar.

Renders above ``TranscriptView`` and provides "find in page" semantics
within the currently loaded transcript. Cross-session FTS lookup lives
in ``screens.search.SearchScreen`` instead.
"""

from __future__ import annotations

from textual.app import ComposeResult
from textual.containers import Horizontal
from textual.widgets import Input, Static


class FindBar(Horizontal):
    """Persistent Ctrl+F-style find bar shown above the transcript.

    Stays open until the user dismisses it (Esc, ×, or Ctrl+F again), so
    the active query remains editable and the match counter remains
    visible across session switches. Unlike ``SearchScreen`` (which
    issues FTS5 queries across ALL indexed sessions), this bar operates
    purely on widgets already rendered in the current ``TranscriptView``
    — it's a within-transcript "find in page", not a cross-session
    search.

    Hidden by default via ``display = False``; App.action_open_find
    toggles it visible and focuses the input.
    """

    def compose(self) -> ComposeResult:
        yield Input(
            placeholder="Find in transcript… (Enter=next, ↑/↓ prev/next, Esc=close)",
            id="find-input",
        )
        yield Static("", id="find-status")
        yield Static("[×]", id="find-close")

    def update_status(self, current: int, total: int, has_query: bool) -> None:
        """Set the match counter. ``has_query=False`` blanks the status."""
        status = self.query_one("#find-status", Static)
        if not has_query:
            status.update("")
        elif total == 0:
            status.update("No matches")
        elif total == 1:
            status.update("1 match")
        else:
            status.update(f"{current} of {total} matches")

    def on_key(self, event) -> None:
        """Arrow / Escape bubble up from the Input — keep them here.

        Up/Down navigate matches without leaving the input. Escape
        closes the bar and clears highlights. Enter is handled by the
        app's ``on_input_submitted`` because Input fires Submitted on
        Enter, not a raw key event here.
        """
        key = event.key
        if key == "escape":
            event.stop()
            event.prevent_default()
            self.app._close_find()
        elif key == "down":
            event.stop()
            event.prevent_default()
            self.app.action_find_next()
        elif key == "up":
            event.stop()
            event.prevent_default()
            self.app.action_find_prev()

    def on_click(self, event) -> None:
        """Click on the × static to dismiss.

        Textual's Click event bubbles up the widget tree; checking the
        event's widget id here lets the whole bar act as a click host
        without extra message plumbing.
        """
        try:
            target = getattr(event, "widget", None)
            if target is not None and target.id == "find-close":
                event.stop()
                self.app._close_find()
        except Exception:
            pass


__all__ = ["FindBar"]
