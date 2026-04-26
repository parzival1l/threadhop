"""Shared observer-state helper used by both the TUI and the CLI.

Lives in the observation module so the TUI no longer reaches across the
``cli/`` boundary for what is fundamentally a domain helper. The function
reconciles the ``observation_state`` row with reality (a stale ``running``
status whose PID no longer exists is corrected to ``stopped``) before
either surface acts on it.
"""

from __future__ import annotations

from . import observer
from ..storage import db


def _refresh_observer_state(conn, session_id: str) -> dict | None:
    """Return the observation_state row, correcting stale running PIDs."""
    state = db.get_observation_state(conn, session_id)
    if state is None:
        return None
    pid = state.get("observer_pid")
    if state.get("status") == "running" and pid is not None:
        if not observer._pid_is_alive(int(pid)):
            db.set_observer_stopped(conn, session_id)
            state = db.get_observation_state(conn, session_id)
    return state
