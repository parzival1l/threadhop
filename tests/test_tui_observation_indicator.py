from __future__ import annotations

from threadhop_core.storage import db
from threadhop_core.tui.utils import render_session_label_text


def test_get_session_sidebar_metadata_marks_observed_sessions(conn):
    db.upsert_session(conn, "sess-observed", "/tmp/sess-observed.jsonl")
    db.upsert_session(conn, "sess-plain", "/tmp/sess-plain.jsonl")
    db.upsert_observation_state(
        conn,
        "sess-observed",
        source_path="/tmp/sess-observed.jsonl",
        obs_path="/tmp/sess-observed.obs.jsonl",
        entry_count=2,
    )
    db.upsert_observation_state(
        conn,
        "sess-plain",
        source_path="/tmp/sess-plain.jsonl",
        obs_path="/tmp/sess-plain.obs.jsonl",
        entry_count=0,
    )

    metadata = db.get_session_sidebar_metadata(conn)

    assert metadata["sess-observed"]["status"] == "active"
    assert metadata["sess-observed"]["has_observations"] is True
    assert metadata["sess-plain"]["status"] == "active"
    assert metadata["sess-plain"]["has_observations"] is False


def test_render_session_label_text_adds_observation_marker(monkeypatch):
    monkeypatch.setenv("THREADHOP_ASCII_OBSERVATION_MARKER", "1")

    label = render_session_label_text(
        {
            "project": "project-alpha",
            "title": "active-work",
            "modified": 0,
            "is_active": True,
            "is_working": True,
            "has_observations": True,
        },
        spinner_frame=0,
    )

    assert label.plain.startswith("◐ ")
    assert "active-work ≡" in label.plain


def test_render_session_label_text_omits_marker_without_observations(monkeypatch):
    monkeypatch.setenv("THREADHOP_ASCII_OBSERVATION_MARKER", "1")

    label = render_session_label_text(
        {
            "project": "project-beta",
            "title": "plain-session",
            "modified": 0,
            "is_active": False,
            "is_working": False,
            "has_observations": False,
        }
    )

    assert label.plain.startswith("○ ")
    assert "≡" not in label.plain
