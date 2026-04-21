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
            _user_line("u1", "What is event sourcing?", session_id),
            _assistant_line("a1", "m1", "It records changes as events.", session_id),
            _user_line("u2", "Bookmark that term.", session_id),
        ]) + "\n"
    )
    return path


def _write_fake_claude(bin_dir: Path, markdown: str) -> Path:
    script = bin_dir / "claude"
    script.write_text(
        f"""#!/usr/bin/env bash
set -euo pipefail
cat <<'EOF'
{markdown}
EOF
"""
    )
    script.chmod(script.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return script


def _run_threadhop(home: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["HOME"] = str(home)
    env["PATH"] = f"{home / 'bin'}:{env['PATH']}"
    return subprocess.run(
        [str(THREADHOP), *args],
        cwd=ROOT,
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def test_bookmark_add_and_categories_listing(tmp_path: Path):
    home = tmp_path / "home"
    projects_dir = home / ".claude" / "projects"
    (home / "bin").mkdir(parents=True)

    sid = "11111111-1111-1111-1111-111111111111"
    project = "-Users-alice-alpha"
    _write_session(projects_dir, project, sid)

    add = _run_threadhop(
        home,
        "bookmark",
        "add",
        "--session",
        sid,
        "--message",
        "u1",
        "--category",
        "unknown-term",
        "--note",
        "look this up later",
    )

    assert add.returncode == 0, add.stderr
    assert "unknown-term" in add.stdout

    categories = _run_threadhop(home, "bookmark", "categories")
    assert categories.returncode == 0, categories.stderr
    assert "unknown-term\t1 bookmark\tprompt:missing" in categories.stdout


def test_bookmark_category_set_prompt_updates_listing(tmp_path: Path):
    home = tmp_path / "home"
    projects_dir = home / ".claude" / "projects"
    (home / "bin").mkdir(parents=True)

    sid = "22222222-2222-2222-2222-222222222222"
    project = "-Users-alice-beta"
    _write_session(projects_dir, project, sid)
    _run_threadhop(
        home,
        "bookmark",
        "add",
        "--session",
        sid,
        "--message",
        "u1",
        "--category",
        "background-topic",
    )

    result = _run_threadhop(
        home,
        "bookmark",
        "category",
        "set-prompt",
        "background-topic",
        "--prompt",
        "Research the bookmarked topic and summarize it.",
    )

    assert result.returncode == 0, result.stderr
    categories = _run_threadhop(home, "bookmark", "categories")
    assert "background-topic\t1 bookmark\tprompt:set" in categories.stdout


def test_bookmark_research_fails_cleanly_when_prompt_missing(tmp_path: Path):
    home = tmp_path / "home"
    projects_dir = home / ".claude" / "projects"
    (home / "bin").mkdir(parents=True)

    sid = "33333333-3333-3333-3333-333333333333"
    project = "-Users-alice-gamma"
    _write_session(projects_dir, project, sid)
    _run_threadhop(
        home,
        "bookmark",
        "add",
        "--session",
        sid,
        "--message",
        "u1",
        "--category",
        "unknown-term",
    )

    result = _run_threadhop(
        home,
        "bookmark",
        "research",
        "--category",
        "unknown-term",
    )

    assert result.returncode == 1
    assert "has no research prompt" in result.stderr


def test_bookmark_research_writes_markdown_and_marks_rows(tmp_path: Path):
    home = tmp_path / "home"
    projects_dir = home / ".claude" / "projects"
    fake_bin = home / "bin"
    fake_bin.mkdir(parents=True)
    _write_fake_claude(
        fake_bin,
        "# Research memo\n\n## Event sourcing\n\nIt stores changes as an append-only log.",
    )

    sid = "44444444-4444-4444-4444-444444444444"
    project = "-Users-alice-delta"
    _write_session(projects_dir, project, sid)
    _run_threadhop(
        home,
        "bookmark",
        "add",
        "--session",
        sid,
        "--message",
        "u1",
        "--category",
        "background-topic",
        "--note",
        "follow up on this pattern",
    )
    _run_threadhop(
        home,
        "bookmark",
        "category",
        "set-prompt",
        "background-topic",
        "--prompt",
        "Research the bookmarked topic and summarize it.",
    )

    result = _run_threadhop(
        home,
        "bookmark",
        "research",
        "--category",
        "background-topic",
    )

    assert result.returncode == 0, result.stderr
    assert "Researched 1 bookmark(s)" in result.stdout

    research_dir = home / ".config" / "threadhop" / "research" / "background-topic"
    files = sorted(research_dir.glob("*.md"))
    assert len(files) == 1
    assert "Event sourcing" in files[0].read_text()

    conn = db.init_db(home / ".config" / "threadhop" / "sessions.db")
    try:
        rows = db.list_bookmarks_for_research(conn, "background-topic", force=True)
    finally:
        conn.close()
    assert len(rows) == 1
    assert rows[0]["researched_at"] is not None
