"""``threadhop observations`` — dump per-session observation JSONL."""

from __future__ import annotations

import sys

from ..bootstrap import cli_bootstrap
from ..helpers import _load_observation_lines


def cmd_observations(args) -> int:
    """Dump raw observation JSONL lines, newest first."""
    with cli_bootstrap() as ctx:
        lines, errors = _load_observation_lines(
            ctx.conn,
            project=args.project,
            session_id=args.session,
        )

    for raw in lines:
        print(raw)
    for err in errors:
        print(f"threadhop observations: {err}", file=sys.stderr)
    return 0
