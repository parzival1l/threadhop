"""Argparse tree for the ``threadhop`` CLI.

Centralised here so every subcommand handler can be tested against the
real argument shape and so the dispatcher in ``./threadhop`` stays a
~2-line ``parse_args -> dispatch`` skeleton.
"""

from __future__ import annotations

import argparse
import difflib
import re
import sys

from .. import __version__
from ..observation import observer
from ..storage import db
from .helpers import _resolve_cli_session  # noqa: F401  — re-exported for legacy callers


class _SuggestingParser(argparse.ArgumentParser):
    """ArgumentParser that offers a did-you-mean hint when the user supplies
    an unknown subcommand or enum value. Example: `threadhop obsrve` →
    \"unknown value 'obsrve'. Did you mean 'observe'?\"."""

    _INVALID_CHOICE_RE = re.compile(
        r"invalid choice: '([^']+)' \(choose from (.+?)\)"
    )

    def error(self, message):
        match = self._INVALID_CHOICE_RE.search(message)
        if match:
            bad = match.group(1)
            # argparse varies between "'a', 'b'" and "a, b" across Python
            # versions — strip quotes after the split to handle both.
            choices = [
                c.strip().strip("'\"")
                for c in match.group(2).split(",")
                if c.strip()
            ]
            suggestion = difflib.get_close_matches(bad, choices, n=1, cutoff=0.6)
            if suggestion:
                self.print_usage(sys.stderr)
                self.exit(
                    2,
                    f"{self.prog}: error: unknown value '{bad}'. "
                    f"Did you mean '{suggestion[0]}'?\n",
                )
        super().error(message)


def build_parser():
    # Lazy import so the parser module remains import-cheap.
    from ..config.loader import CLI_CONFIG_KEYS  # noqa: PLC0415

    # No-subcommand path keeps the original TUI flags (--project/--days/--all).
    # Subcommands route to CLI mode and share --project/--session via a parent
    # parser (ADR-011).
    parser = _SuggestingParser(
        prog="threadhop",
        description=(
            "ThreadHop — Claude Code session browser.\n"
            "  No subcommand  → launch the TUI.\n"
            "  Subcommand     → CLI mode (tag, bookmark, todos, decisions, observations, conflicts)."
        ),
        epilog=(
            "Examples:\n"
            "  threadhop                                  # launch the TUI\n"
            "  threadhop --project myproject --days 7     # TUI, filtered by project\n"
            "  threadhop tag in_progress                  # tag the current session\n"
            "  threadhop observe --session abc123         # start the observer sidecar\n"
            "  threadhop handoff abc123                   # produce a handoff brief\n"
            "\n"
            "Run `threadhop <command> --help` for subcommand-specific examples."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--version",
        action="version",
        version=f"threadhop {__version__}",
    )
    parser.add_argument(
        "--project",
        type=str,
        default=None,
        help="TUI: filter sessions by project (substring match on directory name)",
    )
    parser.add_argument(
        "--days",
        type=int,
        default=10,
        help="TUI: show sessions from the last N days (default: 10)",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        default=False,
        help="TUI: show all projects (ignore CWD auto-detection)",
    )

    # Shared flags for CLI subcommands. Parent parser with add_help=False so
    # each subcommand inherits --project/--session without duplicating help.
    shared = argparse.ArgumentParser(add_help=False)
    shared.add_argument(
        "--project",
        type=str,
        default=None,
        help="Filter by project (substring match on directory name)",
    )
    shared.add_argument(
        "--session",
        type=str,
        default=None,
        help="Target a specific session id (defaults to current terminal's session)",
    )

    subparsers = parser.add_subparsers(
        dest="command",
        metavar="<command>",
        title="subcommands",
    )

    tag_p = subparsers.add_parser(
        "tag",
        parents=[shared],
        help="Tag a session with a status",
        description="Tag a session with a status (backlog, in_progress, in_review, done, archived).",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop tag in_progress\n"
            "  threadhop tag in_review --session abc123\n"
            "  !threadhop tag done          # from inside a Claude Code session"
        ),
    )
    tag_p.add_argument(
        "status",
        choices=db.SESSION_STATUS_ORDER,
        help="Status value to set",
    )

    bookmark_p = subparsers.add_parser(
        "bookmark",
        parents=[shared],
        help="Bookmark the latest message in a session, or a specific message",
        description=(
            "Create or update a bookmark against a session/message target. "
            "Without --message, the target is the latest indexed message in "
            "the current Claude Code session. `kind` stays intentionally "
            "narrow for now: bookmark (general keep-for-later) or research "
            "(deferred research follow-up)."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop bookmark\n"
            "  threadhop bookmark --note \"useful retry pattern\"\n"
            "  threadhop bookmark research --note \"revisit this later\"\n"
            "  threadhop bookmark --session abc123 --message <uuid>"
        ),
    )
    bookmark_p.add_argument(
        "kind",
        nargs="?",
        default="bookmark",
        choices=db.BOOKMARK_KIND_ORDER,
        help="Built-in bookmark class (default: bookmark)",
    )
    bookmark_p.add_argument(
        "--message",
        type=str,
        default=None,
        help="Explicit message uuid inside the target session (defaults to the latest message)",
    )
    bookmark_p.add_argument(
        "--note",
        type=str,
        default=None,
        help="Optional short note to store alongside the bookmark",
    )

    copy_p = subparsers.add_parser(
        "copy",
        parents=[shared],
        help="Copy cleaned session transcript to the clipboard",
        description=(
            "Copy a session's cleaned transcript to the macOS clipboard "
            "as markdown. Tool calls, tool results, sidechains, system "
            "reminders, and assistant thinking blocks are stripped — "
            "only user and assistant prose turns survive. Backs both "
            "the CLI and the /threadhop:copy plugin command."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop copy            # last turn\n"
            "  threadhop copy 3          # last 3 turns\n"
            "  threadhop copy all        # entire session\n"
            "  !threadhop copy 2         # from inside a Claude Code session"
        ),
    )
    copy_p.add_argument(
        "count",
        nargs="?",
        default=None,
        help=(
            "Number of recent turns to copy (default: 1), or 'all' for "
            "the whole session. Counts rendered turns after filtering, "
            "not raw JSONL lines."
        ),
    )

    subparsers.add_parser(
        "todos",
        parents=[shared],
        help="List open TODOs from project memory",
        description="List open TODOs. Use --project to filter.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop todos\n"
            "  threadhop todos --project myproject"
        ),
    )

    subparsers.add_parser(
        "decisions",
        parents=[shared],
        help="List decisions from project memory",
        description="List decisions. Use --project to filter.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop decisions\n"
            "  threadhop decisions --project myproject"
        ),
    )

    subparsers.add_parser(
        "observations",
        parents=[shared],
        help="Dump observations JSONL, newest first",
        description=(
            "Dump all observation entries as raw JSON lines, newest first. "
            "Reads per-session files under ~/.config/threadhop/observations/ "
            "and uses SQLite session metadata for --project filtering."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop observations\n"
            "  threadhop observations --session abc123\n"
            "  threadhop observations --project myproject | jq '.topic'"
        ),
    )

    conflicts_p = subparsers.add_parser(
        "conflicts",
        parents=[shared],
        help="List unresolved cross-session decision conflicts",
        description=(
            "List decision conflicts detected by the reflector. "
            "Uses --project to filter and --resolved to mark the displayed "
            "conflicts as reviewed."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop conflicts\n"
            "  threadhop conflicts --project myproject\n"
            "  threadhop conflicts --resolved       # mark displayed conflicts as reviewed"
        ),
    )
    conflicts_p.add_argument(
        "--resolved",
        action="store_true",
        default=False,
        help="Mark the displayed conflicts as reviewed",
    )

    observe_p = subparsers.add_parser(
        "observe",
        parents=[shared],
        help="Run the background observer sidecar for a session",
        description=(
            "Run the observer sidecar for a session. The process performs an "
            "initial catch-up extraction, then watches the transcript for new "
            "messages, appends observations into "
            "~/.config/threadhop/observations/<id>.jsonl, and triggers the "
            "reflector when enough new observations accumulate. Use --once "
            "for the old on-demand single-pass mode."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop observe --session abc123                 # start resident sidecar\n"
            "  threadhop observe --session abc123 --once          # single extraction pass\n"
            "  threadhop observe --session abc123 --stop          # stop this session's observer\n"
            "  threadhop observe --stop-all                       # stop every running observer\n"
            "  threadhop observe --session abc123 --model sonnet  # use a stronger extractor\n"
            "  threadhop observe --session abc123 --reset         # wipe state and restart"
        ),
    )
    mode_group = observe_p.add_mutually_exclusive_group()
    mode_group.add_argument(
        "--once",
        action="store_true",
        help="Run a single observation pass and exit",
    )
    mode_group.add_argument(
        "--stop",
        action="store_true",
        help="Stop the observer for this session",
    )
    mode_group.add_argument(
        "--stop-all",
        action="store_true",
        help="Stop every running observer",
    )
    observe_p.add_argument(
        "--batch-threshold",
        type=int,
        default=observer.BATCH_THRESHOLD,
        help=(
            f"Minimum new message turns before extraction runs "
            f"(default {observer.BATCH_THRESHOLD}). Lower for demos."
        ),
    )
    observe_p.add_argument(
        "--poll-interval",
        type=float,
        default=observer.WATCH_POLL_INTERVAL_SEC,
        help=(
            "Watcher fallback poll interval in seconds "
            f"(default {observer.WATCH_POLL_INTERVAL_SEC})."
        ),
    )
    observe_p.add_argument(
        "--watch-backend",
        choices=[
            observer.WATCH_BACKEND_AUTO,
            observer.WATCH_BACKEND_POLL,
            observer.WATCH_BACKEND_FSEVENTS,
        ],
        default=observer.WATCH_BACKEND_AUTO,
        help="Watch backend for background mode (default: auto).",
    )
    observe_p.add_argument(
        "--model",
        default="haiku",
        help=(
            "Model to pass to `claude -p --model` (default: haiku). "
            "Use 'sonnet' or 'opus' to get a stronger extractor."
        ),
    )
    observe_p.add_argument(
        "--claude-bin",
        type=str,
        default="claude",
        help="Path to the claude CLI binary used for observer/reflector runs",
    )
    observe_p.add_argument(
        "--timeout",
        type=float,
        default=observer.DEFAULT_TIMEOUT_SEC,
        help=(
            "Timeout in seconds for each observer/reflector claude call "
            f"(default {observer.DEFAULT_TIMEOUT_SEC})."
        ),
    )
    observe_p.add_argument(
        "--reset",
        action="store_true",
        help=(
            "Delete the observation_state row and the on-disk observations "
            "JSONL for this session before running, so extraction restarts "
            "from byte 0 with a clean output file."
        ),
    )

    config_p = subparsers.add_parser(
        "config",
        help="Read or update app-level config values",
        description=(
            "Read or update settings stored in "
            "~/.config/threadhop/config.json. Currently the supported "
            "CLI-managed key is `observe.enabled`, a durable flag for "
            "hook-driven observer auto-start."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop config get observe.enabled\n"
            "  threadhop config set observe.enabled true\n"
            "  threadhop config set observe.enabled false"
        ),
    )
    config_subparsers = config_p.add_subparsers(
        dest="config_command",
        required=True,
    )
    config_get_p = config_subparsers.add_parser(
        "get",
        help="Print a config value",
    )
    config_get_p.add_argument(
        "key",
        choices=sorted(CLI_CONFIG_KEYS),
    )
    config_set_p = config_subparsers.add_parser(
        "set",
        help="Persist a config value",
    )
    config_set_p.add_argument(
        "key",
        choices=sorted(CLI_CONFIG_KEYS),
    )
    config_set_p.add_argument(
        "value",
        help="Value to persist (for observe.enabled: true/false).",
    )

    handoff_p = subparsers.add_parser(
        "handoff",
        help="Produce a handoff brief for a session",
        description=(
            "Run the observer (first-time or catch-up) followed by the "
            "reflector, then print a markdown handoff brief composed "
            "from the per-session observation JSONL. Short observation "
            "sets format directly; larger sets (or --full) go through "
            "a Haiku sub-agent for polish. The `/threadhop:handoff` "
            "skill shells out to this subcommand. Unlike `tag`/`observe`, "
            "handoff is always about a *different* session than the "
            "caller's — pass the session id explicitly."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop handoff abc123\n"
            "  threadhop handoff abc123 --full         # rationale + transcript excerpts\n"
            "  threadhop handoff abc123 --no-reflect   # debug: skip reflector pass\n"
            "  /threadhop:handoff abc123               # from inside a Claude Code session"
        ),
    )
    handoff_p.add_argument(
        "session_id",
        type=str,
        help="Session id to hand off (required, positional)",
    )
    handoff_p.add_argument(
        "--full",
        action="store_true",
        default=False,
        help=(
            "Produce a comprehensive handoff with rationale and "
            "verbatim transcript excerpts. Always uses the Haiku "
            "sub-agent regardless of observation count."
        ),
    )
    handoff_p.add_argument(
        "--no-reflect",
        action="store_true",
        default=False,
        help=(
            "Skip the reflector pass between observer and brief. "
            "Defaults off — ADR-022 mandates the reflector runs as a "
            "follow-up step for on-demand observer invocations. Flag "
            "is primarily for debugging; production runs should leave "
            "it disabled."
        ),
    )

    update_p = subparsers.add_parser(
        "update",
        help="Update the installed ThreadHop checkout",
        description=(
            "Refresh the installed ThreadHop checkout in place. With no "
            "flags, runs `git fetch` + `git reset --hard origin/main` "
            "inside the repo that contains the running script. "
            "`--to <ref>` pins to any git ref (tag, branch, SHA) for "
            "rollback. `--check` reports without pulling. Refuses to "
            "run against a dirty working tree unless `--force` is "
            "passed, because `reset --hard` would silently discard "
            "uncommitted work. The Claude Code plugin is updated "
            "separately via `/plugin update threadhop` — ADR-027."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop update                 # pull latest origin/main\n"
            "  threadhop update --check         # report without pulling\n"
            "  threadhop update --to v0.1.0     # pin to a tag\n"
            "  threadhop update --to 1efdcf5    # pin to a specific SHA\n"
            "  threadhop update --force         # override the dirty-tree guard"
        ),
    )
    update_p.add_argument(
        "--to",
        type=str,
        default=None,
        metavar="<ref>",
        help="Git ref to check out (tag, branch, or SHA). Defaults to origin/main.",
    )
    update_p.add_argument(
        "--check",
        action="store_true",
        default=False,
        help="Only report whether an update is available; do not pull.",
    )
    update_p.add_argument(
        "--force",
        action="store_true",
        default=False,
        help=(
            "Override the dirty-tree / non-main-branch safety guard. "
            "Only use this if you're sure you want to discard "
            "uncommitted changes in the installed checkout."
        ),
    )

    subparsers.add_parser(
        "changelog",
        help="Print the ThreadHop changelog",
        description=(
            "Print CHANGELOG.md. Paginated through `less -R` when stdout "
            "is a TTY; raw otherwise. On installs that predate the file, "
            "falls back to fetching it from GitHub (1s timeout)."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop changelog\n"
            "  threadhop changelog | head -40"
        ),
    )

    subparsers.add_parser(
        "future",
        help="Show the top 5 roadmap entries",
        description=(
            "Print the top five entries from ROADMAP.md in file order. "
            "No network call — the roadmap travels with the checkout, "
            "so `threadhop update` is what brings it forward."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop future"
        ),
    )

    return parser
