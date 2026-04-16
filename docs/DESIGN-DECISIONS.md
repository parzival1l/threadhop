# ThreadHop — Design Decisions & Implementation Plan

Extracted from design discussion on 2026-04-14.
Status: **Design complete, implementation not started.**

---

## Table of Contents

- [Decisions (ADRs)](#decisions-adrs)
- [Implementation Plan](#implementation-plan)
- [Schema](#schema)
- [Skill Plugin Architecture](#skill-plugin-architecture)
- [TODO](#todo)
- [Open Questions](#open-questions)

---

## Decisions (ADRs)

### ADR-001: SQLite over JSON for metadata storage

**Context:** The app currently uses `~/.config/threadhop/config.json` for
all persistent state (theme, session names, ordering, last_viewed). New features
(FTS search, bookmarks, tags, project memory) require richer query patterns.

**Decision:** Migrate to SQLite at `~/.config/threadhop/sessions.db`.
Keep `config.json` only for app-level settings (theme, sidebar_width).

**Rationale:**
- FTS5 requires SQLite — no way around this
- Tags/bookmarks need relational queries ("all bookmarks tagged 'decision' from project X")
- Append-only memory ledger needs filtering by type, project, date
- WAL mode handles concurrent reads (TUI + skill plugin)
- One-time migration from config.json on first run

**Rejected:** Keeping JSON alongside SQLite (two sources of truth).

---

### ADR-002: SQLite FTS5 with porter stemming over embeddings

**Context:** Need full-text search across all session transcripts.

**Decision:** Use SQLite FTS5 with `porter unicode61` tokenizer.

**Rationale:**
- Personal-scale data (hundreds of sessions, not millions)
- Users remember keywords, not abstract concepts
- Zero infrastructure — SQLite is Python stdlib
- Sub-millisecond queries
- Deterministic, explainable results

**Revisit when:** Keyword search demonstrably fails on actual usage patterns.

---

### ADR-003: Chunk merging for assistant messages in FTS index

**Context:** Claude Code writes assistant responses as multiple JSONL lines
sharing the same `message.id` but each with a unique `uuid`. Lines 9-11 of a
typical session might all be `msg_01McJaN...` — text block, tool_use, more tool_use.

**Decision:** Group consecutive assistant lines by `message.id`, concatenate
their text content, index as one row. Use the first `uuid` as PK.

**Rationale:**
- Search results should be logical messages, not streaming chunks
- A tool call and its surrounding text are one thought unit
- User messages are always one line = one row (no grouping needed)

---

### ADR-004: Conductor-style status tags for sessions

**Context:** Sessions are currently a flat list sorted by manual ordering or
modification time. No way to track work status.

**Decision:** Add a `status` field to sessions with values:
`active` (default) | `in_progress` | `in_review` | `done` | `archived`

Sessions display grouped by status. Manual reorder works within each group.
Archived sessions are hidden by default (toggle with `A`).

**Rationale:**
- Matches the plan → implement → verify workflow
- Archived sessions declutter the list without deleting data
- Status groups provide natural visual organization

---

### ADR-005: Append-only JSONL ledger for project memory

**Context:** Need a sequential, chronological record of decisions, TODOs, and
work completed — distinct from CLAUDE.md which is a snapshot/declarative doc.

**Decision:** Store project memory in the SQLite `memory` table with typed
entries (decision, todo, done, adr, observation). Entries are append-only.
Rendered as markdown for injection into new sessions.

**Rationale:**
- CLAUDE.md is the wrong shape — it's a snapshot that gets rewritten
- The ledger answers "what happened and when" not "what are the rules"
- Cross-project by default, filterable by project
- Same SQLite DB, no separate files to manage

---

### ADR-006: Strict LLM vs instantaneous boundary

**Context:** Early design mixed LLM-powered skills and instant operations
without a clear boundary. For example, search and context insertion were
proposed as Claude Code skills — but search must be per-keystroke instant,
and context insertion is a visual selection in the TUI, not a typed command
with opaque message ranges.

**Decision:** Draw a hard line between what requires an LLM call and what
must be instantaneous:

| Operation | Type | Where it lives |
|-----------|------|----------------|
| Search across sessions | Instantaneous (FTS) | TUI |
| Message selection + copy | Instantaneous | TUI |
| Message export to temp file | Instantaneous | TUI |
| Bookmark a message | Instantaneous | TUI |
| Tag a session | Instantaneous | TUI |
| Handoff brief generation | LLM call (sub-agent) | Skill plugin |
| Project memory injection | File read (no LLM) | Skill plugin |

**Rationale:**
- Search must update results on every keystroke — an LLM round-trip is absurd
- Context insertion requires visual message selection — you can't type "15-25"
  without seeing the messages first, so the TUI handles selection and the
  clipboard/temp-file handles transport
- Only handoff (transcript compression) genuinely needs an LLM
- Skills are for operations that benefit from being invoked mid-conversation
  without switching to the TUI (handoff, memory injection)

**Rejected:** `/threadhop:insert-context <id> [range]` as a skill
(range numbers are opaque without visual context).
**Rejected:** `/threadhop:search <query>` as a skill (too slow, wrong UX).

---

### ADR-007: Real-time search architecture in TUI

**Context:** Need to search across all session transcripts from the TUI.
Must be per-keystroke instant — results update as you type, like a fuzzy finder.

**Decision:** Two-tier search powered by the FTS index:

**Tier 1 (v1): FTS5 prefix matching — instant, per-keystroke**
- User presses `/` (or a search keybind) → search input appears
- Each keystroke queries FTS5 with prefix matching:
  `messages_fts MATCH 'rate* lim*'` as user types "rate lim"
- Results displayed in a panel: matching message snippets with session name,
  project, and timestamp
- Navigate results with `j`/`k`, press `Enter` to jump to that message
  in the source transcript
- FTS5 prefix queries are sub-millisecond on personal-scale data
- Porter stemmer handles word forms ("running" matches "run")

**Tier 2 (future): Fuzzy matching for typos**
- Add trigram tokenizer (`tokenize='trigram'`) as a secondary FTS table
- Trigram matching handles spelling mistakes: "retr" matches "retry"
- Fall back to trigram search when FTS5 prefix returns zero results
- Alternatively: compute Levenshtein distance on FTS5 results for reranking

**Scope:** Searches all indexed sessions by default. Filter by project with
a modifier (e.g., `project:atlas rate limiting`). Filter by role with
`user:` or `assistant:` prefix.

**Rationale:**
- FTS5 prefix matching is the simplest path to per-keystroke search
- SQLite runs the query in C — Python overhead is just the binding call
- The index is already built by the background refresh cycle
- No regex needed for v1 — FTS5 tokenization handles word boundaries
- Regex can be offered as an advanced mode later (`/regex:pattern`)

---

### ADR-008: Context export via clipboard + temp files

**Context:** Users need to carry messages from one session into another.
Two transport mechanisms, both instantaneous (no LLM).

**Decision:**

**Clipboard copy (primary):**
- Select messages in TUI → press `y` → formatted text copied to clipboard
- Paste into any Claude Code session, T3 Code, or any other tool
- Format includes source labels:
  ```
  [From "API contracts" — ~/agent-atlas — 2026-04-12 10:30]
  User: What about rate limiting?
  Claude: Two options: leaky bucket vs token bucket...
  ```

**Temp file export (for larger selections):**
- Select messages → press `e` (export) → written to temp directory
- Path: `/tmp/threadhop/<session_id>-<timestamp>.md`
- NOT stored in any repo directory — these are ephemeral reference files
- TUI displays the full path after export
- User references from Claude Code: `Read /tmp/threadhop/abc123-20260414.md`
- Temp files are auto-cleaned on OS reboot (standard /tmp behavior)

**Rationale:**
- Clipboard for quick grabs (1-5 messages)
- Temp file for larger context blocks (10+ messages) that would be unwieldy
  as clipboard paste
- Temp directory avoids polluting any repo or config directory
- Full absolute paths so any session on the machine can reference them
- No LLM needed — this is a copy/format operation

**Rejected:** Storing exports in the repo structure.
**Rejected:** Storing exports in `~/.config/threadhop/` (not temp data).

---

### ADR-009: Stay with Python + Textual

**Context:** Considered Rust (ratatui) and TypeScript (Ink) as alternatives.

**Decision:** Stay with Python + Textual.

**Rationale:**
- App is I/O bound (file reads, subprocess calls), not CPU bound
- Textual provides complete widget system — ratatui would require hand-rolling
- SQLite FTS runs in C regardless of host language
- Single-file deployment via `uv run --script` is a major UX advantage
- Only benefit of Rust: ~5ms startup vs ~200ms (not perceptible in TUI)

---

### ADR-010: Observer-first architecture

**Context:** Initial design was TUI-first — observations only happened when the
TUI was running. But the real value is in the observations themselves, not the
TUI. Users want to query observations from the CLI without launching the TUI.

**Decision:** The observer is the core. The TUI and CLI are both consumers.

```
Chats happen → observer processes them → observations.jsonl accumulates
                                              ↓
                             ThreadHop TUI reads them (browsing)
                             threadhop todos (CLI query)
                             grep/jq reads them (raw)
```

**Observer uses Haiku** for extraction:
- ~200ms response time, ~$0.25/MTok input — fast and cheap
- Processes conversation chunks and outputs typed JSONL observations
- Types: `todo | decision | done | adr | question | blocker`
- Prompt: extract only explicitly discussed items, do not infer

**Observations stored as JSONL** at `~/.config/threadhop/observations.jsonl`:
```jsonl
{"type":"decision","text":"REST over gRPC","context":"Client SDK constraints","project":"agent-atlas","session":"abc123","ts":"2026-04-14T10:30:00Z"}
{"type":"todo","text":"Implement /workflows endpoint","project":"agent-atlas","session":"abc123","ts":"2026-04-14T11:15:00Z"}
```

JSONL format means observations are queryable without any app — `grep`, `jq`,
or the ThreadHop CLI all work.

**Observer triggers on CLI query or TUI launch** — not a daemon, not a hook.
When you run `threadhop todos`, it:
1. Checks for unprocessed messages (byte offset tracking)
2. Runs Haiku on new batches
3. Appends observations
4. Filters and displays

**Rationale:**
- The value is in the data, not the UI
- JSONL is universally queryable — no vendor lock-in to our app
- Haiku is fast/cheap enough to run on-demand without perceptible delay
- No daemon or background process to manage

---

### ADR-011: Dual-mode CLI (TUI + subcommands)

**Context:** ThreadHop was initially TUI-only. With the observer-first
architecture, users need CLI access to observations, tagging, and handoffs
without launching the TUI.

**Decision:** Single executable, two modes:

```bash
# No subcommand = TUI mode
threadhop
threadhop --project atlas --days 7

# With subcommand = CLI mode
threadhop todos                        # list all open TODOs
threadhop todos --project atlas        # filtered by project
threadhop decisions                    # all decisions
threadhop observations                 # everything
threadhop tag backlog                  # tag current session
threadhop tag in_review --session abc  # tag specific session
threadhop handoff abc123               # generate handoff brief
```

**Session detection for `threadhop tag`:** When called without `--session`,
detects the current session by scanning `ps` for claude processes in the
current terminal. Same detection logic the TUI already uses.

**Rationale:**
- One executable, no separate CLI tool to install
- Subcommand pattern is familiar (git, docker, etc.)
- CLI queries trigger the observer, so observations are always fresh
- Tags can be set from any terminal tab without switching to the TUI

---

### ADR-012: Three skills — tag, context, handoff

**Context:** Need to interact with ThreadHop from within a Claude Code session
without switching to the TUI or a terminal. Earlier design had two skills.
Expanded to three with clearer separation.

**Decision:** Three Claude Code skills with distinct roles:

| Skill | What it does | Uses LLM? |
|---|---|---|
| `/threadhop:tag <status>` | Tags current session | No — calls `threadhop tag` CLI |
| `/threadhop:context` | Formats clipboard content as sourced context | No — reads `pbpaste`, formats |
| `/threadhop:handoff <id>` | Compresses a full session into a brief | Yes — sub-agent with Haiku |

**`/threadhop:tag <status>`** — instant, no LLM:
1. Detects current session ID from process context
2. Calls `threadhop tag <status>`
3. Confirms: "Tagged this session as backlog"

**`/threadhop:context`** — instant, no LLM:
1. Reads clipboard (`pbpaste`) containing messages copied from TUI
2. Detects ThreadHop source labels in the content
3. Presents as a clearly bounded context block in the conversation
4. The model can now work with the injected context

**`/threadhop:handoff <id> [--full]`** — LLM call:
1. Reads the JSONL transcript for the given session
2. Default: sub-agent generates a structured brief (~30-50 lines)
3. `--full`: sub-agent produces comprehensive handoff with rationale and excerpts
4. Injects the brief into the current conversation

**Rationale:**
- Tag: must work mid-conversation without context switching
- Context: bridges TUI (visual selection) to Claude Code (injection)
- Handoff: the only one that needs an LLM, clearly separated
- Three skills is still minimal — each does one thing

**Rejected:** Merging context and handoff into one skill (different mechanisms,
different cost profiles).

---

### ADR-013: Session tagging from three entry points

**Context:** Session tags (backlog, in_progress, in_review, done, archived)
need to be settable from multiple places depending on the user's context.

**Decision:** Three entry points, one database:

| Entry point | How | When you'd use it |
|---|---|---|
| ThreadHop TUI | Press `s` to cycle status | Triaging multiple sessions |
| Claude Code skill | `/threadhop:tag backlog` | Mid-conversation, without leaving |
| Terminal CLI | `threadhop tag backlog` | Quick tag from another tab |

All three write to the same SQLite `sessions` table. The TUI reflects
changes from CLI/skill on the next 5s refresh.

**Rationale:**
- Different moments call for different interfaces
- Shared database means no sync issues
- The skill is the lightest possible — shells out to the CLI

---

### ADR-014: Sidebar resize via keybindings

**Context:** Session list is hardcoded at 36 characters (`grid-columns: 36 1fr`).
No way to resize from the UI.

**Decision:** Add `[` and `]` keybindings to shrink/grow sidebar (min 20, max 60,
step 4). Persist width in config. Consider Textual `Splitter` widget later for
drag-to-resize.

---

## Implementation Plan

### Phase 1: SQLite Foundation + Session Tags + Archive
_Immediate value, enables all future features._

1. Create SQLite DB initialization with schema (sessions, settings tables)
2. One-time migration from config.json
3. Add `status` field to session model
4. Render sessions grouped by status in the TUI
5. Keybindings: `s`/`S` cycle status, `a` archive, `A` toggle archive view
6. Sidebar resize (`[`/`]`)
7. Update CLAUDE.md with new architecture

### Phase 2: FTS Index + Message Selection + Search
_Enables instant search and cross-session context sharing. All TUI features._

1. Add messages table + FTS5 virtual table
2. Build indexer: parse JSONL, merge assistant chunks, strip system-reminders
3. Incremental indexing via index_state table (byte offset tracking)
4. Piggyback indexing on the 5s refresh cycle
5. Add message selection mode (`m` to enter, `j`/`k` between messages)
6. Range selection (`v` + movement)
7. Copy selected messages with source labels to clipboard (`y`)
8. Export selected messages to temp file (`e`) → `/tmp/threadhop/<id>.md`
9. Real-time search panel (`/`):
   - FTS5 prefix matching, results update per keystroke
   - Results show: message snippet, session name, project, timestamp
   - `j`/`k` to navigate results, `Enter` to jump to source transcript
   - Filter syntax: `project:atlas`, `user:`, `assistant:`
10. Future: trigram-based fuzzy search for typo tolerance

### Phase 3: CLI Subcommands + Observer
_Observer-first architecture. CLI access to observations without the TUI._

1. Add argparse subcommand routing: no subcommand = TUI, with subcommand = CLI
2. Implement `threadhop tag <status> [--session <id>]`
   - Auto-detect session from current terminal when `--session` omitted
3. Implement Haiku observer:
   - Process unindexed conversation chunks through Haiku
   - Extract typed observations (todo, decision, done, adr, question, blocker)
   - Append to `~/.config/threadhop/observations.jsonl`
   - Track byte offsets for incremental processing
4. Implement CLI queries:
   - `threadhop todos [--project <name>]`
   - `threadhop decisions [--project <name>]`
   - `threadhop observations [--project <name>]`
   - All trigger observer for unprocessed messages before displaying results

### Phase 4: Skill Plugin
_Three skills for in-session use._

1. Research Claude Code skill plugin packaging/distribution
2. `/threadhop:tag <status>` — detect session ID, call `threadhop tag` CLI
3. `/threadhop:context` — read clipboard, format with source labels, inject
4. `/threadhop:handoff <id> [--full]` — sub-agent compresses transcript

### Phase 5: Project Memory + Bookmarks
_Cross-session knowledge persistence._

1. Add bookmarks table to schema
2. Bookmark action from message selection mode (`space` to toggle)
3. Bookmark browser panel in TUI
4. Explicit annotation detection: recognize "ADR:", "DECISION:", "TODO:" markers
   in conversations and auto-append to observations
5. Memory rendering: generate project memory markdown from observations for injection

### Phase 6: Reflector
_Condensation of accumulated observations._

1. When observations.jsonl exceeds threshold, run Haiku reflector
2. Merge related decisions, archive completed TODOs
3. Produce condensed observation summaries
4. Keep source links to original observations

---

## Schema

```sql
-- Location: ~/.config/threadhop/sessions.db

-- App-level settings (replaces most of config.json)
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL  -- JSON-encoded
);

-- Session metadata
CREATE TABLE sessions (
    session_id    TEXT PRIMARY KEY,
    session_path  TEXT NOT NULL,
    project       TEXT,
    cwd           TEXT,
    custom_name   TEXT,
    status        TEXT DEFAULT 'active',
        -- active | in_progress | in_review | done | archived
    sort_order    INTEGER,
    last_viewed   REAL,
    created_at    REAL,
    modified_at   REAL
);

-- Message index (for FTS and message selection)
CREATE TABLE messages (
    uuid          TEXT PRIMARY KEY,
    session_id    TEXT NOT NULL,
    parent_uuid   TEXT,
    role          TEXT NOT NULL,      -- 'user' | 'assistant'
    timestamp     TEXT NOT NULL,
    session_path  TEXT NOT NULL,
    line_number   INTEGER NOT NULL,   -- for jump-to-source
    cwd           TEXT,
    text          TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);

-- Full-text search
CREATE VIRTUAL TABLE messages_fts USING fts5(
    text,
    content='messages',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

-- Incremental index tracking
CREATE TABLE index_state (
    session_path  TEXT PRIMARY KEY,
    last_offset   INTEGER NOT NULL,  -- byte offset into JSONL
    last_modified REAL NOT NULL
);

-- Bookmarks
CREATE TABLE bookmarks (
    id            INTEGER PRIMARY KEY,
    message_uuid  TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    label         TEXT,
    tags          TEXT,               -- JSON array
    created_at    REAL NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);

-- Project memory ledger
CREATE TABLE memory (
    id            INTEGER PRIMARY KEY,
    project       TEXT NOT NULL,
    type          TEXT NOT NULL,
        -- decision | todo | done | adr | observation
    text          TEXT NOT NULL,
    session_id    TEXT,
    source        TEXT DEFAULT 'explicit',  -- explicit | auto
    resolved      INTEGER DEFAULT 0,        -- for TODOs: 0=open, 1=done
    created_at    REAL NOT NULL
);
```

---

## Skill Plugin Architecture

### Principle: Three skills, clear boundaries

Skills are for operations invoked mid-conversation from Claude Code. The TUI
handles everything visual and instantaneous. The CLI handles queries and tagging
from the terminal.

### Plugin: `threadhop`

```
threadhop/
  skills/
    tag.md              # /threadhop:tag <status>
    context.md          # /threadhop:context
    handoff.md          # /threadhop:handoff <session_id>
```

### What lives where

| Feature | Lives in | Why |
|---------|----------|-----|
| Search | TUI | Per-keystroke instant, visual results |
| Message select + copy | TUI | Visual selection, clipboard transport |
| Message export to .md | TUI | Visual selection, writes to /tmp |
| Bookmark | TUI | Visual selection, one-key action |
| Tag session | TUI + Skill + CLI | All three entry points, one DB |
| Observation queries | CLI | `threadhop todos`, `threadhop decisions` |
| Context injection | Skill | Formats clipboard content with source labels |
| Handoff | Skill | Needs LLM sub-agent for compression |

### Skill 1: `/threadhop:tag <status>` (instant, no LLM)

```
User (in Claude Code): /threadhop:tag backlog

1. Skill detects current session ID from process context
2. Calls: threadhop tag backlog --session <session_id>
3. ThreadHop CLI writes tag to SQLite
4. Confirms: "Tagged this session as backlog"
```

### Skill 2: `/threadhop:context` (instant, no LLM)

Bridges the TUI (visual selection) to Claude Code (context injection).
User copies messages from the TUI, then invokes this skill to present
them cleanly in the current conversation.

```
User (in Claude Code): /threadhop:context

1. Skill reads clipboard (pbpaste)
2. Detects ThreadHop source labels in the content
3. Presents as a clearly bounded context block:

   ┌─ From "API contracts" — ~/agent-atlas — 2026-04-12 ─┐
   │ User: What about rate limiting?                       │
   │ Claude: Two options: leaky bucket vs token bucket...  │
   └───────────────────────────────────────────────────────┘

4. The model now has this context and can work with it
```

### Skill 3: `/threadhop:handoff <id> [--full]` (LLM call)

The only skill that uses an LLM. Compresses an entire session transcript
into a brief for starting a fresh session.

```
User (in Claude Code): /threadhop:handoff abc123

1. Skill locates ~/.claude/projects/**/abc123.jsonl
2. Parses JSONL → clean (role, text) pairs
3. Strips system-reminders, abbreviates tool calls
4. Spawns sub-agent (Haiku for speed, configurable) with:
   - Parsed transcript
   - Prompt: "Generate a handoff brief. Focus on: decisions made (with
     rationale), current implementation state, open questions, and what
     the next session needs to know."
5. Sub-agent returns brief (~30-50 lines)
6. Brief injected into current conversation

With --full flag:
   Sub-agent produces comprehensive handoff with rationale,
   code references, and conversation excerpts.
```

The raw transcript lives only in the sub-agent's context.
The main session sees only the compressed brief.

### Context injection flow (TUI → clipboard → skill)

The full workflow for carrying context between sessions:

```
1. Open ThreadHop TUI
2. Navigate to source session, view transcript
3. Enter message select mode (m)
4. Select messages visually (j/k to move, v for range)
5. Press y → copied to clipboard with source labels
6. Switch to Claude Code session
7. Type /threadhop:context → clipboard content formatted and injected
```

For larger exports:
```
5. Press e → exported to /tmp/threadhop/<id>-<ts>.md
6. In Claude Code: "Read /tmp/threadhop/..."
```

---

## TODO

### Immediate (Phase 1)
- [ ] Create SQLite DB module (init, migrate, query helpers)
- [ ] Migrate config.json → SQLite (one-time, on first run)
- [ ] Add session status field + grouped display
- [ ] Implement status cycling keybinds (`s`/`S`)
- [ ] Implement archive (`a`) + archive toggle (`A`)
- [ ] Implement sidebar resize (`[`/`]`)
- [ ] Write tests for DB migration

### Next (Phase 2)
- [ ] Build JSONL indexer with chunk merging
- [ ] Implement incremental indexing (byte offset tracking)
- [ ] Add message selection mode to TUI (`m` to enter, `j`/`k` between messages)
- [ ] Add range selection (`v` + movement)
- [ ] Clipboard copy with source labels (`y`)
- [ ] Temp file export (`e`) → `/tmp/threadhop/`
- [ ] Real-time search panel (`/`) with FTS5 prefix matching
- [ ] Per-keystroke result updates in search
- [ ] Jump-to-source from search results (`Enter`)
- [ ] Search filter syntax: `project:`, `user:`, `assistant:`

### Phase 3: CLI + Observer
- [ ] Add argparse subcommand routing (no subcommand = TUI)
- [ ] Implement `threadhop tag <status> [--session <id>]`
- [ ] Session auto-detection from current terminal (ps/lsof)
- [ ] Haiku observer: process conversation chunks, extract typed observations
- [ ] Observations JSONL output at `~/.config/threadhop/observations.jsonl`
- [ ] Incremental processing (byte offset tracking per session)
- [ ] `threadhop todos [--project]` CLI query
- [ ] `threadhop decisions [--project]` CLI query
- [ ] `threadhop observations [--project]` CLI query

### Phase 4: Skills
- [ ] Research Claude Code skill plugin packaging
- [ ] `/threadhop:tag <status>` skill (calls CLI)
- [ ] `/threadhop:context` skill (clipboard formatting + injection)
- [ ] `/threadhop:handoff <id> [--full]` skill (sub-agent compression)

### Phase 5-6: Memory + Reflector
- [ ] Build bookmark system (TUI feature)
- [ ] Explicit annotation detection (ADR:, DECISION:, TODO: markers)
- [ ] Project memory markdown rendering from observations
- [ ] Reflector: condense old observations via Haiku
- [ ] Trigram-based fuzzy search for typo tolerance

---

## Open Questions

> These need resolution before or during implementation.

### Q1: Custom status tags or fixed set?
Current design uses a fixed set: `active | in_progress | in_review | done | archived`.
Should users be able to define custom tags? Custom tags add flexibility but
complicate the UI (keybind cycling, group headers, color coding).
**Leaning:** Fixed set for v1. Custom tags as a later enhancement.

### Q2: Project memory — per-project or per-feature?
We discussed both. Per-project is simpler (project name is already in session
metadata). Per-feature requires explicit tagging of sessions to features.
**Leaning:** Per-project for v1. Feature concept layered on top later — a feature
is essentially a tag that groups sessions and memory entries across projects.

### Q3: Observer trigger — when does auto-observation run?
**Resolved:** On CLI query or TUI launch. When you run `threadhop todos`,
it processes any unindexed messages first, then filters. No daemon, no hook,
no background process. The TUI does the same on launch. See ADR-010.

### Q4: Skill plugin packaging
How are Claude Code skill plugins distributed? As a directory of .md files in
`~/.claude/skills/`? As an npm/pip package? Need to verify the plugin contract.
**Action:** Research Claude Code skill plugin packaging before Phase 3.

### Q5: FTS indexing — index tool results or not?
Current design: index only user + assistant text. Tool results are huge (file
contents, command output) and would dominate search with noise.
**Leaning:** Skip tool results for v1. Add opt-in tool result indexing if users
find themselves wanting to search "what was the output of that command."

### Q6: Handoff sub-agent model
**Resolved:** Haiku for the observer (fast, cheap, structured extraction).
Handoff skill defaults to Haiku for speed (~200ms, ~$0.25/MTok). `--full` flag
can use a stronger model. Configurable via CLI flag or config. See ADR-010.

---

## What We Learned

### JSONL message structure is richer than expected
Every message line has native `uuid`, `parentUuid`, `sessionId`, `timestamp`,
`cwd`, `isSidechain`. No synthetic IDs needed for the index. Assistant messages
come as multiple lines sharing the same `message.id` but unique `uuid`s — these
must be merged for search.

### The 36-char sidebar is hardcoded in CSS
`grid-columns: 36 1fr` at line 408. Textual supports dynamic CSS manipulation
via `self.styles.grid.columns`, so resize keybindings are straightforward.

### Message widgets are non-interactive
`UserMessage`, `AssistantMessage`, `ToolMessage` (lines 159-171) are bare
`Static` subclasses. Adding message selection requires either:
- Making them focusable/highlightable (Textual supports this via CSS classes)
- Or overlaying a selection cursor that tracks position independently

### config.json is the only persistence
No database, no cache. Everything is in-memory during runtime, persisted to a
single JSON file. Migration to SQLite is a clean cut — one-time import, then
JSON for app settings only.

### The app has 26 keybindings already
Modal (focus-aware). Available keys for new features: `m`, `s`, `a`, `b`, `v`,
`y`, `[`, `]`, `f`, `p`, `1-4`, `space`. Sufficient for everything planned.
