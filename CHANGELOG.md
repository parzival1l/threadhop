# Changelog

All notable changes to ThreadHop are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions
follow `major.minor.patch`.

## [0.3.1] — 2026-04-26

### Fixed
- Sidebar and Kanban board now show the same set of sessions. Previously
  with more than 50 sessions, the sidebar's global cap could slice
  later-status buckets (`in_review`, `done`, `archived`) off the bottom
  of the sorted list while the Kanban board still rendered them. The
  symptom: clicking a card on the board that wasn't in the sidebar did
  nothing — `_on_kanban_dismissed` walks the sidebar's `ListView` to
  drive selection and silently bailed when the session was missing.
- Both views now read from a single `_visible_sessions()` helper that
  applies `MAX_SESSIONS` per status bucket (so each column gets its own
  budget) and honors the `_show_archived` toggle uniformly. Manual
  reorder (`shift+j`/`shift+k`) reuses the same helper so swap-with-
  neighbor matches what's on screen.
- `_on_kanban_dismissed` now flips `_show_archived` on and rebuilds when
  an archived card is clicked from the board with the archived view
  hidden, so the navigation always lands instead of silently failing.

### Notes
- Added `pyrightconfig.json` so basedpyright/pyright can resolve
  third-party imports (`textual`, `rich`, `watchdog`, `pydantic`) from
  a local `.venv/`. Editor-only — runtime still uses PEP 723 inline
  metadata via `uv run --script`. Contributors who want red-squiggle-
  free editor support should run `uv venv && uv pip install textual
  watchdog pydantic pytest` once after cloning.

## [0.3.0] — 2026-04-26

### Added
- TUI theme system with five vendored themes — `opencode`, `nord`,
  `gruvbox`, `catppuccin`, `tokyonight`. Themes are JSON files loaded
  by `threadhop_core/tui/theme/loader.py` and registered as Textual
  themes at app startup. The default look-and-feel mirrors OpenCode's
  palette and message-surface treatment so transcripts feel at home
  next to a Claude Code terminal session.
- TUI visual updates inspired by OpenCode — refreshed message
  surfaces, role borders, status pills, sidebar density, and
  contextual-footer styling. Behavior unchanged; the diff is in the
  `*.tcss` stylesheets and message-widget rendering.

### Changed
- Slash-command rendering in the transcript no longer stalls on long
  sessions. The indexer now streams parsed messages incrementally
  instead of loading the entire JSONL file before mounting the first
  widget, so the transcript paints quickly even on multi-megabyte
  sessions.
- Session-loading spinner clears as soon as the first batch of
  messages is mounted rather than waiting for the full parse.

### Notes
- **Internal repo refactor (no user-visible behavior change).** The
  monolithic `./threadhop` script (2500+ lines) and `./tui.py`
  (5900+ lines) have been split into a proper Python package at
  `threadhop_core/` with submodules for `cli/`, `cli/commands/`,
  `storage/`, `observation/`, `harness/`, `session/`, `config/`, and
  `tui/{widgets,screens,css,theme}/`. The `./threadhop` entrypoint is
  now a ~27-line dispatcher that hands off to
  `threadhop_core.cli.dispatch.main`. `tui.py` at the repo root
  remains as a thin backward-compat shim. All 201 tests pass; CLI
  surface, DB schema, and observation pipeline are unchanged.
- New `threadhop_core/harness/claude.py` adapter unifies the three
  near-identical `claude -p` subprocess call sites (`observer`,
  `reflector`, `handoff`) behind one `run_claude_p()` entrypoint.
  Documented as ADR-028 in `docs/DESIGN-DECISIONS.md`; prepares the
  seam for future `codex.py` / `gemini.py` adapters.
- `RELEASE.md` rewritten as an agent-followable runbook with explicit
  pre-flight, version-decision, failure-mode, and recovery sections.
  CI workflows (`validate.yml`, `release.yml`) repointed at the new
  `threadhop_core/__init__.py` version-of-truth so version detection
  survives the package refactor.

## [0.2.1] — 2026-04-22

### Added
- `threadhop copy [N|all]` — copy the last N rendered turns (default
  `1`, `all` for the whole session) of the current session to the
  macOS clipboard as markdown. Sidechains, tool calls, tool results,
  thinking blocks, `<system-reminder>` tags, and Claude Code harness
  wrappers (`<bash-input>`, `<bash-stdout>`, `<bash-stderr>`,
  `<local-command-caveat>`, `<command-name>`, `<command-message>`,
  `<command-args>`) are stripped so the paste reads as conversation.
  Uses `indexer.parse_messages(include_tool_calls=False)` — the same
  cleaning pipeline the TUI renders — so the output is deterministic
  and byte-reproducible across invocations. Prints only a one-line
  `✓ copied N turns (≈W words) to clipboard.` confirmation; the
  payload is never echoed to stdout (defeats the point of reducing
  context). Falls back to dumping to
  `/tmp/threadhop/<sid>-copy-<ts>.md` and copying the *path* if
  `pbcopy` is unavailable.
- `/threadhop:copy [N|all]` plugin command — one-line bash passthrough
  into `threadhop copy`, matching the existing `/threadhop:tag` /
  `/threadhop:bookmark` pattern. Auto-detects the host session via
  the same parent-process resolver the other plugin commands use.

### Notes
- Python module is `copier.py`, not `copy.py`, to avoid losing the
  import race against stdlib `copy` (pytest and anything transitive
  through `copy.deepcopy` preload `sys.modules['copy']`). The CLI
  subcommand and plugin command are still spelled `copy`.

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
