# Origin & Attribution

ThreadHop began as a macOS port of [claude-sessions](https://github.com/thomasrice/claude-sessions)
by [Thomas Rice](https://www.thomasrice.com/) ([@thomasrice_au](https://x.com/thomasrice_au)),
originally built for [Omarchy](https://omarchy.org) / Hyprland on Linux.

## What the original provided

A Textual TUI for viewing Claude Code session transcripts on Hyprland/Linux:
- Session list with active detection via `hyprctl` (Hyprland window manager)
- Transcript viewer with basic message rendering
- Reply via `wtype` (Wayland keyboard simulator)
- Session tracking hook using `/proc` filesystem

## What changed in the macOS fork

The initial fork by [@parzival1l](https://github.com/parzival1l) replaced all
Linux/Hyprland dependencies with portable alternatives:

- **Replaced `hyprctl`** with `ps`/`lsof` process scanning for active session detection
- **Replaced `wtype`** with `claude -p --resume <id>` for message sending
- **Removed `/proc` dependency** — macOS uses `ps`/`lsof` instead of the PID-tracking hook
- **Added system-reminder stripping** — filters `<system-reminder>` tags from transcript display
- **Rewrote transcript rendering** — full scrollable conversation with distinct User/Assistant/Tool widgets
- **Added session titles** — displays AI-generated or custom titles from JSONL metadata
- **Added day-scale age display** — shows `Xd` for sessions older than 24 hours
- **Increased session limit** — raised from 20 to 50

## Why ThreadHop is a separate repository

The fork diverged significantly from the original:

- **~1,000 lines rewritten** out of a ~1,345-line original — effectively a full rewrite
- **Scope expanded** from transcript viewing to cross-session context management (search, memory, handoff, session tagging)
- **Target platform changed** from Hyprland/Linux to macOS-first
- **Architecture expanding** from single-file viewer to SQLite-backed system with a skill plugin

To avoid noisy PR diffs on the upstream and to reflect the different direction,
ThreadHop was established as an independent repository with full attribution to the original work.

## License

ThreadHop inherits the MIT license from the original claude-sessions project.
