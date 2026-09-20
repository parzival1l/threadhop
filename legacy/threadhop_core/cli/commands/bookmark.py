"""``threadhop bookmark`` — pin a message in a session."""

from __future__ import annotations

import json
import sqlite3
import sys

from ... import indexer
from ...session.detection import CLAUDE_PROJECTS, find_session_path
from ...storage import db
from ..bootstrap import cli_bootstrap
from ..helpers import _ensure_cli_session_row, _resolve_cli_session


def _ensure_cli_messages_indexed(
    conn: sqlite3.Connection,
    session_id: str,
) -> dict | None:
    """Seed the session row from disk and incrementally index its transcript."""
    session_path = find_session_path(session_id)
    if session_path is not None:
        db.upsert_session(
            conn,
            session_id,
            str(session_path),
            project=session_path.parent.name,
        )
    row = db.get_session(conn, session_id) or _ensure_cli_session_row(conn, session_id)
    if row is None:
        return None
    try:
        indexer.index_session_incremental(conn, session_id, row["session_path"])
    except Exception as e:  # noqa: BLE001
        raise RuntimeError(f"failed to index session {session_id}: {e}") from e
    return db.get_session(conn, session_id)


def _format_bookmark_confirmation(
    target: dict,
    bookmark: dict,
) -> str:
    """Return one deterministic success line for bookmark ingest."""
    text = " ".join((target.get("text") or "").split())
    if len(text) > 100:
        text = text[:99] + "…"
    note = bookmark.get("note")
    note_part = f" note={json.dumps(note)}" if note is not None else ""
    return (
        "✓ bookmarked "
        f"kind={bookmark['kind']} "
        f"session={target['session_id']} "
        f"message={target['uuid']} "
        f"role={target.get('role') or 'unknown'} "
        f"text={json.dumps(text)}"
        f"{note_part}"
    )


def cmd_bookmark(args) -> int:
    """Create or update a bookmark against a session/message target.

    Chat ergonomics default to the newest indexed message in the current Claude
    session. Callers may override the target with ``--session`` and
    ``--message`` when bookmarking from outside that live terminal.
    """
    rc = _resolve_cli_session(args)
    if rc != 0:
        return rc

    with cli_bootstrap() as ctx:
        try:
            session = _ensure_cli_messages_indexed(ctx.conn, args.session)
            if session is None:
                print(
                    f"threadhop bookmark: no transcript found for session {args.session} "
                    f"under {CLAUDE_PROJECTS}.",
                    file=sys.stderr,
                )
                return 1
            target = db.resolve_bookmark_target(
                ctx.conn,
                session_id=args.session,
                message_uuid=args.message,
            )
            if target is None:
                detail = (
                    f"message {args.message!r} in session {args.session}"
                    if args.message
                    else f"latest message in session {args.session}"
                )
                print(
                    f"threadhop bookmark: could not resolve {detail}.",
                    file=sys.stderr,
                )
                return 1
            kwargs = {"kind": args.kind}
            if args.note is not None:
                kwargs["note"] = args.note
            bookmark = db.upsert_bookmark(ctx.conn, target["uuid"], **kwargs)
        except RuntimeError as e:
            print(f"threadhop bookmark: {e}", file=sys.stderr)
            return 1

    print(_format_bookmark_confirmation(target, bookmark))
    return 0
