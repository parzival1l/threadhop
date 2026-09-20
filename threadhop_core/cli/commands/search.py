"""``threadhop search`` — FTS5 search across every indexed session.

Reuses the exact query layer the TUI search panel uses
(``storage.search_queries.search_messages``: porter-prefix FTS with a
trigram fuzzy fallback). Before querying, runs the *incremental* indexer
over every transcript so CLI results are fresh even when the TUI isn't
running — incremental only, so unchanged files cost one ``stat`` each.
"""

from __future__ import annotations

import json
import sys

from ... import indexer
from ...session import detection
from ...storage.search_queries import (
    FTS_MATCH_END,
    FTS_MATCH_START,
    search_messages,
)
from ..bootstrap import cli_bootstrap


def _refresh_index(conn) -> None:
    """Incrementally index new JSONL bytes for every known transcript.

    Same per-session entry point the TUI refresh cycle uses. A failure
    on one file must not block searching the rest.
    """
    root = detection.CLAUDE_PROJECTS
    if not root.is_dir():
        return
    for jsonl in root.glob("*/*.jsonl"):
        if jsonl.name.startswith("agent-"):
            continue
        try:
            indexer.index_session_incremental(conn, jsonl.stem, jsonl)
        except Exception as e:  # noqa: BLE001 — keep sweeping
            print(f"threadhop search: index skip {jsonl.name}: {e}",
                  file=sys.stderr)


def _clean_snippet(snippet: str, *, highlight: bool) -> str:
    """Convert FTS sentinel-bracketed snippets to CLI text.

    ``highlight=True`` wraps matches in ``**``; ``False`` (the JSON
    path) drops the sentinels entirely. Newlines collapse to spaces so
    one hit stays one visual block.
    """
    start, end = ("**", "**") if highlight else ("", "")
    text = snippet.replace(FTS_MATCH_START, start).replace(FTS_MATCH_END, end)
    return " ".join(text.split())


def cmd_search(args) -> int:
    """Refresh the index incrementally, query FTS, print hits."""
    raw_query = args.query
    if args.project:
        # search_messages' own parser understands `project:` tokens —
        # reuse it instead of duplicating the filter plumbing.
        raw_query = f"{raw_query} project:{args.project}"

    with cli_bootstrap() as ctx:
        _refresh_index(ctx.conn)
        rows, used_fuzzy = search_messages(ctx.conn, raw_query, limit=args.limit)

    if args.json:
        payload = [
            {
                "session_id": row.get("session_id"),
                "session_name": (
                    row.get("custom_name")
                    or str(row.get("session_id") or "")[:8]
                ),
                "project": row.get("project"),
                "timestamp": row.get("timestamp"),
                "snippet": _clean_snippet(
                    str(row.get("snippet") or ""), highlight=False,
                ),
                "uuid": row.get("uuid"),
            }
            for row in rows
        ]
        print(json.dumps(payload, indent=2))
        return 0

    if not rows:
        print(f"No matches for {args.query!r}.")
        return 0

    if used_fuzzy:
        print("(no exact matches — showing fuzzy results)\n", file=sys.stderr)

    for row in rows:
        sid = str(row.get("session_id") or "")
        name = row.get("custom_name") or sid[:8]
        project = row.get("project") or "unknown project"
        ts = row.get("timestamp") or "?"
        snippet = _clean_snippet(str(row.get("snippet") or ""), highlight=True)
        print(f"{sid[:8]}  {name}  [{project}]  {ts}")
        print(f"  {snippet}")
        print()

    print(
        f"Tip: threadhop peek <session> --grep '{args.query}' "
        "shows full exchanges."
    )
    return 0
