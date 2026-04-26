"""CLI helper tests for ``threadhop observe`` lifecycle management."""

from __future__ import annotations

import runpy
from pathlib import Path

import pytest

from threadhop_core.storage import db


ROOT = Path(__file__).resolve().parent.parent
THREADHOP = ROOT / "threadhop"


@pytest.fixture
def threadhop_ns() -> dict:
    """Load the CLI script as a module namespace without executing main()."""
    return runpy.run_path(str(THREADHOP))


def _seed_observation_state(conn, tmp_path: Path, session_id: str, **kwargs) -> None:
    source_path = tmp_path / f"{session_id}.jsonl"
    source_path.write_text("")
    obs_dir = tmp_path / "observations"
    obs_dir.mkdir(exist_ok=True)
    obs_path = obs_dir / f"{session_id}.jsonl"
    db.upsert_session(conn, session_id, str(source_path), project="test-project")
    db.upsert_observation_state(
        conn,
        session_id,
        str(source_path),
        str(obs_path),
        **kwargs,
    )


def test_refresh_observer_state_marks_stale_pid_stopped(
    conn, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, threadhop_ns: dict
):
    _seed_observation_state(
        conn,
        tmp_path,
        "sess-1",
        observer_pid=4242,
        status="running",
    )
    monkeypatch.setattr(threadhop_ns["observer"], "_pid_is_alive", lambda pid: False)

    state = threadhop_ns["_refresh_observer_state"](conn, "sess-1")

    assert state is not None
    assert state["status"] == "stopped"
    assert state["observer_pid"] is None


def test_stop_observer_session_sends_sigterm_to_live_pid(
    conn,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    _seed_observation_state(
        conn,
        tmp_path,
        "sess-1",
        observer_pid=4242,
        status="running",
    )
    monkeypatch.setattr(threadhop_ns["observer"], "_pid_is_alive", lambda pid: True)
    sent: list[tuple[int, int]] = []

    def fake_kill(pid: int, signum: int) -> None:
        sent.append((pid, signum))

    monkeypatch.setattr(threadhop_ns["os"], "kill", fake_kill)

    rc = threadhop_ns["_stop_observer_session"](conn, "sess-1")

    assert rc == 0
    assert sent == [(4242, threadhop_ns["signal"].SIGTERM)]
    assert "Sent SIGTERM to observer for session sess-1 (pid 4242)." in capsys.readouterr().out


def test_stop_observer_session_reports_stale_pid_correction(
    conn,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    _seed_observation_state(
        conn,
        tmp_path,
        "sess-1",
        observer_pid=4242,
        status="running",
    )
    monkeypatch.setattr(threadhop_ns["observer"], "_pid_is_alive", lambda pid: False)

    rc = threadhop_ns["_stop_observer_session"](conn, "sess-1")

    state = db.get_observation_state(conn, "sess-1")
    assert rc == 0
    assert state is not None
    assert state["status"] == "stopped"
    assert state["observer_pid"] is None
    assert (
        "Observer PID 4242 for session sess-1 was stale; state corrected to stopped."
        in capsys.readouterr().out
    )


def test_stop_all_observers_reports_live_and_stale_counts(
    conn,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    _seed_observation_state(
        conn,
        tmp_path,
        "sess-live",
        observer_pid=1111,
        status="running",
    )
    _seed_observation_state(
        conn,
        tmp_path,
        "sess-stale",
        observer_pid=2222,
        status="running",
    )
    sent: list[tuple[int, int]] = []

    def fake_kill(pid: int, signum: int) -> None:
        if pid == 2222:
            raise ProcessLookupError
        sent.append((pid, signum))

    monkeypatch.setattr(threadhop_ns["os"], "kill", fake_kill)

    rc = threadhop_ns["_stop_all_observers"](conn)

    stale_state = db.get_observation_state(conn, "sess-stale")
    assert rc == 0
    assert sent == [(1111, threadhop_ns["signal"].SIGTERM)]
    assert stale_state is not None
    assert stale_state["status"] == "stopped"
    assert stale_state["observer_pid"] is None
    out = capsys.readouterr().out
    assert "Sent SIGTERM to 1 observer(s)." in out
    assert "Corrected 1 stale observer PID(s)." in out


def test_refresh_observer_state_returns_none_when_row_missing(
    conn, threadhop_ns: dict
):
    assert threadhop_ns["_refresh_observer_state"](conn, "ghost-session") is None


def test_refresh_observer_state_leaves_live_running_state_untouched(
    conn, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, threadhop_ns: dict
):
    _seed_observation_state(
        conn,
        tmp_path,
        "sess-1",
        observer_pid=4242,
        status="running",
    )
    monkeypatch.setattr(threadhop_ns["observer"], "_pid_is_alive", lambda pid: True)

    state = threadhop_ns["_refresh_observer_state"](conn, "sess-1")

    assert state is not None
    assert state["status"] == "running"
    assert state["observer_pid"] == 4242


def test_refresh_observer_state_ignores_already_stopped_row(
    conn, tmp_path: Path, monkeypatch: pytest.MonkeyPatch, threadhop_ns: dict
):
    _seed_observation_state(
        conn,
        tmp_path,
        "sess-1",
        observer_pid=None,
        status="stopped",
    )
    # Should not even need to consult _pid_is_alive — crash it to prove that.
    def boom(_pid):
        raise AssertionError("_pid_is_alive should not be called for non-running rows")

    monkeypatch.setattr(threadhop_ns["observer"], "_pid_is_alive", boom)

    state = threadhop_ns["_refresh_observer_state"](conn, "sess-1")

    assert state is not None
    assert state["status"] == "stopped"
    assert state["observer_pid"] is None


def test_stop_observer_session_noop_when_no_state_row(
    conn,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    rc = threadhop_ns["_stop_observer_session"](conn, "ghost-session")

    assert rc == 0
    assert "Observer is not running for session ghost-session." in capsys.readouterr().out


def test_stop_observer_session_noop_when_state_has_no_pid(
    conn,
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    _seed_observation_state(
        conn,
        tmp_path,
        "sess-1",
        observer_pid=None,
        status="idle",
    )

    rc = threadhop_ns["_stop_observer_session"](conn, "sess-1")

    assert rc == 0
    assert "Observer is not running for session sess-1." in capsys.readouterr().out


def test_stop_observer_session_handles_kill_race_when_process_disappears(
    conn,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    """Pre-check sees a live PID, but the process exits before ``os.kill`` —
    the legacy ``ProcessLookupError`` fallback must still correct the row."""
    _seed_observation_state(
        conn,
        tmp_path,
        "sess-1",
        observer_pid=4242,
        status="running",
    )
    monkeypatch.setattr(threadhop_ns["observer"], "_pid_is_alive", lambda pid: True)

    def racing_kill(pid: int, signum: int) -> None:
        raise ProcessLookupError

    monkeypatch.setattr(threadhop_ns["os"], "kill", racing_kill)

    rc = threadhop_ns["_stop_observer_session"](conn, "sess-1")

    state = db.get_observation_state(conn, "sess-1")
    assert rc == 0
    assert state is not None
    assert state["status"] == "stopped"
    assert state["observer_pid"] is None
    assert (
        "Observer PID 4242 for session sess-1 was stale; state corrected to stopped."
        in capsys.readouterr().out
    )


def test_stop_all_observers_reports_when_none_running(
    conn,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    rc = threadhop_ns["_stop_all_observers"](conn)

    assert rc == 0
    assert "No running observers." in capsys.readouterr().out
