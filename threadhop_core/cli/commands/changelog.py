"""``threadhop changelog`` — print CHANGELOG.md, paginated when on TTY (ADR-027)."""

from __future__ import annotations

import subprocess
import sys

from ...config.update_check import (
    UPDATE_FETCH_TIMEOUT_S,
    UPDATE_RAW_CHANGELOG,
    UPDATE_REPO_URL,
)
from .update import _repo_root


def cmd_changelog(args) -> int:
    """Print CHANGELOG.md, paging through ``less -R`` on TTY (ADR-027)."""
    del args  # no flags
    repo = _repo_root()
    local = repo / "CHANGELOG.md"
    content: str | None = None

    if local.exists():
        try:
            content = local.read_text()
        except OSError as e:
            print(f"threadhop changelog: {e}", file=sys.stderr)
            return 1
    else:
        import urllib.request
        try:
            with urllib.request.urlopen(
                UPDATE_RAW_CHANGELOG, timeout=UPDATE_FETCH_TIMEOUT_S,
            ) as resp:
                content = resp.read().decode("utf-8", errors="replace")
        except Exception:
            print(
                "CHANGELOG not available offline. See "
                f"{UPDATE_REPO_URL}/blob/main/CHANGELOG.md",
                file=sys.stderr,
            )
            return 1

    if sys.stdout.isatty():
        try:
            subprocess.run(["less", "-R"], input=content, text=True)
            return 0
        except FileNotFoundError:
            pass  # fall through to raw print
    print(content)
    return 0
