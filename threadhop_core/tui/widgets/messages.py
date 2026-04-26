"""Message widgets shown inside ``TranscriptView``.

Each role gets its own subclass of ``_SelectableMessage`` so the App-level
CSS (see ``tui/css/app.tcss``) can target them with type selectors and
give each role its own border accent / background tint.
"""

from __future__ import annotations

from textual.widgets import Static


class _SelectableMessage(Static):
    """Base for message widgets that participate in selection mode.

    can_focus is deliberately False — focus stays on the parent
    TranscriptView so its on_key handler receives all keypresses.
    Highlighting is driven purely by the .message-selected CSS class.
    """
    can_focus = False


class UserMessage(_SelectableMessage):
    """A user message in the transcript"""
    pass


class AssistantMessage(_SelectableMessage):
    """An assistant message in the transcript"""
    pass


class ToolMessage(_SelectableMessage):
    """A tool call/result in the transcript"""
    pass


class CommandPill(_SelectableMessage):
    """Compact one-line pill for slash-command invocations and skill loads.

    Claude Code synthesises two flavours of user-role JSONL line that
    aren't really user content: ``<command-name>/foo</command-name>``
    invocation markers, and the skill-load banner + injected skill body
    that follows them. Rendering either as a full UserMessage drowns the
    real conversation in 60-line skill markdown blocks (see the screenshot
    in the issue that prompted this widget). Collapsing both into one
    dim pill keeps the audit trail (you can still see *that* a command
    fired) without spending vertical space on what the user sees as UI
    chrome.

    The pill participates in selection mode so y/e still work — but it
    has no border-left accent, so it visually recedes next to real
    UserMessage / AssistantMessage rows.
    """
    pass


class ObservationInfoHeader(Static):
    """Passive observation metadata shown above the transcript."""

    can_focus = False


__all__ = [
    "_SelectableMessage",
    "UserMessage",
    "AssistantMessage",
    "ToolMessage",
    "CommandPill",
    "ObservationInfoHeader",
]
