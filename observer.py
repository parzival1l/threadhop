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
import shutil
import sqlite3
import subprocess
from datetime import datetime
from pathlib import Path
from typing import Any

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

# Default subprocess timeout. Haiku usually responds in seconds, but the
# first call of a session can pay an auth/warmup cost, and extremely large
# chunks (first-time catch-up on a long transcript) take longer.
DEFAULT_TIMEOUT_SEC = 180.0

# Known observation types, in display order for the summary line.
_TYPE_ORDER = ("decision", "todo", "done", "adr", "observation", "conflict")


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


# --- Main entry point -----------------------------------------------------


def observe_session(
    conn: sqlite3.Connection,
    session_id: str,
    *,
    source_path: str | Path | None = None,
    batch_threshold: int = BATCH_THRESHOLD,
    claude_bin: str = "claude",
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
