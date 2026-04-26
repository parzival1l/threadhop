"""ADR-027 update-check helpers.

The CLI and TUI both consult the GitHub releases API to nudge users
toward upgrades. The four gates (env, TTY, context, cache) cap traffic
to one API call per machine per 24h, even when the network is
flapping — the cache touches the file on *every attempt*, not only on
success, so a transient outage doesn't degrade into a re-fetch loop.
"""

from __future__ import annotations

import json
import os
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from .. import __version__

# GitHub-side endpoints + cache file location. Public so the CLI
# `changelog` / `update` handlers can re-use them.
UPDATE_REPO = "parzival1l/threadhop"
UPDATE_REPO_URL = f"https://github.com/{UPDATE_REPO}"
UPDATE_RELEASES_API = f"https://api.github.com/repos/{UPDATE_REPO}/releases/latest"
UPDATE_RAW_CHANGELOG = f"https://raw.githubusercontent.com/{UPDATE_REPO}/main/CHANGELOG.md"
UPDATE_CACHE_DIR = Path.home() / ".cache" / "threadhop"
UPDATE_CACHE_FILE = UPDATE_CACHE_DIR / "last_check"
UPDATE_CHECK_INTERVAL_S = 24 * 60 * 60
UPDATE_FETCH_TIMEOUT_S = 1.0


@dataclass(frozen=True)
class UpdateInfo:
    """Result of the version check — only constructed when latest > current."""

    latest: str  # already stripped of a leading `v`
    current: str


def _parse_version(v: str) -> tuple[int, ...]:
    """Parse ``v0.2.0`` / ``0.2.0`` into ``(0, 2, 0)``. Raises on bad input."""
    return tuple(int(x) for x in v.lstrip("v").split("."))


def _fetch_latest_release_tag() -> str | None:
    """GET the latest release tag, or None on any failure. 1s timeout."""
    import urllib.request  # local import keeps CLI cold-start cheap
    req = urllib.request.Request(
        UPDATE_RELEASES_API,
        headers={"Accept": "application/vnd.github+json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=UPDATE_FETCH_TIMEOUT_S) as resp:
            data = json.load(resp)
    except Exception:
        return None
    tag = data.get("tag_name")
    return tag if isinstance(tag, str) and tag else None


def _check_for_update(*, force: bool = False) -> "UpdateInfo | None":
    """Return ``UpdateInfo`` if a newer release is out; ``None`` otherwise.

    Four gates (ADR-027) — env, TTY, context, cache — stop the check
    early when ``force`` is False. ``force=True`` is reserved for
    ``threadhop update --check`` where the user explicitly asked for
    the check and we should ignore all four.
    Any failure path returns ``None`` silently; a version check must
    never break the CLI.
    """
    if not force:
        # Env / TTY / context / cache gates apply only to the ambient
        # startup check. `threadhop update --check` is an explicit user
        # request and should always hit the network, so it passes
        # `force=True` and skips all four.
        if os.environ.get("THREADHOP_NO_UPDATE_CHECK"):
            return None
        if not (sys.stdout.isatty() and sys.stderr.isatty()):
            return None
        # Local import — session/detection pulls in subprocess-based
        # process-tree walking that we want to keep off the hot path.
        from ..session.detection import _invoked_from_claude_code  # noqa: PLC0415
        if _invoked_from_claude_code():
            return None
        try:
            last = UPDATE_CACHE_FILE.stat().st_mtime
            if time.time() - last < UPDATE_CHECK_INTERVAL_S:
                return None
        except FileNotFoundError:
            pass
        except OSError:
            return None

    tag = _fetch_latest_release_tag()

    # Touch cache even on fetch failure: we're caching the attempt, not
    # the result. Prevents a network outage from re-trying GitHub on
    # every single CLI invocation for the next 24h.
    try:
        UPDATE_CACHE_DIR.mkdir(parents=True, exist_ok=True)
        UPDATE_CACHE_FILE.touch()
    except OSError:
        pass

    if not tag:
        return None
    try:
        latest_v = _parse_version(tag)
        current_v = _parse_version(__version__)
    except (ValueError, AttributeError):
        return None
    if latest_v <= current_v:
        return None
    return UpdateInfo(latest=tag.lstrip("v"), current=__version__)


def _print_cli_update_notice(info: UpdateInfo) -> None:
    """Three-line stderr nudge (ADR-027 notification shape — CLI)."""
    print(
        f"\nThreadHop {info.latest} is available (you have {info.current}).\n"
        f"  What's new:  threadhop changelog\n"
        f"  Update:      threadhop update\n",
        file=sys.stderr,
    )
