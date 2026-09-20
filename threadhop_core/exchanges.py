"""Exchange model — the shared conversation unit for peek/prepare (ADR-030).

An *exchange* is one real user turn (a human prompt — NOT a tool-result
row, which is ``type=user`` in the JSONL but carries tool output) plus
all assistant activity until the next real user turn. Exchanges are
derived at parse time from the session JSONL; nothing is persisted.

Cleaning matches the ``threadhop copy`` pipeline exactly (the paste
recipient sees what the copy recipient sees):

* ``indexer._extract_user_text`` — skips ``toolUseResult`` user lines,
  strips ``<system-reminder>`` / ``<local-command-*>`` / ``<command-*>``
  blocks, drops skill-load banners.
* ``indexer._extract_assistant_blocks(include_tool_calls=False)`` —
  drops ``tool_use`` and ``thinking`` blocks entirely; consecutive
  assistant lines sharing ``message.id`` merge into one logical turn.
* Sidechain rows are dropped.
* ``HARNESS_TAG_RE`` strips ``!cmd`` bash-passthrough wrappers.

This module additionally tracks the **byte offsets** of each exchange in
the source JSONL so ``threadhop prepare`` can cache its head summary
against a stable position (ADR-033) without a second parse.

The regex ``HARNESS_TAG_RE`` used to live in ``copier.py``; it moved
here so both ``copy`` and the exchange pipeline share one definition
(``copier`` re-imports it for backward compatibility).
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterator

from . import indexer

# Claude Code surfaces several harness-tooling wrappers as plain-text
# content inside user JSONL turns: ``!cmd`` passthroughs wrap their input
# and captured stdout/stderr in ``<bash-input>`` / ``<bash-stdout>`` /
# ``<bash-stderr>``; each of those chunks is also prefixed with a
# ``<local-command-caveat>`` telling the assistant not to respond; and
# slash-command invocations emit ``<command-name>`` / ``<command-message>``
# / ``<command-args>``. None of this is conversation — strip it so a
# rendered exchange reads like a chat, not a shell transcript.
HARNESS_TAG_RE = re.compile(
    r"<(bash-input|bash-stdout|bash-stderr"
    r"|local-command-caveat"
    r"|command-name|command-message|command-args)>"
    r".*?"
    r"</\1>",
    re.DOTALL,
)


# --- Cleaned row iteration -------------------------------------------------


def iter_clean_rows(session_path: Path) -> Iterator[dict]:
    """Yield cleaned, human-visible conversation rows in file order.

    Each row is a dict::

        {"role": "user"|"assistant", "text": str, "timestamp": str|None,
         "start_offset": int, "end_offset": int}

    ``start_offset`` is the byte offset of the first JSONL line
    contributing to the row; ``end_offset`` is one past the final byte
    (including the trailing newline) of the last contributing line —
    for merged assistant chunks that spans every chunk line.

    Reuses the indexer's extraction helpers so the cleaning is identical
    to what the FTS index and ``threadhop copy`` see, with sidechains
    dropped and ``HARNESS_TAG_RE`` wrappers stripped on top (matching
    ``copier._iter_rendered_turns``). Rows that reduce to empty text
    after cleaning are dropped. Malformed JSON lines are silently
    skipped.
    """
    try:
        fh = open(session_path, "rb")
    except OSError:
        return

    # In-flight assistant chunk-merge buffer (ADR-003 semantics).
    buf_mid: str | None = None
    buf_parts: list[str] = []
    buf_row: dict | None = None

    def _flush() -> dict | None:
        nonlocal buf_mid, buf_parts, buf_row
        if buf_row is None:
            buf_mid = None
            buf_parts = []
            return None
        row = buf_row
        text = "\n\n".join(p for p in buf_parts if p).strip()
        text = HARNESS_TAG_RE.sub("", text).strip()
        row["text"] = text
        buf_mid = None
        buf_parts = []
        buf_row = None
        return row if row["text"] else None

    offset = 0
    with fh as f:
        for raw in f:
            line_start = offset
            offset += len(raw)
            line_end = offset
            try:
                msg = json.loads(raw.decode("utf-8", errors="replace"))
            except (json.JSONDecodeError, ValueError):
                continue
            if not isinstance(msg, dict):
                continue

            mtype = msg.get("type")
            if mtype not in ("user", "assistant"):
                continue
            if msg.get("isSidechain"):
                continue

            if mtype == "user":
                flushed = _flush()
                if flushed:
                    yield flushed
                # _extract_user_text returns None for tool-result rows
                # (``toolUseResult``) and rows that clean to nothing.
                text = indexer._extract_user_text(msg)
                if not text:
                    continue
                text = HARNESS_TAG_RE.sub("", text).strip()
                if not text:
                    continue
                yield {
                    "role": "user",
                    "text": text,
                    "timestamp": msg.get("timestamp"),
                    "start_offset": line_start,
                    "end_offset": line_end,
                }
                continue

            # --- assistant line ---
            mid = msg.get("message", {}).get("id")
            parts = indexer._extract_assistant_blocks(
                msg, include_tool_calls=False,
            )

            # Streaming chunk of the current logical message → append.
            if mid is not None and buf_mid == mid and buf_row is not None:
                buf_parts.extend(parts)
                buf_row["end_offset"] = line_end
                continue

            flushed = _flush()
            if flushed:
                yield flushed

            buf_mid = mid
            buf_parts = list(parts)
            buf_row = {
                "role": "assistant",
                "text": "",  # populated by _flush
                "timestamp": msg.get("timestamp"),
                "start_offset": line_start,
                "end_offset": line_end,
            }

        flushed = _flush()
        if flushed:
            yield flushed


# --- Exchange grouping -----------------------------------------------------


@dataclass
class Exchange:
    """One user turn plus everything until the next user turn.

    ``user_text`` is ``None`` for a leading exchange in transcripts that
    open with assistant output (rare, but resumed sessions can). Offsets
    reference the source JSONL bytes — see :func:`iter_clean_rows`.
    """

    user_text: str | None
    assistant_texts: list[str] = field(default_factory=list)
    first_timestamp: str | None = None
    last_timestamp: str | None = None
    start_offset: int = 0
    end_offset: int = 0

    def render(self) -> str:
        """Render as ``User:`` / ``Assistant:`` labelled blocks."""
        blocks: list[str] = []
        if self.user_text:
            blocks.append(f"User:\n{self.user_text}")
        for text in self.assistant_texts:
            blocks.append(f"Assistant:\n{text}")
        return "\n\n".join(blocks)

    @property
    def text(self) -> str:
        """Plain concatenated text — the grep target for ``peek --grep``."""
        parts = []
        if self.user_text:
            parts.append(self.user_text)
        parts.extend(self.assistant_texts)
        return "\n\n".join(parts)


def load_exchanges(session_path: Path) -> list[Exchange]:
    """Parse a session JSONL into its ordered list of exchanges.

    A new exchange starts at every real user turn (tool-result rows were
    already dropped by :func:`iter_clean_rows`, so they never split an
    exchange). Assistant rows before the first user turn group into a
    leading exchange with ``user_text=None``.
    """
    exchanges: list[Exchange] = []
    current: Exchange | None = None

    for row in iter_clean_rows(session_path):
        if row["role"] == "user":
            if current is not None:
                exchanges.append(current)
            current = Exchange(
                user_text=row["text"],
                first_timestamp=row["timestamp"],
                last_timestamp=row["timestamp"],
                start_offset=row["start_offset"],
                end_offset=row["end_offset"],
            )
            continue

        if current is None:
            # Transcript opens with assistant output — leading exchange
            # with no user prompt.
            current = Exchange(
                user_text=None,
                first_timestamp=row["timestamp"],
                start_offset=row["start_offset"],
            )
        current.assistant_texts.append(row["text"])
        if row["timestamp"] is not None:
            current.last_timestamp = row["timestamp"]
        current.end_offset = max(current.end_offset, row["end_offset"])

    if current is not None:
        exchanges.append(current)
    return exchanges


def render_exchanges(exchanges: list[Exchange]) -> str:
    """Render several exchanges, blank-line separated."""
    return "\n\n".join(ex.render() for ex in exchanges)
