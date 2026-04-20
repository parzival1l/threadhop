"""Reflector core function — detects cross-session decision conflicts.

The reflector reads structured observation JSONL (not raw transcripts),
compares new decisions from one session against other sessions in the same
project, and appends ``type: "conflict"`` entries to that same session's
observation file per ADR-020.
"""

from __future__ import annotations

import json
import shutil
import sqlite3
import subprocess
from pathlib import Path
from typing import Any

import db


PROMPT_PATH = Path(__file__).resolve().parent / "prompts" / "reflector.md"
DEFAULT_TIMEOUT_SEC = 180.0


def _read_jsonl_entries(obs_path: Path) -> list[tuple[int, dict[str, Any]]]:
    """Return ``(line_index, entry)`` tuples for valid non-empty JSON lines."""
    parsed: list[tuple[int, dict[str, Any]]] = []
    line_index = 0
    if not obs_path.exists():
        return parsed
    with open(obs_path) as f:
        for raw in f:
            if not raw.strip():
                continue
            try:
                entry = json.loads(raw)
            except (json.JSONDecodeError, ValueError):
                line_index += 1
                continue
            if isinstance(entry, dict):
                parsed.append((line_index, entry))
            line_index += 1
    return parsed


def _jsonl_block(entries: list[dict[str, Any]]) -> str:
    """Render entries as compact JSONL for prompt embedding."""
    if not entries:
        return "(none)"
    return "\n".join(
        json.dumps(entry, separators=(",", ":"), sort_keys=True)
        for entry in entries
    )


def _build_prompt(
    template: str,
    current_session_decisions: list[dict[str, Any]],
    project_decisions: list[dict[str, Any]],
    existing_conflicts: list[dict[str, Any]],
    obs_path: Path,
) -> str:
    return (
        f"{template.rstrip()}\n\n"
        "---\n\n"
        "## Current session decisions\n\n"
        "<current_session_decisions>\n"
        f"{_jsonl_block(current_session_decisions)}\n"
        "</current_session_decisions>\n\n"
        "## Project decisions\n\n"
        "<project_decisions>\n"
        f"{_jsonl_block(project_decisions)}\n"
        "</project_decisions>\n\n"
        "## Existing conflicts\n\n"
        "<existing_conflicts>\n"
        f"{_jsonl_block(existing_conflicts)}\n"
        "</existing_conflicts>\n\n"
        "## Output file\n\n"
        f"Append conflicts to: {obs_path}\n"
    )


def reflect_session(
    conn: sqlite3.Connection,
    session_id: str,
    *,
    claude_bin: str = "claude",
    timeout: float = DEFAULT_TIMEOUT_SEC,
    prompt_path: Path | None = None,
) -> dict[str, Any]:
    """Run one reflector comparison pass for ``session_id``.

    Returns a summary dict with ``status`` similar to the observer:
    ``up_to_date`` / ``no_observations`` / ``no_decisions`` /
    ``no_project`` / ``no_peer_decisions`` / ``extracted`` / ``failed``.
    """
    prompt_path = prompt_path or PROMPT_PATH
    state = db.get_observation_state(conn, session_id)
    if state is None:
        return {
            "status": "no_observations",
            "new_entries": 0,
            "entry_count": 0,
            "reflector_entry_offset": 0,
            "message": f"No observations found for session {session_id}.",
        }

    obs_path = Path(state["obs_path"])
    if not obs_path.exists():
        return {
            "status": "no_observations",
            "new_entries": 0,
            "entry_count": 0,
            "reflector_entry_offset": int(state["reflector_entry_offset"]),
            "message": f"Observation file not found: {obs_path}",
        }

    parsed = _read_jsonl_entries(obs_path)
    entry_count = 0 if not parsed else parsed[-1][0] + 1
    offset = int(state["reflector_entry_offset"])

    if entry_count <= offset:
        return {
            "status": "up_to_date",
            "new_entries": 0,
            "entry_count": entry_count,
            "reflector_entry_offset": offset,
            "message": "Reflector already up to date.",
        }

    current_session_decisions = [
        entry
        for line_index, entry in parsed
        if line_index >= offset and entry.get("type") == "decision"
    ]
    if not current_session_decisions:
        db.update_reflector_offset(
            conn, session_id, entry_count, entry_count=entry_count
        )
        return {
            "status": "no_decisions",
            "new_entries": 0,
            "entry_count": entry_count,
            "reflector_entry_offset": entry_count,
            "message": "No new decisions to compare.",
        }

    session_row = db.get_session(conn, session_id)
    project = None if session_row is None else session_row.get("project")
    if not project:
        db.update_reflector_offset(
            conn, session_id, entry_count, entry_count=entry_count
        )
        return {
            "status": "no_project",
            "new_entries": 0,
            "entry_count": entry_count,
            "reflector_entry_offset": entry_count,
            "message": f"No project mapping found for session {session_id}.",
        }

    project_decisions: list[dict[str, Any]] = []
    for other_path in sorted(db.OBS_DIR.glob("*.jsonl")):
        other_session_id = other_path.stem
        if other_session_id == session_id:
            continue
        other_session = db.get_session(conn, other_session_id)
        if other_session is None or other_session.get("project") != project:
            continue
        for _line_index, entry in _read_jsonl_entries(other_path):
            if entry.get("type") == "decision":
                project_decisions.append(entry)

    if not project_decisions:
        db.update_reflector_offset(
            conn, session_id, entry_count, entry_count=entry_count
        )
        return {
            "status": "no_peer_decisions",
            "new_entries": 0,
            "entry_count": entry_count,
            "reflector_entry_offset": entry_count,
            "message": f"No peer decisions found in project {project}.",
        }

    existing_conflicts = [
        entry for _line_index, entry in parsed if entry.get("type") == "conflict"
    ]

    try:
        prompt_template = prompt_path.read_text()
    except OSError as e:
        return {
            "status": "failed",
            "new_entries": 0,
            "entry_count": entry_count,
            "reflector_entry_offset": offset,
            "error": f"read prompt {prompt_path}: {e}",
            "message": f"Could not read reflector prompt: {e}",
        }

    if shutil.which(claude_bin) is None and not Path(claude_bin).exists():
        return {
            "status": "failed",
            "new_entries": 0,
            "entry_count": entry_count,
            "reflector_entry_offset": offset,
            "error": f"{claude_bin} not found on PATH",
            "message": f"Could not find {claude_bin} on PATH.",
        }

    prompt = _build_prompt(
        prompt_template,
        current_session_decisions,
        project_decisions,
        existing_conflicts,
        obs_path,
    )
    before_count = entry_count
    try:
        proc = subprocess.run(
            [
                claude_bin, "-p", prompt,
                "--model", "haiku",
                "--permission-mode", "acceptEdits",
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return {
            "status": "failed",
            "new_entries": 0,
            "entry_count": before_count,
            "reflector_entry_offset": offset,
            "error": f"claude -p timed out after {timeout}s",
            "message": f"claude -p timed out after {timeout}s.",
        }
    except OSError as e:
        return {
            "status": "failed",
            "new_entries": 0,
            "entry_count": before_count,
            "reflector_entry_offset": offset,
            "error": f"claude -p failed to start: {e}",
            "message": f"Could not invoke {claude_bin}: {e}",
        }

    if proc.returncode != 0:
        return {
            "status": "failed",
            "new_entries": 0,
            "entry_count": before_count,
            "reflector_entry_offset": offset,
            "error": f"claude -p exited {proc.returncode}",
            "stderr": (proc.stderr or "").strip(),
            "message": (
                f"claude -p exited {proc.returncode}. "
                "Reflector state not advanced; next run will retry."
            ),
        }

    after_entries = _read_jsonl_entries(obs_path)
    after_count = 0 if not after_entries else after_entries[-1][0] + 1
    new_entries = max(0, after_count - before_count)
    db.update_reflector_offset(
        conn, session_id, after_count, entry_count=after_count
    )
    return {
        "status": "extracted",
        "new_entries": new_entries,
        "entry_count": after_count,
        "reflector_entry_offset": after_count,
        "message": (
            f"Processed {len(current_session_decisions)} new decision(s). "
            f"Appended {new_entries} conflict(s)."
        ),
    }
