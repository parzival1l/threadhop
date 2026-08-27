"""Top-level CLI dispatch.

The ``./threadhop`` script is now a thin shebang + sys.path setup +
``main()`` shim. The actual parse/dispatch logic lives here so it's
unit-testable and importable without paying the cost of running the
script through ``uv run``.
"""

from __future__ import annotations

from .parser import build_parser


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()

    # Startup update-check (ADR-027). CLI mode only — the TUI runs its
    # own check in `ClaudeSessions.on_mount()` and surfaces the result
    # as a toast. `update` is skipped because it has its own --check
    # flag and already talks to GitHub.
    if args.command not in (None, "update"):
        from ..config.update_check import (  # noqa: PLC0415
            _check_for_update,
            _print_cli_update_notice,
        )
        info = _check_for_update()
        if info is not None:
            _print_cli_update_notice(info)

    if args.command is None:
        from threadhop_core.tui.app import run_tui  # noqa: PLC0415 — Textual import is heavy

        return run_tui(
            project=args.project,
            days=args.days,
            all_projects=args.all,
        )

    # Lazy imports — keep startup snappy for fast subcommands like
    # `tag` which don't need the TUI dependencies.
    if args.command == "tag":
        from .commands.tag import cmd_tag  # noqa: PLC0415
        return cmd_tag(args)
    if args.command == "bookmark":
        from .commands.bookmark import cmd_bookmark  # noqa: PLC0415
        return cmd_bookmark(args)
    if args.command == "copy":
        from .commands.copy import cmd_copy  # noqa: PLC0415
        return cmd_copy(args)
    if args.command == "peek":
        from .commands.peek import cmd_peek  # noqa: PLC0415
        return cmd_peek(args)
    if args.command == "search":
        from .commands.search import cmd_search  # noqa: PLC0415
        return cmd_search(args)
    if args.command == "prepare":
        from .commands.prepare import cmd_prepare  # noqa: PLC0415
        return cmd_prepare(args)
    if args.command == "receive":
        from .commands.receive import cmd_receive  # noqa: PLC0415
        return cmd_receive(args)
    if args.command == "update":
        from .commands.update import cmd_update  # noqa: PLC0415
        return cmd_update(args)
    if args.command == "changelog":
        from .commands.changelog import cmd_changelog  # noqa: PLC0415
        return cmd_changelog(args)
    if args.command == "future":
        from .commands.future import cmd_future  # noqa: PLC0415
        return cmd_future(args)

    from .helpers import _cli_stub  # noqa: PLC0415
    return _cli_stub(args.command)
