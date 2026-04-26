"""``threadhop conflicts`` — list cross-session decision conflicts."""

from __future__ import annotations

import json

from ...observation import observer
from ...session.detection import CLAUDE_PROJECTS
from ..bootstrap import cli_bootstrap
from .. import queries as cli_queries


def cmd_conflicts(args) -> int:
    """List cross-session conflict entries as compact JSON lines."""
    with cli_bootstrap() as ctx:
        results = cli_queries.query_conflicts(
            ctx.conn,
            claude_projects_dir=CLAUDE_PROJECTS,
            project=args.project,
            session_id=args.session,
            mark_resolved=args.resolved,
            reflect_fn=observer.maybe_reflect_session,
        )

    for row in results:
        print(json.dumps(row, separators=(",", ":"), sort_keys=True))
    return 0
