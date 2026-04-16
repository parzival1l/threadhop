"""JSONL transcript indexer for ThreadHop.

Reads Claude Code session JSONL files (``~/.claude/projects/**/*.jsonl``)
and populates the SQLite ``messages`` table + FTS5 index created by
``db._migration_002_messages``.

Per ADR-003 in ``docs/DESIGN-DECISIONS.md``:

* Consecutive assistant lines sharing the same ``message.id`` are merged
  into one row. The first chunk's ``uuid`` is the PK and its
  ``timestamp`` / ``cwd`` / ``parentUuid`` / ``isSidechain`` are the row
  metadata. Text blocks from every chunk are concatenated, and
  ``tool_use`` blocks are rendered as one-line abbreviations inline —
  see `abbreviate_tool_use`.
* ``<system-reminder>`` blocks are stripped from visible text so search
  isn't polluted with harness plumbing.
* User lines carrying a ``toolUseResult`` (i.e. tool output) are skipped
  — tool output is noisy and would dominate FTS (Open Question Q5).
* ``thinking`` blocks are skipped (they're internal reasoning and aren't
  what the TUI renders either).

The indexer is idempotent: ``INSERT OR REPLACE`` on the ``uuid`` PK
means re-running overwrites rather than duplicates. Task #9 will layer
byte-offset tracking on top to avoid re-parsing unchanged bytes.

Public API::

    index_session(conn, jsonl_path) -> int
    index_all(conn, root=CLAUDE_PROJECTS) -> dict[Path, int]
    parse_messages(jsonl_path) -> Iterator[dict]
"""

from __future__ import annotations

import json
import re
import sqlite3
import sys
from pathlib import Path
from typing import Iterator

import db

# Root of Claude Code's per-project session directories.
CLAUDE_PROJECTS = Path.home() / ".claude" / "projects"

# Matches ``<system-reminder>…</system-reminder>``, spanning newlines.
# Kept module-level so the compiled regex is shared across rows.
SYSTEM_REMINDER_RE = re.compile(
    r"<system-reminder>.*?</system-reminder>", re.DOTALL
)


# --- Text cleanup --------------------------------------------------------


def strip_system_reminders(text: str) -> str:
    """Remove ``<system-reminder>`` blocks and trim surrounding whitespace."""
    return SYSTEM_REMINDER_RE.sub("", text).strip()


def abbreviate_tool_use(tool_name: str, tool_input: dict) -> str:
    """Render a ``tool_use`` block as one human-readable line.

    Mirrors ``TranscriptView._format_tool_use`` in the ``threadhop``
    script so the indexed text matches what the user sees in the TUI.
    Kept in lockstep with the TUI version on purpose: a divergence here
    would mean search hits point at labels the user never saw.
    """
    inp = tool_input or {}
    if tool_name == "Read":
        p = inp.get("file_path", "")
        return f"Reading {Path(p).name}" if p else "Reading file"
    if tool_name == "Write":
        p = inp.get("file_path", "")
        return f"Writing {Path(p).name}" if p else "Writing file"
    if tool_name == "Edit":
        p = inp.get("file_path", "")
        return f"Editing {Path(p).name}" if p else "Editing file"
    if tool_name == "Bash":
        cmd = inp.get("command", "") or ""
        short = cmd.split()[0] if cmd else "command"
        return f"Running {short}"
    if tool_name == "Glob":
        pat = inp.get("pattern", "")
        return f"Searching for {pat}" if pat else "Searching files"
    if tool_name == "Grep":
        pat = inp.get("pattern", "")
        return f"Searching for '{pat}'" if pat else "Searching content"
    if tool_name == "Agent":
        desc = inp.get("description", "")
        return f"Agent: {desc}" if desc else "Running agent"
    if tool_name == "WebFetch":
        url = inp.get("url", "")
        return f"Fetching {url[:50]}..." if len(url) > 50 else f"Fetching {url}"
    if tool_name == "WebSearch":
        q = inp.get("query", "")
        return f"Searching web for '{q}'"
    if tool_name == "TodoWrite":
        return "Updating todo list"
    return tool_name


# --- Per-line extraction -------------------------------------------------


def _extract_user_text(msg: dict) -> str | None:
    """Return cleaned text for a user JSONL line, or None to skip.

    Skips lines that carry ``toolUseResult`` (tool output — see Q5) and
    lines that reduce to empty after stripping. ``content`` may be either
    a raw string or a list of ``{type: 'text', text: ...}`` blocks —
    both shapes exist in real transcripts.
    """
    if msg.get("toolUseResult") is not None:
        return None
    content = msg.get("message", {}).get("content", "")
    if isinstance(content, list):
        parts = [
            b.get("text", "")
            for b in content
            if isinstance(b, dict) and b.get("type") == "text"
        ]
        content = " ".join(parts)
    if not isinstance(content, str):
        return None
    text = strip_system_reminders(content)
    return text or None


def _extract_assistant_blocks(msg: dict) -> list[str]:
    """Return the text + abbreviated-tool-call snippets for one assistant line.

    ``thinking`` blocks are intentionally dropped — they aren't rendered
    in the TUI and wouldn't help keyword search. The resulting list is
    joined by the caller into the final indexed text.
    """
    content = msg.get("message", {}).get("content", [])
    if not isinstance(content, list):
        return []
    out: list[str] = []
    for block in content:
        if not isinstance(block, dict):
            continue
        btype = block.get("type")
        if btype == "text":
            t = strip_system_reminders(block.get("text", ""))
            if t:
                out.append(t)
        elif btype == "tool_use":
            out.append(
                abbreviate_tool_use(
                    block.get("name", "Unknown"),
                    block.get("input", {}) or {},
                )
            )
        # thinking / other — skipped
    return out


# --- Streaming parse -----------------------------------------------------


def parse_messages(jsonl_path: Path) -> Iterator[dict]:
    """Yield rows ready for insertion into the ``messages`` table.

    Consecutive assistant lines sharing the same ``message.id`` are
    merged. A non-assistant line, a differing ``message.id``, or EOF
    flushes the in-flight buffer. Each yielded dict's keys line up 1:1
    with the table columns so the caller can hand it straight to
    ``executemany``.

    Malformed JSON lines are silently skipped — one corrupt line should
    not abort indexing the rest of the file.
    """
    # Buffer state for in-flight assistant message merging.
    buf_mid: str | None = None
    buf_parts: list[str] = []
    buf_row: dict | None = None

    def _flush() -> dict | None:
        """Finalize the buffered assistant row, or return None if empty."""
        nonlocal buf_mid, buf_parts, buf_row
        if buf_row is None:
            buf_mid = None
            buf_parts = []
            return None
        row = buf_row
        # Two newlines between chunks makes text/tool-call boundaries
        # readable in snippets and keeps FTS tokenization clean.
        row["text"] = "\n\n".join(p for p in buf_parts if p).strip()
        buf_mid = None
        buf_parts = []
        buf_row = None
        # Drop rows that collapsed to empty (e.g. a line that held only
        # a `thinking` block). No point indexing empty FTS entries.
        return row if row["text"] else None

    try:
        fh = open(jsonl_path)
    except OSError:
        return

    with fh as f:
        for line in f:
            try:
                msg = json.loads(line)
            except (json.JSONDecodeError, ValueError):
                continue

            mtype = msg.get("type")
            if mtype not in ("user", "assistant"):
                continue

            if mtype == "user":
                flushed = _flush()
                if flushed:
                    yield flushed

                text = _extract_user_text(msg)
                if not text:
                    continue
                uid = msg.get("uuid")
                sid = msg.get("sessionId")
                if not uid or not sid:
                    # Without a uuid or sessionId the row can't land in
                    # the DB — skip rather than fabricating values.
                    continue
                yield {
                    "uuid": uid,
                    "session_id": sid,
                    "role": "user",
                    "text": text,
                    "timestamp": msg.get("timestamp"),
                    "cwd": msg.get("cwd"),
                    "parent_uuid": msg.get("parentUuid"),
                    "is_sidechain": 1 if msg.get("isSidechain") else 0,
                }
                continue

            # --- assistant line ---
            mid = msg.get("message", {}).get("id")
            parts = _extract_assistant_blocks(msg)

            # Streaming chunk of the current logical message → append.
            if mid is not None and buf_mid == mid and buf_row is not None:
                buf_parts.extend(parts)
                continue

            # Different message.id (or no buffer yet) → flush and start fresh.
            flushed = _flush()
            if flushed:
                yield flushed

            uid = msg.get("uuid")
            sid = msg.get("sessionId")
            if not uid or not sid:
                continue
            buf_mid = mid
            buf_parts = list(parts)
            buf_row = {
                "uuid": uid,
                "session_id": sid,
                "role": "assistant",
                "text": "",  # populated by _flush
                "timestamp": msg.get("timestamp"),
                "cwd": msg.get("cwd"),
                "parent_uuid": msg.get("parentUuid"),
                "is_sidechain": 1 if msg.get("isSidechain") else 0,
            }

        # End of file — flush whatever's still buffered.
        flushed = _flush()
        if flushed:
            yield flushed


# --- Insertion -----------------------------------------------------------

_INSERT_SQL = (
    "INSERT OR REPLACE INTO messages "
    "(uuid, session_id, role, text, timestamp, cwd, parent_uuid, is_sidechain) "
    "VALUES "
    "(:uuid, :session_id, :role, :text, :timestamp, :cwd, :parent_uuid, :is_sidechain)"
)


def _ensure_session_stub(
    conn: sqlite3.Connection,
    session_id: str,
    jsonl_path: Path,
) -> None:
    """Create a minimal ``sessions`` row if one doesn't yet exist.

    The indexer can run before (or independent of) task #2's config
    migration — without a stub sessions row, any future FOREIGN KEY on
    ``messages.session_id`` would reject our inserts, and even today a
    join from ``messages`` back to ``sessions`` would return nothing.
    The TUI's refresh cycle and the config migration will enrich the
    row later; we only seed the fields required to make it exist.
    """
    conn.execute(
        "INSERT OR IGNORE INTO sessions "
        "(session_id, session_path, project, status) "
        "VALUES (?, ?, ?, 'active')",
        (session_id, str(jsonl_path), jsonl_path.parent.name),
    )


def index_session(conn: sqlite3.Connection, jsonl_path: Path) -> int:
    """Parse one JSONL file and upsert its messages. Returns row count.

    All writes happen in a single transaction so a crash mid-file leaves
    the DB unchanged. Returns 0 if the file has no indexable content
    (e.g. a brand-new session with only ``summary`` lines).
    """
    jsonl_path = Path(jsonl_path)
    rows = list(parse_messages(jsonl_path))
    if not rows:
        return 0
    session_id = rows[0]["session_id"]
    with db.transaction(conn):
        _ensure_session_stub(conn, session_id, jsonl_path)
        conn.executemany(_INSERT_SQL, rows)
    return len(rows)


def index_all(
    conn: sqlite3.Connection,
    root: Path | None = None,
) -> dict[Path, int]:
    """Index every ``*.jsonl`` under ``root`` (default ``~/.claude/projects``).

    Returns ``{path: rows_upserted}``. A single malformed file logs to
    stderr and contributes 0 rows rather than aborting the sweep — a
    handful of bad files shouldn't block the rest of the index.
    Sub-agent transcripts (``agent-*.jsonl``) are skipped: their content
    appears in the parent session and double-indexing would inflate FTS.
    """
    root = Path(root) if root is not None else CLAUDE_PROJECTS
    results: dict[Path, int] = {}
    if not root.is_dir():
        return results
    for jsonl in root.rglob("*.jsonl"):
        if jsonl.name.startswith("agent-"):
            continue
        try:
            results[jsonl] = index_session(conn, jsonl)
        except Exception as e:  # noqa: BLE001 — keep sweeping
            print(f"indexer: failed {jsonl}: {e}", file=sys.stderr)
            results[jsonl] = 0
    return results


if __name__ == "__main__":  # pragma: no cover
    # Manual sweep entry point: `python indexer.py`
    conn = db.init_db()
    summary = index_all(conn)
    total = sum(summary.values())
    print(f"Indexed {total} messages from {len(summary)} file(s)")
