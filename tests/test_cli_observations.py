"""CLI tests for ``threadhop observations``."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

from threadhop_core.storage import db


ROOT = Path(__file__).resolve().parent.parent
THREADHOP = ROOT / "threadhop"


def _home_dirs(home: Path) -> tuple[Path, Path]:
    cfg = home / ".config" / "threadhop"
    obs = cfg / "observations"
    obs.mkdir(parents=True, exist_ok=True)
    return cfg / "sessions.db", obs


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


def test_observations_dumps_raw_jsonl_newest_first(tmp_path: Path):
    home = tmp_path / "home"
    db_path, obs_dir = _home_dirs(home)
    db.init_db(db_path).close()

    older = '{"type":"todo","text":"wire CLI","context":"","ts":"2026-04-17T09:00:00Z"}'
    newest = '{"type":"decision","text":"read per-session files","context":"ADR-019","ts":"2026-04-17T11:00:00Z"}'
    middle = '{"type":"observation","text":"observer is core","context":"ADR-010","ts":"2026-04-17T10:00:00Z"}'

    (obs_dir / "alpha.jsonl").write_text(f"{older}\n{newest}\n")
    (obs_dir / "beta.jsonl").write_text(f"{middle}\n")

    result = _run_threadhop(home, "observations")

    assert result.returncode == 0
    assert result.stderr == ""
    assert result.stdout.splitlines() == [newest, middle, older]


def test_observations_project_filter_uses_sqlite_session_mapping(tmp_path: Path):
    home = tmp_path / "home"
    db_path, obs_dir = _home_dirs(home)
    conn = db.init_db(db_path)
    try:
        db.upsert_session(
            conn, "sess-alpha", "/missing/alpha.jsonl",
            project="atlas",
        )
        db.upsert_session(
            conn, "sess-beta", "/missing/beta.jsonl",
            project="zephyr",
        )
    finally:
        conn.close()

    atlas_line = '{"type":"decision","text":"use sqlite","context":"atlas","ts":"2026-04-17T10:00:00Z"}'
    zephyr_line = '{"type":"decision","text":"use postgres","context":"zephyr","ts":"2026-04-17T11:00:00Z"}'
    (obs_dir / "sess-alpha.jsonl").write_text(f"{atlas_line}\n")
    (obs_dir / "sess-beta.jsonl").write_text(f"{zephyr_line}\n")

    result = _run_threadhop(home, "observations", "--project", "atl")

    assert result.returncode == 0
    assert result.stderr == ""
    assert result.stdout.splitlines() == [atlas_line]
