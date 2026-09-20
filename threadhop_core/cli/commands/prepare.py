"""``threadhop prepare`` — freeze this session into a transfer ticket.

One Haiku call (at most — see the ADR-033 cache in ``transfers.py``)
summarizes the conversation head; the last ``--tail N`` exchanges ride
along verbatim, capped at ``--tail-budget`` characters. On success the
ticket path and a paste-ready ``!threadhop receive tk_…`` line go to
stdout; every diagnostic goes to stderr. On LLM failure no ticket is
written and the exit code is 1.
"""

from __future__ import annotations

import sys
from datetime import datetime, timezone

from ...exchanges import load_exchanges
from ...session.detection import CLAUDE_PROJECTS, find_session_path
from ... import transfers
from ..bootstrap import cli_bootstrap
from ..helpers import _ensure_cli_session_row, _resolve_cli_session


def cmd_prepare(args) -> int:
    """Build and write the transfer ticket for the targeted session."""
    rc = _resolve_cli_session(args)
    if rc != 0:
        return rc

    session_path = find_session_path(args.session)
    if session_path is None:
        print(
            f"threadhop prepare: no transcript found for session "
            f"{args.session} under {CLAUDE_PROJECTS}.",
            file=sys.stderr,
        )
        return 1

    exchanges = load_exchanges(session_path)
    if not exchanges:
        print(
            f"threadhop prepare: session {args.session[:8]} has no "
            "user/assistant exchanges to transfer.",
            file=sys.stderr,
        )
        return 1

    head, tail = transfers.split_head_tail(exchanges, args.tail)
    tail_text, dropped = transfers.fit_tail_to_budget(tail, args.tail_budget)
    if dropped:
        print(
            f"threadhop prepare: dropped {dropped} oldest tail exchange(s) "
            f"to fit --tail-budget {args.tail_budget}.",
            file=sys.stderr,
        )

    with cli_bootstrap() as ctx:
        row = _ensure_cli_session_row(ctx.conn, args.session)
        display_name = (row or {}).get("custom_name") or args.session[:8]
        project = (row or {}).get("project")

        if not head:
            # Short session: everything already rides verbatim in the
            # tail (subject to budget) — skip the LLM call entirely.
            print(
                "threadhop prepare: session too short to summarize — "
                "skipping the LLM call.",
                file=sys.stderr,
            )
            summary = transfers.TOO_SHORT_NOTE
        else:
            try:
                summary = transfers.summarize_head(
                    ctx.conn, args.session, head, model=args.model,
                )
            except transfers.SummaryError as e:
                print(f"threadhop prepare: {e}", file=sys.stderr)
                return 1

    ticket_id = transfers.new_ticket_id()
    content = transfers.build_ticket(
        ticket_id,
        display_name=display_name,
        session_id=args.session,
        project=project,
        prepared_at=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        summary=summary,
        tail_text=tail_text,
    )
    path = transfers.write_ticket(ticket_id, content)

    print(path)
    print(f"Paste in the target chat: !threadhop receive {ticket_id}")
    print(f"  (or: threadhop receive {ticket_id})")
    return 0
