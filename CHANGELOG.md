# Changelog

All notable changes to ThreadHop are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow `major.minor.patch`.

## [0.2.0] — 2026-04-22

### Added
- `threadhop update [--to <ref>] [--check]` — refresh the installed
  checkout in place. With no flags, runs `git fetch` + `git reset --hard
  origin/main`. `--to <ref>` pins to a tag, branch, or SHA for rollback.
  `--check` reports without pulling.
- `threadhop changelog` — print this file, paginated through `less -R`
  when stdout is a TTY. Falls back to fetching
  `raw.githubusercontent.com/.../main/CHANGELOG.md` on installs that
  predate the file.
- `threadhop future` — print the top five entries from `ROADMAP.md`.
- 24-hour startup version check. The first CLI command of the day and
  each TUI launch compares the installed `__version__` against the
  latest GitHub release tag; if newer, the CLI prints a three-line
  stderr nudge and the TUI raises a transient toast. Suppressed inside
  Claude Code sessions (context gate), in pipelines (TTY gate), and
  when `THREADHOP_NO_UPDATE_CHECK` is set (env gate).
- `CHANGELOG.md`, `ROADMAP.md`, `RELEASE.md` at the repo root.

### Changed
- Plugin manifests (`.claude-plugin/marketplace.json`,
  `plugin/.claude-plugin/plugin.json`) bumped to `0.2.0` to stay in
  lockstep with the CLI. See `RELEASE.md` for the release discipline.

## [0.1.0] — 2026-04-20

### Added
- Initial public release.
- Textual TUI over `~/.claude/projects/**/*.jsonl` with FTS5 search,
  bookmarks, status tags, archive toggle, message-range selection, and
  AI-generated session titles.
- CLI subcommands (`tag`, `bookmark`, `todos`, `decisions`,
  `observations`, `conflicts`, `observe`, `handoff`, `config`) with
  parent-process session auto-detection for use inside live `claude`
  chats.
- Observer + reflector sidecars that append typed observations and
  cross-session decision conflicts per session (ADR-018 – ADR-020).
- Claude Code plugin with the `/threadhop:handoff` skill plus
  `/threadhop:observe`, `/threadhop:tag`, `/threadhop:bookmark`
  commands.
- `--version` flag, did-you-mean suggestions on unknown subcommands or
  enum values, `curl | bash` installer, plugin marketplace manifest.
