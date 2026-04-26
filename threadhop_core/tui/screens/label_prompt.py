"""``LabelPromptScreen`` — tiny single-line input modal."""

from __future__ import annotations

from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.screen import ModalScreen
from textual.widgets import Input, Static


class LabelPromptScreen(ModalScreen):
    """Tiny modal that captures a single line of text.

    Used both when setting a note from selection mode and when editing one from
    the browser. Returns the submitted string on Enter, or ``None`` on Escape.
    The caller decides whether an empty submission means "clear" (bookmark
    browser does that via ``set_bookmark_note``'s blank-collapses-to-NULL
    behaviour).
    """

    BINDINGS = [
        Binding("enter", "submit", "Save", priority=True, show=False),
        Binding("escape", "cancel", "Cancel", priority=True, show=False),
    ]


    def __init__(self, initial: str = ""):
        super().__init__()
        self._initial = initial

    def compose(self) -> ComposeResult:
        with Vertical(id="label-container"):
            yield Static("Bookmark note", id="label-title")
            yield Input(
                value=self._initial,
                placeholder="Note (blank = clear)",
                id="label-input",
            )
            yield Static("Enter to save • Esc to cancel", id="label-hint")

    def on_mount(self) -> None:
        self.query_one("#label-input", Input).focus()

    def action_submit(self) -> None:
        value = self.query_one("#label-input", Input).value or ""
        self.dismiss(value)

    def action_cancel(self) -> None:
        self.dismiss(None)

    def on_input_submitted(self, event) -> None:
        try:
            if event.input.id != "label-input":
                return
        except AttributeError:
            return
        event.stop()
        self.action_submit()


__all__ = ["LabelPromptScreen"]
