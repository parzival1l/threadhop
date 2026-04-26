"""``threadhop update`` — refresh the installed checkout in place (ADR-027)."""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

from ... import __version__
from ...config.update_check import (
    UPDATE_REPO,
    _check_for_update,
    _print_cli_update_notice,
)


def _repo_root() -> Path:
    """Directory the installed `threadhop` script lives in.

    Two levels up from this file: ``threadhop_core/cli/commands/update.py``
    → ``threadhop_core/cli`` → ``threadhop_core`` → repo root.
    """
    return Path(__file__).resolve().parents[3]


def _read_version_from_script(path: Path) -> str | None:
    """Read ``__version__`` from an on-disk copy of ``threadhop_core/__init__.py``.

    Used by `threadhop update` after a git reset: the running process
    still has the pre-update string cached in memory, so we re-parse
    the file on disk to report the new version back to the user.
    """
    try:
        text = path.read_text()
    except OSError:
        return None
    m = re.search(r'^__version__\s*=\s*"([^"]+)"', text, flags=re.MULTILINE)
    return m.group(1) if m else None


def _run_git(cwd: Path, *args: str) -> tuple[int, str, str]:
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True,
    )
    return result.returncode, result.stdout.strip(), result.stderr.strip()


def _dirty_tree_refusal(repo: Path) -> str | None:
    """Return a formatted refusal message if the working tree isn't safe
    to ``git reset --hard`` against, else None.

    Guards two scenarios we learned about the hard way:
      1. Uncommitted changes (tracked modifications, staged blobs) —
         ``git reset --hard`` silently discards them.
      2. Current branch differs from `main` — a reset would move the
         current branch's tip, abandoning commits that weren't on main.

    ``--force`` skips this check entirely; the caller handles that.
    """
    rc, porcelain, _ = _run_git(repo, "status", "--porcelain")
    if rc != 0:
        # If `git status` itself fails, don't block — `git reset` will
        # surface its own error and we'd rather that than a spurious
        # refusal.
        return None
    rc, branch, _ = _run_git(repo, "rev-parse", "--abbrev-ref", "HEAD")
    if rc != 0:
        branch = "(unknown)"

    problems: list[str] = []
    if porcelain.strip():
        count = sum(1 for line in porcelain.splitlines() if line.strip())
        problems.append(
            f"  {count} uncommitted change(s) in the working tree."
        )
    if branch not in ("main", "HEAD"):
        problems.append(
            f"  current branch is '{branch}', not 'main'."
        )

    if not problems:
        return None

    return (
        "threadhop update: refusing to run against a non-clean "
        "checkout:\n"
        + "\n".join(problems)
        + "\n\n"
        "A `git reset --hard` here would discard uncommitted work or "
        "abandon unmerged commits. Commit/stash first, or pass "
        "`--force` to override (you'll lose the listed state)."
    )


def cmd_update(args) -> int:
    """Refresh the installed checkout in place (ADR-027).

    Safety rail: refuses to proceed when the working tree is dirty or
    the current branch isn't `main`. Override with `--force`. (Learned
    from task-027's own debut: running the first implementation against
    the development checkout silently wiped an afternoon of
    uncommitted work.)
    """
    repo = _repo_root()
    if not (repo / ".git").exists():
        print(
            "threadhop update: not a git checkout; re-run the installer instead:\n"
            f"  curl -LsSf https://raw.githubusercontent.com/{UPDATE_REPO}/main/install.sh | bash",
            file=sys.stderr,
        )
        return 1

    if args.check:
        info = _check_for_update(force=True)
        if info is None:
            print(f"ThreadHop {__version__} is up to date.")
            return 0
        _print_cli_update_notice(info)
        return 0

    if not args.force:
        refusal = _dirty_tree_refusal(repo)
        if refusal is not None:
            print(refusal, file=sys.stderr)
            return 1

    rc, out, err = _run_git(repo, "fetch", "--tags", "origin")
    if rc != 0:
        print(
            f"threadhop update: git fetch failed:\n{err or out}",
            file=sys.stderr,
        )
        return 1

    ref = args.to or "origin/main"
    rc, _, _ = _run_git(repo, "rev-parse", "--verify", ref)
    if rc != 0:
        print(
            f"threadhop update: ref '{ref}' not found. "
            "Try `git -C <repo> fetch --tags` or pick a different --to value.",
            file=sys.stderr,
        )
        return 1

    rc, _, err = _run_git(repo, "reset", "--hard", ref)
    if rc != 0:
        print(f"threadhop update: git reset failed:\n{err}", file=sys.stderr)
        return 1

    # `threadhop_core/__init__.py` was replaced on disk while we were
    # running. mmap'd process text is safe on macOS/Linux, but we
    # intentionally do no further Python work past this point (ADR-027
    # self-replacement note) — just report and return.
    if args.to:
        print(f"Pinned to {args.to}.")
    else:
        new_version = _read_version_from_script(
            repo / "threadhop_core" / "__init__.py",
        ) or "unknown"
        print(f"Updated to {new_version}.")
    return 0
