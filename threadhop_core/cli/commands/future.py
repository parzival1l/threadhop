"""``threadhop future`` — print the top 5 ROADMAP.md entries (ADR-027)."""

from __future__ import annotations

import re
import sys

from ...config.update_check import UPDATE_REPO_URL
from .update import _repo_root

ROADMAP_ENTRY_RE = re.compile(r"^- #(\d+) — (.+)$")


def cmd_future(args) -> int:
    """Print the top 5 ROADMAP.md entries (ADR-027)."""
    del args
    repo = _repo_root()
    roadmap = repo / "ROADMAP.md"
    full_url = f"{UPDATE_REPO_URL}/blob/main/ROADMAP.md"

    if not roadmap.exists():
        print(
            "Roadmap is not available on this install.\n"
            f"See {full_url}"
        )
        return 0

    entries: list[tuple[str, str]] = []
    try:
        for line in roadmap.read_text().splitlines():
            m = ROADMAP_ENTRY_RE.match(line)
            if m:
                entries.append((m.group(1), m.group(2)))
                if len(entries) >= 5:
                    break
    except OSError as e:
        print(f"threadhop future: {e}", file=sys.stderr)
        return 1

    if not entries:
        print(
            "Roadmap is empty on this install.\n"
            f"See {full_url}"
        )
        return 0

    print("ThreadHop — what's coming up:\n")
    for num, desc in entries:
        print(f"  #{num}  {desc}")
    print(f"\nFull roadmap:\n  {full_url}")
    return 0
