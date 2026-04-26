"""Module-level constants for the ThreadHop Textual UI.

These are the small, value-only globals the TUI used to keep at script
top-level. Keeping them in a single module lets widget/screen modules
import what they need without re-importing :mod:`tui` (which would
cycle through the App).
"""

from __future__ import annotations

import re

from ..storage import db


DISPLAY_NAME_WIDTH = 22
OBSERVATION_MARKER = "🗒"
OBSERVATION_MARKER_FALLBACK = "≡"
MAX_SESSIONS = 50
REFRESH_INTERVAL = 5
SPINNER_INTERVAL = 0.25
SEARCH_PAGE_SIZE = 50

SPINNER_FRAMES = ["◐", "◓", "◑", "◒"]

# Regex to strip <system-reminder>...</system-reminder> blocks from text.
SYSTEM_REMINDER_RE = re.compile(r"<system-reminder>.*?</system-reminder>", re.DOTALL)

# Claude Code prepends <local-command-*> blocks to the first user message
# when a session begins via a slash command or `!` bash passthrough. Strip
# before using the text as a sidebar title.
LOCAL_COMMAND_RE = re.compile(
    r"<local-command-(?:caveat|stdout|stderr)>.*?</local-command-(?:caveat|stdout|stderr)>",
    re.DOTALL,
)

# Slash-command invocations surface as <command-name>/foo</command-name>
# plus <command-message>/<command-args> siblings. Strip the tag markup
# but keep the inner text so the title stays meaningful.
COMMAND_TAG_RE = re.compile(
    r"</?(?:command-name|command-message|command-args)>"
)

# Filter observer/reflector subprocess sessions out of the sidebar.
OBSERVER_SUBPROCESS_SIGNATURES: tuple[str, ...] = (
    "# ThreadHop Observer Prompt",
    "# ThreadHop Reflector Prompt",
)

# Status display order + labels (ADR-004) — duplicated from db.py so the
# TUI doesn't reach into storage internals.
STATUS_ORDER: list[str] = db.SESSION_STATUS_ORDER
STATUS_LABELS: dict[str, str] = {
    "active":      "Active",
    "in_progress": "In Progress",
    "in_review":   "In Review",
    "done":        "Done",
    "archived":    "Archived",
}
STATUS_RANK: dict[str, int] = {s: i for i, s in enumerate(STATUS_ORDER)}
STATUS_CYCLE: list[str] = [s for s in STATUS_ORDER if s != "archived"]


__all__ = [
    "DISPLAY_NAME_WIDTH",
    "OBSERVATION_MARKER",
    "OBSERVATION_MARKER_FALLBACK",
    "MAX_SESSIONS",
    "REFRESH_INTERVAL",
    "SPINNER_INTERVAL",
    "SEARCH_PAGE_SIZE",
    "SPINNER_FRAMES",
    "SYSTEM_REMINDER_RE",
    "LOCAL_COMMAND_RE",
    "COMMAND_TAG_RE",
    "OBSERVER_SUBPROCESS_SIGNATURES",
    "STATUS_ORDER",
    "STATUS_LABELS",
    "STATUS_RANK",
    "STATUS_CYCLE",
]
