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

### ADR-010: Sidebar resize via keybindings

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

### Phase 3: Skill Plugin (lean — only LLM-powered operations)
_Only skills that genuinely need an LLM or benefit from mid-conversation invocation._

1. Research Claude Code skill plugin packaging/distribution
2. `/threadhop:handoff <session_id>` — read JSONL, delegate to sub-agent
   for compression, inject brief into current session
3. `/threadhop:memory <project>` — read project memory, inject as context
   (no LLM needed, just formatted file read)

**Explicitly NOT skills (these are TUI features):**
- Search (instantaneous, per-keystroke)
- Message selection + copy (visual, clipboard)
- Message export (visual, temp file)
- Bookmarks (one-key action)
- Session tagging (one-key action)

### Phase 4: Project Memory + Bookmarks
_Cross-session knowledge persistence._

1. Add memory table + bookmarks table to schema
2. Bookmark action from message selection mode (`space` to toggle)
3. Bookmark browser panel in TUI
4. Memory ledger: manual entry from TUI (type + text)
5. `/threadhop:memory <project>` skill to inject project memory
6. Explicit annotation detection: recognize "ADR:", "DECISION:", "TODO:" markers
   in conversations and offer to append to ledger

### Phase 5: Auto-Observer + Reflector
_Automatic knowledge extraction._

1. Background observer: detect new messages in active sessions during refresh
2. Auto-extract observations (pattern matching first, `claude -p` later)
3. Append to memory ledger with `source: "auto"`
4. Reflector: periodic condensation of old observations
5. Archive completed TODOs, merge related decisions

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

### Principle: Only skills that need LLM or can't be done visually

The TUI handles everything that should be instantaneous or visual (search,
selection, copy, export, tag, bookmark). Skills handle only what benefits
from being invoked mid-conversation without context-switching to the TUI.

### Plugin: `threadhop`

```
threadhop/
  skills/
    handoff.md          # /threadhop:handoff <session_id>
    memory.md           # /threadhop:memory <project>
```

**Only two skills.** Everything else is a TUI feature.

### What lives where

| Feature | Lives in | Why |
|---------|----------|-----|
| Search | TUI | Per-keystroke instant, visual results |
| Message select + copy | TUI | Visual selection, clipboard transport |
| Message export to .md | TUI | Visual selection, writes to /tmp |
| Bookmark | TUI | Visual selection, one-key action |
| Tag session | TUI | One-key action on session list |
| Handoff | Skill | Needs LLM sub-agent for compression |
| Memory inject | Skill | Reads project memory file, no LLM needed |

### Handoff flow (the only LLM skill)

```
User (in Claude Code): /threadhop:handoff abc123

1. Skill locates ~/.claude/projects/**/abc123.jsonl
2. Parses JSONL → clean (role, text) pairs
3. Strips system-reminders, abbreviates tool calls
4. Spawns sub-agent with:
   - Parsed transcript (full conversation)
   - Prompt: "Generate a handoff brief. Focus on: decisions made (with
     rationale), current implementation state, open questions, and what
     the next session needs to know. Format as structured markdown."
5. Sub-agent returns handoff brief (~30-50 lines)
6. Skill injects brief into current conversation:
   "[Handoff from session 'API contracts' (abc123, 2026-04-12)]
    ## Decisions made..."
```

The raw transcript lives only in the sub-agent's context.
The main session sees only the compressed brief.

### Memory inject flow (file read, no LLM)

```
User (in Claude Code): /threadhop:memory agent-atlas

1. Skill reads project memory from SQLite (or rendered .md cache)
2. Formats as structured context with section headers
3. Injects directly — no sub-agent, no compression
```

### Context injection flow (TUI → clipboard/file → other session)

This is NOT a skill. It's a TUI workflow:

```
1. User opens ThreadHop TUI
2. Navigates to source session, views transcript
3. Enters message select mode (m)
4. Selects messages visually (j/k to move, v for range)
5. Either:
   a. Press y → copied to clipboard with source labels
      → paste into any Claude Code / T3 session
   b. Press e → exported to /tmp/threadhop/<id>-<ts>.md
      → reference from any session: "Read /tmp/threadhop/..."
6. The TUI shows confirmation: "3 messages copied" or "Exported to /tmp/..."
```

No message numbers. No opaque ranges. Visual selection, instant transport.

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

### Later (Phase 3-5)
- [ ] Research Claude Code skill plugin packaging
- [ ] Implement handoff skill with sub-agent delegation
- [ ] Implement memory injection skill (file read, no LLM)
- [ ] Build bookmark system (TUI feature, not skill)
- [ ] Build project memory ledger
- [ ] Add explicit annotation detection
- [ ] Build auto-observer
- [ ] Build reflector
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
Options:
- On the TUI's 5s refresh cycle (piggyback, always-on)
- On session close/inactivity detection (event-driven)
- On-demand only (manual trigger)
**Leaning:** On-demand for v1 (via skill or TUI action). Piggyback for v2.

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
The handoff skill delegates transcript compression to a sub-agent. Which model?
Haiku for speed/cost? Sonnet for quality? Configurable?
**Leaning:** Default to the same model as the parent session. Allow override.

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
