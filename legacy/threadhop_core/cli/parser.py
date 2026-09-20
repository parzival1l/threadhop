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
from ..storage import db
from .helpers import _resolve_cli_session  # noqa: F401  — re-exported for legacy callers


class _SuggestingParser(argparse.ArgumentParser):
    """ArgumentParser that offers a did-you-mean hint when the user supplies
    an unknown subcommand or enum value. Example: `threadhop bookmrk` →
    \"unknown value 'bookmrk'. Did you mean 'bookmark'?\"."""

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
    # No-subcommand path keeps the original TUI flags (--project/--days/--all).
    # Subcommands route to CLI mode and share --project/--session via a parent
    # parser (ADR-011).
    parser = _SuggestingParser(
        prog="threadhop",
        description=(
            "ThreadHop — Claude Code session browser.\n"
            "  No subcommand  → launch the TUI.\n"
            "  Subcommand     → CLI mode (tag, bookmark, copy, peek, "
            "search, prepare, receive, update)."
        ),
        epilog=(
            "Examples:\n"
            "  threadhop                                  # launch the TUI\n"
            "  threadhop --project myproject --days 7     # TUI, filtered by project\n"
            "  threadhop tag in_progress                  # tag the current session\n"
            "  threadhop copy 3                           # copy the last 3 turns\n"
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

    # --- Lazy borrow surface (ADR-029): peek / search / prepare / receive ---

    peek_p = subparsers.add_parser(
        "peek",
        help="Print cleaned exchanges from another session (zero LLM)",
        description=(
            "Print cleaned verbatim messages from another Claude Code "
            "session — zero LLM, zero writes. The unit is the exchange "
            "(ADR-030): one real user turn plus all assistant activity "
            "until the next real user turn. Tool calls, tool results, "
            "sidechains, and system reminders are stripped; only user "
            "and assistant prose survives. <session> is a full session "
            "id or any unique prefix."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop peek 41f3                         # last 5 exchanges\n"
            "  threadhop peek 41f3 --last 10\n"
            "  threadhop peek 41f3 --range 3:7             # 1-based inclusive\n"
            "  threadhop peek 41f3 --grep 'retry.*backoff' # matching exchanges, in full\n"
            "  !threadhop peek 41f3 --last 3    # from inside a Claude Code session"
        ),
    )
    peek_p.add_argument(
        "session_ref",
        metavar="<session>",
        help="Session id or unique prefix to read from",
    )
    peek_window = peek_p.add_mutually_exclusive_group()
    peek_window.add_argument(
        "--last",
        type=int,
        default=None,
        metavar="N",
        help="Show the last N exchanges (default: 5)",
    )
    peek_window.add_argument(
        "--range",
        type=str,
        default=None,
        metavar="A:B",
        help="Show exchanges A through B (1-based, inclusive)",
    )
    peek_window.add_argument(
        "--grep",
        type=str,
        default=None,
        metavar="PATTERN",
        help=(
            "Case-insensitive regex; prints every matching exchange in "
            "full (the window is exchange-bounded)"
        ),
    )

    search_p = subparsers.add_parser(
        "search",
        help="Full-text search across every indexed session",
        description=(
            "Search all sessions via the same FTS5 index the TUI search "
            "panel uses (porter-stemmed prefix match, trigram fuzzy "
            "fallback on zero hits). Runs a fast incremental index "
            "refresh first so results include messages written since "
            "the TUI last ran."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop search 'retry backoff'\n"
            "  threadhop search migration --project threadhop --limit 5\n"
            "  threadhop search sqlite --json"
        ),
    )
    search_p.add_argument(
        "query",
        metavar="<query>",
        help="Search terms (stemmed prefix match, AND-combined)",
    )
    search_p.add_argument(
        "--project",
        type=str,
        default=None,
        help="Filter by project (substring match on directory name)",
    )
    search_p.add_argument(
        "--limit",
        type=int,
        default=20,
        metavar="N",
        help="Maximum hits to print (default: 20)",
    )
    search_p.add_argument(
        "--json",
        action="store_true",
        default=False,
        help="Emit results as a JSON list instead of text blocks",
    )

    prepare_p = subparsers.add_parser(
        "prepare",
        parents=[shared],
        help="Freeze this session into a transfer ticket (one Haiku call)",
        description=(
            "Build a frozen transfer ticket for continuing this "
            "session's work in another chat. At most one `claude -p` "
            "call summarizes the conversation head (cached per session "
            "— an unchanged head reuses the previous summary with zero "
            "LLM calls); the last N exchanges are kept verbatim, capped "
            "by --tail-budget. Prints the ticket path plus a paste-ready "
            "`!threadhop receive tk_<id>` line."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop prepare                    # auto-detect current session\n"
            "  threadhop prepare --session 41f3a2b8-...\n"
            "  threadhop prepare --tail 5 --tail-budget 12000\n"
            "  !threadhop prepare        # from inside a Claude Code session"
        ),
    )
    prepare_p.add_argument(
        "--tail",
        type=int,
        default=3,
        metavar="N",
        help="Exchanges to carry verbatim (default: 3)",
    )
    prepare_p.add_argument(
        "--tail-budget",
        type=int,
        default=8000,
        metavar="CHARS",
        help=(
            "Character cap for the verbatim tail; oldest tail exchanges "
            "are dropped first (default: 8000)"
        ),
    )
    prepare_p.add_argument(
        "--model",
        type=str,
        default="haiku",
        metavar="M",
        help="Model passed to `claude -p` for the head summary (default: haiku)",
    )

    receive_p = subparsers.add_parser(
        "receive",
        help="Print a transfer ticket verbatim (zero LLM)",
        description=(
            "Print a ThreadHop transfer ticket to stdout so this chat "
            "can pick up where another session left off — zero LLM, "
            "zero DB. Accepts the ticket id printed by `threadhop "
            "prepare` (tk_xxxxxxxx or bare xxxxxxxx) or a file path."
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=(
            "Examples:\n"
            "  threadhop receive tk_ab12cd34\n"
            "  threadhop receive ab12cd34\n"
            "  !threadhop receive tk_ab12cd34   # in the target chat"
        ),
    )
    receive_p.add_argument(
        "ticket",
        metavar="<ticket-id>",
        help="Ticket id (tk_xxxxxxxx / xxxxxxxx) or path to a ticket file",
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
