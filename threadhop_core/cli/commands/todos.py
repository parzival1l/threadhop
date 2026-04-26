"""``threadhop todos`` — list TODO observations as compact JSONL."""

from __future__ import annotations

import sys

from ...observation import queries as observation_queries
from ..bootstrap import cli_bootstrap


def cmd_todos(args) -> int:
    """List open TODO observations as compact JSONL."""
    with cli_bootstrap() as ctx:
        results = observation_queries.refresh_unprocessed_observations(
            ctx.conn,
            project=args.project,
            session_id=args.session,
        )
        for result in results:
            if result.get("status") == "failed":
                print(
                    "threadhop todos: observer failed for session "
                    f"{result['session_id']}: {result.get('message', 'unknown error')}",
                    file=sys.stderr,
                )
        entries = observation_queries.list_observation_entries(
            ctx.conn,
            observation_type="todo",
            project=args.project,
            session_id=args.session,
        )

    for entry in entries:
        print(observation_queries.format_entry_json(entry))
    return 0
