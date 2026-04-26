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
  isn't polluted with harness plumbing. User lines additionally lose
  ``<local-command-*>`` and ``<command-*>`` blocks, and skill-load
  banners (the synthetic user line Claude Code injects when a slash-
  command loads a skill) collapse to nothing — see ``clean_user_text``
  and ``classify_user_text``.
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

# Claude Code prepends ``<local-command-{caveat,stdout,stderr}>`` blocks
# to bash-passthrough turns (``!cmd``). They are harness plumbing — the
# user saw them as UI chrome, not as words they typed — so they pollute
# both FTS hits and the observer feed when left in place.
LOCAL_COMMAND_BLOCK_RE = re.compile(
    r"<local-command-(?:caveat|stdout|stderr)>"
    r".*?"
    r"</local-command-(?:caveat|stdout|stderr)>",
    re.DOTALL,
)

# Slash-command invocations land as a synthetic user JSONL line whose
# whole content is ``<command-name>/foo</command-name>`` plus optional
# ``<command-message>`` / ``<command-args>`` siblings. Stripping the
# whole block (not just the markup) keeps FTS focused on real prose;
# the TUI separately classifies these lines into a compact pill so the
# command invocation is still visible to the user. (See
# ``classify_user_text`` and ``CommandPill`` in tui.py.)
COMMAND_BLOCK_RE = re.compile(
    r"<(command-name|command-message|command-args)>"
    r".*?"
    r"</\1>",
    re.DOTALL,
)

# When Claude Code loads a skill (e.g. via ``/improve-codebase-architecture``)
# it injects the entire skill markdown body as a synthetic user-role
# JSONL line, prefixed with this banner. From the transcript's POV it
# looks identical to a real user message — but it's pure harness chrome.
# Detecting on the banner is structural enough to survive skill renames.
SKILL_LOAD_BANNER_RE = re.compile(
    r"^\s*Base directory for this skill:\s*(\S+)"
)


# --- Text cleanup --------------------------------------------------------


def strip_system_reminders(text: str) -> str:
    """Remove ``<system-reminder>`` blocks and trim surrounding whitespace."""
    return SYSTEM_REMINDER_RE.sub("", text).strip()


def clean_user_text(text: str) -> str:
    """Strip every flavour of harness chrome from a user-line content string.

    Removes ``<system-reminder>`` blocks, ``<local-command-*>`` bash
    passthrough plumbing, and ``<command-{name,message,args}>`` slash-
    command markup. If the residue is a skill-load banner (the markdown
    body Claude Code injects when a skill loads), the whole line collapses
    to ``''`` — it's plumbing, not user content, and indexing it would
    bury real user prose under boilerplate.

    Used by the indexer's FTS path and the TUI renderer's user branch so
    both surfaces see the same cleaned view.
    """
    text = SYSTEM_REMINDER_RE.sub("", text)
    text = LOCAL_COMMAND_BLOCK_RE.sub("", text)
    text = COMMAND_BLOCK_RE.sub("", text)
    if SKILL_LOAD_BANNER_RE.match(text.lstrip()):
        return ""
    return text.strip()


def classify_user_text(text: str) -> tuple[str, str]:
    """Classify a user-line content string into (kind, display_text).

    Kinds:
    * ``"command"`` — the line is a slash-command invocation; ``display_text``
      is the command name (e.g. ``/improve-codebase-architecture``).
    * ``"skill_load"`` — the line is a Claude Code skill-load banner +
      injected skill body; ``display_text`` is the skill basename for
      the pill label.
    * ``"user"`` — a normal human-typed message; ``display_text`` is the
      cleaned content (system-reminders / local-command blocks stripped).
    * ``"empty"`` — the line had no displayable content after cleaning.

    The TUI uses this to render commands and skill-loads as compact
    pills instead of full user-message widgets — which is what fixes
    the "skill body shows up as a fat You: message" pollution.
    """
    cleaned = SYSTEM_REMINDER_RE.sub("", text)
    cleaned = LOCAL_COMMAND_BLOCK_RE.sub("", cleaned)

    cmd_match = re.search(
        r"<command-name>(.*?)</command-name>", cleaned, re.DOTALL,
    )
    residue = COMMAND_BLOCK_RE.sub("", cleaned).strip()

    if cmd_match and not residue:
        return ("command", cmd_match.group(1).strip())

    banner = SKILL_LOAD_BANNER_RE.match(residue.lstrip()) if residue else None
    if banner:
        skill_path = banner.group(1)
        skill_name = Path(skill_path).name if skill_path else "skill"
        return ("skill_load", skill_name)

    if not residue:
        return ("empty", "")

    if cmd_match:
        # User typed extra prose alongside the slash-command — keep
        # both, with the command first so the line still reads naturally.
        return ("user", f"{cmd_match.group(1).strip()}\n{residue}")

    return ("user", residue)


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
    # ``clean_user_text`` strips system-reminders, local-command blocks,
    # command-tag blocks, and drops skill-load banners entirely so FTS
    # never indexes harness plumbing. Assistant lines never carry these
    # patterns, so ``_extract_assistant_blocks`` keeps the lighter
    # ``strip_system_reminders`` call on each text block.
    text = clean_user_text(content)
    return text or None


def _extract_assistant_blocks(
    msg: dict,
    *,
    include_tool_calls: bool = True,
) -> list[str]:
    """Return the text + abbreviated-tool-call snippets for one assistant line.

    ``thinking`` blocks are intentionally dropped — they aren't rendered
    in the TUI and wouldn't help keyword search. The resulting list is
    joined by the caller into the final indexed text.

    ``include_tool_calls=False`` drops ``tool_use`` blocks entirely
    rather than abbreviating them — used by the ``copy`` command so
    pasted transcripts contain only human-visible prose. Default
    preserves the indexer/TUI/observer behaviour. Config-driven control
    over this knob is tracked in issue #63.
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
        elif btype == "tool_use" and include_tool_calls:
            out.append(
                abbreviate_tool_use(
                    block.get("name", "Unknown"),
                    block.get("input", {}) or {},
                )
            )
        # thinking / other — skipped
    return out


# --- Streaming parse -----------------------------------------------------


def parse_messages(
    jsonl_path: Path,
    *,
    include_tool_calls: bool = True,
) -> Iterator[dict]:
    """Yield rows ready for insertion into the ``messages`` table.

    Consecutive assistant lines sharing the same ``message.id`` are
    merged. A non-assistant line, a differing ``message.id``, or EOF
    flushes the in-flight buffer. Each yielded dict's keys line up 1:1
    with the table columns so the caller can hand it straight to
    ``executemany``.

    Malformed JSON lines are silently skipped — one corrupt line should
    not abort indexing the rest of the file.

    ``include_tool_calls`` is forwarded to ``_extract_assistant_blocks``.
    Defaults to ``True`` so indexer/TUI/observer callers are unaffected;
    ``copy.py`` passes ``False`` for clean-prose-only rendering (see
    issue #63 for config-driven evolution).
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
            parts = _extract_assistant_blocks(
                msg, include_tool_calls=include_tool_calls,
            )

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


# --- Incremental indexing (task #9) ----------------------------------------


def parse_byte_range(
    raw_bytes: bytes,
    *,
    fallback_session_id: str | None = None,
) -> list[dict]:
    """Parse a JSONL byte range into cleaned message groups.

    Applies ADR-003 chunk-merging (consecutive assistant lines sharing
    ``message.id`` collapse into one row), strips ``<system-reminder>``
    blocks, skips tool-result user lines, and abbreviates ``tool_use``
    blocks. Empty rows (a line that reduced to nothing after cleaning)
    are dropped.

    Callers are expected to trim any trailing partial line before
    calling — see ``index_session_incremental`` for how to do that with
    a byte offset. Malformed JSON lines are silently skipped.

    Returns one dict per logical message with the same shape
    ``index_session_incremental`` writes to ``messages``::

        {"uuid", "session_id", "role", "text", "timestamp",
         "cwd", "parent_uuid", "is_sidechain", "message_id"}

    ``fallback_session_id`` is substituted when a line omits its own
    ``sessionId`` (rare, but happens on older transcripts).

    Extracted as a public helper so the observer (ADR-018) can reuse
    the exact rendering the FTS index uses — "observer sees what the
    user sees."
    """
    raw_lines = raw_bytes.decode("utf-8", errors="replace").split("\n")
    # split() on "line1\nline2\n" → ["line1", "line2", ""] — drop trailing empty.
    if raw_lines and raw_lines[-1] == "":
        raw_lines.pop()

    groups: list[dict] = []
    current_chunk: dict | None = None
    current_chunk_parts: list[str] = []

    def _flush_chunk() -> None:
        nonlocal current_chunk, current_chunk_parts
        if current_chunk is not None:
            current_chunk["text"] = "\n\n".join(
                p for p in current_chunk_parts if p
            ).strip()
            if current_chunk["text"]:
                groups.append(current_chunk)
            current_chunk = None
            current_chunk_parts = []

    for raw_line in raw_lines:
        if not raw_line.strip():
            continue
        try:
            msg = json.loads(raw_line)
        except (json.JSONDecodeError, ValueError):
            continue

        mtype = msg.get("type")
        if mtype not in ("user", "assistant"):
            continue

        if mtype == "user":
            _flush_chunk()

            text = _extract_user_text(msg)
            if not text:
                continue

            uid = msg.get("uuid")
            sid = msg.get("sessionId") or fallback_session_id
            if not uid:
                continue

            groups.append({
                "uuid": uid,
                "session_id": sid,
                "role": "user",
                "text": text,
                "timestamp": msg.get("timestamp"),
                "cwd": msg.get("cwd"),
                "parent_uuid": msg.get("parentUuid"),
                "is_sidechain": 1 if msg.get("isSidechain") else 0,
                "message_id": msg.get("message", {}).get("id"),
            })
            continue

        # --- assistant line ---
        mid = msg.get("message", {}).get("id")
        parts = _extract_assistant_blocks(msg)

        # Continuing the same logical message → accumulate.
        if (
            mid is not None
            and current_chunk is not None
            and current_chunk.get("message_id") == mid
        ):
            current_chunk_parts.extend(parts)
            continue

        # Different message.id → flush previous and start fresh.
        _flush_chunk()

        uid = msg.get("uuid")
        sid = msg.get("sessionId") or fallback_session_id
        if not uid:
            continue

        current_chunk = {
            "uuid": uid,
            "session_id": sid,
            "role": "assistant",
            "text": "",  # populated by _flush_chunk
            "timestamp": msg.get("timestamp"),
            "cwd": msg.get("cwd"),
            "parent_uuid": msg.get("parentUuid"),
            "is_sidechain": 1 if msg.get("isSidechain") else 0,
            "message_id": mid,
        }
        current_chunk_parts = list(parts)

    # Flush the last chunk.
    _flush_chunk()
    return groups


def index_session_incremental(
    conn: sqlite3.Connection,
    session_id: str,
    session_path: str | Path,
) -> None:
    """Incrementally index new JSONL content for a session.

    Called from the TUI's 5-second refresh cycle.  Reads only bytes
    appended since the last index, parses new lines via
    ``parse_byte_range``, and upserts into the ``messages`` +
    ``messages_fts`` tables.

    Edge cases:

    * **File truncation / rotation**: ``file_size < last_offset``
      triggers deletion of all indexed messages for the session and a
      full re-index from byte 0.
    * **Partial line at EOF**: the read stops at the last complete
      newline; the incomplete trailing bytes are picked up on the next
      refresh.
    * **Chunk boundary**: if new bytes begin with a continuation of a
      previous assistant message chunk (same ``message.id``), the
      existing row is updated by appending the new text.
    """
    from datetime import datetime as _dt

    file_path = Path(session_path)
    if not file_path.exists():
        return

    try:
        file_size = file_path.stat().st_size
    except OSError:
        return

    state = db.get_index_state(conn, session_id)
    last_offset = state["last_byte_offset"] if state else 0

    # --- Truncation / rotation detection ---
    if file_size < last_offset:
        with db.transaction(conn):
            db.delete_session_messages(conn, session_id)
            db.delete_index_state(conn, session_id)
        last_offset = 0

    if file_size == last_offset:
        return  # Nothing new

    # --- Read new bytes ---
    try:
        with open(file_path, "rb") as f:
            f.seek(last_offset)
            new_bytes = f.read()
    except OSError:
        return

    # --- Partial line at EOF ---
    last_newline = new_bytes.rfind(b"\n")
    if last_newline == -1:
        return  # No complete line yet
    processable = new_bytes[: last_newline + 1]
    new_offset = last_offset + last_newline + 1

    parsed_groups = parse_byte_range(
        processable, fallback_session_id=session_id,
    )

    if not parsed_groups:
        db.upsert_index_state(
            conn, session_id, str(file_path), new_offset,
            _dt.now().timestamp(),
        )
        return

    # --- Write to DB ---
    with db.transaction(conn):
        _ensure_session_stub(conn, session_id, file_path)

        for group in parsed_groups:
            if not group["text"]:
                continue

            # Cross-batch chunk merging: if this assistant message.id was
            # partially indexed in a previous refresh, append new text to
            # the existing row instead of inserting a duplicate.
            if group["role"] == "assistant" and group.get("message_id"):
                existing = db.query_one(
                    conn,
                    "SELECT uuid, text FROM messages "
                    "WHERE message_id = ? AND session_id = ?",
                    (group["message_id"], group["session_id"]),
                )
                if existing:
                    merged = existing["text"] + "\n\n" + group["text"]
                    conn.execute(
                        "UPDATE messages SET text = ? WHERE uuid = ?",
                        (merged, existing["uuid"]),
                    )
                    continue

            conn.execute(
                "INSERT OR IGNORE INTO messages "
                "(uuid, session_id, role, text, timestamp, cwd, "
                "parent_uuid, is_sidechain, message_id) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    group["uuid"], group["session_id"],
                    group["role"], group["text"],
                    group["timestamp"], group["cwd"],
                    group["parent_uuid"], group["is_sidechain"],
                    group.get("message_id"),
                ),
            )

        db.upsert_index_state(
            conn, session_id, str(file_path), new_offset,
            _dt.now().timestamp(),
        )


if __name__ == "__main__":  # pragma: no cover
    # Manual sweep entry point: `python indexer.py`
    conn = db.init_db()
    summary = index_all(conn)
    total = sum(summary.values())
    print(f"Indexed {total} messages from {len(summary)} file(s)")
