"""``threadhop tag`` — tag a session with a status."""

from __future__ import annotations

import sys

from ...session.detection import CLAUDE_PROJECTS, find_session_path
from ...storage import db
from ..bootstrap import cli_bootstrap
from ..helpers import _resolve_cli_session


def cmd_tag(args) -> int:
    """Tag the targeted session with the requested status."""
    rc = _resolve_cli_session(args)
    if rc != 0:
        return rc
    # Ensure the session row exists (CLI may run before TUI discovery).
    session_path = find_session_path(args.session)
    if session_path is None:
        print(
            f"threadhop tag: no transcript found for session {args.session} "
            f"under {CLAUDE_PROJECTS}.",
            file=sys.stderr,
        )
        return 1
    with cli_bootstrap() as ctx:
        db.upsert_session(ctx.conn, args.session, str(session_path))
        db.set_session_status(ctx.conn, args.session, args.status)
    print(f"✓ tagged {args.session[:8]} as {args.status}")
    return 0
