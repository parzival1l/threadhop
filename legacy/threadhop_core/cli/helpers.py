"""Shared helpers for ``threadhop`` CLI subcommands.

Keeps the per-subcommand modules thin: every verb resolves the same set
of (project, session) targets the same way. That common shape lives
here so each handler reads as a near-empty wrapper.
"""

from __future__ import annotations

import sqlite3
import sys

from ..session import detection
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

    Project filtering intentionally goes through SQLite's ``sessions``
    table (ADR-019) rather than inferring the project from filenames.
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


def _resolve_session_prefix(
    conn: sqlite3.Connection,
    token: str,
) -> list[str]:
    """Resolve a session id or unique prefix against known sessions.

    Candidates come from both the ``sessions`` table and the on-disk
    transcripts under ``~/.claude/projects`` (skipping ``agent-*``
    sub-agent files), so ``peek`` works on sessions the TUI has never
    scanned. An exact id match wins outright; otherwise every id
    starting with ``token`` is returned sorted — the caller decides
    what 0 or >1 candidates mean (not-found vs ambiguous).
    """
    known: set[str] = set()
    projects_root = detection.CLAUDE_PROJECTS
    if projects_root.is_dir():
        for jsonl in projects_root.glob("*/*.jsonl"):
            if jsonl.name.startswith("agent-"):
                continue
            known.add(jsonl.stem)
    for row in db.query_all(conn, "SELECT session_id FROM sessions"):
        known.add(row["session_id"])

    if token in known:
        return [token]
    return sorted(sid for sid in known if sid.startswith(token))


def _resolve_cli_session(args) -> int:
    """Populate args.session via auto-detection when omitted.

    Shared by every CLI subcommand that takes ``--session`` (tag, copy,
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
