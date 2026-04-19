# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**ThreadHop** is a Textual TUI plus CLI for browsing, searching, and carrying context across Claude Code session transcripts. macOS-only — uses `ps`/`lsof` for active session detection.

The project is expanding from a transcript viewer into a cross-session context manager with SQLite FTS search, project memory, session tagging, and a skill plugin for handoff generation. See `docs/DESIGN-DECISIONS.md` for the full architecture.

## Running

```bash
# Run directly (uv auto-installs textual dependency)
./threadhop

# With filters
./threadhop --project myproject --days 7

# CLI query mode
./threadhop todos
./threadhop todos --project myproject
```

No build step. The script uses `uv run --script` with PEP 723 inline metadata. Only dependency: `textual>=0.89.0`.

## Architecture

Core modules:
- `threadhop` — executable entry point, TUI, argparse routing, CLI handlers
- `db.py` — SQLite schema, migrations, session / observation-state helpers
- `indexer.py` — transcript normalization and FTS ingestion
- `observer.py` — on-demand observer orchestration over transcript byte ranges
- `observation_queries.py` — CLI query helpers for reading per-session observation JSONL

### Key Classes

| Class | Role |
|-------|------|
| `ClaudeSessions(App)` | Main Textual app — layout, refresh loop, keybindings, session discovery |
| `TranscriptView(VerticalScroll)` | Parses JSONL, renders conversation as `UserMessage`/`AssistantMessage`/`ToolMessage` widgets |
| `SessionItem(ListItem)` | Renders one session row: status icon (◐ working / ● active / ○ inactive) + name + age |

### Data Flow

1. **Discovery**: `_gather_session_data()` runs in a background worker every 5s — scans `~/.claude/projects/**/*.jsonl`, reads first 100 lines for metadata
2. **Active detection**: `_get_active_claude_sessions()` runs `ps -eo pid,args`, finds `claude` processes, resolves CWD via `lsof -a -d cwd -p <pid>`, matches to session IDs
3. **Display**: `_update_session_list()` diffs old/new session lists — full rebuild on change, in-place spinner updates otherwise
4. **Transcript**: `load_transcript()` parses full JSONL, strips `<system-reminder>` tags, abbreviates tool calls, mounts message widgets
5. **Observation CLI**: `threadhop todos` silently runs observer catch-up for already-tracked sessions, then reads `~/.config/threadhop/observations/<session_id>.jsonl` and prints compact JSONL rows newest-first

### Session State

- **is_active**: A `claude` process is running for this session (matched by session ID in args or CWD)
- **is_working**: Active + recently modified + has pending tool call or last message was from user

### Persistent Config

`~/.config/threadhop/config.json` stores app-level settings like theme. Session metadata and observation state live in SQLite at `~/.config/threadhop/sessions.db`.

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

## In Progress

- Observation-backed CLI queries are landing incrementally: `todos` is implemented, `decisions` / `observations` remain stubs
- Skill plugin work such as `/threadhop:handoff <session_id>` still depends on the observer pipeline
- See `docs/DESIGN-DECISIONS.md` for the detailed rollout plan
