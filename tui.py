"""Backward-compat shim. Prefer importing from ``threadhop_core.tui`` directly.

This module re-exports the App and the small helper surface that tests
and the entrypoint script have historically reached for via ``import
tui``. New code should import from ``threadhop_core.tui.app`` and
sibling submodules instead.
"""

from threadhop_core.tui.app import ClaudeSessions, run_tui
from threadhop_core.tui.constants import (
    OBSERVATION_MARKER,
    OBSERVATION_MARKER_FALLBACK,
)
from threadhop_core.tui.screens.search import (
    _build_fts_query,
    search_messages,
)
from threadhop_core.tui.utils import (
    build_observe_command,
    render_session_label_text,
    _supports_observation_emoji,
)
from threadhop_core.tui.widgets.transcript import TranscriptView

__all__ = [
    "ClaudeSessions",
    "OBSERVATION_MARKER",
    "OBSERVATION_MARKER_FALLBACK",
    "TranscriptView",
    "build_observe_command",
    "render_session_label_text",
    "run_tui",
    "search_messages",
    "_build_fts_query",
    "_supports_observation_emoji",
]
