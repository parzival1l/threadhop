"""CLI tests for ``threadhop bookmark``."""

from __future__ import annotations

import json
import os
import stat
import subprocess
from pathlib import Path

import db


ROOT = Path(__file__).resolve().parent.parent
THREADHOP = ROOT / "threadhop"


def _user_line(uuid: str, text: str, sid: str) -> str:
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-20T10:00:00Z",
        "message": {"id": f"umsg_{uuid}", "content": [{"type": "text", "text": text}]},
    })


def _assistant_line(uuid: str, mid: str, text: str, sid: str) -> str:
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-20T10:00:01Z",
        "message": {"id": mid, "content": [{"type": "text", "text": text}]},
    })


def _write_session(home: Path, project: str, session_id: str) -> Path:
    project_dir = home / ".claude" / "projects" / project
    project_dir.mkdir(parents=True, exist_ok=True)
    path = project_dir / f"{session_id}.jsonl"
    path.write_text(
        "\n".join([
            _user_line("u1", "bookmark the next answer", session_id),
            _assistant_line("a1", "m1", "This is the message to keep.", session_id),
            _user_line("u2", "and this is the latest turn", session_id),
        ]) + "\n"
    )
    return path


def _run_threadhop(home: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["HOME"] = str(home)
    return subprocess.run(
        [str(THREADHOP), *args],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


def _write_fake_ps(bin_dir: Path, session_id: str) -> None:
    script = bin_dir / "ps"
    script.write_text(
        f"""#!/usr/bin/env bash
set -euo pipefail
printf '  PID  PPID ARGS\\n'
printf '%s %s %s\\n' "$PPID" "9000" "python {THREADHOP}"
printf '%s %s %s\\n' "9000" "1" "claude --resume {session_id}"
"""
    )
    script.chmod(script.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)


def test_bookmark_cli_defaults_to_latest_message(tmp_path: Path):
    home = tmp_path / "home"
    sid = "11111111-1111-1111-1111-111111111111"
    _write_session(home, "-Users-alice-alpha", sid)

    result = _run_threadhop(home, "bookmark", "--session", sid)

    assert result.returncode == 0, result.stderr
    assert result.stderr == ""
    assert f"kind=bookmark session={sid} message=u2" in result.stdout
    assert 'text="and this is the latest turn"' in result.stdout

    conn = db.init_db(home / ".config" / "threadhop" / "sessions.db")
    try:
        row = db.get_bookmark(conn, "u2")
        assert row is not None
        assert row["kind"] == "bookmark"
        assert row["note"] is None
    finally:
        conn.close()


def test_bookmark_cli_supports_research_kind_and_explicit_message_note(
    tmp_path: Path,
):
    home = tmp_path / "home"
    sid = "22222222-2222-2222-2222-222222222222"
    _write_session(home, "-Users-alice-beta", sid)

    result = _run_threadhop(
        home,
        "bookmark",
        "research",
        "--session",
        sid,
        "--message",
        "a1",
        "--note",
        "compare later",
    )

    assert result.returncode == 0, result.stderr
    assert f"kind=research session={sid} message=a1" in result.stdout
    assert 'note="compare later"' in result.stdout

    conn = db.init_db(home / ".config" / "threadhop" / "sessions.db")
    try:
        row = db.get_bookmark(conn, "a1")
        assert row is not None
        assert row["kind"] == "research"
        assert row["note"] == "compare later"
    finally:
        conn.close()


def test_bookmark_cli_auto_detects_session_when_omitted(tmp_path: Path):
    home = tmp_path / "home"
    sid = "33333333-3333-3333-3333-333333333333"
    _write_session(home, "-Users-alice-gamma", sid)
    fake_bin = home / "bin"
    fake_bin.mkdir(parents=True)
    _write_fake_ps(fake_bin, sid)

    env = os.environ.copy()
    env["HOME"] = str(home)
    env["PATH"] = f"{fake_bin}:{env['PATH']}"
    result = subprocess.run(
        [str(THREADHOP), "bookmark", "research", "--note", "auto detected"],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, result.stderr
    assert f"kind=research session={sid} message=u2" in result.stdout

    conn = db.init_db(home / ".config" / "threadhop" / "sessions.db")
    try:
        row = db.get_bookmark(conn, "u2")
        assert row is not None
        assert row["kind"] == "research"
        assert row["note"] == "auto detected"
    finally:
        conn.close()
