"""``threadhop copy`` — copy a session transcript to the clipboard."""

from __future__ import annotations

import sys

from ...session.detection import CLAUDE_PROJECTS, find_session_path
from ..helpers import _resolve_cli_session


def cmd_copy(args) -> int:
    """Copy the cleaned last-N-turns (or full session) to the clipboard.

    Auto-detects the current session via ``_resolve_cli_session`` so
    both plugin invocations (``/threadhop:copy`` → ``!threadhop copy``)
    and bare CLI calls from inside a ``claude`` terminal work without
    ``--session``. Delegates to ``copier.run_copy`` for the actual work;
    this wrapper only handles session discovery + path resolution.
    """
    rc = _resolve_cli_session(args)
    if rc != 0:
        return rc
    session_path = find_session_path(args.session)
    if session_path is None:
        print(
            f"threadhop copy: no transcript found for session {args.session} "
            f"under {CLAUDE_PROJECTS}.",
            file=sys.stderr,
        )
        return 1
    # Lazy import — keeps CLI startup fast for other subcommands.
    # Module is named ``copier`` to avoid shadowing stdlib ``copy``.
    from ...copier import run_copy  # noqa: PLC0415
    return run_copy(args.count, args.session, session_path)
