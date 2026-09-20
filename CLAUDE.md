# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**ThreadHop** is a Textual TUI plus CLI for browsing, searching, and carrying context across Claude Code session transcripts. macOS-only — uses `ps`/`lsof` for active session detection.

The project is expanding from a transcript viewer into a cross-session context manager with SQLite FTS search, project memory, session tagging, and a lazy borrow surface (`peek` / `search` for zero-LLM reads of other sessions, `prepare` / `receive` for one-LLM-call transfer tickets — ADR-029). See `docs/DESIGN-DECISIONS.md` for the full architecture.

## Running

```bash
# TUI
./threadhop
./threadhop --project myproject --days 7

# CLI subcommands (all accept --project / --session)
./threadhop tag <status>            # backlog|in_progress|in_review|done|archived
./threadhop bookmark [kind]         # bookmark|research against latest msg or --message <uuid>
./threadhop peek <session> [--last N] [--range A:B] [--grep PATTERN]   # verbatim exchanges, zero LLM
./threadhop search <query> [--project P] [--limit N] [--json]          # FTS5 search, zero LLM
./threadhop prepare [--session <id>] [--tail N]                        # transfer ticket, one Haiku call
./threadhop receive <ticket>                                           # print ticket verbatim, zero LLM
./threadhop copy [N|all]            # cleaned transcript to clipboard
```

No build step. The script uses `uv run --script` with PEP 723 inline metadata. Runtime deps: `textual`, `pydantic`. Tests use `pytest`.

## Architecture

ThreadHop is organized as a Python package `threadhop_core/` plus a thin
`./threadhop` executable script (PEP 723 inline metadata, runs via uv).
The script does sys.path setup and dispatches into
`threadhop_core.cli.dispatch.main`; everything else lives in the package.

### Top-level layout

| Path | Owns |
|------|------|
| `threadhop` | Executable entrypoint (~27 lines): PEP 723 metadata, sys.path setup, hands off to `threadhop_core.cli.dispatch.main` |
| `threadhop_core/` | The package — all runtime code |
| `threadhop_core/cli/dispatch.py` | Argparse tree + top-level command router |
| `threadhop_core/cli/bootstrap.py` | `cli_bootstrap()` ctx-manager — opens DB, runs migrations, lazy-loads config, yields a `CLIContext` |
| `threadhop_core/cli/commands/` | One file per CLI verb: `tag`, `bookmark`, `copy`, `peek`, `search`, `prepare`, `receive`, `update`, `changelog`, `future` |
| `threadhop_core/storage/` | SQLite schema (`db.py`), migrations, FTS search helpers, recent-search history. CHECK constraints (ADR-004) live here. |
| `threadhop_core/harness/` | `claude.py` exposing `run_claude_p()` (unified `claude -p` subprocess wrapper, ADR for harness seam below) and `prompts.py::load_prompt()` for prompt-template loading. One concrete adapter today; future `codex.py` / `gemini.py` adapters become parallel files. |
| `threadhop_core/session/` | macOS session-detection (`ps`/`lsof` process-tree walking) |
| `threadhop_core/config/loader.py` | App-level config loader; one-time migration of session metadata from `config.json` into SQLite (ADR-001) lives here, idempotent + transactional |
| `threadhop_core/config/update_check.py` | ADR-027 update-check |
| `threadhop_core/models.py` | Pydantic schemas — JSONL parsing + DB-row validation boundary; `Literal` enums mirrored by SQL CHECKs (task #24) |
| `threadhop_core/indexer.py` | JSONL transcript parser + FTS ingestion; merges assistant streaming chunks by `message.id` (ADR-003); `parse_byte_range` provides the cleaned-transcript view shared by TUI + CLI |
| `threadhop_core/copier.py` | Clipboard / export logic |
| `threadhop_core/tui/app.py` | `ClaudeSessions` App class + `run_tui()` |
| `threadhop_core/tui/widgets/` | Reusable widgets: `session_list`, `transcript`, `messages`, `find_bar`, `contextual_footer` |
| `threadhop_core/tui/screens/` | Modal screens: `search`, `bookmark`, `kanban`, `help`, `label_prompt`, `confirm` |
| `threadhop_core/tui/css/` | External Textual stylesheets, one per surface, loaded via `App.CSS_PATH` |
| `threadhop_core/tui/theme/` | OpenCode theme loader + vendored theme JSON |
| `threadhop_core/tui/keybindings.py` | `COMMAND_REGISTRY` (declarative key + scope + label registry; drives footer + help overlay) |
| `threadhop_core/tui/constants.py` | Module-level constants used across TUI surfaces |
| `threadhop_core/tui/utils.py` | Pure helpers (`render_session_label_text`, `format_age`, `commands_for_scope`) |
| `prompts/` | Bundled LLM prompt templates — loaded by `harness/prompts.py` |
| `tests/` | pytest suite — 201 tests as of this layout |

### Key Classes

| Class | Location | Role |
|-------|----------|------|
| `ClaudeSessions(App)` | `threadhop_core/tui/app.py` | Main Textual app — layout, refresh loop, screen management, command registry dispatch |
| `TranscriptView(VerticalScroll)` | `threadhop_core/tui/widgets/transcript.py` | Parses JSONL, renders `UserMessage`/`AssistantMessage`/`ToolMessage` widgets; owns selection + find state + bookmark toggle |
| `SessionListPanel(Vertical)` | `threadhop_core/tui/widgets/session_list.py` | Sidebar with one `SessionItem` per session; status icon (◐ working / ● active / ○ inactive) + name + age |
| `SearchScreen(ModalScreen)` | `threadhop_core/tui/screens/search.py` | FTS5 search across indexed messages |
| `BookmarkBrowserScreen(ModalScreen)` | `threadhop_core/tui/screens/bookmark.py` | Pinned-message browser; list → enter jumps to message in transcript (task #18) |
| `KanbanScreen(ModalScreen)` | `threadhop_core/tui/screens/kanban.py` | Status-board view of sessions |
| `CLIContext` | `threadhop_core/cli/bootstrap.py` | Carries `conn` + lazy `config` to every CLI handler |
| `HarnessResult` | `threadhop_core/harness/claude.py` | Frozen dataclass mirroring `subprocess.CompletedProcess` field shape |

### Data Flow

1. **Discovery**: `_gather_session_data()` (in `threadhop_core/tui/app.py`) runs in a background worker every 5s — scans `~/.claude/projects/**/*.jsonl`, reads first 100 lines for metadata
2. **Active detection**: `threadhop_core/session/` runs `ps -eo pid,args`, finds `claude` processes, resolves CWD via `lsof -a -d cwd -p <pid>`, matches to session IDs
3. **Display**: `_update_session_list()` diffs old/new session lists — full rebuild on change, in-place spinner updates otherwise
4. **Transcript**: `TranscriptView.load_transcript()` parses full JSONL via `threadhop_core.models.parse_transcript_line`, strips `<system-reminder>` tags, abbreviates tool calls, mounts message widgets
5. **Borrow surface (ADR-029)**: `peek` / `search` read other sessions with zero LLM calls — `peek` prints cleaned verbatim exchanges (tool results, sidechains, system-reminders stripped), `search` queries the FTS5 index. `prepare` builds a frozen transfer ticket: exactly one `claude -p` (Haiku) call summarizes the conversation head, the last N exchanges are kept verbatim, and the ticket is written to `~/.config/threadhop/transfers/tk_<id>.md`. Re-preparing reuses a cached summary via byte-offset caching (`transfer_state`). `receive` prints the ticket verbatim, zero LLM.

### Session State

- **is_active**: A `claude` process is running for this session (matched by session ID in args or CWD)
- **is_working**: Active + recently modified + has pending tool call or last message was from user

### Persistent Config

- `~/.config/threadhop/config.json` — app-level settings only (theme, sidebar width). Unknown keys are preserved by `threadhop_core/migration.py`.
- `~/.config/threadhop/sessions.db` — SQLite: sessions, messages + FTS5, bookmarks, memory, transfer_state. Migrations live in `threadhop_core/storage/db.py` and run on every `init_db()`.
- `~/.config/threadhop/transfers/tk_<id>.md` — frozen transfer tickets written by `threadhop prepare` (ADR-029).

## Styling

Textual CSS lives in `threadhop_core/tui/css/*.tcss` files, loaded via `App.CSS_PATH`. One stylesheet per surface (app, session_list, transcript, search, bookmark, kanban, help, label_prompt, confirm, contextual_footer). Grid layout: 2-column (36-char session list + fill transcript), 2-row (content + reply input). Message types use colored left borders and background tints.

## Session Detection (macOS)

- **Message sending**: Uses `claude -p --resume <id>` subprocess
- **Active detection**: `ps`/`lsof` process scanning — finds running `claude` processes and resolves their CWD
- **No hooks required**: The `hooks/` directory is a Linux artifact (uses `/proc`), not used on macOS

## JSONL Message Structure

Every message line has native fields useful for indexing:
- `uuid` — unique per JSONL line
- `parentUuid` — linked-list threading
- `sessionId`, `timestamp`, `cwd`, `isSidechain`
- Assistant messages: multiple lines share the same `message.id` (streaming chunks) — must be merged for search

## Anti-patterns

- **Don't feed `prepare`'s summarizer raw JSONL.** It must see the same cleaned transcript the TUI shows (via `threadhop_core.indexer.parse_byte_range`) — otherwise tool output, system-reminders, and thinking blocks dominate and Haiku summarizes trivia.
- **Don't add a DB enum-like column without matching `Literal`+CHECK.** `threadhop_core/models.py` and `threadhop_core/storage/db.py` enforce the same shape in two places on purpose (task #24). Drift here re-introduces the bugs the hardening was meant to prevent.
- **Don't add background LLM calls.** Every LLM call is user-intent-gated (ADR-029) — exactly one call, at `prepare`. `peek`, `search`, and `receive` must stay zero-LLM.

## In Progress

- Claude Code plugin at `plugin/` — six commands (`/threadhop:peek`, `/threadhop:prepare`, `/threadhop:receive`, `/threadhop:tag`, `/threadhop:bookmark`, `/threadhop:copy`), all under the `/threadhop:` namespace; commands only, no skills (ADR-029). Plugin is PATH-dependent on a separately-installed `threadhop` CLI (see `docs/skill-packaging.md`).
- Chat-side bookmark ingest: `!threadhop bookmark [--note "..."]` (bash passthrough) and `/threadhop:bookmark` (plugin) both target the latest indexed message in the auto-detected session. Shared primitive with the TUI — same `bookmarks` table, same normalization.
- Phase 5 release work (marketplace.json, CLI-side discoverability for `threadhop tag` no-args, interactive install verification) still open.
- See `docs/DESIGN-DECISIONS.md` for ADRs and the phase roadmap, and `docs/TASKS.md` for open tasks.
