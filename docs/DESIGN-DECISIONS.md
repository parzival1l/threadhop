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

**Observer uses Haiku via `claude -p`** (amended 2026-04-17, see ADR-018):
- Invoked as `claude -p --model haiku --permission-mode acceptEdits`
- NOT the Anthropic API — uses the same Claude subscription, same binary
- ~200ms response time — fast and cheap under one subscription
- Processes conversation chunks and outputs typed JSONL observations
- Types: `todo | decision | done | adr | observation | conflict`
- Prompt: extract only explicitly discussed items, do not infer
- Reusable prompt lives at `~/.config/threadhop/prompts/observer.md`

**Observations stored as per-session JSONL** (amended 2026-04-17, see ADR-019)
at `~/.config/threadhop/observations/<session_id>.jsonl`:
```jsonl
{"type":"decision","text":"REST over gRPC","context":"Client SDK constraints","ts":"2026-04-14T10:30:00Z"}
{"type":"todo","text":"Implement /workflows endpoint","context":"","ts":"2026-04-14T11:15:00Z"}
```

One file per session — the session ID is in the filename, not duplicated in
every line. Project is looked up from the `sessions` table. Byte offsets are
tracked in the `observation_state` table, not in observation entries. Each
line stays minimal: type + text + context + timestamp.

JSONL format means observations are queryable without any app — `grep`, `jq`,
or the ThreadHop CLI all work. Per-session scoping means single-session
queries read one file, not grep through a global log.

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

### ADR-012: Two skills — context, handoff (tagging uses bash passthrough, not a skill)

**Context:** Need to interact with ThreadHop from within a Claude Code session
without switching to the TUI or a terminal. Earlier design had three skills
including `/threadhop:tag`; that skill was dropped in favor of the `!`
bash passthrough (see ADR-013 evolution note) — skills invoke the LLM,
which is pure overhead for a one-shot SQLite write.

**Decision:** Two Claude Code skills with distinct roles, plus the `!`
passthrough for tagging:

| Surface | What it does | Uses LLM? |
|---|---|---|
| `!threadhop tag <status>` (bash passthrough) | Tags current session | No — direct shell invocation, no turn |
| `/threadhop:context` | Formats clipboard content as sourced context | No — reads `pbpaste`, formats |
| `/threadhop:handoff <id>` | Compresses a full session into a brief | Yes — sub-agent with Haiku |

**`!threadhop tag <status>`** — zero LLM turn:
1. Claude Code's `!` prefix runs the command directly in the host shell
2. `threadhop tag` auto-detects the current session id from its process ancestry
3. Prints one tight line: `✓ tagged <short-id> as <status>`

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
- Tag: the `!` bash passthrough is zero-LLM, already supported by
  Claude Code, and surfaces in `!`-history autocomplete after first use.
  No reason to burn a skill on it.
- Context: bridges TUI (visual selection) to Claude Code (injection)
- Handoff: the only one that needs an LLM, clearly separated

**Rejected:**
- Merging context and handoff into one skill (different mechanisms,
  different cost profiles).
- A dedicated `/threadhop:tag` skill: a skill invokes the LLM — pure
  overhead for a one-shot SQLite write. Users who want slash-style
  ergonomics can install the optional `UserPromptSubmit` hook documented
  in the README. Trade-off accepted: no `/` autocomplete discoverability.

---

### ADR-013: Session tagging from three entry points

**Context:** Session tags (backlog, in_progress, in_review, done, archived)
need to be settable from multiple places depending on the user's context.

**Decision:** Three entry points, one database:

| Entry point | How | When you'd use it |
|---|---|---|
| ThreadHop TUI | Press `s` to cycle status | Triaging multiple sessions |
| Terminal CLI | `threadhop tag backlog` | Quick tag from another tab |
| In-session bash passthrough | `!threadhop tag backlog` from inside Claude Code | Mid-conversation, without leaving |

All three write to the same SQLite `sessions` table. The TUI reflects
changes from CLI/passthrough on the next 5s refresh.

**Rationale:**
- Different moments call for different interfaces
- Shared database means no sync issues
- The in-session entry point is the `!` bash passthrough — zero LLM turn,
  instantaneous, already built into Claude Code. An optional
  `UserPromptSubmit` hook gives `/tag <status>` ergonomics for users who
  want it (documented in README).

**Evolution:** Earlier revisions of this ADR proposed a `/threadhop:tag`
skill as the in-session entry point. A skill invokes the LLM — pure
overhead for a one-shot SQLite write. The `!` passthrough delivers the
same outcome with no model call, and its history is surfaced in
Claude Code's `!`-history autocomplete after first use. Trade-off: no
`/` autocomplete discoverability. Mitigated by a README section and the
optional hook.

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
Observer process (claude -p --model haiku --permission-mode acceptEdits)
    ↓ appends typed observations
~/.config/threadhop/observations/<session_id>.jsonl    ← per-session file (ADR-019)
    ↑ reads periodically (every 5-6 new entries)
Reflector process (claude -p --model haiku, companion to observer)
    ↓ appends type:"conflict" entries to SAME file     ← unified output (ADR-020)
~/.config/threadhop/observations/<session_id>.jsonl
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

**Reflector behaviour — conflict detection (amended 2026-04-17, see ADR-020):**

The reflector's purpose is NOT condensation (Mastra's approach). It is
specifically to **detect contradictory decisions** across sessions.

Example conflict:
```jsonl
{"type":"decision","text":"REST over gRPC for client API","project":"atlas","session":"abc","ts":"..."}
{"type":"decision","text":"gRPC for all service-to-service comms","project":"atlas","session":"def","ts":"..."}
→ Reflector appends: {"type":"conflict","text":"REST vs gRPC scope overlap","refs":["abc","def"],...}
```

The Reflector:
1. Runs as a companion to the observer, NOT independently triggered
2. Accumulates like the observer — processes every 5-6 new messages, not per-decision
3. Groups decisions by project and semantic topic
4. Uses Haiku to identify contradictions between decisions
5. **Appends conflict entries to the SAME per-session observation JSONL** (ADR-020) —
   no separate `conflicts.jsonl`. Conflicts are `type: "conflict"` entries alongside
   decisions, TODOs, etc. Forward-only, append-only — same constraints as observer.
6. Surfaces conflicts via TUI notification or CLI query (`threadhop conflicts`)

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

**Pull-based context injection — new session (handoff, amended 2026-04-17):**

The handoff skill always uses the observer as its underlying function.
There is no separate "compress from raw JSONL" path. See ADR-018 for
the observer-as-core-function principle.

```
User (new session): /threadhop:handoff abc123

If observations exist for abc123:
  1. Read observations/<session_id>.jsonl
  2. Observations already typed, structured, compressed
  3. Format into handoff brief (may use Haiku for final polish)
  4. Brief includes: decisions with rationale, open TODOs,
     unresolved conflicts, current state

If NO observations exist for abc123:
  1. Run the observer on the full session (from byte 0)
  2. Observer processes entire JSONL, writes observations/<session_id>.jsonl
  3. Read the freshly-written observations
  4. Format into handoff brief
  → Same result as if the session had been observed all along
```

The observer is the core function — handoff is an entry point that
runs the observer first (if needed), then formats. No separate
compression path exists. This guarantees identical results whether
a session was observed incrementally or in one shot at handoff time.

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
  1. /threadhop:handoff A → runs observer if needed, formats from observations
  2. /threadhop:insights → includes conflict entries from the same observation file
  3. User resolves conflict in this conversation
  4. /threadhop:observe → now observing Session B, captures the resolution
  5. Resolution appears in observations/<session_B_id>.jsonl as a new decision
```

The loop closes: **observe → extract → surface → resolve → observe the
resolution**. Each session can opt in independently. Observations accumulate
across sessions. Conflicts are detected cross-session and surfaced on demand.

**Updated in-session surface — four skills + one bash passthrough for tagging:**

| Surface | What it does | LLM? | New? |
|---|---|---|---|
| `!threadhop tag` (bash passthrough) | Tag session status | No | Existing (ADR-012, ADR-013) — replaces former `/threadhop:tag` skill |
| `/threadhop:context` | Inject clipboard content | No | Existing (ADR-012) |
| `/threadhop:handoff` | Generate handoff brief | Yes | Enhanced (ADR-016) |
| `/threadhop:observe` | Start background observer for this session | No | New (ADR-016) |
| `/threadhop:insights` | Pull observations + conflicts into conversation | No | New (ADR-016) |

The bash-passthrough tag costs no LLM turn. The observe skill itself is
instant (spawns a process). The background observer uses Haiku. The
insights skill is instant (reads files, formats).

**Rationale:**
- Per-session opt-in respects the user's attention — only important
  conversations get the Haiku cost
- Mid-conversation trigger with retroactive catch-up means you never miss
  context, even if you decide to observe 30 minutes into a discussion
- Pull-based injection is the only model that works with Claude Code's
  black-box architecture
- Enhanced handoff with observations is strictly better — faster (less
  input to process) and higher quality (structured vs raw)
- Four skills + a bash passthrough is still manageable — each does exactly one thing

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

### ADR-018: Observer as core function — `claude -p` invocation, not API

**Context:** ADR-010 and ADR-015 described the observer using "Haiku" without
specifying the invocation mechanism. There was ambiguity about whether this
meant an Anthropic API call (requiring an API key and separate billing) or
something else. The intent was always to use the same Claude subscription.

**Decision:** The observer invokes `claude -p --model haiku --permission-mode
acceptEdits` — headless Claude Code, not the Anthropic API.

**Invocation:**

```bash
claude -p "$(cat ~/.config/threadhop/prompts/observer.md)

<session_chunk>
$(tail -c +$BYTE_OFFSET <source_jsonl_path>)
</session_chunk>

Append observations to: $OBS_FILE_PATH" \
  --model haiku \
  --permission-mode acceptEdits
```

**The observer prompt** lives at `~/.config/threadhop/prompts/observer.md`
(or bundled with the app). It is a reusable, static prompt that constrains:

1. **Append-only**: You may only append to the observation file. Never delete
   or modify existing lines.
2. **One JSON line per observation**: If you identify 3 decisions, write 3
   separate JSON lines. Each line is a complete, self-contained JSON object.
3. **No permission to delete**: The `acceptEdits` mode allows file writes
   but the prompt explicitly forbids deletion or modification of existing
   content.
4. **Typed extraction only**: Extract items that were explicitly discussed.
   Do not infer, speculate, or synthesize. Types:
   `todo | decision | done | adr | observation | conflict`

**Observation JSONL line format (minimal — metadata lives elsewhere):**

```jsonl
{"type":"decision","text":"REST over gRPC","context":"Client SDK constraints","ts":"2026-04-14T10:30:00Z"}
{"type":"todo","text":"Implement /workflows endpoint","context":"","ts":"2026-04-14T11:15:00Z"}
```

Each line has only four fields: `type`, `text`, `context`, `ts`. No session ID
(encoded in filename: `observations/<session_id>.jsonl`), no project (looked
up from `sessions` table), no byte offset (tracked in `observation_state`
table). This keeps every line minimal and avoids duplicating metadata that
the caller already knows.

**Why `claude -p` and not the Anthropic API:**
- No API key management — uses the same Claude subscription
- No separate billing — all under one account
- Uses the same `claude` binary already installed
- `--permission-mode acceptEdits` provides just enough filesystem access
  to append to the observation file, nothing more
- The observer process is just another `claude -p` invocation with a
  crafted prompt — same as how the user uses Claude Code

**The observer is the core function:**

Every feature that needs observations uses the same observer logic:

| Entry point | Calls observer? | Then what? |
|---|---|---|
| `threadhop observe --session X` (CLI) | Yes, in watch mode | Keeps running, appends as messages arrive |
| `/threadhop:observe` (skill) | Yes, spawns background | Same as CLI but auto-detects session |
| `/threadhop:handoff X` (skill) | Yes, if no observations exist | Runs observer on full session, then formats |
| `threadhop todos` (CLI query) | Yes, on-demand for unprocessed | Then filters and displays |
| TUI re-observe trigger | Yes, resumes from last offset | Same observer, picks up where it left off |

All entry points produce identical observations. A session observed
incrementally over 2 hours produces the same JSONL as one observed
in a single shot at handoff time. The observer function is deterministic
given the same input — entry point doesn't affect output.

**Rationale:**
- Single function, multiple entry points — no code duplication
- Reusable prompt file means the extraction logic is testable and
  versionable independently of the app code
- `acceptEdits` is the minimum permission — can't read arbitrary files,
  can't execute commands, can only write to the specified output path
- Headless mode means the observer process is invisible to the user

---

### ADR-019: Per-session observation files with SQLite state tracking

**Context:** ADR-010 stored all observations in a single global
`observations.jsonl`. With per-session opt-in (ADR-016) and the observer
as a core function (ADR-018), observations need to be scoped to individual
sessions. The observer also needs state persistence — byte offsets, PID
tracking, observation counts — to support stop/resume, TUI indicators,
and handoff lookups.

**Decision:** One observation file per session, state tracked in SQLite.

**File layout:**

```
~/.config/threadhop/observations/
  abc123.jsonl          ← observations for session abc123
  def456.jsonl          ← observations for session def456
```

Per-session files make:
- Session-scoped queries instant (read one file, not grep through global)
- The "has observations" indicator trivial (file exists + entry count > 0)
- Handoff a single file read
- Cleanup straightforward (delete when session archived)

**SQLite state table (`observation_state`, updated with reflector offset per ADR-022):**

```sql
CREATE TABLE observation_state (
    session_id              TEXT PRIMARY KEY,
    source_path             TEXT NOT NULL,       -- path to source session JSONL
    obs_path                TEXT NOT NULL,       -- path to observations JSONL
    source_byte_offset      INTEGER NOT NULL DEFAULT 0,  -- where observer last read
    entry_count             INTEGER NOT NULL DEFAULT 0,   -- total observations written
    reflector_entry_offset  INTEGER NOT NULL DEFAULT 0,   -- last entry reflector processed
    observer_pid            INTEGER,             -- PID if running, NULL otherwise
    status                  TEXT NOT NULL DEFAULT 'idle',
        -- idle | running | stopped
    started_at              REAL,                -- when observation first started
    last_observed_at        REAL,                -- when last observation appended
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
```

**State lifecycle:**

```
idle → running (observer starts, PID recorded)
  ↓
running → stopped (observer exits or user stops it)
  ↓
stopped → running (user re-observes, resumes from source_byte_offset)
```

**Re-observation from any entry point:**

When the user triggers observation on a session that was previously
observed (and stopped), the observer checks `source_byte_offset`:
- If the source JSONL has grown since last observation → process only
  new bytes (from offset to EOF)
- If no new bytes → "Already up to date. N observations on file."
- Then switches to watch mode (if background) or exits (if on-demand)

This is the same incremental logic regardless of entry point. The state
table is the single source of truth for "where did we leave off."

**PID tracking for lifecycle management:**

The `observer_pid` column enables:
- TUI detection of "is this session currently being observed?"
- `threadhop observe --stop` sends SIGTERM to the recorded PID
- Stale PID detection: if the PID is recorded but the process is dead
  (checked via `kill -0 $PID`), status is corrected to `stopped`
- `threadhop observe --stop-all` queries all rows with non-null PIDs

**Stop mechanisms:**

```bash
threadhop observe --stop                    # stops current session's observer
threadhop observe --stop --session abc123   # stops specific observer
threadhop observe --stop-all                # stops all running observers
```

All use SIGTERM to the recorded PID. Observer handles SIGTERM gracefully:
flushes any pending observations, updates `source_byte_offset` in SQLite,
sets `status = 'stopped'`, exits cleanly.

**Rationale:**
- Per-session files eliminate grep/filter overhead for single-session queries
- SQLite state tracking enables stop/resume, indicator queries, stale
  PID detection — all the lifecycle management the TUI and CLI need
- `entry_count` is maintained alongside byte offset so the TUI indicator
  can check "has observations" without reading the file
- The state table bridges all entry points — CLI, skill, TUI all read
  and write the same state

---

### ADR-020: Unified observation JSONL — observer and reflector share one file

**Context:** ADR-015 originally specified separate files: `observations.jsonl`
for the observer and `conflicts.jsonl` for the reflector. With per-session
files (ADR-019), a separate conflicts file per session adds storage overhead
and query complexity. The reflector's output (conflict entries) is semantically
an observation — it's an insight extracted from the conversation, just at a
higher level of abstraction.

**Decision:** Observer and reflector both append to the same per-session
observation JSONL. No separate `conflicts.jsonl`. Conflicts are entries
with `type: "conflict"` alongside all other observation types.

**Unified entry types:**

```jsonl
{"type":"decision","text":"REST over gRPC","context":"SDK constraints","ts":"..."}
{"type":"todo","text":"Implement /workflows endpoint","context":"","ts":"..."}
{"type":"conflict","text":"REST vs gRPC scope overlap","refs":["abc123","def456"],"topic":"api-protocol","ts":"..."}
{"type":"done","text":"Auth flow tests passing","context":"","ts":"..."}
```

Lines are minimal — session and project are NOT stored per line. Session is
encoded in the filename. Conflict entries add `refs` and `topic` for
cross-session linking and dedup.

**Write rules (same for observer and reflector):**
1. **Forward-only**: Append new lines. Never delete or modify existing lines.
2. **One JSON line per entry**: Self-contained, independently parseable.
3. **No edit permission**: Even if a TODO is marked done later, a new
   `type: "done"` entry is appended — the original TODO line remains.

**Reflector cadence:**

The reflector does NOT wake on every new decision. It accumulates like
the observer — every 5-6 new messages worth of observations, it scans
for contradictions across the project's sessions. It is a companion
process to the observer, not an independent daemon.

```
Observer appends observations → reflector notices growth
  → after 5-6 new entries, reflector reads recent decisions
  → compares with decisions from other sessions in same project
  → appends type:"conflict" entries if contradictions found
```

**Why not a separate conflicts file:**
- One file per session is simpler to manage, query, and clean up
- Conflicts are semantically observations — higher-level ones
- The insights skill reads one file, not two
- The handoff skill reads one file, not two
- `grep "conflict" observations/abc123.jsonl` works for quick conflict checks
- Append-only, forward-only means no write contention between observer and
  reflector — they can both safely append to the same file

**Rejected:** Separate `conflicts.jsonl` per session (extra file overhead,
split queries, two files to manage per session).
**Rejected:** Global `conflicts.jsonl` (requires filtering, defeats
per-session scoping).

---

### ADR-022: Reflector implementation — prompt, invocation, and state tracking

**Context:** ADR-015 and ADR-020 established that the reflector detects
contradictory decisions across sessions and appends `type: "conflict"`
entries to the same per-session observation JSONL. But the design never
specified how the reflector is actually invoked, what input it receives,
what its prompt looks like, or how it tracks its own state. Unlike the
observer (which reads raw JSONL transcripts), the reflector works at the
**observation layer** — it reads decisions from observation files, not
source transcripts. This makes its input shape fundamentally different.

**Decision:** The reflector is a second `claude -p` call, triggered by
the observer process, with its own prompt and its own offset tracking.

**Input shape — observer vs reflector:**

```
Observer reads:        Source JSONL (raw transcript) → extracts observations
Reflector reads:       Observation JSONLs (extracted decisions) → finds contradictions
```

The reflector never touches the source transcripts. It operates entirely
on the already-extracted observation layer. Its input is:

1. **Recent decisions from the current session** — new `type: "decision"`
   entries since the reflector last ran (tracked by `reflector_entry_offset`)
2. **All decisions from other sessions in the same project** — gathered by
   scanning `observations/<other_session>.jsonl` files that share the same
   `project` value

Both sets are piped into the prompt as structured input.

**Invocation:**

```bash
claude -p "$(cat ~/.config/threadhop/prompts/reflector.md)

<current_session_decisions>
# Recent decisions from session abc123 (since reflector last ran)
$(jq -c 'select(.type==\"decision\")' observations/abc123.jsonl | tail -n +$REFLECTOR_OFFSET)
</current_session_decisions>

<project_decisions>
# All decisions from other sessions in project 'atlas'
$(for f in observations/*.jsonl; do
    jq -c 'select(.type==\"decision\" and .project==\"atlas\")' "$f"
  done | grep -v '\"session\":\"abc123\"')
</project_decisions>

If you find contradictions, append conflict entries to: observations/abc123.jsonl" \
  --model haiku \
  --permission-mode acceptEdits
```

**The reflector prompt** lives at `~/.config/threadhop/prompts/reflector.md`
(or bundled with the app at `prompts/reflector.md`, alongside `observer.md`).
It constrains:

1. **Append-only**: Same rules as observer — forward-only, no deletions.
2. **One JSON line per conflict**: Each conflict is a self-contained entry.
3. **Conflict deduplication**: Before appending, check if the same pair of
   sessions + same topic already has a conflict entry. If yes, skip.
   (The prompt includes existing conflict entries for this check.)
4. **Structured conflict format**: Each conflict entry must reference both
   sessions and explain the contradiction clearly.

**Conflict entry format:**

```jsonl
{"type":"conflict","text":"REST vs gRPC scope overlap — session abc decided REST for client API, session def decided gRPC for all services","refs":["abc123","def456"],"topic":"api-protocol","ts":"2026-04-14T12:00:00Z"}
```

Fields:
- `type`: always `"conflict"`
- `text`: concise explanation of the contradiction
- `refs`: array of session IDs involved in the contradiction
- `topic`: semantic grouping key (helps dedup and display)
- `ts`: ISO 8601 timestamp

The conflict entry does **not** include inline `session` or `project` fields.
The session is implied by which observation file the entry was appended to,
and project context comes from SQLite session mapping.

**Trigger mechanism — observer spawns reflector:**

The reflector is NOT an independent process. The observer triggers it:

```
Observer loop:
  1. Watch source JSONL for new messages
  2. When ~3-4 new messages: run observer extraction (claude -p)
  3. Observer appends observations, increments entry_count
  4. Check: has entry_count grown by ≥5 since reflector_entry_offset?
     → YES: spawn reflector (claude -p with reflector prompt)
     → NO: continue watching
  5. Reflector appends any conflicts, updates reflector_entry_offset
```

The observer process owns the reflector's lifecycle. There is no separate
reflector daemon, PID, or stop mechanism. When the observer stops, the
reflector stops. When the observer resumes, the reflector resumes from
its own offset.

**On-demand reflector (for handoff and CLI queries):**

When the observer core function runs on-demand (e.g., `threadhop handoff`
or `threadhop conflicts`), the reflector runs as a follow-up step:

```
threadhop conflicts --project atlas:
  1. For each session in project: run observer if unprocessed messages exist
  2. Run reflector for each session with new decisions
  3. Display all type:"conflict" entries across project
```

Same function, different trigger — just like the observer.

**State tracking — reflector offset in observation_state:**

The `observation_state` table gains one column for the reflector:

```sql
ALTER TABLE observation_state ADD COLUMN
    reflector_entry_offset  INTEGER NOT NULL DEFAULT 0;
    -- last entry index the reflector has processed
```

This means:
- `entry_count = 15, reflector_entry_offset = 10` → 5 unprocessed entries,
  reflector should run
- `entry_count = 15, reflector_entry_offset = 15` → up to date, skip
- After reflector runs: `reflector_entry_offset = entry_count`

The offset tracks entries (line count in observation JSONL), not bytes —
because the reflector reads structured observations, not raw transcript.

**Where conflicts are written — single-session scoping:**

When session A said "REST" and session B said "gRPC", the conflict is
written to **the session currently being observed** (the one whose observer
triggered the reflector). The `refs` array links both sessions.

If the other session is later observed, the reflector will discover the
same contradiction from the other side and write its own conflict entry
there. This is intentional — each session's observation file tells its
own complete story, including conflicts it's involved in.

**Deduplication prevents noise:** The reflector prompt includes existing
`type: "conflict"` entries from the current session. Before writing a new
conflict, it checks: "is there already a conflict entry with the same
`refs` pair and `topic`?" If yes, skip. This means re-running the
reflector is idempotent.

**Prompt file layout:**

```
~/.config/threadhop/prompts/
  observer.md           ← extraction prompt (ADR-018)
  reflector.md          ← conflict detection prompt (this ADR)
```

**Rationale:**
- Observer triggers reflector → no extra daemon, no extra PID management
- Reflector operates on observation layer, not transcript layer — smaller
  input, faster processing, and it doesn't need to understand raw JSONL
- Entry-count offset tracking is simpler than byte offsets (observations
  are structured, line-per-entry)
- Single-session scoping with `refs` links means each session file is
  self-contained while still enabling cross-session conflict queries
- Dedup in the prompt means reflector is idempotent — safe to re-run
- Same `claude -p --model haiku --permission-mode acceptEdits` invocation
  as observer — no new execution model to build

---

### ADR-021: Observation indicator in TUI session list + transcript header

**Context:** When a session has been observed (observations exist), the
user needs a way to know this from the TUI without opening the transcript
or running a CLI command. The existing session status circles (◐ ● ○)
indicate process state and should not be overloaded with observation state —
remembering what each circle variant means is already enough cognitive load.

**Decision:** Add a small notepad-style icon next to the session name for
observed sessions. Add a subtle header line in the transcript view showing
the observation file path and entry count.

**Session list indicator:**

```
● my-session 🗒             ← observed (has observations)
○ another-session            ← not observed
◐ active-work                ← working, no observations
◐ active-work 🗒             ← working AND observed
```

The `🗒` (or a terminal-safe fallback like `≡` or `[O]`) appears after the
session name when `observation_state.entry_count > 0` for that session.
This is checked during the existing 5s refresh cycle — no extra DB queries.

If emoji rendering is unreliable across terminals, fall back to a Rich
markup colored marker: `[dim]≡[/dim]` or `[dim cyan]obs[/dim cyan]`.

**Transcript header (Option B — subtle, non-interfering):**

When viewing a transcript that has observations, show a one-line header
above the first message:

```
─── 🗒 12 observations · ~/.config/threadhop/observations/abc123.jsonl ───
```

This header:
- Is positioned above the transcript content, below the session title area
- Does NOT interfere with the persistent search bar (which is at the bottom)
- Is static (not a focusable widget) — purely informational
- Shows the entry count and file path for quick reference
- Can be selected/copied for use in another terminal

**TUI action for observation path:**

When an observed session is highlighted in the session list, pressing `o`
(for "observations") copies the file path to clipboard:

```
~/.config/threadhop/observations/abc123.jsonl
```

Notification: "Observation path copied — view in terminal or IDE"

This gives the user a fast path to `cat`, `jq`, or IDE-open the
observation file without remembering the path structure.

**TUI re-observe trigger:**

When pressing `o` on a session that has NO observations yet, instead of
copying a non-existent path, offer to start observation:

```
Press o on unobserved session → "No observations yet. Start observing? (y/n)"
  y → spawns observer, same as `threadhop observe --session <id>`
  n → dismiss
```

When pressing `o` on a session that has observations but the observer is
stopped, offer to resume:

```
Press o on observed+stopped session → copies path (observations exist)
Press O (shift) → "Resume observing? (y/n)"
  y → resumes from last byte offset
```

**Rationale:**
- The notepad icon is additive — doesn't change the meaning of existing circles
- The transcript header is passive (no interaction needed) and positioned
  to avoid conflicting with search or footer
- Copying the file path is the lowest-overhead way to bridge TUI → terminal/IDE
- Re-observe from TUI closes the loop: discover observations exist → re-observe
  if session has grown → observations update

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
Observer uses `claude -p --model haiku --permission-mode acceptEdits` (ADR-018).
Per-session observation files with SQLite state tracking (ADR-019)._

1. Add argparse subcommand routing: no subcommand = TUI, with subcommand = CLI
2. Implement `threadhop tag <status> [--session <id>]`
   - Auto-detect session from current terminal when `--session` omitted
3. Create reusable observer prompt at `~/.config/threadhop/prompts/observer.md`
   - Append-only, one JSON line per observation, typed extraction only
   - Types: `todo | decision | done | adr | observation | conflict`
4. Add `observation_state` table to SQLite schema (ADR-019)
5. Implement observer core function (ADR-018):
   - Reads source JSONL from `source_byte_offset` (or byte 0 for new sessions)
   - Invokes `claude -p --model haiku --permission-mode acceptEdits`
   - Appends typed observations to `~/.config/threadhop/observations/<session_id>.jsonl`
   - Updates `source_byte_offset` and `entry_count` in SQLite
   - Same function used by all entry points (CLI, skill, handoff, TUI)
6. Implement background observer mode (ADR-015):
   - `threadhop observe --session <id>` — runs as background sidecar
   - File-watching via fsevents (macOS), fallback to polling
   - Batched extraction (configurable, default ~3-4 new messages trigger)
   - Records PID in `observation_state.observer_pid`
   - Exits when Claude Code session ends or `--stop` is sent
7. Implement stop/resume lifecycle (ADR-019):
   - `threadhop observe --stop [--session <id>]` — SIGTERM to recorded PID
   - `threadhop observe --stop-all` — stops all running observers
   - Resume: reads `source_byte_offset`, processes only new bytes
   - Stale PID detection via `kill -0 $PID`
8. Implement CLI queries:
   - `threadhop todos [--project <name>]`
   - `threadhop decisions [--project <name>]`
   - `threadhop observations [--project <name>]`
   - `threadhop conflicts [--project <name>]` (reads `type: "conflict"` entries)
   - All trigger observer for unprocessed messages before displaying results

### Phase 4: Skill Plugin + TUI Observation Indicator
_Four skills for in-session use (ADR-012, ADR-016), plus the
`!threadhop tag` bash passthrough for tagging (ADR-013). TUI observation
indicator and transcript header (ADR-021)._

1. Research Claude Code skill plugin packaging/distribution
2. `!threadhop tag <status>` — document the bash passthrough + valid
   statuses in README. Optional: sample `UserPromptSubmit` hook for
   `/tag <status>` ergonomics. Replaces the former `/threadhop:tag` skill
   (ADR-013).
3. `/threadhop:context` — read clipboard, format with source labels, inject
4. `/threadhop:handoff <id> [--full]` — runs observer first if no observations
   exist (ADR-018), then formats from observations. No separate JSONL compression path.
5. `/threadhop:observe` — per-session opt-in, spawns background observer
   (retroactive catch-up + watch mode)
6. `/threadhop:insights` — reads per-session observation file (includes conflict
   entries from reflector), formats and injects into conversation
7. TUI observation indicator (ADR-021):
   - 🗒 icon next to session name when `observation_state.entry_count > 0`
   - Subtle transcript header: entry count + file path
   - `o` key: copy observation path to clipboard (or start observing if none)
   - `O` key: resume observation on a stopped session

### Phase 5: Project Memory + Bookmarks
_Cross-session knowledge persistence._

1. Add bookmarks table to schema
2. Bookmark action from message selection mode (`space` to toggle)
3. Bookmark browser panel in TUI
4. Explicit annotation detection: recognize "ADR:", "DECISION:", "TODO:" markers
   in conversations and auto-append to observations
5. Memory rendering: generate project memory markdown from observations for injection

### Phase 6: Reflector — Conflict Detection
_Contradiction detection across sessions. Second `claude -p` call triggered
by the observer process (ADR-022). Writes `type: "conflict"` entries to the
SAME per-session observation JSONL (ADR-020). Uses `reflector_entry_offset`
for incremental processing._

1. Create reflector prompt (`~/.config/threadhop/prompts/reflector.md`):
   - Input: recent decisions from current session + all decisions from
     other sessions in same project
   - Constrains: append-only, dedup by `refs` pair + `topic`
   - Output: `type: "conflict"` entries with `refs`, `topic`, `text`
2. Build reflector core function (Haiku via `claude -p`):
   - Reads decisions from observation files (NOT raw transcripts)
   - Gathers cross-session decisions by scanning `observations/*.jsonl`
   - Appends conflicts to current session's observation JSONL
   - Updates `reflector_entry_offset` in `observation_state`
3. Observer-triggered background mode:
   - Observer checks: `entry_count - reflector_entry_offset >= 5`
   - If yes: spawns reflector `claude -p` call
   - No separate PID — observer owns reflector lifecycle
3. TUI notification: surface unreviewed conflicts in sidebar or status bar
4. Condensation (secondary goal): merge related decisions, archive completed
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

-- Observer + reflector state tracking (ADR-019, ADR-022)
CREATE TABLE observation_state (
    session_id              TEXT PRIMARY KEY,
    source_path             TEXT NOT NULL,       -- path to source session JSONL
    obs_path                TEXT NOT NULL,       -- path to per-session observations JSONL
    source_byte_offset      INTEGER NOT NULL DEFAULT 0,  -- where observer last read in source
    entry_count             INTEGER NOT NULL DEFAULT 0,   -- total observation entries written
    reflector_entry_offset  INTEGER NOT NULL DEFAULT 0,   -- last entry index reflector processed
    observer_pid            INTEGER,             -- PID if running, NULL otherwise
    status                  TEXT NOT NULL DEFAULT 'idle',
        -- idle | running | stopped
    started_at              REAL,                -- when observation first started
    last_observed_at        REAL,                -- when last observation appended
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
```

---

## Skill Plugin Architecture

### Principle: Four skills + bash passthrough for tagging, clear boundaries

Skills are for operations invoked mid-conversation from Claude Code. The TUI
handles everything visual and instantaneous. The CLI handles queries and tagging
from the terminal. In-session tagging uses Claude Code's `!` bash passthrough
rather than a skill (zero LLM turn — see ADR-013). See ADR-012 (original
set) and ADR-016 (observe + insights).

### Plugin: `threadhop`

```
threadhop/
  skills/
    context.md          # /threadhop:context
    handoff.md          # /threadhop:handoff <session_id>
    observe.md          # /threadhop:observe
    insights.md         # /threadhop:insights

# Tagging: !threadhop tag <status> (bash passthrough, no skill needed)
# Optional: ~/.claude/hooks/threadhop-tag.sh for /tag <status> ergonomics — see README
```

### What lives where

| Feature | Lives in | Why |
|---------|----------|-----|
| Search | TUI | Per-keystroke instant, visual results |
| Message select + copy | TUI | Visual selection, clipboard transport |
| Message export to .md | TUI | Visual selection, writes to /tmp |
| Bookmark | TUI | Visual selection, one-key action |
| Tag session | TUI + CLI + `!` bash passthrough | Three entry points, one DB (ADR-013) |
| Observation queries | CLI | `threadhop todos`, `threadhop decisions` |
| Start observation | Skill + TUI | Per-session opt-in, spawns background process |
| Pull observations/conflicts | Skill | Read per-session observation file, format for injection |
| Context injection | Skill | Formats clipboard content with source labels |
| Handoff | Skill | Runs observer first if needed, formats from observations |
| Observation indicator | TUI | 🗒 icon + transcript header for observed sessions |

### Tag entry point 3: `!threadhop tag <status>` (bash passthrough, zero LLM turn)

Not a skill — Claude Code's `!` prefix runs the command directly in the
host shell. See ADR-013 for why tagging was moved off the skill plane.

```
User (in Claude Code): !threadhop tag backlog

1. Claude Code runs `threadhop tag backlog` in the host shell (no model turn)
2. threadhop auto-detects the current session id by walking the parent
   process tree for its `claude` CLI ancestor (task #17)
3. ThreadHop CLI writes the tag to SQLite
4. Prints one tight line: "✓ tagged <short-id> as backlog"
```

On detection failure the command exits `2` with the helpful error from
`_resolve_cli_session()` and makes no DB write.

Optional: a `UserPromptSubmit` hook can provide `/tag <status>` ergonomics
— documented in README. Hooks do not appear in `/` autocomplete or `/help`.

### Skill 1: `/threadhop:context` (instant, no LLM)

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

### Skill 2: `/threadhop:handoff <id> [--full]` (observer + format)

Uses the observer as its underlying function (ADR-018). There is no
separate "compress raw JSONL" path — the handoff always works from
observations, running the observer first if needed.

```
User (in Claude Code): /threadhop:handoff abc123

1. Skill checks observation_state for session abc123
2. If observations exist (entry_count > 0):
   a. Check if source JSONL has grown since last observation
   b. If yes: run observer on new bytes (incremental catch-up)
   c. Read observations/abc123.jsonl
3. If NO observations exist:
   a. Run observer on full session (from byte 0)
   b. Observer writes observations/abc123.jsonl
   c. Read the freshly-written observations
4. Format observations into handoff brief:
   - Short sets: format directly without another LLM call
   - Large sets: spawns Haiku sub-agent for final polish/compression
5. Brief injected into current conversation (~30-50 lines)

With --full flag:
   Sub-agent produces comprehensive handoff with rationale,
   code references, and conversation excerpts.
```

The observer function is the same regardless of entry point.
A session observed incrementally over 2 hours produces identical
observations to one observed in a single shot at handoff time.

### Skill 3: `/threadhop:observe` (instant, spawns background process)

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

### Skill 4: `/threadhop:insights` (instant, no LLM)

Pull-based context injection for observations and conflicts (ADR-016).
Reads from the observer's output files and formats findings into the
current conversation.

```
User (in Claude Code): /threadhop:insights

1. Skill detects current session ID
2. Reads observations/<session_id>.jsonl (single file contains all
   observation types including conflicts appended by the reflector)
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

5. The model now has this context and can work with it
```

`/threadhop:insights` without an observed session shows nothing useful.
It reads from per-session observation files that only exist because
the observer ran (via `/threadhop:observe`, handoff, or CLI). Conflict
entries from the reflector appear inline as `type: "conflict"` — no
separate file to read. This coupling is intentional — no observation,
no insights.

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
- [ ] Create reusable observer prompt (`~/.config/threadhop/prompts/observer.md`)
- [ ] Add `observation_state` table to SQLite schema (ADR-019)
- [ ] Observer core function: `claude -p --model haiku --permission-mode acceptEdits` (ADR-018)
- [ ] Per-session observation files at `~/.config/threadhop/observations/<session_id>.jsonl` (ADR-019)
- [ ] Incremental processing (byte offset tracking in `observation_state` table)
- [ ] Background observer mode: `threadhop observe --session <id>` sidecar (ADR-015)
- [ ] File-watching via fsevents (macOS) with polling fallback
- [ ] Batched extraction: trigger on ~3-4 new messages
- [ ] Observer stop/resume lifecycle: `--stop`, `--stop-all`, PID tracking (ADR-019)
- [ ] Stale PID detection via `kill -0 $PID`
- [ ] `threadhop todos [--project]` CLI query
- [ ] `threadhop decisions [--project]` CLI query
- [ ] `threadhop observations [--project]` CLI query
- [ ] `threadhop conflicts [--project]` CLI query (reads `type: "conflict"` entries)

### Phase 4: Skills + TUI Observation Indicator (ADR-012, ADR-016, ADR-021)
- [ ] Research Claude Code skill plugin packaging
- [ ] `!threadhop tag <status>` bash-passthrough workflow — README doc + optional UserPromptSubmit hook (ADR-013; replaces the former `/threadhop:tag` skill)
- [ ] `/threadhop:context` skill (clipboard formatting + injection)
- [ ] `/threadhop:handoff <id> [--full]` skill (runs observer first if needed, formats from observations)
- [ ] `/threadhop:observe` skill (per-session opt-in, spawns background observer)
- [ ] `/threadhop:insights` skill (reads unified per-session observation file)
- [ ] TUI observation indicator: 🗒 icon next to observed sessions (ADR-021)
- [ ] Transcript header: entry count + observation file path (ADR-021)
- [ ] `o` key: copy observation path / start observing (ADR-021)
- [ ] `O` key: resume observation on stopped session (ADR-021)

### Phase 5: Memory + Bookmarks
- [ ] Build bookmark system (TUI feature)
- [ ] Explicit annotation detection (ADR:, DECISION:, TODO: markers)
- [ ] Project memory markdown rendering from observations

### Phase 6: Reflector — Conflict Detection (ADR-015, ADR-020, ADR-022)
- [ ] Create reflector prompt (`prompts/reflector.md`) — dedup, structured conflict format
- [ ] Build reflector core function — second `claude -p` call, reads observation layer
- [ ] Observer-triggered reflector — spawns when `entry_count - reflector_entry_offset >= 5`
- [ ] On-demand reflector — runs as follow-up after observer in CLI queries and handoff
- [ ] TUI notification for unreviewed `type: "conflict"` entries
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
