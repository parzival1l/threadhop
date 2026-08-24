"""Regression tests for cross-session search jump targeting (issue #76).

Jumping to an FTS hit used to load the right transcript while leaving
the sidebar highlight, `g` (copy resume command), and Enter (reply) on
the previously selected session. These helpers are the canonical
"current session" resolution those actions now share.
"""

from __future__ import annotations

from threadhop_core.tui.utils import (
    compute_visible_sessions,
    resolve_action_session,
)


def _session(sid: str, status: str = "active", **extra) -> dict:
    row = {
        "session_id": sid,
        "status": status,
        "path": f"/tmp/{sid}.jsonl",
        "project": "demo",
        "modified": 0,
        "title": sid,
    }
    row.update(extra)
    return row


def test_resolve_action_session_prefers_selected_id_over_highlight():
    sessions = [_session("aaa"), _session("bbb")]
    highlighted = sessions[0]
    current = resolve_action_session(
        selected_session_id="bbb",
        sessions=sessions,
        highlighted_session_data=highlighted,
    )
    assert current is not None
    assert current["session_id"] == "bbb"


def test_resolve_action_session_does_not_fall_back_to_a_different_highlight():
    """If the jumped-to session isn't in the list yet, don't silently
    target the old highlight — that's how replies were misdirected."""
    sessions = [_session("aaa")]
    current = resolve_action_session(
        selected_session_id="bbb",
        sessions=sessions,
        highlighted_session_data=sessions[0],
    )
    assert current is None


def test_resolve_action_session_falls_back_to_highlight_when_unselected():
    sessions = [_session("aaa")]
    current = resolve_action_session(
        selected_session_id=None,
        sessions=sessions,
        highlighted_session_data=sessions[0],
    )
    assert current is sessions[0]


def test_visible_sessions_injects_pinned_session_not_in_window():
    """A search jump to a session outside the days/cap window still
    gets a sidebar row so `g` and reply can target it."""
    in_window = [_session(f"s{i}") for i in range(3)]
    pinned = _session("old-session")
    visible = compute_visible_sessions(
        in_window, show_archived=False, pinned_session=pinned
    )
    ids = [s["session_id"] for s in visible]
    assert ids[0] == "old-session"
    assert "s0" in ids


def test_visible_sessions_does_not_duplicate_pinned_session():
    sessions = [_session("aaa"), _session("bbb")]
    visible = compute_visible_sessions(
        sessions, show_archived=False, pinned_session=sessions[1]
    )
    ids = [s["session_id"] for s in visible]
    assert ids.count("bbb") == 1


def test_visible_sessions_pins_archived_session_without_showing_all_archived():
    active = _session("live")
    archived_other = _session("other-archived", status="archived")
    pinned = _session("jumped-archived", status="archived")
    visible = compute_visible_sessions(
        [active, archived_other, pinned],
        show_archived=False,
        pinned_session=pinned,
    )
    ids = [s["session_id"] for s in visible]
    assert "jumped-archived" in ids
    assert "live" in ids
    assert "other-archived" not in ids
