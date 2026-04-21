"""Bookmark orchestration helpers.

Owns the shared bookmark-ingest primitive and the category research runner.
The low-level SQL helpers live in ``db.py``; this module handles transcript
context extraction, CLI-friendly validation, and ``claude -p`` execution.
"""

from __future__ import annotations

import shutil
import subprocess
from datetime import datetime, timezone
from pathlib import Path
import re
import sqlite3

import db
import indexer

DEFAULT_CONTEXT_WINDOW = 1
DEFAULT_TIMEOUT_SEC = 180.0


def ensure_bookmark_target(
    conn: sqlite3.Connection,
    session_id: str,
    message_uuid: str,
    *,
    session_path: Path | str | None = None,
) -> dict:
    """Ensure the target session/message is present in SQLite before bookmarking."""
    path = Path(session_path) if session_path is not None else None
    if path is None:
        row = db.get_session(conn, session_id)
        if row is not None and row.get("session_path"):
            path = Path(row["session_path"])
    if path is None or not path.exists():
        raise FileNotFoundError(f"No transcript found for session {session_id}.")

    db.upsert_session(
        conn,
        session_id,
        str(path),
        project=path.parent.name,
    )
    indexer.index_session_incremental(conn, session_id, path)
    message = db.query_one(
        conn,
        "SELECT uuid, session_id, role, text, timestamp FROM messages "
        "WHERE uuid = ? AND session_id = ?",
        (message_uuid, session_id),
    )
    if message is None:
        raise ValueError(
            f"Message {message_uuid} was not found in session {session_id}."
        )
    return message


def add_bookmark(
    conn: sqlite3.Connection,
    session_id: str,
    message_uuid: str,
    category_name: str,
    *,
    note: str | None = None,
    session_path: Path | str | None = None,
) -> dict:
    """Shared ingest primitive for CLI, future TUI category picker, and skills."""
    ensure_bookmark_target(
        conn,
        session_id,
        message_uuid,
        session_path=session_path,
    )
    return db.add_bookmark(
        conn,
        message_uuid,
        session_id=session_id,
        category_name=category_name,
        note=note,
    )


def _safe_category_dirname(name: str) -> str:
    safe = re.sub(r"[\\/]+", "-", name).strip()
    return safe or "category"


def _load_session_messages(session_path: Path) -> list[dict]:
    return list(indexer.parse_messages(session_path))


def _format_context_window(
    bookmark_row: dict,
    session_messages: list[dict] | None,
    *,
    window: int = DEFAULT_CONTEXT_WINDOW,
) -> str:
    target_uuid = bookmark_row["message_uuid"]
    lines: list[str] = []
    if session_messages:
        target_idx = next(
            (i for i, row in enumerate(session_messages) if row.get("uuid") == target_uuid),
            None,
        )
        if target_idx is not None:
            lo = max(0, target_idx - window)
            hi = min(len(session_messages), target_idx + window + 1)
            for idx in range(lo, hi):
                row = session_messages[idx]
                marker = "target" if row.get("uuid") == target_uuid else "context"
                text = (row.get("text") or "").strip()
                lines.append(
                    f"- {marker} [{row.get('role')}] {row.get('uuid')}: {text}"
                )
    if not lines:
        text = (bookmark_row.get("text") or "").strip()
        lines.append(
            f"- target [{bookmark_row.get('role')}] {target_uuid}: {text}"
        )
    return "\n".join(lines)


def _build_research_prompt(
    category_row: dict,
    bookmarks_rows: list[dict],
) -> str:
    sections: list[str] = [
        str(category_row["research_prompt"]).strip(),
        "",
        "Write markdown only.",
        f"Category: {category_row['name']}",
        f"Bookmarks in this run: {len(bookmarks_rows)}",
        "",
        "Bookmarks context:",
    ]
    for idx, row in enumerate(bookmarks_rows, start=1):
        sections.extend([
            "",
            f"## Bookmark {idx}",
            f"- Bookmark ID: {row['id']}",
            f"- Session ID: {row['session_id']}",
            f"- Message UUID: {row['message_uuid']}",
            f"- Project: {row.get('project') or '(unknown)'}",
            f"- Session label: {row.get('custom_name') or '(none)'}",
            f"- Saved at: {row.get('created_at')}",
            f"- Note: {row.get('note') or '(none)'}",
            "- Transcript window:",
            row["context_window"],
        ])
    return "\n".join(sections).strip() + "\n"


def run_bookmark_research(
    conn: sqlite3.Connection,
    category_name: str,
    *,
    force: bool = False,
    model: str = "haiku",
    claude_bin: str = "claude",
    timeout: float = DEFAULT_TIMEOUT_SEC,
) -> dict:
    """Run one background-research pass for a bookmark category."""
    category = db.get_bookmark_category(conn, category_name)
    if category is None:
        return {
            "status": "missing_category",
            "message": f"Bookmark category '{category_name}' does not exist.",
        }
    prompt_text = (category.get("research_prompt") or "").strip()
    if not prompt_text:
        return {
            "status": "missing_prompt",
            "message": (
                f"Bookmark category '{category_name}' has no research prompt. "
                "Set one with `threadhop bookmark category set-prompt` first."
            ),
        }

    rows = db.list_bookmarks_for_research(conn, category_name, force=force)
    if not rows:
        return {
            "status": "up_to_date",
            "message": (
                f"No bookmarks to research for category '{category_name}'."
            ),
            "processed": 0,
        }

    session_cache: dict[str, list[dict]] = {}
    for row in rows:
        session_messages = None
        session_path_raw = row.get("session_path")
        if session_path_raw:
            session_path = Path(session_path_raw)
            if session_path.exists():
                session_messages = session_cache.get(row["session_id"])
                if session_messages is None:
                    session_messages = _load_session_messages(session_path)
                    session_cache[row["session_id"]] = session_messages
        row["context_window"] = _format_context_window(row, session_messages)

    prompt = _build_research_prompt(category, rows)
    if shutil.which(claude_bin) is None and not Path(claude_bin).exists():
        return {
            "status": "failed",
            "message": f"Could not find {claude_bin} on PATH.",
            "error": f"{claude_bin} not found on PATH",
        }

    try:
        proc = subprocess.run(
            [
                claude_bin,
                "-p",
                prompt,
                "--model",
                model,
                "--permission-mode",
                "acceptEdits",
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return {
            "status": "failed",
            "message": f"claude -p timed out after {timeout}s.",
            "error": f"claude -p timed out after {timeout}s",
        }
    except OSError as exc:
        return {
            "status": "failed",
            "message": f"Could not invoke {claude_bin}: {exc}",
            "error": f"claude -p failed to start: {exc}",
        }

    if proc.returncode != 0:
        return {
            "status": "failed",
            "message": (
                f"claude -p exited {proc.returncode}. "
                "Bookmark research state was not advanced."
            ),
            "error": f"claude -p exited {proc.returncode}",
            "stderr": (proc.stderr or "").strip(),
        }

    now = datetime.now(timezone.utc)
    out_dir = db.RESEARCH_DIR / _safe_category_dirname(category_name)
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"{now.strftime('%Y%m%dT%H%M%SZ')}.md"
    body = (proc.stdout or "").strip()
    if not body:
        body = (
            f"# Research — {category_name}\n\n"
            "Claude returned no markdown content for this run.\n"
        )
    out_path.write_text(body if body.endswith("\n") else body + "\n")

    db.mark_bookmarks_researched(
        conn,
        [int(row["id"]) for row in rows],
        researched_at=now.timestamp(),
    )
    return {
        "status": "ok",
        "message": (
            f"Researched {len(rows)} bookmark(s) in '{category_name}'."
        ),
        "processed": len(rows),
        "output_path": str(out_path),
    }
