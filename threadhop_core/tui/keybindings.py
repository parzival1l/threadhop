"""Command metadata registry (ADR-017, task #42).

One source of truth for app-level bindings, widget-local keys, the
contextual footer, and the help overlay. Any new command lands here
and automatically becomes discoverable everywhere.

Scopes group commands by where they're reachable. Widget-local
handlers (TranscriptView selection mode, FindBar, SearchScreen) own
their behaviour — the registry just records the metadata so the rest
of the UI can surface it.
"""

from __future__ import annotations

from dataclasses import dataclass

SCOPE_GLOBAL = "global"
SCOPE_SESSION_LIST = "session_list"
SCOPE_TRANSCRIPT = "transcript"
SCOPE_SELECTION = "selection"
SCOPE_REPLY = "reply"
SCOPE_FIND = "find"
SCOPE_SEARCH = "search"
SCOPE_BOOKMARKS = "bookmarks"

SCOPE_ORDER: list[str] = [
    SCOPE_GLOBAL,
    SCOPE_SESSION_LIST,
    SCOPE_TRANSCRIPT,
    SCOPE_SELECTION,
    SCOPE_REPLY,
    SCOPE_FIND,
    SCOPE_SEARCH,
    SCOPE_BOOKMARKS,
]

SCOPE_LABELS: dict[str, str] = {
    SCOPE_GLOBAL: "Global",
    SCOPE_SESSION_LIST: "Session list",
    SCOPE_TRANSCRIPT: "Transcript",
    SCOPE_SELECTION: "Selection mode",
    SCOPE_REPLY: "Reply input",
    SCOPE_FIND: "Find bar",
    SCOPE_SEARCH: "Search modal",
    SCOPE_BOOKMARKS: "Bookmarks modal",
}

# Pretty-print Textual key strings for the help overlay and footer.
# Lookup is exact-match first, then falls back to capitalizing the raw
# string so unknown keys still show up reasonably.
_KEY_DISPLAY: dict[str, str] = {
    "up": "↑",
    "down": "↓",
    "left": "←",
    "right": "→",
    "enter": "Enter",
    "escape": "Esc",
    "pageup": "PgUp",
    "pagedown": "PgDn",
    "home": "Home",
    "end": "End",
    "space": "Space",
    "tab": "Tab",
    "left_square_bracket": "[",
    "right_square_bracket": "]",
    "shift+up": "⇧↑",
    "shift+down": "⇧↓",
    "alt+enter": "⌥Enter",
    "alt+j": "⌥j",
    "alt+k": "⌥k",
    "ctrl+f": "^F",
    "ctrl+n": "^N",
    "ctrl+p": "^P",
}


def format_key(key: str) -> str:
    return _KEY_DISPLAY.get(key, key)


@dataclass(frozen=True)
class Command:
    """One row of the command registry.

    ``keys`` holds Textual-valid key names (``"ctrl+f"``, ``"shift+down"``,
    ``"left_square_bracket"``) — prettification for display happens in
    ``format_key``. When ``action`` is set the command is wired as an
    App-level ``Binding``; otherwise the command is widget-local and the
    registry entry exists purely for discoverability (footer, help
    overlay).
    """

    keys: tuple[str, ...]
    description: str
    scope: str
    action: str | None = None
    priority: bool = False
    # Footer commands are the minimal contextual hint row — only the
    # highest-value bindings for each scope are marked ``footer=True``.
    footer: bool = False


COMMAND_REGISTRY: list[Command] = [
    # --- Global ---
    Command(("?",), "Help", SCOPE_GLOBAL, "open_help", footer=True),
    Command(("q",), "Quit", SCOPE_GLOBAL, "maybe_quit", footer=True),
    Command(("r",), "Refresh sessions", SCOPE_GLOBAL, "maybe_refresh"),
    Command(("t",), "Next theme", SCOPE_GLOBAL, "toggle_theme_next"),
    Command(("T",), "Previous theme", SCOPE_GLOBAL, "toggle_theme_prev"),
    Command(("/",), "Search all sessions", SCOPE_GLOBAL, "open_search", footer=True),
    Command(("ctrl+f",), "Find in transcript", SCOPE_GLOBAL, "open_find", footer=True),
    Command(("b",), "Browse bookmarks", SCOPE_GLOBAL, "open_bookmarks", footer=True),
    Command(("left_square_bracket",), "Shrink sidebar", SCOPE_GLOBAL, "shrink_sidebar"),
    Command(("right_square_bracket",), "Grow sidebar", SCOPE_GLOBAL, "grow_sidebar"),

    # --- Session list ---
    Command(("j",), "Next session", SCOPE_SESSION_LIST, "cursor_down", footer=True),
    Command(("k",), "Previous session", SCOPE_SESSION_LIST, "cursor_up", footer=True),
    Command(("l", "right"), "Focus transcript", SCOPE_SESSION_LIST, "focus_transcript"),
    Command(
        ("enter",), "Reply to session", SCOPE_SESSION_LIST,
        "start_reply_or_send", priority=True, footer=True,
    ),
    Command(("n",), "Rename session", SCOPE_SESSION_LIST, "rename_session", footer=True),
    Command(("g",), "Copy resume command", SCOPE_SESSION_LIST, "copy_session_id"),
    Command(("o",), "Copy observation path / start observing", SCOPE_SESSION_LIST, "observe_session"),
    Command(("O",), "Resume observation", SCOPE_SESSION_LIST, "resume_observation"),
    Command(("s",), "Cycle status forward", SCOPE_SESSION_LIST, "cycle_status_forward"),
    Command(("S",), "Cycle status backward", SCOPE_SESSION_LIST, "cycle_status_backward"),
    Command(("a",), "Archive session", SCOPE_SESSION_LIST, "archive_session"),
    Command(("A",), "Toggle archived view", SCOPE_SESSION_LIST, "toggle_archive_view"),
    Command(
        ("J", "shift+down"), "Move session down",
        SCOPE_SESSION_LIST, "move_session_down",
    ),
    Command(
        ("K", "shift+up"), "Move session up",
        SCOPE_SESSION_LIST, "move_session_up",
    ),

    # --- Transcript ---
    Command(("h", "left"), "Focus session list", SCOPE_TRANSCRIPT, "focus_list"),
    Command(
        ("pageup",), "Scroll page up", SCOPE_TRANSCRIPT,
        "scroll_transcript_up", priority=True,
    ),
    Command(
        ("pagedown",), "Scroll page down", SCOPE_TRANSCRIPT,
        "scroll_transcript_down", priority=True,
    ),
    Command(
        ("home",), "Scroll to top", SCOPE_TRANSCRIPT,
        "scroll_transcript_home", priority=True,
    ),
    Command(
        ("end",), "Scroll to bottom", SCOPE_TRANSCRIPT,
        "scroll_transcript_end", priority=True,
    ),
    Command(("m",), "Enter selection mode", SCOPE_TRANSCRIPT, footer=True),

    # --- Selection mode (widget-local; TranscriptView.on_key) ---
    Command(("j", "down"), "Next message", SCOPE_SELECTION, footer=True),
    Command(("k", "up"), "Previous message", SCOPE_SELECTION, footer=True),
    Command(("v",), "Toggle range select", SCOPE_SELECTION, footer=True),
    Command(("y",), "Copy selection", SCOPE_SELECTION, footer=True),
    Command(("e",), "Export to /tmp", SCOPE_SELECTION, footer=True),
    Command(("space",), "Toggle bookmark", SCOPE_SELECTION, footer=True),
    Command(("L",), "Edit bookmark note", SCOPE_SELECTION),
    Command(("m", "escape"), "Exit selection", SCOPE_SELECTION, footer=True),

    # --- Reply input ---
    Command(
        ("enter",), "Send message", SCOPE_REPLY,
        "start_reply_or_send", priority=True, footer=True,
    ),
    Command(("alt+enter",), "Newline", SCOPE_REPLY, "insert_newline", footer=True),
    Command(("escape",), "Cancel / return to list", SCOPE_REPLY, "cancel_reply", footer=True),
    Command(("alt+j",), "Nav sessions while replying", SCOPE_REPLY, "nav_down_from_reply"),
    Command(("alt+k",), "Nav sessions while replying", SCOPE_REPLY, "nav_up_from_reply"),

    # --- Find bar (widget-local; FindBar.on_key + Input.Submitted) ---
    Command(("enter",), "Next match", SCOPE_FIND, footer=True),
    Command(("down",), "Next match", SCOPE_FIND, footer=True),
    Command(("up",), "Previous match", SCOPE_FIND, footer=True),
    Command(("N",), "Previous match (app-wide)", SCOPE_FIND, "find_prev"),
    Command(("escape",), "Close find", SCOPE_FIND, footer=True),

    # --- Search modal (widget-local; SearchScreen.on_key) ---
    Command(("up", "down", "ctrl+n", "ctrl+p"), "Navigate results", SCOPE_SEARCH, footer=True),
    Command(("pageup", "pagedown"), "Jump through results", SCOPE_SEARCH),
    Command(("enter",), "Open result", SCOPE_SEARCH, footer=True),
    Command(("ctrl+x",), "Clear query/history", SCOPE_SEARCH),
    Command(("escape",), "Close search", SCOPE_SEARCH, footer=True),

    # --- Bookmark browser modal (widget-local; BookmarkBrowserScreen.on_key) ---
    Command(("up", "down", "ctrl+n", "ctrl+p"), "Navigate bookmarks", SCOPE_BOOKMARKS, footer=True),
    Command(("enter",), "Jump to message", SCOPE_BOOKMARKS, footer=True),
    Command(("L",), "Edit note", SCOPE_BOOKMARKS, footer=True),
    Command(("d",), "Delete bookmark", SCOPE_BOOKMARKS, footer=True),
    Command(("escape",), "Close", SCOPE_BOOKMARKS, footer=True),
]


__all__ = [
    "Command",
    "COMMAND_REGISTRY",
    "SCOPE_ORDER",
    "SCOPE_LABELS",
    "SCOPE_GLOBAL",
    "SCOPE_SESSION_LIST",
    "SCOPE_TRANSCRIPT",
    "SCOPE_SELECTION",
    "SCOPE_REPLY",
    "SCOPE_FIND",
    "SCOPE_SEARCH",
    "SCOPE_BOOKMARKS",
    "format_key",
]
