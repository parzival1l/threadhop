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

### ADR-015: Background observer-reflector as sidecar process

**Context:** ADR-010 established the observer-first architecture with on-demand
triggering (CLI query or TUI launch). But during active Claude Code sessions,
observations only happen after the fact — never while you're working. We want
the observer running continuously in the background, extracting observations
in real-time, with a reflector identifying contradictory decisions across
sessions. This should be a flag you enable — like Claude Code's remote control
mode — and then forget about while you continue working.

**Attribution:** The Observer/Reflector architecture is inspired by
[Mastra's Observational Memory](https://mastra.ai/docs/memory/observational-memory)
(`@mastra/memory@1.1.0`) — a three-tier system where Observer and Reflector
agents run alongside the primary agent, compressing context and maintaining
long-term memory. Credit to the Mastra team for the foundational pattern.
See `docs/observational-memory.md` for the full reference.

**The inherent problem with Mastra's approach:** Mastra's Observer and Reflector
are *inline agents* — they run within the same process, share memory with the
primary agent, and can directly modify its context window (removing old messages,
injecting compressed observations, managing token budgets). In Claude Code, we
have no access to the agent's context window. Claude Code is a black box that
writes JSONL transcripts to disk. We can *read* those transcripts but cannot
*modify* the running agent's context. This makes inline observation impossible.

This constraint means ThreadHop's observer-reflector serves a fundamentally
different purpose than Mastra's:

| | Mastra OM | ThreadHop Observer-Reflector |
|---|---|---|
| Architecture | Inline (same process) | Sidecar (separate process) |
| Access | Reads + writes agent context | Read-only transcript watcher |
| Observer goal | Context compression | Knowledge extraction |
| Reflector goal | Condense observations | Detect contradictory decisions |
| Lifecycle | Coupled to agent | Independent of agent |
| Value for | The agent (stays effective) | The human (understands what happened) |

Mastra optimizes the agent's *ability to continue working* (context management).
ThreadHop optimizes the human's *ability to understand what happened* across
sessions (knowledge extraction + contradiction detection). Complementary goals,
but architecturally distinct — which is why the inline approach was taken out
of the design.

**Decision:** Observer and Reflector run as background sidecar processes, enabled
via a flag. The Claude Code terminal does NOT pause.

**Architecture:**

```
Claude Code session (primary agent)
    ↓ writes JSONL
~/.claude/projects/.../<session>.jsonl
    ↑ watches (fsevents / polling)
Observer process (background, Haiku)
    ↓ appends typed observations
~/.config/threadhop/observations.jsonl
    ↑ reads periodically
Reflector process (background, Haiku)
    ↓ writes conflict reports
~/.config/threadhop/conflicts.jsonl
```

**Enabling from Claude Code — per-session opt-in (primary, see ADR-016):**

```
/threadhop:observe
→ Spawns observer for THIS session only. Retroactive catch-up + watch mode.
```

**Alternative entry points (power users):**

```bash
# CLI: observe a specific session from another terminal
threadhop observe --session <id> &

# Auto-observe all sessions (NOT default, opt-in via config)
threadhop config set observe.auto true
```

The primary model is per-session opt-in via the skill. Most conversations
don't warrant observation. The user chooses which ones are valuable.
See ADR-016 for the full trigger and injection design.

**Observer behaviour (background mode):**

1. Targets a specific session's JSONL (not all sessions)
2. Retroactive catch-up: reads from byte 0, processes all existing messages
3. Sets byte offset, switches to watch mode (fsevents on macOS, polling fallback)
4. When new messages accumulate (configurable batch size, default ~10 messages):
   - Reads new bytes from JSONL (byte offset tracking, same as ADR-010)
   - Sends conversation chunk to Haiku
   - Prompt: extract only explicitly discussed items across these five types:
     `todo | decision | done | adr | observation`
   - Appends typed JSONL observations to `observations.jsonl`
5. Runs until the Claude Code session exits or manually stopped

**Reflector behaviour — conflict detection:**

The reflector's purpose is NOT condensation (Mastra's approach). It is
specifically to **detect contradictory decisions** across sessions.

Example conflict:
```jsonl
{"type":"decision","text":"REST over gRPC for client API","project":"atlas","session":"abc","ts":"..."}
{"type":"decision","text":"gRPC for all service-to-service comms","project":"atlas","session":"def","ts":"..."}
→ CONFLICT: REST vs gRPC scope overlap — both cover service communication
```

The Reflector:
1. Runs at lower frequency (every N new observations, or every M minutes)
2. Groups decisions by project and semantic topic
3. Uses Haiku to identify contradictions between decisions
4. Writes conflict reports to `conflicts.jsonl` for human review
5. Surfaces conflicts via TUI notification or CLI query (`threadhop conflicts`)

**Why NOT a daemon:**

The observer is a background *process*, not a system daemon. It lives for the
duration of a Claude Code session and exits when the session ends. No launchd
plist, no systemd service, no process manager. Start it with `&` or a hook,
kill it when done.

**Rationale:**
- Background process means zero friction — enable a flag and forget
- File-watching is the only interface available (Claude Code is a black box)
- Conflict detection is unique to ThreadHop — Mastra doesn't attempt this
- Sidecar architecture means the observer works with any AI coding tool
- On-demand mode (ADR-010) still works — background mode is additive
- **Supersedes Q3** (was resolved as "no background process") — background
  mode is now opt-in alongside the original on-demand trigger

---

### ADR-016: Per-session opt-in trigger and pull-based context injection

**Context:** ADR-015 designed the observer-reflector as a background sidecar.
But it assumed a global flag (auto-start hook, permanent config). In practice,
most conversations don't warrant observation — routine debugging, quick fixes,
file edits. The user needs to *choose* which conversations are valuable enough
to observe. And once observations exist, they need a way to pull them back
into the conversation.

**Decision:** Observation is per-session opt-in, triggered by a skill
(`/threadhop:observe`). Context injection is pull-based, triggered by a
second skill (`/threadhop:insights`). Neither requires the TUI.

**Why per-session, not global:**
- Most Claude Code sessions are short or routine — observing them wastes
  Haiku calls and pollutes `observations.jsonl` with noise
- The user knows which conversations matter — architectural discussions,
  design decisions, complex debugging sessions
- Per-session opt-in means zero cost for throwaway sessions
- A global auto-observe flag remains available as a power-user option
  (`threadhop config set observe.auto true`) but is NOT the default

**Trigger point 1 — beginning of conversation:**

The user knows from the start this will be important:

```
User: /threadhop:observe

1. Skill detects current session ID via ps/lsof
2. JSONL is nearly empty (just started) — minimal retroactive work
3. Observer starts watching in background
4. Confirms: "Observing this session. Watching for new messages."
5. User continues working normally
```

**Trigger point 2 — mid-conversation:**

The user realizes mid-conversation that this discussion is worth capturing:

```
User: /threadhop:observe

1. Skill detects current session ID
2. Observer reads ENTIRE JSONL from byte 0 (retroactive catch-up)
3. Processes all existing messages through Haiku — extracts observations
4. Sets byte offset to current position
5. Switches to watch mode for new messages
6. Confirms: "Observing this session. 47 messages processed retroactively
   — found 5 decisions, 3 TODOs, 1 ADR. Watching for new messages."
7. User continues working — observer runs silently in background
```

The retroactive catch-up is identical to on-demand mode (ADR-010) — same
incremental processing logic, same byte offset tracking. The only difference
is that after catch-up, the observer stays resident instead of exiting.

**Pull-based context injection — same session:**

Claude Code is a black box — we can read its transcripts but cannot push
into its context window. So injection is always **pull-based**: the user
invokes a skill that reads from `observations.jsonl` / `conflicts.jsonl`
and formats the findings into the conversation.

```
User: /threadhop:insights

1. Skill reads observations.jsonl filtered by current session
2. Reads conflicts.jsonl filtered by current project
3. Formats and presents:

   ┌─ ThreadHop Observations — this session ───────────────┐
   │ DECISIONS:                                             │
   │  • REST for client API (rationale: SDK constraints)    │
   │  • Token bucket for rate limiting                      │
   │ TODOs:                                                 │
   │  • Implement /workflows endpoint                       │
   │  • Write integration tests for auth flow               │
   │ ADRs:                                                  │
   │  • ADR-003: Chunk merging for assistant messages        │
   │ CONFLICTS:                                             │
   │  ⚠ Session "infra-design" decided "gRPC for all        │
   │    services" — contradicts "REST for client API" above  │
   └────────────────────────────────────────────────────────┘

4. The model now has this context and can work with it
```

This is the same pattern as `/threadhop:context` (read data, format, inject)
but reads from the observer's output instead of the clipboard.

**Pull-based context injection — new session (enhanced handoff):**

When the source session was observed, `/threadhop:handoff` is enhanced:

```
User (new session): /threadhop:handoff abc123

Without observations (current behaviour):
  1. Read entire JSONL (~thousands of lines)
  2. Parse, strip, abbreviate
  3. Send full transcript to Haiku sub-agent
  4. Sub-agent compresses from scratch → ~30-50 line brief

With observations (enhanced):
  1. Read observations.jsonl filtered by session abc123
  2. Observations already typed, structured, compressed
  3. Send observations + conflicts to Haiku sub-agent
  4. Sub-agent generates brief from structured input
  5. Brief includes: decisions with rationale, open TODOs,
     unresolved conflicts, current state
  → Faster (less input), higher quality (structured input)
```

The handoff doesn't *replace* reading the raw transcript — it uses
observations as primary input and can optionally pull raw messages for
context where the observation is too compressed.

**The complete feedback loop:**

```
Session A (architectural discussion):
  1. Working on feature...
  2. User realizes this is important
  3. /threadhop:observe → retroactive catch-up + watch mode
  4. Continue working... observer silently extracts observations
  5. /threadhop:insights → "Here's what I've captured: 5 decisions, 3 TODOs"
  6. User reviews, continues. Observer captures more.
  7. Session gets long, user wants to continue elsewhere

Session B (continuation):
  1. /threadhop:handoff A → brief built from pre-extracted observations
  2. /threadhop:conflicts → "1 conflict: Session A vs Session C on REST/gRPC"
  3. User resolves conflict in this conversation
  4. /threadhop:observe → now observing Session B, captures the resolution
  5. Resolution appears in observations.jsonl as a new decision
```

The loop closes: **observe → extract → surface → resolve → observe the
resolution**. Each session can opt in independently. Observations accumulate
across sessions. Conflicts are detected cross-session and surfaced on demand.

**Updated skill count — five skills:**

| Skill | What it does | LLM? | New? |
|---|---|---|---|
| `/threadhop:tag` | Tag session status | No | Existing (ADR-012) |
| `/threadhop:context` | Inject clipboard content | No | Existing (ADR-012) |
| `/threadhop:handoff` | Generate handoff brief | Yes | Enhanced (ADR-016) |
| `/threadhop:observe` | Start background observer for this session | No | New (ADR-016) |
| `/threadhop:insights` | Pull observations + conflicts into conversation | No | New (ADR-016) |

The observe skill itself is instant (spawns a process). The background
observer uses Haiku. The insights skill is instant (reads files, formats).

**Rationale:**
- Per-session opt-in respects the user's attention — only important
  conversations get the Haiku cost
- Mid-conversation trigger with retroactive catch-up means you never miss
  context, even if you decide to observe 30 minutes into a discussion
- Pull-based injection is the only model that works with Claude Code's
  black-box architecture
- Enhanced handoff with observations is strictly better — faster (less
  input to process) and higher quality (structured vs raw)
- Five skills is still manageable — each does exactly one thing

---

### ADR-017: Context-aware discoverability via modal help and shared command metadata

**Context:** ThreadHop already has multiple interaction surfaces with
different affordances: the global search modal, the persistent in-transcript
find bar, the stock footer, and transcript-local selection mode. The current
footer only exposes a small subset of bindings, while other commands live in
widget-local `on_key` handlers or transient notifications. That was fine when
the app was smaller, but it does not scale now that bindings are focus-aware,
mode-specific, and sometimes conflicting.

An always-on footer that tries to show every key all the time would turn into
noise and still be incomplete. The app needs one discoverability surface that
answers "what can I do from here?" without flattening all contexts together.

**Decision:** Add a context-aware help overlay, using the same full-app modal
pattern as search, and back it with a shared command metadata registry.

**UI model:**
- Keep the footer minimal and contextual. It remains a compact reminder of the
  highest-value actions currently available, not the source of truth for every
  binding.
- Add a global help overlay that takes over the app like search does and
  groups commands by scope: global app, session list, transcript, selection
  mode, reply input, and search/find.
- The help overlay may optionally expose executable actions later, but v1 is
  discoverability-first rather than a general command palette.
- The help trigger must remain separate from handoff naming. Do not hardcode
  `H` as the permanent key for help.

**Architecture model:**
- Define command metadata in one shared registry rather than duplicating key
  descriptions across `Footer`, modal help text, README tables, and ad hoc
  notifications.
- The registry must support context predicates so commands can be shown only
  when relevant (for example: transcript focused, selection mode active, find
  bar open).
- Widget-local commands still own their behaviour, but they also register
  discoverability metadata so they stop being invisible to the rest of the UI.
- Footer rendering and help-overlay rendering should both read from this same
  metadata source.

**Rationale:**
- Search already established the right interaction precedent for a full-app
  overlay in this TUI.
- Context-aware discoverability matches the app's actual behaviour; a flat
  list of bindings does not.
- A shared registry prevents docs and UI surfaces from drifting apart as more
  commands are added.
- Leaving the help key unresolved avoids creating unnecessary coupling with the
  future handoff shortcut work.

**Rejected:** Expanding the footer into a permanent wall of bindings.
**Rejected:** Maintaining help text separately in code, docs, and notifications.

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
11. Context-aware help overlay + shared command metadata registry:
   - Full-app discoverability overlay, modeled on the search modal
   - Footer stays minimal/contextual instead of listing every keybinding
   - One registry feeds footer hints, help content, and future docs sync
   - Trigger key intentionally left open; do not assume `H`

### Phase 3: CLI Subcommands + Observer (on-demand + background)
_Observer-first architecture. CLI access to observations without the TUI.
Background mode for continuous observation during active sessions._

1. Add argparse subcommand routing: no subcommand = TUI, with subcommand = CLI
2. Implement `threadhop tag <status> [--session <id>]`
   - Auto-detect session from current terminal when `--session` omitted
3. Implement Haiku observer (on-demand mode, ADR-010):
   - Process unindexed conversation chunks through Haiku
   - Extract typed observations: `todo | decision | done | adr | observation`
   - Append to `~/.config/threadhop/observations.jsonl`
   - Track byte offsets for incremental processing
4. Implement background observer mode (ADR-015):
   - `threadhop observe` — runs as background sidecar process
   - File-watching via fsevents (macOS), fallback to polling
   - Auto-detects active session JSONL via ps/lsof
   - Batched extraction (configurable, default ~10 messages)
   - Exits when Claude Code session ends
5. Implement CLI queries:
   - `threadhop todos [--project <name>]`
   - `threadhop decisions [--project <name>]`
   - `threadhop observations [--project <name>]`
   - All trigger observer for unprocessed messages before displaying results

### Phase 4: Skill Plugin
_Five skills for in-session use (ADR-012, ADR-016)._

1. Research Claude Code skill plugin packaging/distribution
2. `/threadhop:tag <status>` — detect session ID, call `threadhop tag` CLI
3. `/threadhop:context` — read clipboard, format with source labels, inject
4. `/threadhop:handoff <id> [--full]` — sub-agent compresses transcript
   (enhanced: uses pre-extracted observations when available)
5. `/threadhop:observe` — per-session opt-in, spawns background observer
   (retroactive catch-up + watch mode)
6. `/threadhop:insights` — pull observations + conflicts into conversation

### Phase 5: Project Memory + Bookmarks
_Cross-session knowledge persistence._

1. Add bookmarks table to schema
2. Bookmark action from message selection mode (`space` to toggle)
3. Bookmark browser panel in TUI
4. Explicit annotation detection: recognize "ADR:", "DECISION:", "TODO:" markers
   in conversations and auto-append to observations
5. Memory rendering: generate project memory markdown from observations for injection

### Phase 6: Reflector — Conflict Detection
_Contradiction detection across sessions. Runs as background sidecar alongside
the observer (ADR-015), or on-demand via CLI._

1. Build conflict detection reflector (Haiku):
   - Group decisions by project and semantic topic
   - Identify contradictory decisions across sessions
   - Write conflict reports to `~/.config/threadhop/conflicts.jsonl`
2. Background reflector mode:
   - Runs at lower frequency than observer (every N observations or M minutes)
   - Triggered automatically when observer appends new decisions
3. CLI query: `threadhop conflicts [--project <name>]`
4. TUI notification: surface unreviewed conflicts in sidebar or status bar
5. Condensation (secondary goal): merge related decisions, archive completed
   TODOs, produce condensed summaries with source links

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

### Principle: Five skills, clear boundaries

Skills are for operations invoked mid-conversation from Claude Code. The TUI
handles everything visual and instantaneous. The CLI handles queries and tagging
from the terminal. See ADR-012 (original three) and ADR-016 (observe + insights).

### Plugin: `threadhop`

```
threadhop/
  skills/
    tag.md              # /threadhop:tag <status>
    context.md          # /threadhop:context
    handoff.md          # /threadhop:handoff <session_id>
    observe.md          # /threadhop:observe
    insights.md         # /threadhop:insights
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
| Start observation | Skill | Per-session opt-in, spawns background process |
| Pull observations/conflicts | Skill | Read observer output, format for injection |
| Context injection | Skill | Formats clipboard content with source labels |
| Handoff | Skill | LLM sub-agent, enhanced with pre-extracted observations |

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

**Enhanced mode (when source session was observed, ADR-016):**

```
User (in Claude Code): /threadhop:handoff abc123

1. Skill checks observations.jsonl for session abc123
2. Observations exist → use structured input instead of raw JSONL
3. Spawns sub-agent with:
   - Pre-extracted observations (typed, structured)
   - Any detected conflicts involving this session's decisions
   - Prompt: "Generate a handoff brief from these structured observations.
     Include: decisions with rationale, open TODOs, unresolved conflicts,
     and what the next session needs to know."
4. Sub-agent returns brief (~30-50 lines)
5. Brief injected into current conversation

Fallback: if no observations exist, reverts to full JSONL mode above.
```

### Skill 4: `/threadhop:observe` (instant, spawns background process)

Per-session opt-in for background observation (ADR-016). The user decides
which conversations are worth observing. Can be invoked at any point —
beginning or mid-conversation.

```
User (in Claude Code): /threadhop:observe

1. Skill detects current session ID from process context
2. Checks if observer is already running for this session
   - If yes: "Already observing this session."
3. Spawns observer as background process:
   threadhop observe --session <session_id> &
4. Observer performs retroactive catch-up:
   - Reads entire JSONL from byte 0
   - Processes all existing messages through Haiku
   - Extracts typed observations (todo | decision | done | adr | observation)
5. Observer switches to watch mode (fsevents / polling)
6. Confirms: "Observing this session. 47 messages processed retroactively
   — found 5 decisions, 3 TODOs, 1 ADR. Watching for new messages."
7. User continues working — observer runs silently
```

### Skill 5: `/threadhop:insights` (instant, no LLM)

Pull-based context injection for observations and conflicts (ADR-016).
Reads from the observer's output files and formats findings into the
current conversation.

```
User (in Claude Code): /threadhop:insights

1. Skill detects current session ID
2. Reads observations.jsonl filtered by current session
3. Reads conflicts.jsonl filtered by current project
4. Formats and presents:

   ┌─ ThreadHop Observations — this session ───────────────┐
   │ DECISIONS:                                             │
   │  • REST for client API (rationale: SDK constraints)    │
   │  • Token bucket for rate limiting                      │
   │ TODOs:                                                 │
   │  • Implement /workflows endpoint                       │
   │  • Write integration tests for auth flow               │
   │ ADRs:                                                  │
   │  • ADR-003: Chunk merging for assistant messages        │
   │ CONFLICTS:                                             │
   │  ⚠ Session "infra-design" decided "gRPC for all        │
   │    services" — contradicts "REST for client API" above  │
   └────────────────────────────────────────────────────────┘

5. The model now has this context and can work with it
```

`/threadhop:insights` without an observed session shows nothing useful.
It reads from files that only exist because `/threadhop:observe` was
invoked. This coupling is intentional — no observation, no insights.

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
- [ ] Context-aware help overlay + shared command metadata registry

### Phase 3: CLI + Observer (on-demand + background)
- [ ] Add argparse subcommand routing (no subcommand = TUI)
- [ ] Implement `threadhop tag <status> [--session <id>]`
- [ ] Session auto-detection from current terminal (ps/lsof)
- [ ] Haiku observer: process conversation chunks, extract typed observations
- [ ] Observations JSONL output at `~/.config/threadhop/observations.jsonl`
- [ ] Incremental processing (byte offset tracking per session)
- [ ] Background observer mode: `threadhop observe` sidecar process (ADR-015)
- [ ] File-watching via fsevents (macOS) with polling fallback
- [ ] Auto-detect active session JSONL, exit when session ends
- [ ] `threadhop todos [--project]` CLI query
- [ ] `threadhop decisions [--project]` CLI query
- [ ] `threadhop observations [--project]` CLI query

### Phase 4: Skills (ADR-012, ADR-016)
- [ ] Research Claude Code skill plugin packaging
- [ ] `/threadhop:tag <status>` skill (calls CLI)
- [ ] `/threadhop:context` skill (clipboard formatting + injection)
- [ ] `/threadhop:handoff <id> [--full]` skill (sub-agent, enhanced with observations)
- [ ] `/threadhop:observe` skill (per-session opt-in, spawns background observer)
- [ ] `/threadhop:insights` skill (pull observations + conflicts into conversation)

### Phase 5: Memory + Bookmarks
- [ ] Build bookmark system (TUI feature)
- [ ] Explicit annotation detection (ADR:, DECISION:, TODO: markers)
- [ ] Project memory markdown rendering from observations

### Phase 6: Reflector — Conflict Detection (ADR-015)
- [ ] Build conflict detection reflector (Haiku)
- [ ] Group decisions by project/topic, identify contradictions across sessions
- [ ] Conflict reports at `~/.config/threadhop/conflicts.jsonl`
- [ ] Background reflector mode (runs alongside observer sidecar)
- [ ] `threadhop conflicts [--project]` CLI query
- [ ] TUI notification for unreviewed conflicts
- [ ] Condensation: merge related decisions, archive completed TODOs
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
**Revised (ADR-015 supersedes original resolution):** Two modes:
- **On-demand (ADR-010):** CLI query or TUI launch triggers observation of
  unprocessed messages. `threadhop todos` processes first, then displays.
- **Background (ADR-015):** `threadhop observe` runs as a sidecar process,
  watching the active session's JSONL via fsevents. Enabled as a flag,
  similar to Claude Code's remote control mode. The Claude Code terminal
  does NOT pause — the observer is a background process, not a daemon.
Both modes coexist. Background mode is additive — on-demand still works.

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
