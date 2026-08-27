"""``threadhop receive`` — print a transfer ticket verbatim.

Zero LLM, zero DB: the target chat's Claude reads the printed markdown
via the ``!threadhop receive`` bash passthrough and picks the work up.
Accepts ``tk_xxxxxxxx``, bare ``xxxxxxxx``, or a filesystem path.
"""

from __future__ import annotations

import sys

from ... import transfers


def cmd_receive(args) -> int:
    """Locate the ticket and echo its content to stdout."""
    path = transfers.resolve_ticket_path(args.ticket)
    if path is None:
        print(
            f"Ticket not found: {args.ticket} "
            f"(looked in {transfers.TRANSFERS_DIR})",
            file=sys.stderr,
        )
        return 1
    sys.stdout.write(path.read_text(encoding="utf-8"))
    return 0
