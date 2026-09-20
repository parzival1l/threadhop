"""Single-line scope-aware footer (replaces Textual's stock ``Footer``).

The stock ``Footer`` widget lists every visible binding at once, which
turns into noise once the app has focus-aware and mode-specific
commands. ``ContextualFooter`` reads ``COMMAND_REGISTRY`` and renders
only the commands whose scope matches the caller-supplied list,
preferring those marked ``footer=True``.
"""

from __future__ import annotations

from rich.markup import escape
from textual.widgets import Static

from ..keybindings import Command, SCOPE_GLOBAL
from ..utils import commands_for_scope, format_command_keys


class ContextualFooter(Static):
    """Single-line footer that shows scope-relevant hints from the registry.

    The stock ``Footer`` widget lists every visible binding at once, which
    turns into noise once the app has focus-aware and mode-specific
    commands. Instead we read ``COMMAND_REGISTRY`` and render only
    commands whose scope matches the caller-supplied context, preferring
    those marked ``footer=True``.
    """

    def __init__(self, *args, **kwargs):
        super().__init__("", *args, **kwargs)
        self._last_render: tuple[tuple[str, ...], str | None] = ((), None)

    def render_for(self, scopes: list[str], note: str | None = None) -> None:
        """Rebuild the footer content for the given list of active scopes.

        Scopes are evaluated in the order passed — the registry entries
        for later scopes win when a key appears in multiple (e.g. Enter
        is Reply/Send globally but Send in the reply scope).
        """
        scope_tuple = tuple(scopes)
        signature = (scope_tuple, note)
        if signature == self._last_render:
            # Footer already reflects the right context — skip the rebuild.
            return
        self._last_render = signature

        # Keep the help shortcut pinned on the far left so it stays
        # discoverable regardless of current focus.
        parts: list[str] = []
        seen_keys: set[tuple[str, ...]] = set()

        def add(cmd: Command) -> None:
            if cmd.keys in seen_keys:
                return
            seen_keys.add(cmd.keys)
            parts.append(f"[b]{format_command_keys(cmd)}[/b] {cmd.description}")

        # Global "?" first, so it's always in the same spot.
        for cmd in commands_for_scope(SCOPE_GLOBAL):
            if cmd.keys == ("?",):
                add(cmd)
                break

        for scope in scopes:
            for cmd in commands_for_scope(scope):
                if cmd.footer and cmd.keys != ("?",):
                    add(cmd)

        if note:
            parts.append(f"[dim]{escape(note)}[/dim]")

        if not parts:
            self.update("")
            return
        sep = "  [dim]·[/dim]  "
        self.update(sep.join(parts))


__all__ = ["ContextualFooter"]
