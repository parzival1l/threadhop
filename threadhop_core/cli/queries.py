"""CLI query helpers for observation-backed subcommands."""

from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path
from typing import Any, Callable

from ..storage import db
from ..observation import observer


def sync_sessions_from_disk(
    conn: sqlite3.Connection,
    claude_projects_dir: Path,
    *,
    project: str | None = None,
) -> None:
    """Refresh the session-to-project mapping from disk into SQLite."""
    if not claude_projects_dir.exists():
        return
    for project_dir in claude_projects_dir.iterdir():
        if not project_dir.is_dir():
            continue
        project_name = project_dir.name
        if project and project.lower() not in project_name.lower():
            continue
        for jsonl in project_dir.glob("*.jsonl"):
            if jsonl.name.startswith("agent-"):
                continue
            try:
                stat = jsonl.stat()
            except OSError:
                continue
            db.upsert_session(
                conn,
                jsonl.stem,
                str(jsonl),
                project=project_name,
                created_at=stat.st_ctime,
                modified_at=stat.st_mtime,
            )


def _read_jsonl_entries(obs_path: Path) -> list[dict[str, Any]]:
    entries: list[dict[str, Any]] = []
    if not obs_path.exists():
        return entries
    with open(obs_path) as f:
        for raw in f:
            raw = raw.strip()
            if not raw:
                continue
            try:
                entry = json.loads(raw)
            except (json.JSONDecodeError, ValueError):
                continue
            if isinstance(entry, dict):
                entries.append(entry)
    return entries


def _select_sessions_for_refresh(
    conn: sqlite3.Connection,
    *,
    project: str | None,
    session_id: str | None,
) -> list[str]:
    if session_id:
        state = db.get_observation_state(conn, session_id)
        return [] if state is None else [session_id]

    params: list[Any] = []
    sql = (
        "SELECT DISTINCT s.session_id "
        "FROM sessions s "
        "JOIN observation_state o ON o.session_id = s.session_id "
    )
    if project:
        sql += "WHERE LOWER(COALESCE(s.project, '')) LIKE ? "
        params.append(f"%{project.lower()}%")
    sql += "ORDER BY s.session_id"
    rows = db.query_all(conn, sql, tuple(params))
    return [row["session_id"] for row in rows]


def _select_decision_for_conflict(
    conflict_ts: str | None,
    decisions: list[dict[str, Any]],
) -> dict[str, Any] | None:
    if not decisions:
        return None
    if not conflict_ts:
        return decisions[-1]
    earlier = [
        decision for decision in decisions
        if isinstance(decision.get("ts"), str) and decision["ts"] <= conflict_ts
    ]
    return earlier[-1] if earlier else decisions[-1]


def _warn(message: str) -> None:
    print(message, file=sys.stderr)


def query_conflicts(
    conn: sqlite3.Connection,
    *,
    claude_projects_dir: Path,
    project: str | None = None,
    session_id: str | None = None,
    mark_resolved: bool = False,
    reflect_fn: Callable[..., dict[str, Any]] = observer.maybe_reflect_session,
) -> list[dict[str, Any]]:
    """Return unresolved conflicts as JSON-serializable dicts.

    Before scanning the observation ledger, this refreshes the sessions
    table from disk, then runs the shared reflector trigger for matching
    already-observed sessions so the results stay current without starting
    new observer work from this query path.
    """
    sync_sessions_from_disk(conn, claude_projects_dir, project=project)

    for target_session_id in _select_sessions_for_refresh(
        conn, project=project, session_id=session_id
    ):
        reflected = reflect_fn(conn, target_session_id)
        if reflected.get("status") == "failed":
            _warn(
                f"threadhop conflicts: reflector failed for {target_session_id}: "
                f"{reflected.get('message', 'unknown error')}"
            )

    session_rows = {
        row["session_id"]: row
        for row in db.query_all(conn, "SELECT session_id, project FROM sessions")
    }

    decisions_by_session: dict[str, list[dict[str, Any]]] = {}
    raw_conflicts: list[dict[str, Any]] = []

    for obs_path in sorted(db.OBS_DIR.glob("*.jsonl")):
        origin_session_id = obs_path.stem
        session_row = session_rows.get(origin_session_id)
        if session_row is None:
            continue
        session_project = session_row.get("project")
        if project and project.lower() not in (session_project or "").lower():
            continue

        entries = _read_jsonl_entries(obs_path)
        for entry in entries:
            entry_type = entry.get("type")
            if entry_type == "decision":
                decisions_by_session.setdefault(origin_session_id, []).append(
                    {
                        "session_id": origin_session_id,
                        "ts": entry.get("ts"),
                        "text": entry.get("text"),
                    }
                )
            elif entry_type == "conflict":
                refs = entry.get("refs")
                if not isinstance(refs, list):
                    refs = []
                if session_id and (
                    origin_session_id != session_id and session_id not in refs
                ):
                    continue
                reviewed = db.is_conflict_reviewed(
                    conn, origin_session_id, refs, entry.get("topic")
                )
                if reviewed and not mark_resolved:
                    continue
                raw_conflicts.append(
                    {
                        "session_id": origin_session_id,
                        "project": session_project,
                        "ts": entry.get("ts"),
                        "topic": entry.get("topic"),
                        "text": entry.get("text"),
                        "refs": refs,
                        "resolved": reviewed,
                    }
                )

    for decisions in decisions_by_session.values():
        decisions.sort(key=lambda entry: entry.get("ts") or "")

    raw_conflicts.sort(key=lambda entry: entry.get("ts") or "", reverse=True)

    results: list[dict[str, Any]] = []
    for conflict in raw_conflicts:
        decisions: list[dict[str, Any]] = []
        for ref_session_id in conflict["refs"]:
            decision = _select_decision_for_conflict(
                conflict.get("ts"),
                decisions_by_session.get(ref_session_id, []),
            )
            if decision is not None:
                decisions.append(decision)

        if mark_resolved:
            db.mark_conflict_reviewed(
                conn,
                conflict["session_id"],
                conflict["refs"],
                conflict.get("topic"),
            )
            conflict["resolved"] = True

        results.append(
            {
                "type": "conflict",
                "session_id": conflict["session_id"],
                "project": conflict.get("project"),
                "ts": conflict.get("ts"),
                "topic": conflict.get("topic"),
                "text": conflict.get("text"),
                "refs": conflict.get("refs"),
                "resolved": conflict.get("resolved", False),
                "decisions": decisions,
            }
        )

    return results
