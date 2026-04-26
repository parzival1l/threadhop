"""``HelpScreen`` — context-aware help overlay grouped by scope (ADR-017)."""

from __future__ import annotations

from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widgets import Static

from ..keybindings import COMMAND_REGISTRY, SCOPE_LABELS, SCOPE_ORDER, format_key


def _commands_for_scope(scope: str):
    return [c for c in COMMAND_REGISTRY if c.scope == scope]


def _format_command_keys(cmd) -> str:
    return "/".join(format_key(k) for k in cmd.keys)


class HelpScreen(ModalScreen):
    """Full-app help overlay grouped by scope (ADR-017, task #42).

    Mirrors the search modal layout so the two discoverability surfaces
    feel consistent. Reads entirely from ``COMMAND_REGISTRY`` — anything
    new added there appears here automatically.
    """

    BINDINGS = [
        Binding("escape", "close", "Close", priority=True),
        Binding("?", "close", "Close", priority=True),
        Binding("q", "close", "Close", priority=True),
    ]

    # OpenCode-style command palette: no border, contrast comes from a
    # 2-step luminance jump between the dimmed page and the panel
    # background. Section headers are muted (not bright accent), and the
    # only colored element is the keybinding column. This matches what
    # makes OpenCode's palette feel "sleek" — one fill defines the
    # surface, one accent highlights the focused row, and everything
    # else is text/muted-text.

    def compose(self) -> ComposeResult:
        with Vertical(id="help-container"):
            yield Static("Commands", id="help-title")
            yield VerticalScroll(id="help-body")
            yield Static("esc to close", id="help-hint")

    def on_mount(self) -> None:
        body = self.query_one("#help-body", VerticalScroll)
        for scope in SCOPE_ORDER:
            commands = _commands_for_scope(scope)
            if not commands:
                continue
            body.mount(Static(SCOPE_LABELS[scope], classes="help-scope"))
            for cmd in commands:
                body.mount(
                    Horizontal(
                        Static(cmd.description, classes="help-row-desc"),
                        Static(_format_command_keys(cmd), classes="help-row-keys"),
                        classes="help-row",
                    )
                )

    def action_close(self) -> None:
        self.dismiss(None)


__all__ = ["HelpScreen"]
