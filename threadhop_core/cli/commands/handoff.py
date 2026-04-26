"""``threadhop handoff`` — produce a handoff brief for another session."""

from __future__ import annotations

import sys

from ... import handoff as handoff_mod
from ...observation import reflector
from ...session.detection import find_session_path
from ...storage import db
from ..bootstrap import cli_bootstrap


def cmd_handoff(args) -> int:
    """Produce a handoff brief for ``args.session_id`` and print it.

    The ``/threadhop:handoff`` skill shells out to this subcommand. It
    parses ``session_id`` positionally and ``--full`` / ``--no-reflect``
    as optional flags. Unlike ``cmd_observe``, this handler does NOT
    auto-detect the current terminal's session — handoff is always
    about a *different* session than the one the skill is invoked
    from, so the caller must name it explicitly.

    Seeds the sessions row when the transcript is discoverable under
    ``~/.claude/projects`` but the TUI hasn't indexed it yet. If the
    session is unknown to both the filesystem scan AND the DB,
    ``build_handoff`` still falls back to reading any existing
    observation file via the ``observation_state`` row.

    Brief goes to stdout (so the skill can inject it as markdown).
    Status / fallback lines go to stderr so stdout stays pure markdown.
    """
    session_path = find_session_path(args.session_id)
    with cli_bootstrap() as ctx:
        if session_path is not None:
            db.upsert_session(
                ctx.conn, args.session_id, str(session_path),
                project=session_path.parent.name,
            )
        reflect_fn = None if args.no_reflect else reflector.reflect_session
        result = handoff_mod.build_handoff(
            ctx.conn, args.session_id,
            full=args.full,
            reflect_fn=reflect_fn,
        )

    status = result.get("status")
    brief = result.get("brief", "")

    if status == "no_source":
        print(
            f"threadhop handoff: {result.get('message', 'no source JSONL')}",
            file=sys.stderr,
        )
        return 1

    if status == "no_observations":
        # Expected outcome for short/routine sessions. Exit 0 so the
        # skill doesn't treat this as an error; message goes to stderr
        # so stdout stays empty and the skill can cleanly surface
        # "nothing to hand off".
        print(
            f"threadhop handoff: {result.get('message', 'no observations')}",
            file=sys.stderr,
        )
        return 0

    # ok path — brief is markdown, emit to stdout for the skill to inject.
    if brief:
        sys.stdout.write(brief if brief.endswith("\n") else brief + "\n")

    msg = result.get("message")
    if msg:
        print(f"threadhop handoff: {msg}", file=sys.stderr)
    if result.get("reflector_error"):
        print(
            f"  reflector:     {result['reflector_error']} (ignored)",
            file=sys.stderr,
        )
    stderr = result.get("polish_stderr")
    if stderr:
        print(f"  polish stderr: {stderr}", file=sys.stderr)
    return 0
