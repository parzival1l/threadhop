"""Focused tests for TUI observation helpers and action routing."""

from __future__ import annotations

import importlib.util
import sys
import time
from importlib.machinery import SourceFileLoader
from pathlib import Path

import db


ROOT = Path(__file__).resolve().parent.parent
THREADHOP = ROOT / "threadhop"


def _load_threadhop_ns() -> dict:
    # Symbols this suite touches (`build_observe_command`,
    # `render_session_label_text`, `ClaudeSessions`, `OBSERVATION_MARKER`,
    # `_supports_observation_emoji`, `OBSERVATION_MARKER_FALLBACK`) all
    # live in tui.py or are re-exported into it. Loading the script
    # primes `sys.modules["threadhop"]` via its own setdefault, then
    # `import tui` resolves and returns a namespace that covers every
    # symbol the tests reach for.
    module_name = "threadhop_app"
    if module_name not in sys.modules:
        loader = SourceFileLoader(module_name, str(THREADHOP))
        spec = importlib.util.spec_from_loader(module_name, loader)
        module = importlib.util.module_from_spec(spec)
        sys.modules[module_name] = module
        loader.exec_module(module)
    import tui  # noqa: PLC0415 — deferred until the script is registered.
    return tui.__dict__


class _FakeItem:
    def __init__(self, session_data: dict):
        self.session_data = session_data


class _FakeApp:
    def __init__(self, item: _FakeItem, state: dict | None = None):
        self._item = item
        self._state = state
        self.notifications: list[tuple[str, dict]] = []
        self.confirmations: list[tuple[str, str, str]] = []

    def _input_has_focus(self) -> bool:
        return False

    def _highlighted_session_item(self):
        return self._item

    def _refresh_session_observation(self, session_id: str) -> dict | None:
        assert session_id == self._item.session_data["session_id"]
        return self._state

    def _confirm_observer_start(
        self, session_id: str, prompt: str, success_message: str
    ) -> None:
        self.confirmations.append((session_id, prompt, success_message))

    def notify(self, message, **kwargs) -> None:
        self.notifications.append((str(message), kwargs))


def test_get_observed_sessions_includes_obs_path(conn, tmp_path: Path):
    source_path = tmp_path / "sess-1.jsonl"
    source_path.write_text("")
    obs_path = tmp_path / "observations" / "sess-1.jsonl"
    obs_path.parent.mkdir()
    db.upsert_session(conn, "sess-1", str(source_path), project="test-project")

    db.upsert_observation_state(
        conn,
        "sess-1",
        str(source_path),
        str(obs_path),
        entry_count=3,
        status="stopped",
    )

    rows = db.get_observed_sessions(conn)

    assert len(rows) == 1
    assert rows[0]["session_id"] == "sess-1"
    assert rows[0]["obs_path"] == str(obs_path)


def test_build_observe_command_targets_current_script():
    ns = _load_threadhop_ns()

    argv = ns["build_observe_command"]("sess-1")

    assert argv == [str(THREADHOP.resolve()), "observe", "--session", "sess-1"]


def test_render_session_label_text_adds_observation_indicator():
    ns = _load_threadhop_ns()
    session_data = {
        "modified": time.time(),
        "project": "proj",
        "title": "demo",
        "has_observations": True,
    }

    observed = ns["render_session_label_text"](session_data)
    plain = ns["render_session_label_text"](
        {**session_data, "has_observations": False}
    )

    indicator = (
        ns["OBSERVATION_MARKER"]
        if ns["_supports_observation_emoji"]()
        else ns["OBSERVATION_MARKER_FALLBACK"]
    )
    assert indicator in observed.plain
    assert indicator not in plain.plain


def test_action_observe_session_copies_observation_path(monkeypatch):
    ns = _load_threadhop_ns()
    copied: list[str] = []
    monkeypatch.setitem(
        ns["ClaudeSessions"].action_observe_session.__globals__,
        "copy_to_clipboard",
        lambda text: copied.append(text) or True,
    )
    app = _FakeApp(
        _FakeItem(
            {
                "session_id": "sess-1",
                "has_observations": True,
            }
        ),
        state={"entry_count": 2, "obs_path": "/tmp/sess-1.jsonl"},
    )

    ns["ClaudeSessions"].action_observe_session(app)

    assert copied == ["/tmp/sess-1.jsonl"]
    assert app.notifications == [("Observation path copied", {})]


def test_action_observe_session_prompts_to_start_when_unobserved():
    ns = _load_threadhop_ns()
    app = _FakeApp(_FakeItem({"session_id": "sess-1", "has_observations": False}))

    ns["ClaudeSessions"].action_observe_session(app)

    assert app.confirmations == [
        (
            "sess-1",
            "No observations yet. Start observing? (y/n)",
            "Observer starting in background",
        )
    ]


def test_action_resume_observation_prompts_when_stopped_and_observed():
    ns = _load_threadhop_ns()
    app = _FakeApp(
        _FakeItem(
            {
                "session_id": "sess-1",
                "has_observations": True,
            }
        ),
        state={"entry_count": 4, "observer_pid": None, "status": "stopped"},
    )

    ns["ClaudeSessions"].action_resume_observation(app)

    assert app.confirmations == [
        (
            "sess-1",
            "Resume observation from last offset? (y/n)",
            "Observation resumed in background",
        )
    ]
