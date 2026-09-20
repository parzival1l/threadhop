# Origin & Attribution

ThreadHop started in early 2026 as a macOS port of
[claude-sessions](https://github.com/thomasrice/claude-sessions) by
[Thomas Rice](https://www.thomasrice.com/)
([@thomasrice_au](https://x.com/thomasrice_au)) — a ~1,345-line single-file
Textual TUI for viewing Claude Code transcripts on
[Omarchy](https://omarchy.org) / Hyprland. The first fork replaced the
Linux-specific bits (`hyprctl`, `wtype`, `/proc`) with `ps`/`lsof` and
`claude -p --resume` so it would run on macOS.

ThreadHop has since grown well past that starting point and is maintained as
an independent project. This document exists to credit the original work and
to explain how the two relate today.

## What ThreadHop is now

A cross-session context manager for Claude Code — not just a transcript
viewer. The tree is ~12k lines across ten modules, a CLI with its own
subcommands, a Claude Code skill plugin, and SQLite-backed persistence.

Capabilities that did not exist in the original:

- **SQLite + FTS5 backend** (`db.py`, `indexer.py`, `search_queries.py`) —
  full-text search across every indexed session, assistant streaming chunks
  merged by `message.id`, Pydantic validation mirrored by SQL CHECK
  constraints (ADR-003, ADR-004, task #24).
- **Observer pipeline** (`observer.py`, `prompts/observer.md`) — a sidecar
  that replays the same cleaned transcript the TUI shows through
  `claude -p --model haiku` to extract TODOs, decisions, and observations
  into per-session JSONL. Supports poll and fsevents watch backends and
  resumes from a persisted byte offset (ADR-018, ADR-019).
- **Reflector** (`reflector.py`, `prompts/reflector.md`) — compares decisions
  across sibling sessions in the same project and appends `type: "conflict"`
  rows back into the observation JSONL (ADR-020). `conflicts --resolved`
  writes review state into the `conflict_reviews` table instead of mutating
  the append-only file.
- **CLI surface** (`cli_queries.py`, `observation_queries.py`) —
  `threadhop tag / todos / decisions / observations / conflicts / observe`
  with auto-detection of the current session via the parent process tree,
  designed to be invoked from inside a Claude Code chat via `!`
  passthrough.
- **Session tagging, bookmarks, project memory, reply selection**, an
  in-TUI command registry with a help overlay, message-range selection,
  day-scale age display, AI-generated session titles.
- **Handoff skill plugin** (`handoff.py`, `skills/handoff`,
  `prompts/handoff.md`) — `/threadhop:handoff <session_id>` compresses a
  session into a brief on top of the observer/reflector output.
- **Migration tooling** (`migration.py`) — one-time, idempotent,
  transactional move of session metadata out of `config.json` into SQLite,
  preserving unknown keys (ADR-001).

The original's scope was a single-file viewer on one window manager;
ThreadHop's scope is persistent, queryable, cross-session memory with a
pluggable extraction layer. The only shared surface area is the idea of a
Textual TUI listing `~/.claude/projects/**/*.jsonl` and rendering one of
them as a conversation.

## What remains from claude-sessions

Small but worth naming:

- The basic TUI shape — a two-column layout with a session list on the left
  and a transcript on the right — came from the original.
- The MIT license is inherited from the original project.
- A handful of early keybindings (`j`/`k`, `q`, theme cycling) survived the
  rewrite because they are the obvious choices for a Textual app.

Everything else — widget hierarchy, data model, persistence, discovery
pipeline, CLI, observer/reflector/handoff — is ThreadHop code.

## Why a separate repository

- Scope is different: transcript viewer vs. cross-session context manager.
- Target platform is different: Hyprland/Linux vs. macOS-first (uses `ps`
  and `lsof` for active-session detection; the `hooks/` directory in the
  original was a `/proc`-based Linux artifact and is not used here).
- Architecture is different: single-file script vs. ten-module package
  backed by SQLite, FTS5, and a skill plugin.
- Upstream PRs at this scale would be noisy and unwelcome, and the
  projects are aimed at different users.

Independent repository, full attribution, shared license.

## License

MIT, inherited from [claude-sessions](https://github.com/thomasrice/claude-sessions).
See `LICENSE` for the full text.
