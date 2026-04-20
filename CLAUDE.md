# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**ThreadHop** is a Textual TUI plus CLI for browsing, searching, and carrying context across Claude Code session transcripts. macOS-only — uses `ps`/`lsof` for active session detection.

The project is expanding from a transcript viewer into a cross-session context manager with SQLite FTS search, project memory, session tagging, and a skill plugin for handoff generation. See `docs/DESIGN-DECISIONS.md` for the full architecture.

## Running

```bash
# TUI
./threadhop
./threadhop --project myproject --days 7

# CLI subcommands (all accept --project / --session)
./threadhop tag <status>            # backlog|in_progress|in_review|done|archived
./threadhop todos                   # open TODOs from observations
./threadhop decisions               # decisions extracted by observer
./threadhop observations            # raw observation JSONL, newest first
./threadhop conflicts [--resolved]  # cross-session decision conflicts (reflector)
./threadhop observe [--once|--stop|--stop-all] [--watch-backend auto|poll|fsevents]
```

No build step. The script uses `uv run --script` with PEP 723 inline metadata. Runtime deps: `textual`, `pydantic`. Tests use `pytest`.

## Architecture

Core modules:
- `threadhop` — executable entry point, TUI, argparse routing, CLI handlers, command registry + help overlay
- `db.py` — SQLite schema, migrations, session / bookmark / observation-state / memory helpers, CHECK constraints (ADR-004)
- `models.py` — Pydantic validation boundary for JSONL parsing and DB-row shapes; `Literal` enums mirrored by SQL CHECKs (task #24)
- `indexer.py` — transcript normalization + FTS ingestion; merges assistant streaming chunks by `message.id` (ADR-003)
- `observer.py` — sidecar orchestrator: seek-from-byte-offset, batch-threshold gate, `claude -p --model haiku` extractor, watch-mode with poll/fsevents backends (ADR-018)
- `reflector.py` — reads observation JSONL across sessions in one project, appends `type: "conflict"` entries back into the same file (ADR-020)
- `cli_queries.py` — shared CLI helpers: session-to-project sync, observer catch-up driver for `todos`/`decisions`/`observations`/`conflicts`
- `observation_queries.py` — low-level readers for per-session observation JSONL
- `migration.py` — one-time move of session metadata from `config.json` into SQLite (ADR-001); idempotent, transactional
- `prompts/observer.md`, `prompts/reflector.md` — system prompts spliced with live context before each `claude -p` call

### Key Classes

| Class | Role |
|-------|------|
| `ClaudeSessions(App)` | Main Textual app — layout, refresh loop, keybindings, session discovery, command registry dispatch |
| `TranscriptView(VerticalScroll)` | Parses JSONL, renders conversation as `UserMessage`/`AssistantMessage`/`ToolMessage` widgets; select-mode + bookmark toggle |
| `SessionItem(ListItem)` | Renders one session row: status icon (◐ working / ● active / ○ inactive) + name + age |
| `SearchScreen(ModalScreen)` | FTS5 search across indexed messages |
| `BookmarkBrowserScreen(ModalScreen)` | Pinned-message browser; list → enter jumps to message in transcript (task #18) |

### Data Flow

1. **Discovery**: `_gather_session_data()` runs in a background worker every 5s — scans `~/.claude/projects/**/*.jsonl`, reads first 100 lines for metadata
2. **Active detection**: `_get_active_claude_sessions()` runs `ps -eo pid,args`, finds `claude` processes, resolves CWD via `lsof -a -d cwd -p <pid>`, matches to session IDs
3. **Display**: `_update_session_list()` diffs old/new session lists — full rebuild on change, in-place spinner updates otherwise
4. **Transcript**: `load_transcript()` parses full JSONL via `models.parse_transcript_line`, strips `<system-reminder>` tags, abbreviates tool calls, mounts message widgets
5. **Observer pipeline**: `observer.observe_session()` reads `observation_state.source_byte_offset`, re-uses the cleaned transcript from `indexer.parse_byte_range` (same view the TUI shows), gates on `BATCH_THRESHOLD` new turns, invokes `claude -p --model haiku --permission-mode acceptEdits` with `prompts/observer.md` — the child process appends JSONL into `~/.config/threadhop/observations/<session_id>.jsonl`. Watch-mode loops this with fsevents/poll until `--stop`.
6. **Reflector pipeline**: after enough new observations accumulate, `reflector.reflect_session()` compares the session's decisions against sibling sessions in the same project and appends `type: "conflict"` rows to the same observation JSONL.
7. **Observation CLI**: `todos` / `decisions` / `observations` / `conflicts` run observer catch-up for tracked sessions via `cli_queries`, then read the per-session JSONL. `conflicts --resolved` writes review state into the `conflict_reviews` table instead of mutating append-only JSONL.

### Session State

- **is_active**: A `claude` process is running for this session (matched by session ID in args or CWD)
- **is_working**: Active + recently modified + has pending tool call or last message was from user

### Persistent Config

- `~/.config/threadhop/config.json` — app-level settings only (theme, sidebar width). Unknown keys are preserved by `migration.py`.
- `~/.config/threadhop/sessions.db` — SQLite: sessions, messages + FTS5, bookmarks, memory, observation_state, conflict_reviews. Migrations live in `db.py` and run on every `init_db()`.
- `~/.config/threadhop/observations/<session_id>.jsonl` — per-session observation file, shared by observer and reflector (ADR-019, ADR-020).

## Styling

Textual CSS is inline in `ClaudeSessions.CSS` string. Grid layout: 2-column (36-char session list + fill transcript), 2-row (content + reply input). Message types use colored left borders and background tints.

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

- **Don't feed the observer raw JSONL.** It must see the same cleaned transcript the TUI shows (via `indexer.parse_byte_range`) — otherwise tool output, system-reminders, and thinking blocks dominate and Haiku extracts trivia.
- **Don't add a DB enum-like column without matching `Literal`+CHECK.** `models.py` and `db.py` enforce the same shape in two places on purpose (task #24). Drift here re-introduces the bugs the hardening was meant to prevent.
- **Don't run the observer under `--permission-mode default`.** The child appends to the observation file itself; `acceptEdits` is the minimum that works.

## In Progress

- Skill plugin `/threadhop:handoff <session_id>` sits on top of the observer/reflector pipeline and is still being assembled
- See `docs/DESIGN-DECISIONS.md` for ADRs and the phase roadmap, and `docs/TASKS.md` for open tasks
