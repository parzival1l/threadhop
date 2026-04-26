from __future__ import annotations

import importlib.util
import sys
from importlib.machinery import SourceFileLoader
from pathlib import Path

from threadhop_core.storage import db


def _load_threadhop_module():
    # Load the `threadhop` script so its line-39 `sys.modules.setdefault`
    # registers it under the plain name "threadhop". That makes `import tui`
    # (whose first line is `import threadhop as _core`) resolve — tui.py
    # then re-exports script symbols *and* adds TUI-only functions like
    # `render_session_label_text`. Returning `tui` gives the tests one
    # reference that covers both surfaces.
    path = Path(__file__).resolve().parent.parent / "threadhop"
    loader = SourceFileLoader("threadhop_app", str(path))
    spec = importlib.util.spec_from_loader("threadhop_app", loader)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules.setdefault("threadhop_app", module)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    import tui  # noqa: PLC0415 — deferred until the script is registered.
    return tui


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
    threadhop = _load_threadhop_module()
    monkeypatch.setenv("THREADHOP_ASCII_OBSERVATION_MARKER", "1")

    label = threadhop.render_session_label_text(
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
    threadhop = _load_threadhop_module()
    monkeypatch.setenv("THREADHOP_ASCII_OBSERVATION_MARKER", "1")

    label = threadhop.render_session_label_text(
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
