"""``threadhop peek`` — print cleaned exchanges from another session.

Zero LLM. The unit is the *exchange* (ADR-030): one real user turn plus
everything until the next real user turn. Three mutually-exclusive
windows: ``--last N`` (default 5), ``--range A:B`` (1-based inclusive),
``--grep PATTERN`` (case-insensitive regex; matching exchanges print in
full — the window is exchange-bounded, never a bare line).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

from ...exchanges import Exchange, load_exchanges
from ...session.detection import CLAUDE_PROJECTS, find_session_path
from ..bootstrap import cli_bootstrap
from ..helpers import _ensure_cli_session_row, _resolve_session_prefix


def _parse_range(raw: str) -> tuple[int, int] | None:
    """Parse ``A:B`` into 1-based inclusive ints, or None if malformed."""
    match = re.fullmatch(r"(\d+):(\d+)", raw.strip())
    if not match:
        return None
    a, b = int(match.group(1)), int(match.group(2))
    if a < 1 or b < a:
        return None
    return a, b


def _format_indices(indices: list[int]) -> str:
    """Human label for the displayed window: ``3-7`` or ``2,5,9``."""
    if not indices:
        return "-"
    contiguous = indices == list(range(indices[0], indices[-1] + 1))
    if contiguous:
        if indices[0] == indices[-1]:
            return str(indices[0])
        return f"{indices[0]}-{indices[-1]}"
    return ",".join(str(i) for i in indices)


def _print_window(
    exchanges: list[Exchange],
    indices: list[int],
    *,
    display_name: str,
    project: str | None,
) -> None:
    """Print the source header + the selected exchanges to stdout."""
    window = [exchanges[i - 1] for i in indices]
    first_ts = next(
        (ex.first_timestamp for ex in window if ex.first_timestamp), "?",
    )
    last_ts = next(
        (ex.last_timestamp for ex in reversed(window) if ex.last_timestamp),
        "?",
    )
    label = "exchange" if len(indices) == 1 else "exchanges"
    print(
        f'[From "{display_name}" — {project or "unknown project"} — '
        f"{first_ts}..{last_ts} — "
        f"{label} {_format_indices(indices)} of {len(exchanges)}]"
    )
    for ex in window:
        print()
        print(ex.render())


def cmd_peek(args) -> int:
    """Resolve the session ref, pick the window, print it. No LLM."""
    with cli_bootstrap() as ctx:
        matches = _resolve_session_prefix(ctx.conn, args.session_ref)
        if not matches:
            print(
                f"threadhop peek: no session matches {args.session_ref!r} "
                f"(searched {CLAUDE_PROJECTS} and the ThreadHop DB).",
                file=sys.stderr,
            )
            return 1
        if len(matches) > 1:
            print(
                f"threadhop peek: {args.session_ref!r} is ambiguous — "
                f"{len(matches)} sessions match:",
                file=sys.stderr,
            )
            for sid in matches:
                print(f"  {sid}", file=sys.stderr)
            return 2
        session_id = matches[0]

        row = _ensure_cli_session_row(ctx.conn, session_id)
        display_name = (row or {}).get("custom_name") or session_id[:8]
        project = (row or {}).get("project")

        session_path = find_session_path(session_id)
        if session_path is None and row and row.get("session_path"):
            candidate = Path(row["session_path"])
            session_path = candidate if candidate.is_file() else None

    if session_path is None:
        print(
            f"threadhop peek: no transcript found for session {session_id} "
            f"under {CLAUDE_PROJECTS}.",
            file=sys.stderr,
        )
        return 1

    exchanges = load_exchanges(session_path)
    total = len(exchanges)
    if total == 0:
        print(
            f"threadhop peek: session {session_id[:8]} has no user/assistant "
            "exchanges (may be empty or contain only tool output).",
            file=sys.stderr,
        )
        return 1

    # --- Window selection (the argparse group made the modes exclusive) ---
    if args.grep is not None:
        try:
            pattern = re.compile(args.grep, re.IGNORECASE)
        except re.error as e:
            print(f"threadhop peek: invalid --grep regex: {e}", file=sys.stderr)
            return 2
        indices = [
            i for i, ex in enumerate(exchanges, start=1)
            if pattern.search(ex.text)
        ]
        if not indices:
            print(
                f"threadhop peek: no exchanges match {args.grep!r} "
                f"in session {session_id[:8]} ({total} exchanges scanned).",
                file=sys.stderr,
            )
            return 1
    elif args.range is not None:
        parsed = _parse_range(args.range)
        if parsed is None:
            print(
                f"threadhop peek: invalid --range {args.range!r} "
                "(expected A:B with 1 <= A <= B, 1-based inclusive).",
                file=sys.stderr,
            )
            return 2
        a, b = parsed
        if a > total:
            print(
                f"threadhop peek: --range {args.range} is out of bounds — "
                f"session has {total} exchange(s).",
                file=sys.stderr,
            )
            return 1
        indices = list(range(a, min(b, total) + 1))
    else:
        n = args.last if args.last is not None else 5
        if n < 1:
            print("threadhop peek: --last must be >= 1.", file=sys.stderr)
            return 2
        indices = list(range(max(1, total - n + 1), total + 1))

    _print_window(
        exchanges, indices,
        display_name=display_name, project=project,
    )
    return 0
