"""Observer core function — extracts typed observations from session JSONL.

This is the single observer used by every entry point (CLI ``threadhop
observe``, the ``/threadhop:observe`` skill, ``/threadhop:handoff``, the TUI
indicator, and CLI query commands). It is an **orchestrator**, not just a
``claude -p`` wrapper — per ADR-018:

  1. Read the per-session ``observation_state`` row to find where the last
     run left off (``source_byte_offset``).
  2. Open the source JSONL, seek to that offset, read to EOF.
  3. Parse the new bytes via ``indexer.parse_byte_range`` — same pipeline
     that feeds the FTS index and the TUI. Tool outputs are already
     filtered out, ``<system-reminder>`` blocks stripped, ``tool_use``
     blocks rendered as one-line abbreviations, streaming chunks merged.
     The observer sees exactly what the user sees.
  4. If fewer than ``BATCH_THRESHOLD`` new turns accumulated, return early —
     Haiku with two turns of context tends to hallucinate or extract trivia.
  5. Format the cleaned turns as a readable role-labelled transcript and
     splice it into ``prompts/observer.md`` along with the output file
     path (``~/.config/threadhop/observations/<session_id>.jsonl``).
  6. Invoke ``claude -p --model haiku --permission-mode acceptEdits`` — the
     Haiku process appends JSONL observations to the output file itself
     (``acceptEdits`` is the minimum permission required for file append).
  7. Diff the observation file line count before/after to compute how many
     new entries were written, then advance ``source_byte_offset`` and
     ``entry_count`` in SQLite via ``db.upsert_observation_state``.

Per ADR-019: per-session observation files. Per ADR-020: observer and
reflector share one file — the reflector appends ``type: "conflict"``
entries to the same JSONL.

Public API::

    observe_session(conn, session_id, ...) -> dict
"""

from __future__ import annotations

import json
import os
import signal
import shutil
import sqlite3
import subprocess
import sys
import threading
import time
from datetime import datetime
from pathlib import Path
from typing import Any, Callable

import db
import indexer


# --- Configuration --------------------------------------------------------

# Minimum human/assistant turns in a new chunk before extraction runs.
# Below this, observer returns early — two-turn chunks don't contain enough
# context for Haiku to distinguish a real decision from idle chatter, and
# paying for the call just to get zero entries is wasteful.
BATCH_THRESHOLD = 3

# Location of the shared observer prompt. Bundled with the app — the runtime
# does not need anything in ``~/.config/threadhop/prompts/``.
PROMPT_PATH = Path(__file__).resolve().parent / "prompts" / "observer.md"

# Location of the reflector prompt. Same packaging rule as the observer prompt.
REFLECTOR_PROMPT_PATH = (
    Path(__file__).resolve().parent / "prompts" / "reflector.md"
)

# Default subprocess timeout. Haiku usually responds in seconds, but the
# first call of a session can pay an auth/warmup cost, and extremely large
# chunks (first-time catch-up on a long transcript) take longer.
DEFAULT_TIMEOUT_SEC = 180.0

# Default poll cadence for the watch-mode loop. Haiku itself takes seconds
# to respond, so sub-second polling buys nothing — 1.0s keeps the loop
# responsive without burning CPU on stat() calls. Background sidecars that
# care about lower latency can swap in fsevents/kqueue without changing the
# public API (see ``watch_session``'s docstring).
WATCH_POLL_INTERVAL_SEC = 1.0

# Reflector cadence: run after this many new observation entries have
# accumulated beyond the reflector's last processed offset.
REFLECTOR_TRIGGER_THRESHOLD = 5

# Known observation types, in display order for the summary line.
_TYPE_ORDER = ("decision", "todo", "done", "adr", "observation", "conflict")

# Watch-mode backends for the sidecar. ``auto`` prefers macOS FSEvents when
# watchdog is available, otherwise falls back to the polling implementation.
WATCH_BACKEND_AUTO = "auto"
WATCH_BACKEND_POLL = "poll"
WATCH_BACKEND_FSEVENTS = "fsevents"


# --- Helpers --------------------------------------------------------------


def _format_transcript(turns: list[dict]) -> str:
    """Render cleaned message turns as a role-labelled transcript.

    Each turn becomes a markdown-ish block::

        ### user · 2026-04-17T10:00:00Z
        <cleaned text>

        ### assistant · 2026-04-17T10:00:01Z
        <cleaned text with [Editing foo.py] style tool-use abbreviations>

    Chosen over JSONL-per-turn because field names repeated per line would
    waste tokens on no new information, and Haiku reasons more reliably
    about a transcript-shaped input. Timestamps are kept so the prompt's
    ``ts`` field requirement (ADR-018) has an authoritative source.
    """
    blocks: list[str] = []
    for turn in turns:
        role = turn.get("role", "user")
        ts = turn.get("timestamp") or ""
        header = f"### {role} · {ts}" if ts else f"### {role}"
        text = (turn.get("text") or "").strip()
        if not text:
            continue
        blocks.append(f"{header}\n{text}")
    return "\n\n".join(blocks)


def _read_new_bytes(
    source_path: Path, offset: int
) -> tuple[bytes, int]:
    """Read from ``offset`` to the last complete newline.

    Mirrors the indexer's partial-line guard: if the session is actively
    being written, the tail may be mid-line — stop at the last ``\\n`` so
    the trailing partial line is picked up on the next run.

    Returns ``(processable_bytes, new_offset)``. ``new_offset`` is
    unchanged if no complete line is available yet.
    """
    try:
        with open(source_path, "rb") as f:
            f.seek(offset)
            raw = f.read()
    except OSError:
        return b"", offset

    if not raw:
        return b"", offset
    last_newline = raw.rfind(b"\n")
    if last_newline == -1:
        return b"", offset
    return raw[: last_newline + 1], offset + last_newline + 1


def _count_obs_lines(obs_path: Path) -> int:
    """Count non-empty lines in the observation JSONL (entry count)."""
    if not obs_path.exists():
        return 0
    count = 0
    with open(obs_path, "rb") as f:
        for line in f:
            if line.strip():
                count += 1
    return count


def _count_new_entries_by_type(
    obs_path: Path, skip_lines: int
) -> dict[str, int]:
    """Parse observation lines after ``skip_lines``, grouping by ``type``.

    ``skip_lines`` is the line count recorded before the extraction ran;
    anything past it was written by the current invocation (the file is
    append-only per ADR-020, so this diff is safe).
    """
    counts: dict[str, int] = {}
    if not obs_path.exists():
        return counts
    with open(obs_path) as f:
        for i, line in enumerate(f):
            if i < skip_lines:
                continue
            line = line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except (json.JSONDecodeError, ValueError):
                continue
            t = entry.get("type")
            if isinstance(t, str):
                counts[t] = counts.get(t, 0) + 1
    return counts


def _summary_message(turns: int, by_type: dict[str, int]) -> str:
    """Format the human-readable summary line returned to the caller."""
    parts: list[str] = []
    for key in _TYPE_ORDER:
        n = by_type.get(key, 0)
        if n:
            label = key if n == 1 else f"{key}s"
            parts.append(f"{n} {label}")
    # Extras Haiku invented (unlikely but defensive).
    for key, n in by_type.items():
        if key in _TYPE_ORDER or not n:
            continue
        parts.append(f"{n} {key}")
    tail = ", ".join(parts) if parts else "no new observations"
    turns_label = "turn" if turns == 1 else "turns"
    return f"Processed {turns} {turns_label}. {tail}."


def _summary_entries_message(entries: int, by_type: dict[str, int]) -> str:
    """Format a reflector-style summary for appended observation entries."""
    parts: list[str] = []
    for key in _TYPE_ORDER:
        n = by_type.get(key, 0)
        if n:
            label = key if n == 1 else f"{key}s"
            parts.append(f"{n} {label}")
    for key, n in by_type.items():
        if key in _TYPE_ORDER or not n:
            continue
        parts.append(f"{n} {key}")
    tail = ", ".join(parts) if parts else "no new observations"
    entry_label = "entry" if entries == 1 else "entries"
    return f"Appended {entries} {entry_label}. {tail}."


def _build_prompt(template: str, transcript: str, obs_path: Path) -> str:
    """Splice the cleaned transcript and output path into the prompt.

    Kept as plain string concat rather than a template engine — the
    observer prompt is a markdown file curated as part of the app, and
    the only substitutions are the transcript body and the file path.
    """
    return (
        f"{template.rstrip()}\n\n"
        "---\n\n"
        "## Conversation chunk\n\n"
        "<session_chunk>\n"
        f"{transcript.strip()}\n"
        "</session_chunk>\n\n"
        "## Output file\n\n"
        f"Append observations to: {obs_path}\n"
    )


def _build_reflector_prompt(
    template: str,
    current_session_decisions: list[dict[str, Any]],
    project_decisions: list[dict[str, Any]],
    existing_conflicts: list[dict[str, Any]],
    obs_path: Path,
) -> str:
    """Splice the reflector input sections into the prompt template."""
    def _render(entries: list[dict[str, Any]]) -> str:
        if not entries:
            return "(none)"
        return "\n".join(json.dumps(entry, ensure_ascii=True) for entry in entries)

    return (
        f"{template.rstrip()}\n\n"
        "---\n\n"
        "## Current session decisions\n\n"
        "<current_session_decisions>\n"
        f"{_render(current_session_decisions)}\n"
        "</current_session_decisions>\n\n"
        "## Project decisions\n\n"
        "<project_decisions>\n"
        f"{_render(project_decisions)}\n"
        "</project_decisions>\n\n"
        "## Existing conflicts\n\n"
        "<existing_conflicts>\n"
        f"{_render(existing_conflicts)}\n"
        "</existing_conflicts>\n\n"
        "## Output file\n\n"
        f"Append observations to: {obs_path}\n"
    )


def _read_obs_entries(obs_path: Path) -> list[dict[str, Any]]:
    """Parse the observation JSONL, skipping blank / invalid lines."""
    entries: list[dict[str, Any]] = []
    if not obs_path.exists():
        return entries
    with open(obs_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                raw = json.loads(line)
            except (json.JSONDecodeError, ValueError):
                continue
            if isinstance(raw, dict):
                entries.append(raw)
    return entries


def _pid_is_alive(pid: int | None) -> bool:
    """Return True when ``pid`` currently exists and is signalable."""
    if pid is None or pid <= 0:
        return False
    try:
        os.kill(int(pid), 0)
    except OSError:
        return False
    return True


def _ensure_observation_state_row(
    conn: sqlite3.Connection,
    session_id: str,
    source_path: str | Path | None = None,
) -> dict[str, Any] | None:
    """Ensure a session has an ``observation_state`` row.

    ``observe_session`` intentionally does not seed state on a first-run
    below-threshold chunk, because it must not advance the byte cursor
    implicitly. The background sidecar still needs a row for PID tracking
    and stop/resume, so it calls this helper before entering watch mode.
    """
    state = db.get_observation_state(conn, session_id)
    if state is not None:
        resolved_source = Path(source_path or state["source_path"])
        obs_path = Path(state["obs_path"])
        db.upsert_observation_state(
            conn,
            session_id,
            str(resolved_source),
            str(obs_path),
            source_byte_offset=int(state["source_byte_offset"]),
            entry_count=int(state["entry_count"]),
            reflector_entry_offset=int(state["reflector_entry_offset"]),
            observer_pid=(
                int(state["observer_pid"])
                if state.get("observer_pid") is not None
                else None
            ),
            status=str(state["status"]),
            started_at=state.get("started_at"),
            last_observed_at=state.get("last_observed_at"),
        )
        return db.get_observation_state(conn, session_id)

    if source_path is None:
        sess = db.get_session(conn, session_id)
        if sess is None or not sess.get("session_path"):
            return None
        source_path = sess["session_path"]

    resolved_source = Path(source_path)
    obs_path = db.OBS_DIR / f"{session_id}.jsonl"
    db.upsert_observation_state(
        conn,
        session_id,
        str(resolved_source),
        str(obs_path),
    )
    return db.get_observation_state(conn, session_id)


# --- Main entry point -----------------------------------------------------


def observe_session(
    conn: sqlite3.Connection,
    session_id: str,
    *,
    source_path: str | Path | None = None,
    batch_threshold: int = BATCH_THRESHOLD,
    claude_bin: str = "claude",
    model: str = "haiku",
    timeout: float = DEFAULT_TIMEOUT_SEC,
    prompt_path: Path | None = None,
) -> dict[str, Any]:
    """Run one observer extraction pass for ``session_id``.

    Args:
        conn: Open DB connection with the schema applied.
        session_id: Claude Code session UUID to observe.
        source_path: Override for the source JSONL. Normally resolved from
            the ``observation_state`` row (on resume) or the ``sessions``
            row (on first observation). Passing this explicitly is mostly
            useful for tests.
        batch_threshold: Minimum number of new message turns required for
            a Haiku call. Below this, the function returns without
            invoking Claude and without advancing ``source_byte_offset``.
        claude_bin: Name or path of the ``claude`` CLI binary. Tests pass
            a fake shim that writes the expected observation JSONL.
        timeout: Subprocess timeout for the Haiku call (seconds).
        prompt_path: Override for the observer prompt file location.
            Defaults to ``prompts/observer.md`` next to this module.

    Returns:
        A summary dict::

            {
              "status":  "extracted" | "up_to_date" | "below_threshold"
                         | "no_source" | "failed",
              "turns":   <int>,
              "new_entries": <int>,
              "by_type": {"decision": N, "todo": M, ...},
              "source_byte_offset": <int>,  # post-run cursor
              "entry_count": <int>,         # total lines in obs file
              "obs_path": <str>,
              "message": <human-readable summary>,
              # on status == "failed":
              "error": <str>,
              "stderr": <str>,   # present when stderr was captured
            }

        Never raises on expected failures (subprocess error, missing
        source file, etc.) — callers branch on ``status``.
    """
    prompt_path = prompt_path or PROMPT_PATH

    # --- Step 1: load state, or seed it from the sessions table -----------
    state = db.get_observation_state(conn, session_id)

    if state is None:
        # First observation for this session. Resolve source_path from
        # the sessions row unless the caller supplied one.
        if source_path is None:
            sess = db.get_session(conn, session_id)
            if sess is None or not sess.get("session_path"):
                return {
                    "status": "no_source",
                    "turns": 0,
                    "new_entries": 0,
                    "by_type": {},
                    "source_byte_offset": 0,
                    "entry_count": 0,
                    "obs_path": "",
                    "message": (
                        f"No source JSONL known for session {session_id}"
                    ),
                }
            source_path = sess["session_path"]
        source_path = Path(source_path)
        obs_path = db.OBS_DIR / f"{session_id}.jsonl"
        offset = 0
    else:
        # Resume from recorded cursor. ``source_path`` override is honoured
        # (tests / file moves) but obs_path is always the recorded one.
        source_path = Path(source_path or state["source_path"])
        obs_path = Path(state["obs_path"])
        offset = int(state["source_byte_offset"])

    if not source_path.exists():
        return {
            "status": "no_source",
            "turns": 0,
            "new_entries": 0,
            "by_type": {},
            "source_byte_offset": offset,
            "entry_count": int(state["entry_count"]) if state else 0,
            "obs_path": str(obs_path),
            "message": f"Source JSONL not found: {source_path}",
        }

    # --- Step 2: read new bytes -------------------------------------------
    try:
        file_size = source_path.stat().st_size
    except OSError as e:
        return {
            "status": "failed",
            "turns": 0,
            "new_entries": 0,
            "by_type": {},
            "source_byte_offset": offset,
            "entry_count": int(state["entry_count"]) if state else 0,
            "obs_path": str(obs_path),
            "error": f"stat {source_path}: {e}",
            "message": f"Could not stat source JSONL: {e}",
        }

    # Truncation / rotation: re-read from byte 0. Observations already
    # written stay — they're append-only per ADR-020 — but the cursor has
    # to rewind since the old offset is past the new EOF.
    if file_size < offset:
        offset = 0

    new_bytes, new_offset = _read_new_bytes(source_path, offset)

    # Parse the new bytes through the same pipeline the FTS index uses
    # (indexer.parse_byte_range): tool outputs dropped, system-reminders
    # stripped, tool_use abbreviated, streaming chunks merged. This keeps
    # Haiku's input token count small and ensures the observer reasons
    # about the same view of the conversation the user reads in the TUI.
    message_turns = indexer.parse_byte_range(
        new_bytes, fallback_session_id=session_id,
    )
    turns = len(message_turns)

    # Ensure the observations directory exists before we risk any writes.
    db.OBS_DIR.mkdir(parents=True, exist_ok=True)

    # --- Step 3: threshold check ------------------------------------------
    if turns < batch_threshold:
        # No state change: we want the next run to re-read the same bytes
        # (plus whatever else has arrived) and try to clear the threshold.
        existing_entries = (
            int(state["entry_count"]) if state
            else _count_obs_lines(obs_path)
        )
        return {
            "status": "up_to_date" if turns == 0 else "below_threshold",
            "turns": turns,
            "new_entries": 0,
            "by_type": {},
            "source_byte_offset": offset,
            "entry_count": existing_entries,
            "obs_path": str(obs_path),
            "message": (
                "Already up to date." if turns == 0
                else f"Only {turns} new turn(s) — need {batch_threshold}."
            ),
        }

    # --- Step 4: assemble the prompt --------------------------------------
    try:
        prompt_template = prompt_path.read_text()
    except OSError as e:
        return {
            "status": "failed",
            "turns": turns,
            "new_entries": 0,
            "by_type": {},
            "source_byte_offset": offset,
            "entry_count": _count_obs_lines(obs_path),
            "obs_path": str(obs_path),
            "error": f"read prompt {prompt_path}: {e}",
            "message": f"Could not read observer prompt: {e}",
        }
    transcript = _format_transcript(message_turns)
    prompt = _build_prompt(prompt_template, transcript, obs_path)

    # --- Step 5: invoke claude -p -----------------------------------------
    # ``shutil.which`` gives a precise error before we pay for the fork.
    if shutil.which(claude_bin) is None and not Path(claude_bin).exists():
        return {
            "status": "failed",
            "turns": turns,
            "new_entries": 0,
            "by_type": {},
            "source_byte_offset": offset,
            "entry_count": _count_obs_lines(obs_path),
            "obs_path": str(obs_path),
            "error": f"{claude_bin} not found on PATH",
            "message": f"Could not find {claude_bin} on PATH.",
        }

    before_count = _count_obs_lines(obs_path)
    try:
        proc = subprocess.run(
            [
                claude_bin, "-p", prompt,
                "--model", model,
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
            "turns": turns,
            "new_entries": 0,
            "by_type": {},
            "source_byte_offset": offset,
            "entry_count": before_count,
            "obs_path": str(obs_path),
            "error": f"claude -p timed out after {timeout}s",
            "message": f"claude -p timed out after {timeout}s.",
        }
    except OSError as e:
        return {
            "status": "failed",
            "turns": turns,
            "new_entries": 0,
            "by_type": {},
            "source_byte_offset": offset,
            "entry_count": before_count,
            "obs_path": str(obs_path),
            "error": f"claude -p failed to start: {e}",
            "message": f"Could not invoke {claude_bin}: {e}",
        }

    if proc.returncode != 0:
        return {
            "status": "failed",
            "turns": turns,
            "new_entries": 0,
            "by_type": {},
            "source_byte_offset": offset,
            "entry_count": before_count,
            "obs_path": str(obs_path),
            "error": f"claude -p exited {proc.returncode}",
            "stderr": (proc.stderr or "").strip(),
            "message": (
                f"claude -p exited {proc.returncode}. "
                "State not advanced; next run will retry from same offset."
            ),
        }

    # --- Step 6: diff observation file, advance state ---------------------
    after_count = _count_obs_lines(obs_path)
    new_entries = max(0, after_count - before_count)
    by_type = (
        _count_new_entries_by_type(obs_path, before_count)
        if new_entries else {}
    )

    now = datetime.now().timestamp()
    db.upsert_observation_state(
        conn,
        session_id,
        str(source_path),
        str(obs_path),
        source_byte_offset=new_offset,
        entry_count=after_count,
        reflector_entry_offset=(
            int(state["reflector_entry_offset"]) if state else 0
        ),
        last_observed_at=now,
    )

    # --- Step 7: summary --------------------------------------------------
    return {
        "status": "extracted",
        "turns": turns,
        "new_entries": new_entries,
        "by_type": by_type,
        "source_byte_offset": new_offset,
        "entry_count": after_count,
        "obs_path": str(obs_path),
        "message": _summary_message(turns, by_type),
    }


# --- Reflector ------------------------------------------------------------


def reflect_session(
    conn: sqlite3.Connection,
    session_id: str,
    *,
    claude_bin: str = "claude",
    timeout: float = DEFAULT_TIMEOUT_SEC,
    prompt_path: Path | None = None,
) -> dict[str, Any]:
    """Run one reflector pass for ``session_id`` if new entries are pending.

    The reflector works on the observation layer, not raw source JSONL. It
    looks at any observation entries after ``reflector_entry_offset`` and
    compares new decisions from the current session against decisions from
    other observed sessions in the same project. Conflict entries are
    appended to the same per-session observation file.
    """
    prompt_path = prompt_path or REFLECTOR_PROMPT_PATH
    state = db.get_observation_state(conn, session_id)
    if state is None:
        return {
            "status": "no_state",
            "new_entries": 0,
            "by_type": {},
            "entry_count": 0,
            "reflector_entry_offset": 0,
            "message": f"No observation state for session {session_id}.",
        }

    obs_path = Path(state["obs_path"])
    if not obs_path.exists():
        return {
            "status": "up_to_date",
            "new_entries": 0,
            "by_type": {},
            "entry_count": 0,
            "reflector_entry_offset": int(state["reflector_entry_offset"]),
            "message": "No observations yet.",
        }

    entries = _read_obs_entries(obs_path)
    before_count = len(entries)
    prior_offset = min(int(state["reflector_entry_offset"]), before_count)
    pending_entries = before_count - prior_offset
    if pending_entries <= 0:
        return {
            "status": "up_to_date",
            "new_entries": 0,
            "by_type": {},
            "entry_count": before_count,
            "reflector_entry_offset": prior_offset,
            "message": "Reflector already up to date.",
        }

    current_slice = entries[prior_offset:]
    current_decisions = [
        entry for entry in current_slice
        if entry.get("type") == "decision"
    ]
    existing_conflicts = [
        entry for entry in entries
        if entry.get("type") == "conflict"
    ]

    session = db.get_session(conn, session_id)
    project = session.get("project") if session else None
    other_decisions: list[dict[str, Any]] = []
    if project:
        for other in db.query_all(
            conn,
            "SELECT session_id FROM sessions WHERE project = ? AND session_id != ?",
            (project, session_id),
        ):
            other_id = other["session_id"]
            other_state = db.get_observation_state(conn, other_id)
            other_obs_path = (
                Path(other_state["obs_path"])
                if other_state is not None
                else db.OBS_DIR / f"{other_id}.jsonl"
            )
            for entry in _read_obs_entries(other_obs_path):
                if entry.get("type") == "decision":
                    other_decisions.append(entry)

    if not current_decisions or not other_decisions:
        db.update_reflector_offset(conn, session_id, before_count)
        return {
            "status": "up_to_date",
            "new_entries": 0,
            "by_type": {},
            "entry_count": before_count,
            "reflector_entry_offset": before_count,
            "message": (
                "No new cross-session decisions to compare."
                if current_decisions else
                "No new decisions to compare."
            ),
        }

    try:
        prompt_template = prompt_path.read_text()
    except OSError as e:
        return {
            "status": "failed",
            "new_entries": 0,
            "by_type": {},
            "entry_count": before_count,
            "reflector_entry_offset": prior_offset,
            "error": f"read prompt {prompt_path}: {e}",
            "message": f"Could not read reflector prompt: {e}",
        }

    if shutil.which(claude_bin) is None and not Path(claude_bin).exists():
        return {
            "status": "failed",
            "new_entries": 0,
            "by_type": {},
            "entry_count": before_count,
            "reflector_entry_offset": prior_offset,
            "error": f"{claude_bin} not found on PATH",
            "message": f"Could not find {claude_bin} on PATH.",
        }

    prompt = _build_reflector_prompt(
        prompt_template,
        current_decisions,
        other_decisions,
        existing_conflicts,
        obs_path,
    )

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
            "by_type": {},
            "entry_count": before_count,
            "reflector_entry_offset": prior_offset,
            "error": f"claude -p timed out after {timeout}s",
            "message": f"claude -p timed out after {timeout}s.",
        }
    except OSError as e:
        return {
            "status": "failed",
            "new_entries": 0,
            "by_type": {},
            "entry_count": before_count,
            "reflector_entry_offset": prior_offset,
            "error": f"claude -p failed to start: {e}",
            "message": f"Could not invoke {claude_bin}: {e}",
        }

    if proc.returncode != 0:
        return {
            "status": "failed",
            "new_entries": 0,
            "by_type": {},
            "entry_count": before_count,
            "reflector_entry_offset": prior_offset,
            "error": f"claude -p exited {proc.returncode}",
            "stderr": (proc.stderr or "").strip(),
            "message": (
                f"claude -p exited {proc.returncode}. "
                "Reflector offset not advanced; next run will retry."
            ),
        }

    after_count = _count_obs_lines(obs_path)
    new_entries = max(0, after_count - before_count)
    by_type = (
        _count_new_entries_by_type(obs_path, before_count)
        if new_entries else {}
    )
    db.update_reflector_offset(conn, session_id, after_count)
    return {
        "status": "reflected",
        "new_entries": new_entries,
        "by_type": by_type,
        "entry_count": after_count,
        "reflector_entry_offset": after_count,
        "message": (
            "Reflector found no new conflicts."
            if new_entries == 0 else
            _summary_entries_message(new_entries, by_type)
        ),
    }


def maybe_reflect_session(
    conn: sqlite3.Connection,
    session_id: str,
    *,
    threshold: int = REFLECTOR_TRIGGER_THRESHOLD,
    claude_bin: str = "claude",
    timeout: float = DEFAULT_TIMEOUT_SEC,
    prompt_path: Path | None = None,
) -> dict[str, Any]:
    """Run the reflector when enough new observation entries accumulated."""
    state = db.get_observation_state(conn, session_id)
    if state is None:
        return {
            "status": "no_state",
            "message": f"No observation state for session {session_id}.",
        }
    entry_count = int(state["entry_count"])
    reflector_offset = int(state["reflector_entry_offset"])
    pending = entry_count - reflector_offset
    if pending < threshold:
        return {
            "status": "not_due",
            "pending_entries": pending,
            "threshold": threshold,
            "entry_count": entry_count,
            "reflector_entry_offset": reflector_offset,
            "message": (
                f"Reflector waiting on {threshold - pending} more "
                "observation(s)."
            ),
        }
    return reflect_session(
        conn,
        session_id,
        claude_bin=claude_bin,
        timeout=timeout,
        prompt_path=prompt_path,
    )


# --- Watch mode -----------------------------------------------------------


def _resolve_source_path(
    conn: sqlite3.Connection,
    session_id: str,
    explicit: str | Path | None,
) -> Path | None:
    """Resolve the source JSONL path for ``session_id``.

    Same precedence as ``observe_session``: explicit override → state row
    → sessions row. Returns ``None`` if no path is known anywhere.
    Centralised here so the watch loop can detect a missing source up
    front rather than discovering it inside the first observer call.
    """
    if explicit is not None:
        return Path(explicit)
    state = db.get_observation_state(conn, session_id)
    if state is not None and state["source_path"]:
        return Path(state["source_path"])
    sess = db.get_session(conn, session_id)
    if sess is not None and sess.get("session_path"):
        return Path(sess["session_path"])
    return None


def _current_offset(
    conn: sqlite3.Connection,
    session_id: str,
    fallback: int = 0,
) -> int:
    """Return the observer's recorded byte offset for ``session_id``.

    Falls back to ``fallback`` when no state row exists yet — this happens
    on the very first iteration before observe_session has had a chance to
    seed the row.
    """
    state = db.get_observation_state(conn, session_id)
    if state is None:
        return fallback
    return int(state["source_byte_offset"])


class _PollingWaitBackend:
    """Portable wait backend: just sleep, then let the loop stat the file."""

    def __init__(
        self,
        poll_interval_sec: float,
        sleep_fn: Callable[[float], None],
    ) -> None:
        self._poll_interval_sec = poll_interval_sec
        self._sleep_fn = sleep_fn

    def wait(self) -> str:
        self._sleep_fn(self._poll_interval_sec)
        return "timeout"

    def close(self) -> None:
        return None


class _WatchdogEventHandler:
    """Track changes affecting exactly one transcript file."""

    def __init__(self, source_path: Path, changed: threading.Event) -> None:
        self._source_path = source_path.resolve()
        self._changed = changed

    def dispatch(self, event) -> None:  # pragma: no cover - exercised via watcher
        paths = [getattr(event, "src_path", None), getattr(event, "dest_path", None)]
        for raw in paths:
            if not raw:
                continue
            try:
                path = Path(raw).resolve()
            except OSError:
                continue
            if path == self._source_path:
                self._changed.set()
                return


class _FSEventsWaitBackend:
    """macOS file watcher backed by watchdog's FSEventsObserver."""

    def __init__(self, source_path: Path, poll_interval_sec: float) -> None:
        if sys.platform != "darwin":
            raise RuntimeError("FSEvents backend is only available on macOS")
        try:
            from watchdog.events import FileSystemEventHandler
            from watchdog.observers.fsevents import FSEventsObserver
        except Exception as e:  # pragma: no cover - env-dependent
            raise RuntimeError(f"watchdog fsevents unavailable: {e}") from e

        changed = threading.Event()
        base_handler = _WatchdogEventHandler(source_path, changed)

        class _Handler(FileSystemEventHandler):  # pragma: no cover - env-dependent
            def dispatch(self, event) -> None:
                base_handler.dispatch(event)

        self._source_path = source_path
        self._changed = changed
        self._poll_interval_sec = poll_interval_sec
        self._observer = FSEventsObserver()
        self._observer.schedule(_Handler(), str(source_path.parent), recursive=False)
        self._observer.start()

    def wait(self) -> str:
        if self._changed.wait(self._poll_interval_sec):
            self._changed.clear()
            return "changed"
        return "timeout"

    def close(self) -> None:
        self._observer.stop()
        self._observer.join(timeout=max(1.0, self._poll_interval_sec * 2))


def _make_wait_backend(
    source_path: Path,
    *,
    watch_backend: str,
    poll_interval_sec: float,
    sleep_fn: Callable[[float], None],
):
    """Instantiate the requested wait backend for watch mode."""
    if watch_backend == WATCH_BACKEND_POLL:
        return _PollingWaitBackend(poll_interval_sec, sleep_fn), WATCH_BACKEND_POLL
    if watch_backend == WATCH_BACKEND_FSEVENTS:
        return _FSEventsWaitBackend(source_path, poll_interval_sec), WATCH_BACKEND_FSEVENTS
    if watch_backend == WATCH_BACKEND_AUTO:
        if sys.platform == "darwin":
            try:
                return (
                    _FSEventsWaitBackend(source_path, poll_interval_sec),
                    WATCH_BACKEND_FSEVENTS,
                )
            except RuntimeError:
                pass
        return _PollingWaitBackend(poll_interval_sec, sleep_fn), WATCH_BACKEND_POLL
    raise ValueError(f"Unknown watch backend: {watch_backend}")


def watch_session(
    conn: sqlite3.Connection,
    session_id: str,
    *,
    source_path: str | Path | None = None,
    batch_threshold: int = BATCH_THRESHOLD,
    poll_interval_sec: float = WATCH_POLL_INTERVAL_SEC,
    watch_backend: str = WATCH_BACKEND_POLL,
    claude_bin: str = "claude",
    model: str = "haiku",
    timeout: float = DEFAULT_TIMEOUT_SEC,
    prompt_path: Path | None = None,
    reflector_prompt_path: Path | None = None,
    reflector_threshold: int = REFLECTOR_TRIGGER_THRESHOLD,
    should_stop: Callable[[], bool] | None = None,
    session_active_fn: Callable[[str], bool] | None = None,
    flush_threshold_on_stop: int | None = None,
    max_iterations: int | None = None,
    sleep_fn: Callable[[float], None] = time.sleep,
    on_result: Callable[[dict[str, Any]], None] | None = None,
    on_reflector_result: Callable[[dict[str, Any]], None] | None = None,
) -> dict[str, Any]:
    """Loop ``observe_session`` over ``session_id`` until told to stop.

    The on-demand path (``observe_session``) already handles the
    incremental "read from ``source_byte_offset``" logic; this function
    is the **watch-mode** layer that lives on top of it. After the
    initial catch-up extraction, it monitors the source JSONL for growth
    and re-invokes the observer whenever the file has grown past the
    current cursor — gated by ``observe_session``'s own batch-threshold
    check, so cheap "1 new turn" updates don't pay for a Haiku call.

    The default backend is still polling (used heavily by the tests), but
    sidecars can request ``watch_backend="auto"`` to prefer FSEvents on
    macOS when watchdog is available, falling back to the same polling
    contract everywhere else.

    Args:
        conn: Open DB connection with the schema applied.
        session_id: Claude Code session UUID to watch.
        source_path: Override for the source JSONL path. Resolved from
            the ``observation_state`` / ``sessions`` rows otherwise.
        batch_threshold: Forwarded to each ``observe_session`` call.
        poll_interval_sec: Seconds between size checks. Defaults to
            ``WATCH_POLL_INTERVAL_SEC`` (1.0s).
        watch_backend: ``poll`` (portable), ``fsevents`` (macOS via
            watchdog), or ``auto`` (prefer fsevents on macOS, else poll).
        claude_bin: Forwarded to ``observe_session``.
        model: Forwarded to ``observe_session``.
        timeout: Forwarded to ``observe_session``.
        prompt_path: Forwarded to ``observe_session``.
        reflector_prompt_path: Forwarded to ``reflect_session`` when the
            reflector threshold is met.
        reflector_threshold: Number of new observation entries required
            before the reflector runs.
        should_stop: Optional callable polled before every iteration.
            Returning ``True`` exits the loop with status ``stopped``.
            Background sidecars wire this to a SIGTERM-set flag.
        session_active_fn: Optional callable returning whether the
            observed Claude session is still alive. When it flips false,
            the loop performs one final flush pass then exits.
        flush_threshold_on_stop: Optional final extraction threshold used
            when stopping or when the session ends. Background sidecars
            pass ``1`` so a trailing 1-2 turns still get extracted.
        max_iterations: Optional cap on iteration count after the
            initial catch-up. Primarily for tests so they don't have to
            rely on the stop callback to terminate.
        sleep_fn: Injected sleep function. Tests pass a no-op so the
            loop runs at full speed.
        on_result: Optional callback invoked with the full result dict
            of every ``observe_session`` call (catch-up included). Lets
            sidecars stream summaries to the user without coupling this
            function to a specific output format.
        on_reflector_result: Optional callback for actual reflector
            attempts (successful or failed).

    Returns:
        A summary dict::

            {
              "status":     "stopped" | "source_gone" | "max_iterations",
              "iterations": <int>,         # loop ticks past the catch-up
              "extractions": <int>,        # observe_session "extracted" runs
              "below_threshold_runs": <int>,
              "failures":   <int>,         # observe_session "failed" runs
              "reflector_runs": <int>,
              "reflector_failures": <int>,
              "watch_backend": "poll" | "fsevents",
              "last_result": <dict | None>,      # last observe_session() return
              "last_reflector_result": <dict | None>
            }

        Never raises on expected failures — the loop swallows
        ``observe_session`` failures (so transient Haiku errors don't
        kill the watcher) and reports them via ``on_result`` and the
        ``failures`` counter. The only fatal condition is the source
        JSONL disappearing, which exits with ``status="source_gone"``.
    """
    resolved = _resolve_source_path(conn, session_id, source_path)
    if resolved is None or not resolved.exists():
        return {
            "status": "source_gone",
            "iterations": 0,
            "extractions": 0,
            "below_threshold_runs": 0,
            "failures": 0,
            "reflector_runs": 0,
            "reflector_failures": 0,
            "watch_backend": WATCH_BACKEND_POLL,
            "last_result": None,
            "last_reflector_result": None,
        }

    extractions = 0
    below_threshold_runs = 0
    failures = 0
    reflector_runs = 0
    reflector_failures = 0
    last_result: dict[str, Any] | None = None
    last_reflector_result: dict[str, Any] | None = None
    wait_backend_impl, backend_used = _make_wait_backend(
        resolved,
        watch_backend=watch_backend,
        poll_interval_sec=poll_interval_sec,
        sleep_fn=sleep_fn,
    )

    def _run_once(run_batch_threshold: int = batch_threshold) -> dict[str, Any]:
        """Invoke observe_session and tally the outcome."""
        nonlocal extractions, below_threshold_runs, failures, last_result
        result = observe_session(
            conn,
            session_id,
            source_path=resolved,
            batch_threshold=run_batch_threshold,
            claude_bin=claude_bin,
            model=model,
            timeout=timeout,
            prompt_path=prompt_path,
        )
        last_result = result
        status = result.get("status")
        if status == "extracted":
            extractions += 1
        elif status == "below_threshold":
            below_threshold_runs += 1
        elif status == "failed":
            failures += 1
        if on_result is not None:
            on_result(result)
        return result

    def _maybe_run_reflector() -> dict[str, Any]:
        nonlocal reflector_runs, reflector_failures, last_reflector_result
        result = maybe_reflect_session(
            conn,
            session_id,
            threshold=reflector_threshold,
            claude_bin=claude_bin,
            timeout=timeout,
            prompt_path=reflector_prompt_path,
        )
        last_reflector_result = result
        if result.get("status") not in {"not_due", "no_state"}:
            reflector_runs += 1
            if result.get("status") == "failed":
                reflector_failures += 1
            if on_reflector_result is not None:
                on_reflector_result(result)
        return result

    def _final_result(status: str) -> dict[str, Any]:
        return {
            "status": status,
            "iterations": iterations,
            "extractions": extractions,
            "below_threshold_runs": below_threshold_runs,
            "failures": failures,
            "reflector_runs": reflector_runs,
            "reflector_failures": reflector_failures,
            "watch_backend": backend_used,
            "last_result": last_result,
            "last_reflector_result": last_reflector_result,
        }

    def _flush_before_exit() -> None:
        if flush_threshold_on_stop is None:
            return
        _run_once(flush_threshold_on_stop)
        _maybe_run_reflector()

    # --- Catch-up: always run once so the cursor is current before we
    # start watching. observe_session() handles the "no new bytes" case
    # itself (status="up_to_date"), so this is safe even if the file is
    # already fully observed.
    try:
        _run_once()
        _maybe_run_reflector()
        try:
            last_seen_size = resolved.stat().st_size
        except FileNotFoundError:
            return _final_result("source_gone")

        iterations = 0
        while True:
            if should_stop is not None and should_stop():
                _flush_before_exit()
                return _final_result("stopped")
            if session_active_fn is not None and not session_active_fn(session_id):
                _flush_before_exit()
                return _final_result("session_ended")
            if max_iterations is not None and iterations >= max_iterations:
                return _final_result("max_iterations")

            wait_status = wait_backend_impl.wait()
            if wait_status == "source_gone":
                return _final_result("source_gone")

            # Source JSONL gone (deleted, moved): exit cleanly. Truncation
            # (file shrank but still exists) is observe_session's problem —
            # it rewinds the cursor to 0 and re-reads, so the watcher just
            # keeps going on the next growth event.
            try:
                size = resolved.stat().st_size
            except FileNotFoundError:
                return _final_result("source_gone")

            offset = _current_offset(conn, session_id)
            # Growth, truncation, or an explicit file-change event can all
            # warrant a pass. observe_session itself decides whether the new
            # bytes clear the threshold and whether truncation requires a
            # rewind, so the watcher only needs to detect "something changed".
            file_changed = size != last_seen_size or wait_status == "changed"
            if file_changed:
                _run_once()
                _maybe_run_reflector()
            last_seen_size = size

            iterations += 1
    finally:
        wait_backend_impl.close()


def observe_sidecar(
    conn: sqlite3.Connection,
    session_id: str,
    *,
    source_path: str | Path | None = None,
    batch_threshold: int = BATCH_THRESHOLD,
    poll_interval_sec: float = WATCH_POLL_INTERVAL_SEC,
    watch_backend: str = WATCH_BACKEND_AUTO,
    claude_bin: str = "claude",
    model: str = "haiku",
    timeout: float = DEFAULT_TIMEOUT_SEC,
    prompt_path: Path | None = None,
    reflector_prompt_path: Path | None = None,
    reflector_threshold: int = REFLECTOR_TRIGGER_THRESHOLD,
    session_active_fn: Callable[[str], bool] | None = None,
    should_stop: Callable[[], bool] | None = None,
    sleep_fn: Callable[[float], None] = time.sleep,
    on_result: Callable[[dict[str, Any]], None] | None = None,
    on_reflector_result: Callable[[dict[str, Any]], None] | None = None,
) -> dict[str, Any]:
    """Run the resident observer sidecar for one session.

    Owns PID registration, SIGTERM handling, final-flush semantics, and the
    reflector lifecycle. The process stays alive until the session ends, the
    source transcript disappears, or SIGTERM/``should_stop`` requests exit.
    """
    state = _ensure_observation_state_row(conn, session_id, source_path)
    if state is None:
        return {
            "status": "no_source",
            "observer_pid": None,
            "message": f"No source JSONL known for session {session_id}.",
        }

    existing_pid = (
        int(state["observer_pid"])
        if state.get("observer_pid") is not None
        else None
    )
    current_pid = os.getpid()
    if existing_pid and existing_pid != current_pid:
        if _pid_is_alive(existing_pid):
            return {
                "status": "already_running",
                "observer_pid": existing_pid,
                "message": (
                    f"Observer already running for session {session_id} "
                    f"(pid {existing_pid})."
                ),
            }
        db.set_observer_stopped(conn, session_id)

    stop_event = threading.Event()

    def _signal_stop(_signum, _frame) -> None:
        stop_event.set()

    def _combined_should_stop() -> bool:
        if stop_event.is_set():
            return True
        if should_stop is not None and should_stop():
            return True
        return False

    old_sigterm = signal.getsignal(signal.SIGTERM)
    old_sigint = signal.getsignal(signal.SIGINT)
    signal.signal(signal.SIGTERM, _signal_stop)
    signal.signal(signal.SIGINT, _signal_stop)

    db.set_observer_running(conn, session_id, current_pid, time.time())
    try:
        result = watch_session(
            conn,
            session_id,
            source_path=source_path,
            batch_threshold=batch_threshold,
            poll_interval_sec=poll_interval_sec,
            watch_backend=watch_backend,
            claude_bin=claude_bin,
            model=model,
            timeout=timeout,
            prompt_path=prompt_path,
            reflector_prompt_path=reflector_prompt_path,
            reflector_threshold=reflector_threshold,
            session_active_fn=session_active_fn,
            should_stop=_combined_should_stop,
            flush_threshold_on_stop=1,
            sleep_fn=sleep_fn,
            on_result=on_result,
            on_reflector_result=on_reflector_result,
        )
        result["observer_pid"] = current_pid
        return result
    finally:
        signal.signal(signal.SIGTERM, old_sigterm)
        signal.signal(signal.SIGINT, old_sigint)
        db.set_observer_stopped(conn, session_id)
