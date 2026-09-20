"""``ConfirmScreen`` — minimal yes/no modal."""

from __future__ import annotations

from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.screen import ModalScreen
from textual.widgets import Static


class ConfirmScreen(ModalScreen):
    """Minimal yes/no confirmation modal for TUI actions."""

    BINDINGS = [
        Binding("y", "confirm", "Yes", priority=True, show=False),
        Binding("n", "cancel", "No", priority=True, show=False),
        Binding("enter", "confirm", "Yes", priority=True, show=False),
        Binding("escape", "cancel", "No", priority=True, show=False),
    ]


    def __init__(self, title: str):
        super().__init__()
        self._title = title

    def compose(self) -> ComposeResult:
        with Vertical(id="confirm-container"):
            yield Static(self._title, id="confirm-title")
            yield Static("y/Enter = yes • n/Esc = no", id="confirm-hint")

    def action_confirm(self) -> None:
        self.dismiss(True)

    def action_cancel(self) -> None:
        self.dismiss(False)


__all__ = ["ConfirmScreen"]
