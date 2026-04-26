from __future__ import annotations

import json
import os
import stat
import subprocess
from pathlib import Path

from threadhop_core.storage import db


ROOT = Path(__file__).resolve().parent.parent
THREADHOP = ROOT / "threadhop"


def _user_line(uuid: str, text: str, sid: str) -> str:
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-17T10:00:00Z",
        "message": {"id": f"umsg_{uuid}", "content": [{"type": "text", "text": text}]},
    })


def _assistant_line(uuid: str, mid: str, text: str, sid: str) -> str:
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-17T10:00:01Z",
        "message": {"id": mid, "content": [{"type": "text", "text": text}]},
    })


def _write_session(projects_dir: Path, project: str, session_id: str) -> Path:
    project_dir = projects_dir / project
    project_dir.mkdir(parents=True, exist_ok=True)
    path = project_dir / f"{session_id}.jsonl"
    path.write_text(
        "\n".join([
            _user_line("u1", "Should we keep SQLite?", session_id),
            _assistant_line("a1", "m1", "Yes, keep it.", session_id),
            _user_line("u2", "Noted.", session_id),
        ]) + "\n"
    )
    return path


def _write_fake_claude(bin_dir: Path, payload_by_session: dict[str, list[dict]]) -> Path:
    payload_dir = bin_dir / "payloads"
    payload_dir.mkdir(parents=True, exist_ok=True)
    for session_id, entries in payload_by_session.items():
        (payload_dir / f"{session_id}.jsonl").write_text(
            "\n".join(json.dumps(entry) for entry in entries) + "\n"
        )

    script = bin_dir / "claude"
    script.write_text(
        f"""#!/usr/bin/env bash
set -euo pipefail
prompt="$2"
obs_path=$(printf '%s\\n' "$prompt" | sed -n 's/^Append observations to: //p')
session_id=$(basename "$obs_path" .jsonl)
payload="{payload_dir}/$session_id.jsonl"
if [[ -f "$payload" ]]; then
  cat "$payload" >> "$obs_path"
fi
"""
    )
    script.chmod(script.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return script


def _seed_db(home: Path, sessions: list[tuple[str, Path, str]]) -> None:
    db_path = home / ".config" / "threadhop" / "sessions.db"
    conn = db.init_db(db_path)
    try:
        for session_id, session_path, project in sessions:
            db.upsert_session(
                conn,
                session_id,
                str(session_path),
                project=project,
            )
            db.upsert_observation_state(
                conn,
                session_id,
                str(session_path),
                str(home / ".config" / "threadhop" / "observations" / f"{session_id}.jsonl"),
            )
    finally:
        conn.close()


def _run_threadhop(home: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    fake_bin = home / "bin"
    env["HOME"] = str(home)
    env["PATH"] = f"{fake_bin}:{env['PATH']}"
    return subprocess.run(
        [str(THREADHOP), *args],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


class TestDecisionsCli:
    def test_outputs_compact_jsonl_newest_first_after_running_observer(
        self, tmp_path: Path
    ):
        home = tmp_path / "home"
        projects_dir = home / ".claude" / "projects"
        fake_bin = home / "bin"
        fake_bin.mkdir(parents=True)

        sid_old = "11111111-1111-1111-1111-111111111111"
        sid_new = "22222222-2222-2222-2222-222222222222"
        project_old = "-Users-alice-alpha"
        project_new = "-Users-alice-beta"
        path_old = _write_session(projects_dir, project_old, sid_old)
        path_new = _write_session(projects_dir, project_new, sid_new)
        _seed_db(home, [
            (sid_old, path_old, project_old),
            (sid_new, path_new, project_new),
        ])
        _write_fake_claude(fake_bin, {
            sid_old: [
                {
                    "type": "decision",
                    "text": "Keep SQLite",
                    "context": "Single-file deployment",
                    "ts": "2026-04-17T10:00:00Z",
                },
                {
                    "type": "todo",
                    "text": "Delete this later",
                    "context": "",
                    "ts": "2026-04-17T10:00:02Z",
                },
            ],
            sid_new: [
                {
                    "type": "decision",
                    "text": "Use per-session JSONL",
                    "context": "Observer output stays grep-friendly",
                    "ts": "2026-04-17T11:00:00Z",
                },
            ],
        })

        result = _run_threadhop(home, "decisions")

        assert result.returncode == 0, result.stderr
        assert result.stderr == ""
        lines = [line for line in result.stdout.splitlines() if line.strip()]
        assert len(lines) == 2

        parsed = [json.loads(line) for line in lines]
        assert [entry["session"] for entry in parsed] == [sid_new, sid_old]
        assert [entry["project"] for entry in parsed] == [project_new, project_old]
        assert [entry["timestamp"] for entry in parsed] == [
            "2026-04-17T11:00:00Z",
            "2026-04-17T10:00:00Z",
        ]
        assert parsed[0]["text"] == "Use per-session JSONL"
        assert parsed[1]["text"] == "Keep SQLite"

    def test_project_filter_limits_observer_and_output_to_matching_sessions(
        self, tmp_path: Path
    ):
        home = tmp_path / "home"
        projects_dir = home / ".claude" / "projects"
        fake_bin = home / "bin"
        fake_bin.mkdir(parents=True)

        sid_alpha = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        sid_beta = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
        project_alpha = "-Users-alice-alpha"
        project_beta = "-Users-alice-beta"
        path_alpha = _write_session(projects_dir, project_alpha, sid_alpha)
        path_beta = _write_session(projects_dir, project_beta, sid_beta)
        _seed_db(home, [
            (sid_alpha, path_alpha, project_alpha),
            (sid_beta, path_beta, project_beta),
        ])
        _write_fake_claude(fake_bin, {
            sid_alpha: [
                {
                    "type": "decision",
                    "text": "Alpha only",
                    "context": "",
                    "ts": "2026-04-17T12:00:00Z",
                },
            ],
            sid_beta: [
                {
                    "type": "decision",
                    "text": "Beta only",
                    "context": "",
                    "ts": "2026-04-17T13:00:00Z",
                },
            ],
        })

        result = _run_threadhop(home, "decisions", "--project", "alpha")

        assert result.returncode == 0, result.stderr
        lines = [line for line in result.stdout.splitlines() if line.strip()]
        assert len(lines) == 1

        entry = json.loads(lines[0])
        assert entry["session"] == sid_alpha
        assert entry["project"] == project_alpha
        assert entry["timestamp"] == "2026-04-17T12:00:00Z"
        assert entry["text"] == "Alpha only"

        obs_dir = home / ".config" / "threadhop" / "observations"
        assert (obs_dir / f"{sid_alpha}.jsonl").exists()
        assert not (obs_dir / f"{sid_beta}.jsonl").exists()
