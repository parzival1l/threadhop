"""Transfer tickets — business logic for ``prepare`` / ``receive`` (ADR-029/033).

``threadhop prepare`` freezes a session into a markdown *transfer
ticket*: one Haiku call compresses the conversation head into a
continuation brief; the last N exchanges ride along verbatim (capped by
a character budget). ``threadhop receive`` prints a ticket back out in
the target chat — zero LLM, zero DB.

Kept free of argparse so every piece is unit-testable; the thin CLI
wrappers live in ``cli/commands/prepare.py`` and ``receive.py``
(mirroring the ``copier.py`` / ``commands/copy.py`` split).

Summary caching (ADR-033) lives in the ``transfer_state`` table — see
``db.upsert_transfer_state`` for the byte-offset definition. Three
paths on ``prepare``:

* no cache row            → summarize the full head (one LLM call)
* row + new head content  → merge call: previous summary + only the
                            NEW head exchanges (still one LLM call)
* row, head unchanged     → reuse ``cached_summary``, ZERO LLM calls
"""

from __future__ import annotations

import re
import secrets
import sqlite3
import sys
import time
from pathlib import Path

from .exchanges import Exchange, render_exchanges
from .harness import claude
from .harness.prompts import load_prompt
from .storage import db

# Where tickets land. Module constant so tests can monkeypatch it
# instead of touching the real ~/.config.
TRANSFERS_DIR = Path.home() / ".config" / "threadhop" / "transfers"

# Note used in place of the LLM summary when the session has no head
# (everything fits in the verbatim tail).
TOO_SHORT_NOTE = (
    "Session too short to summarize — full conversation included verbatim."
)

TICKET_ID_RE = re.compile(r"^(tk_)?[0-9a-f]{8}$")

# Marker inserted when the final tail exchange alone exceeds the budget
# and its middle is cut out.
TRUNCATION_MARKER = "\n\n[... middle truncated to fit --tail-budget ...]\n\n"


# --- Ticket ids / paths ------------------------------------------------------


def new_ticket_id() -> str:
    """Return a fresh ``tk_<8 lowercase hex>`` ticket id."""
    return f"tk_{secrets.token_hex(4)}"


def ticket_path(ticket_id: str) -> Path:
    """On-disk path for a ticket id (``TRANSFERS_DIR/<id>.md``)."""
    return TRANSFERS_DIR / f"{ticket_id}.md"


def resolve_ticket_path(raw: str) -> Path | None:
    """Resolve a ``receive`` argument to a ticket file path.

    Accepts ``tk_xxxxxxxx``, bare ``xxxxxxxx``, or a filesystem path.
    Returns None when nothing exists at the resolved location.
    """
    raw = raw.strip()
    if TICKET_ID_RE.match(raw):
        tid = raw if raw.startswith("tk_") else f"tk_{raw}"
        candidate = ticket_path(tid)
        return candidate if candidate.is_file() else None
    # Treat anything else as a path (absolute, relative, or ~-prefixed).
    candidate = Path(raw).expanduser()
    return candidate if candidate.is_file() else None


# --- Head/tail split ----------------------------------------------------------


def split_head_tail(
    exchanges: list[Exchange],
    tail_n: int,
) -> tuple[list[Exchange], list[Exchange]]:
    """Split into (head, tail): tail = last ``tail_n`` exchanges (≥1)."""
    tail_n = max(1, tail_n)
    if len(exchanges) <= tail_n:
        return [], list(exchanges)
    return list(exchanges[:-tail_n]), list(exchanges[-tail_n:])


def fit_tail_to_budget(
    tail: list[Exchange],
    budget: int,
) -> tuple[str, int]:
    """Render the tail within ``budget`` characters.

    Drops oldest tail exchanges first; always keeps at least the final
    exchange. If that lone final exchange still exceeds the budget, its
    middle is cut out with :data:`TRUNCATION_MARKER`. Returns
    ``(rendered_text, dropped_count)``. Dropped exchanges are simply
    omitted from the ticket (they are not re-summarized — the head/tail
    boundary was fixed before budgeting).
    """
    rendered = [ex.render() for ex in tail]
    dropped = 0

    def _total() -> int:
        # Two newlines join each block when rendered together.
        return sum(len(r) for r in rendered) + 2 * (len(rendered) - 1)

    while len(rendered) > 1 and _total() > budget:
        rendered.pop(0)
        dropped += 1

    if len(rendered) == 1 and len(rendered[0]) > budget:
        text = rendered[0]
        keep = max(1, (budget - len(TRUNCATION_MARKER)) // 2)
        rendered[0] = text[:keep] + TRUNCATION_MARKER + text[-keep:]

    return "\n\n".join(rendered), dropped


# --- Head summary (one LLM call max, ADR-033 cache) ---------------------------


class SummaryError(Exception):
    """LLM summarization failed — message carries the stderr passthrough."""


def summarize_head(
    conn: sqlite3.Connection,
    session_id: str,
    head: list[Exchange],
    *,
    model: str = "haiku",
) -> str:
    """Return the head summary, spending at most one ``claude -p`` call.

    Consults the ``transfer_state`` cache: exchanges whose
    ``start_offset`` is ``>= source_byte_offset`` are new since the
    cached summary (see ``db.upsert_transfer_state`` for the boundary
    definition). Upserts the cache after any successful LLM call.

    Raises :class:`SummaryError` on LLM failure (non-zero exit, timeout,
    or missing binary) — the caller must not write a ticket then.
    """
    template = load_prompt("prepare")
    state = db.get_transfer_state(conn, session_id)

    if state and state.get("cached_summary"):
        cutoff = int(state.get("source_byte_offset") or 0)
        new_head = [ex for ex in head if ex.start_offset >= cutoff]
        if not new_head:
            # Head unchanged since the last prepare — zero LLM calls.
            print(
                "threadhop prepare: head unchanged — reusing cached summary "
                "(no LLM call).",
                file=sys.stderr,
            )
            return str(state["cached_summary"])
        prompt = (
            f"{template}\n\n"
            f"## PREVIOUS SUMMARY\n{state['cached_summary']}\n\n"
            f"## NEW MESSAGES\n{render_exchanges(new_head)}"
        )
        print(
            f"threadhop prepare: merging {len(new_head)} new exchange(s) "
            f"into the cached summary via {model}…",
            file=sys.stderr,
        )
    else:
        prompt = f"{template}\n\n## CONVERSATION\n{render_exchanges(head)}"
        print(
            f"threadhop prepare: summarizing {len(head)} exchange(s) "
            f"via {model}…",
            file=sys.stderr,
        )

    try:
        result = claude.run_claude_p(prompt, model=model)
    except Exception as e:  # TimeoutExpired / OSError / FileNotFoundError
        raise SummaryError(f"claude -p failed: {e}") from e
    if result.returncode != 0 or not result.stdout.strip():
        detail = result.stderr.strip() or f"exit code {result.returncode}"
        raise SummaryError(f"claude -p failed: {detail}")

    summary = result.stdout.strip()
    db.upsert_transfer_state(
        conn,
        session_id,
        head[-1].end_offset,
        summary,
        time.time(),
    )
    return summary


# --- Ticket rendering ----------------------------------------------------------


def build_ticket(
    ticket_id: str,
    *,
    display_name: str,
    session_id: str,
    project: str | None,
    prepared_at: str,
    summary: str,
    tail_text: str,
) -> str:
    """Assemble the ticket markdown per the ADR-029 contract."""
    return (
        f"# ThreadHop transfer ticket {ticket_id}\n"
        f"Source: {display_name} ({session_id}) — {project or 'unknown project'}"
        f" — prepared {prepared_at}\n"
        f"\n"
        f"## Context summary\n"
        f"{summary}\n"
        f"\n"
        f"## Recent conversation (verbatim)\n"
        f"{tail_text}\n"
    )


def write_ticket(ticket_id: str, content: str) -> Path:
    """Write a ticket under :data:`TRANSFERS_DIR` (created on demand)."""
    TRANSFERS_DIR.mkdir(parents=True, exist_ok=True)
    path = ticket_path(ticket_id)
    path.write_text(content, encoding="utf-8")
    return path
