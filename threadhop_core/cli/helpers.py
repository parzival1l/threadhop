"""Shared helpers for ``threadhop`` CLI subcommands.

Keeps the per-subcommand modules thin: every observation/conflict/todo
verb resolves the same set of (project, session) targets, runs an
observer catch-up, and reads back per-session JSONL. That common shape
lives here so each handler reads as a near-empty wrapper.
"""

from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path

from ..observation import observer
from ..session.detection import detect_current_session_id, find_session_path
from ..storage import db


def _cli_stub(command: str) -> int:
    """Print a stub notice and exit 0. Real implementation lands in later tasks."""
    print(f"threadhop {command}: not yet implemented (stub)", file=sys.stderr)
    return 0


def _ensure_cli_session_row(
    conn: sqlite3.Connection,
    session_id: str,
) -> dict | None:
    """Return the session row for ``session_id``, seeding it from disk if needed."""
    row = db.get_session(conn, session_id)
    if row is not None:
        return row
    session_path = find_session_path(session_id)
    if session_path is None:
        return None
    db.upsert_session(
        conn, session_id, str(session_path),
        project=session_path.parent.name,
    )
    return db.get_session(conn, session_id)


def _query_cli_sessions(
    conn: sqlite3.Connection,
    *,
    project: str | None = None,
    session_id: str | None = None,
) -> list[dict]:
    """Resolve the sessions targeted by a CLI query.

    Project filtering intentionally goes through SQLite's ``sessions`` table
    (ADR-019) rather than inferring the project from observation filenames.
    """
    if session_id:
        row = _ensure_cli_session_row(conn, session_id)
        return [row] if row is not None else []

    sql = (
        "SELECT session_id, session_path, project, modified_at "
        "FROM sessions"
    )
    params: list[str] = []
    if project:
        sql += " WHERE project LIKE ?"
        params.append(f"%{project}%")
    sql += " ORDER BY modified_at IS NULL, modified_at DESC, session_id"
    return db.query_all(conn, sql, tuple(params))


def _obs_path_for_session(
    conn: sqlite3.Connection,
    session_id: str,
) -> Path:
    """Return the canonical observation-file path for ``session_id``."""
    state = db.get_observation_state(conn, session_id)
    if state is not None and state.get("obs_path"):
        return Path(state["obs_path"])
    return db.OBS_DIR / f"{session_id}.jsonl"


def _catch_up_cli_observations(
    conn: sqlite3.Connection,
    sessions: list[dict],
) -> list[str]:
    """Run the observer once per targeted session, collecting hard failures."""
    errors: list[str] = []
    for row in sessions:
        sid = row["session_id"]
        result = observer.observe_session(conn, sid)
        if result.get("status") == "failed":
            errors.append(
                f"{sid}: {result.get('message') or result.get('error') or 'observer failed'}"
            )
    return errors


def _sessions_with_observation_files(
    conn: sqlite3.Connection,
    sessions: list[dict],
) -> tuple[list[dict], list[Path]]:
    """Filter ``sessions`` down to rows that already participate in observations."""
    filtered_sessions: list[dict] = []
    obs_paths: list[Path] = []
    for row in sessions:
        obs_path = _obs_path_for_session(conn, row["session_id"])
        state = db.get_observation_state(conn, row["session_id"])
        if not obs_path.exists() and state is None:
            continue
        filtered_sessions.append(row)
        obs_paths.append(obs_path)
    return filtered_sessions, obs_paths


def _load_observation_lines(
    conn: sqlite3.Connection,
    *,
    project: str | None = None,
    session_id: str | None = None,
) -> tuple[list[str], list[str]]:
    """Read observation JSONL lines, newest-first, preserving raw line text."""
    errors: list[str] = []
    if session_id:
        sessions = _query_cli_sessions(
            conn, project=project, session_id=session_id,
        )
        errors.extend(_catch_up_cli_observations(conn, sessions))
        obs_paths = [_obs_path_for_session(conn, row["session_id"]) for row in sessions]
    elif project:
        sessions = _query_cli_sessions(conn, project=project)
        sessions, obs_paths = _sessions_with_observation_files(conn, sessions)
        errors.extend(_catch_up_cli_observations(conn, sessions))
    else:
        obs_paths = sorted(db.OBS_DIR.glob("*.jsonl"))
        session_rows: list[dict] = []
        for obs_path in obs_paths:
            row = db.get_session(conn, obs_path.stem)
            if row is not None:
                session_rows.append(row)
        errors.extend(_catch_up_cli_observations(conn, session_rows))

    ranked: list[tuple[str, int, str]] = []
    seq = 0
    for obs_path in obs_paths:
        if not obs_path.exists():
            continue
        try:
            with open(obs_path) as f:
                for line_no, raw_line in enumerate(f, start=1):
                    raw = raw_line.rstrip("\n")
                    if not raw.strip():
                        continue
                    try:
                        entry = json.loads(raw)
                    except (json.JSONDecodeError, ValueError) as e:
                        errors.append(f"{obs_path}:{line_no}: invalid JSON: {e}")
                        continue
                    ts = entry.get("ts")
                    ranked.append((ts if isinstance(ts, str) else "", seq, raw))
                    seq += 1
        except OSError as e:
            errors.append(f"{obs_path}: {e}")

    ranked.sort(key=lambda item: (item[0], item[1]), reverse=True)
    return [raw for _ts, _seq, raw in ranked], errors


def _resolve_cli_session(args) -> int:
    """Populate args.session via auto-detection when omitted.

    Shared by every CLI subcommand that takes ``--session`` (tag, observe,
    …). Returns 0 on success (args.session is set), non-zero on failure
    (already printed an error). macOS-only — detection relies on
    ``ps``/``lsof``.
    """
    if args.session:
        return 0
    detected = detect_current_session_id()
    if detected:
        args.session = detected
        return 0
    print(
        f"threadhop {args.command}: could not auto-detect the current session id.\n"
        "  Run this from inside a `claude` terminal, or pass --session <id> explicitly.\n"
        "  (Auto-detection walks the current process tree for a claude CLI ancestor; macOS only — relies on `ps`/`lsof`.)",
        file=sys.stderr,
    )
    return 2
