"""Helpers for observation-backed CLI queries.

The query commands share the same shape:

1. Run the on-demand observer for any already-tracked sessions so byte
   offsets are current.
2. Read per-session observation JSONL files from ``db.OBS_DIR``.
3. Filter / normalize rows for grep- and jq-friendly CLI output.

This module stays free of TUI dependencies so it can be unit-tested
directly without importing the executable ``threadhop`` script.
"""

from __future__ import annotations

import json
import sqlite3
from pathlib import Path
from typing import Any, Iterable

from ..storage import db
from . import observer


def _session_rows_for_filter(
    conn: sqlite3.Connection,
    *,
    project: str | None = None,
    session_id: str | None = None,
) -> list[dict[str, Any]]:
    """Return matching session rows for CLI filters.

    ``--project`` keeps the existing CLI semantics: case-insensitive
    substring match against ``sessions.project``.
    """
    clauses: list[str] = []
    params: list[Any] = []

    if session_id:
        clauses.append("session_id = ?")
        params.append(session_id)
    if project:
        clauses.append("LOWER(COALESCE(project, '')) LIKE ?")
        params.append(f"%{project.lower()}%")

    sql = "SELECT session_id, project FROM sessions"
    if clauses:
        sql += " WHERE " + " AND ".join(clauses)
    return db.query_all(conn, sql, tuple(params))


def _tracked_session_ids(
    conn: sqlite3.Connection,
    *,
    project: str | None = None,
    session_id: str | None = None,
) -> list[str]:
    """Return sessions with persisted observer state, optionally filtered."""
    clauses: list[str] = []
    params: list[Any] = []

    if session_id:
        clauses.append("os.session_id = ?")
        params.append(session_id)
    if project:
        clauses.append("LOWER(COALESCE(s.project, '')) LIKE ?")
        params.append(f"%{project.lower()}%")

    sql = (
        "SELECT os.session_id "
        "FROM observation_state os "
        "LEFT JOIN sessions s ON s.session_id = os.session_id"
    )
    if clauses:
        sql += " WHERE " + " AND ".join(clauses)
    rows = db.query_all(conn, sql, tuple(params))
    return [row["session_id"] for row in rows]


def refresh_unprocessed_observations(
    conn: sqlite3.Connection,
    *,
    project: str | None = None,
    session_id: str | None = None,
    batch_threshold: int = observer.BATCH_THRESHOLD,
) -> list[dict[str, Any]]:
    """Run the observer once for matching already-tracked sessions.

    Query commands are catch-up consumers of the observer. They should not
    implicitly start observation for brand-new sessions with no prior
    state, but they *should* keep existing observation files current.
    """
    results: list[dict[str, Any]] = []
    for sid in _tracked_session_ids(conn, project=project, session_id=session_id):
        result = observer.observe_session(
            conn,
            sid,
            batch_threshold=batch_threshold,
        )
        result["session_id"] = sid
        results.append(result)
    return results


def _observation_files(
    conn: sqlite3.Connection,
    *,
    project: str | None = None,
    session_id: str | None = None,
) -> list[Path]:
    """Return observation files to read for a query."""
    if session_id:
        path = db.OBS_DIR / f"{session_id}.jsonl"
        return [path] if path.exists() else []

    if project:
        rows = _session_rows_for_filter(conn, project=project)
        paths: list[Path] = []
        for row in rows:
            path = db.OBS_DIR / f"{row['session_id']}.jsonl"
            if path.exists():
                paths.append(path)
        return paths

    return sorted(db.OBS_DIR.glob("*.jsonl"))


def _project_map(
    conn: sqlite3.Connection,
    session_ids: Iterable[str],
) -> dict[str, str]:
    session_ids = list(dict.fromkeys(session_ids))
    if not session_ids:
        return {}
    placeholders = ",".join("?" for _ in session_ids)
    rows = db.query_all(
        conn,
        (
            "SELECT session_id, COALESCE(project, '') AS project "
            f"FROM sessions WHERE session_id IN ({placeholders})"
        ),
        tuple(session_ids),
    )
    return {row["session_id"]: row["project"] for row in rows}


def list_observation_entries(
    conn: sqlite3.Connection,
    *,
    observation_type: str | None = None,
    project: str | None = None,
    session_id: str | None = None,
) -> list[dict[str, str]]:
    """Read observation JSONL files and return normalized entries.

    Returned rows are sorted newest-first and shaped for CLI output:
    ``project``, ``session``, ``timestamp``, ``text``, plus ``type`` for
    callers that need it.
    """
    files = _observation_files(conn, project=project, session_id=session_id)
    project_by_session = _project_map(
        conn,
        [path.stem for path in files],
    )

    entries: list[dict[str, Any]] = []
    order = 0
    for path in files:
        sid = path.stem
        try:
            with path.open() as fh:
                for raw_line in fh:
                    line = raw_line.strip()
                    if not line:
                        continue
                    try:
                        entry = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if not isinstance(entry, dict):
                        continue
                    if observation_type and entry.get("type") != observation_type:
                        continue
                    entries.append(
                        {
                            "project": project_by_session.get(sid, ""),
                            "session": sid,
                            "timestamp": str(entry.get("ts") or ""),
                            "text": str(entry.get("text") or ""),
                            "type": str(entry.get("type") or ""),
                            "_order": order,
                        }
                    )
                    order += 1
        except OSError:
            continue

    entries.sort(
        key=lambda row: (row["timestamp"], row["_order"]),
        reverse=True,
    )
    for entry in entries:
        entry.pop("_order", None)
    return entries


def format_entry_json(
    entry: dict[str, str],
    *,
    include_type: bool = False,
) -> str:
    """Serialize an observation row as one compact JSON object."""
    payload = {
        "project": entry["project"],
        "session": entry["session"],
        "timestamp": entry["timestamp"],
        "text": entry["text"],
    }
    if include_type:
        payload["type"] = entry["type"]
    return json.dumps(payload, separators=(",", ":"))
