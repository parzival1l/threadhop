"""Module-level TUI helpers extracted from the legacy ``tui.py``.

These are pure functions / lightweight derivations used across the App,
widgets, and screens. Search- and Kanban-specific helpers stay close to
their owning screens; keep this file scoped to "general TUI plumbing".
"""

from __future__ import annotations

import os
import subprocess
import sys
from datetime import datetime
from pathlib import Path

from rich.text import Text
from textual.binding import Binding

from threadhop_core import indexer

from .constants import (
    DISPLAY_NAME_WIDTH,
    OBSERVATION_MARKER,
    OBSERVATION_MARKER_FALLBACK,
    SPINNER_FRAMES,
)
from .keybindings import COMMAND_REGISTRY, Command, format_key


def commands_for_scope(scope: str) -> list[Command]:
    return [c for c in COMMAND_REGISTRY if c.scope == scope]


def format_command_keys(cmd: Command) -> str:
    """Render a command's keys as a slash-separated list for display."""
    return "/".join(format_key(k) for k in cmd.keys)


def app_bindings_from_registry() -> list[Binding]:
    """Derive App-level ``Binding`` entries from the registry.

    Every Command with an ``action`` is bound at the App level. The
    scope field is advisory for display — app bindings fire globally,
    but each action already gates itself on focus / mode state (see
    e.g. ``_input_has_focus`` in action handlers). Widget-local keys
    (selection mode, find bar, search modal) are deliberately skipped
    because their widgets handle the key in ``on_key`` and stop the
    event before it reaches an app binding.
    """
    bindings: list[Binding] = []
    for cmd in COMMAND_REGISTRY:
        if cmd.action is None:
            continue
        for key in cmd.keys:
            bindings.append(
                Binding(
                    key,
                    cmd.action,
                    cmd.description,
                    show=False,
                    priority=cmd.priority,
                )
            )
    # Escape on the reply input action is the app-level cancel_reply;
    # it doubles as "close find bar" when the bar is up (see
    # action_cancel_reply). Registered via the SCOPE_REPLY entry above.
    return bindings


def format_age(timestamp: float) -> str:
    """Format a timestamp as a human-readable age string."""
    age = datetime.now().timestamp() - timestamp
    if age < 60:
        return f"{int(age)}s"
    elif age < 3600:
        return f"{int(age / 60)}m"
    elif age < 86400:
        return f"{int(age / 3600)}h"
    else:
        return f"{int(age / 86400)}d"


def format_msg_clock(ts: str | float | None) -> str:
    """Render a JSONL message timestamp as ``HH:MM`` for inline display.

    Accepts the ISO-8601 strings Claude writes into transcript JSONL
    (``"2026-04-26T15:42:03.123Z"``) and returns the local-time hour and
    minute. Empty / unparseable input returns "" so callers can simply
    skip rendering the suffix.
    """
    if not ts:
        return ""
    try:
        if isinstance(ts, (int, float)):
            dt = datetime.fromtimestamp(float(ts))
        else:
            dt = datetime.fromisoformat(str(ts).replace("Z", "+00:00"))
        return dt.strftime("%H:%M")
    except (ValueError, AttributeError, TypeError):
        return ""


def _supports_observation_emoji() -> bool:
    """Return whether the current terminal can safely encode the emoji marker."""
    if os.environ.get("THREADHOP_ASCII_OBSERVATION_MARKER") == "1":
        return False
    encoding = sys.stdout.encoding or "utf-8"
    try:
        OBSERVATION_MARKER.encode(encoding)
    except (LookupError, UnicodeEncodeError):
        return False
    return True


def _observation_marker_text() -> Text:
    """Return the ADR-021 observed-session marker."""
    if _supports_observation_emoji():
        return Text(OBSERVATION_MARKER, style="dim")
    return Text(OBSERVATION_MARKER_FALLBACK, style="dim cyan")


def _session_display_name(session_data: dict, custom_name: str | None) -> str:
    """Resolve the user-facing session label before sidebar truncation."""
    title = session_data.get("title", "")
    if custom_name:
        return custom_name
    if title:
        return title
    return session_data["project"]


def copy_to_clipboard(text: str) -> bool:
    """Copy text to the system clipboard."""
    try:
        if sys.platform == "darwin":
            subprocess.run(["pbcopy"], input=text.encode(), check=True)
        else:
            subprocess.run(
                ["xclip", "-selection", "clipboard"],
                input=text.encode(),
                check=True,
            )
        return True
    except Exception:
        return False


def _threadhop_script_path() -> Path:
    """Locate the ``./threadhop`` executable that owns this checkout.

    The script lives at the project root, one level above the
    ``threadhop_core`` package. Resolved through the package's
    ``__file__`` so we follow symlinks (e.g. ``~/.local/bin/threadhop``
    pointing at the cloned checkout) rather than guessing from
    ``sys.argv[0]``.
    """
    return Path(indexer.__file__).resolve().parents[1] / "threadhop"


def build_observe_command(session_id: str) -> list[str]:
    """Spawn the CLI sidecar through the current executable script."""
    return [str(_threadhop_script_path()), "observe", "--session", session_id]


def render_session_label_text(
    session_data: dict,
    *,
    custom_name: str | None = None,
    spinner_frame: int = 0,
) -> Text:
    """Build the Rich label for one sidebar session row."""
    is_working = session_data.get("is_working", False)
    is_active = session_data.get("is_active", False)
    if is_working:
        status = SPINNER_FRAMES[spinner_frame % len(SPINNER_FRAMES)]
    elif is_active:
        status = "●"
    else:
        status = "○"

    display = _session_display_name(session_data, custom_name)
    indicator = (
        _observation_marker_text()
        if session_data.get("has_observations")
        else None
    )
    reserved_width = 0
    if indicator is not None:
        reserved_width = 1 + indicator.cell_len
    name_width = max(DISPLAY_NAME_WIDTH - reserved_width, 0)

    middle = Text(display[:name_width])
    if indicator is not None:
        middle.append(" ")
        middle.append_text(indicator)
    if middle.cell_len < DISPLAY_NAME_WIDTH:
        middle.append(" " * (DISPLAY_NAME_WIDTH - middle.cell_len))

    age_str = format_age(session_data["modified"])
    text = Text()
    text.append(f"{status} ")
    text.append_text(middle)
    text.append(f" {age_str:>4}")
    return text


__all__ = [
    "app_bindings_from_registry",
    "build_observe_command",
    "commands_for_scope",
    "copy_to_clipboard",
    "format_age",
    "format_command_keys",
    "format_msg_clock",
    "render_session_label_text",
    "_observation_marker_text",
    "_session_display_name",
    "_supports_observation_emoji",
    "_threadhop_script_path",
]
