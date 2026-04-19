# ThreadHop — Implementation Task List

Extracted from [DESIGN-DECISIONS.md](DESIGN-DECISIONS.md) and
[PERFORMANCE.md](PERFORMANCE.md). Last reconciled 2026-04-19.

---

## Phase 1: SQLite Foundation + Session Tags + Archive
_Immediate value, enables all future features._

- [ ] **1. Create SQLite DB module (init, migrate, query helpers)**
  Create SQLite DB at `~/.config/threadhop/sessions.db` with WAL mode. Include schema for `settings`, `sessions` tables. Write init and query helper functions. _(ADR-001)_

- [ ] **2. Migrate config.json → SQLite** *(blocked by: #1)*
  One-time migration on first run: move session names, session order, last-viewed timestamps from config.json into the sessions table. Keep config.json only for app-level settings (theme, sidebar_width). _(ADR-001)_

- [ ] **3. Add session status field + grouped display** *(blocked by: #1)*
  Add `status` field to session model with values: `active | in_progress | in_review | done | archived`. Render sessions grouped by status in the TUI sidebar. _(ADR-004, ADR-013)_

- [ ] **4. Implement status cycling keybinds (s/S)** *(blocked by: #3)*
  `s` cycles status forward (active → in_progress → in_review → done), `S` cycles backward. Manual reorder works within each status group. First of three tag entry points. _(ADR-013)_

- [ ] **5. Implement archive (a) + archive toggle (A)** *(blocked by: #3)*
  `a` sets session status to archived. `A` toggles visibility of archived sessions. Archived sessions hidden by default. _(ADR-004)_

- [ ] **6. Implement sidebar resize ([/])**
  Add `[` and `]` keybindings to shrink/grow sidebar. Min 20, max 60, step 4. Persist width in config. Update `grid-columns` CSS dynamically via `self.styles.grid.columns`. _(ADR-014)_

- [ ] **7. Write tests for DB migration** *(blocked by: #1, #2)*
  Test that config.json values are correctly migrated to SQLite. Test idempotency (running migration twice doesn't duplicate data). Test that config.json is preserved for app-level settings.

---

## Phase 2: FTS Index + Message Selection + Search
_Enables instant search and cross-session context sharing. All TUI features._

- [ ] **8. Build JSONL indexer with chunk merging** *(blocked by: #1)*
  Parse JSONL files, group consecutive assistant lines by `message.id`, concatenate text content, use first `uuid` as PK. Strip system-reminders, abbreviate tool calls. Populate `messages` table. _(ADR-003)_

- [ ] **9. Implement incremental indexing (byte offset tracking)** *(blocked by: #8)*
  Use `index_state` table to track byte offset per session file. On each refresh, only parse new bytes appended since last index. Piggyback on the existing 5s refresh cycle.

- [ ] **10. Add message selection mode (m to enter, j/k navigation)**
  Press `m` to enter message selection mode in transcript view. `j`/`k` moves between messages. Messages become focusable/highlightable via CSS classes. _(ADR-006, ADR-008)_

- [ ] **11. Add range selection (v + movement)** *(blocked by: #10)*
  In message selection mode, press `v` to start range selection. Move with `j`/`k` to extend the range. Visual highlight on all selected messages.

- [ ] **12. Clipboard copy with source labels (y)** *(blocked by: #10)*
  Press `y` on selected messages to copy to clipboard. Format includes source labels: `[From "session name" — ~/cwd — timestamp]`. Source labels are what `/threadhop:context` later parses. _(ADR-008)_

- [ ] **13. Temp file export (e) → /tmp/threadhop/** *(blocked by: #10)*
  Press `e` to export selected messages to `/tmp/threadhop/<session_id>-<timestamp>.md`. Display full path in TUI after export. Auto-cleaned on OS reboot. _(ADR-008)_

- [ ] **14. Real-time search panel (/) with FTS5 prefix matching** *(blocked by: #8, #9)*
  Press `/` to open search input. FTS5 prefix matching per keystroke (e.g., `rate* lim*`). Results show message snippet, session name, project, timestamp. Navigate with `j`/`k`, `Enter` to jump to source. Filter syntax: `project:`, `user:`, `assistant:`. _(ADR-002, ADR-007)_

- [ ] **42. Context-aware help overlay + command metadata registry**
  Add a full-app discoverability surface modeled on the search modal, but for commands instead of FTS results. Keep the footer minimal/contextual rather than trying to show every keybinding all the time. Define a shared command metadata registry that covers both app-level bindings and widget-local modes (session list, transcript, selection mode, reply input, search/find), and use it to drive the help overlay plus footer hints from one source of truth. The implementation should leave the final help trigger key open and must not assume `H`, since handoff shortcut work is still in flux. _(ADR-017)_

- [ ] **50. Event-driven session discovery via fsevents** *(blocked by: #1, #8; supersedes polling in `_gather_session_data()`)*
  Replace the 5 s poll with a `watchdog` observer rooted at `~/.claude/projects`. Dispatch typed Textual messages (`SessionFileChanged`, `SessionFileCreated`, `SessionFileDeleted`) on file-system events and update just the affected session row incrementally — seek to the stored byte offset, parse only the tail, update metadata + FTS. Retain polling only for cold-start discovery. Distinct from task #34 (single-session observer sidecar): this is the global session-list refresh path. Add `watchdog` to the PEP 723 dependency block. _(ADR-023, PERFORMANCE.md)_

- [ ] **51. Defer transcript parse until focus with lazy tail load** *(blocked by: #8; prereq for: #52)*
  Refactor `load_transcript()` into an async pipeline. On session focus, read ~64 KB from the tail, parse backwards from EOF, paint the visible range immediately, then stream earlier messages from a `@work` background task. Add a `TranscriptCache` LRU keyed by `session_id`, invalidated by `SessionFileChanged` events from #50. Discovery-side parse stays cheap (first 100 lines only, unchanged). _(ADR-024, PERFORMANCE.md)_

- [ ] **33. Data model hardening: CHECK constraints + Pydantic schemas** *(blocked by: #1; prereq for: #8)*
  Add a migration introducing `CHECK (status IN ('active','in_progress','in_review','done','archived'))` on the `sessions` table so the enum is enforced at the DB layer, not just the app. Design Pydantic models as the validation boundary for JSONL transcript parsing — user/assistant messages, `tool_use` blocks, `tool_result` blocks — so the indexer in #8 folds over typed instances instead of raw dicts and malformed lines fail loudly. Use `Literal` types for enums (session status, message role, memory `type`). Define `Session`, `Message`, `Bookmark`, `MemoryEntry` shapes alongside their table migrations so the Python types and SQL schemas evolve together. Add `pydantic` to the script's PEP 723 dependency block. _(ADR-001, ADR-003, ADR-004)_

---

## Phase 3: CLI Subcommands + Observer (on-demand + background)
_Observer-first architecture. Observer uses `claude -p --model haiku --permission-mode acceptEdits`
(ADR-018). Per-session observation files with SQLite state tracking (ADR-019)._

- [ ] **15. Add argparse subcommand routing**
  No subcommand → TUI mode (preserve existing behaviour, including `--project` / `--days` flags). With subcommand → CLI mode. Shared arg parsing for `--project`, `--session`. _(ADR-011)_

- [ ] **16. Implement `threadhop tag <status> [--session <id>]` CLI** *(blocked by: #1, #3, #15)*
  Writes status to the same SQLite `sessions` table the TUI reads. Second of three tag entry points. _(ADR-011, ADR-013)_

- [ ] **17. Session auto-detection from current terminal** *(blocked by: #16)*
  When `threadhop tag` is called without `--session`, detect the current session ID by scanning `ps` for `claude` processes in the current terminal's process tree. Reuse the detection logic the TUI already runs in `_get_active_claude_sessions()`.

- [ ] **43. Create reusable observer prompt** *(prereq for: #18)*
  Write `~/.config/threadhop/prompts/observer.md` (or bundle with app at `prompts/observer.md`). Constrains Haiku: append-only, one JSON line per observation, no deletions, typed extraction only. Types: `todo | decision | done | adr | observation`. Each line is minimal: `type`, `text`, `context`, `ts` — no session/project/offset (metadata lives in filename and DB). **DONE — prompt exists at `prompts/observer.md`.** _(ADR-018)_

- [x] **44. Add `observation_state` table to SQLite schema** *(blocked by: #1; prereq for: #18)*
  **DONE.** Migration 004 in `db.py`. Table: `session_id`, `source_path`, `obs_path`, `source_byte_offset`, `entry_count`, `reflector_entry_offset`, `observer_pid`, `status` (idle/running/stopped), `started_at`, `last_observed_at`. Helper functions: `get_observation_state`, `upsert_observation_state`, `update_observer_offset`, `update_reflector_offset`, `set_observer_running`, `set_observer_stopped`, `get_observed_sessions`, `get_running_observers`. _(ADR-019, ADR-022)_

- [ ] **18. Build observer core function** *(blocked by: #8, #43, #44)*
  The single observer function used by ALL entry points (CLI, skill, handoff, TUI). This is an **orchestrator**, not just a `claude -p` call. The full sequence:

  **Step 1 — Check state:** Read `observation_state` row for the session from SQLite. If no row exists, create one with `source_byte_offset=0` (first observation). If row exists, read `source_byte_offset` for where the observer last left off.

  **Step 2 — Read new messages:** Open the source JSONL at `source_path`, seek to `source_byte_offset`, read from there to EOF. Parse the new bytes into JSONL lines. Count how many new human/assistant message turns are in this chunk (not raw lines — group by `message.id` for assistant streaming chunks).

  **Step 3 — Batch threshold check:** If fewer than 3 new message turns since last observation, skip (not enough context for meaningful extraction). Return early. In background mode, continue watching; in on-demand mode, report "up to date."

  **Step 4 — Build the prompt:** Read `prompts/observer.md`. Append the new message chunk as the conversation input. Append the output file path: `~/.config/threadhop/observations/<session_id>.jsonl`. Ensure the `observations/` directory exists.

  **Step 5 — Invoke `claude -p`:**
  ```bash
  claude -p "<prompt_with_chunk>" \
    --model haiku \
    --permission-mode acceptEdits
  ```
  The Haiku process reads the chunk, extracts observations, and appends JSON lines to the observation file. Each line has only: `type`, `text`, `context`, `ts`.

  **Step 6 — Update state:** Count lines in the observation file (or diff before/after to get new entry count). Record new `source_byte_offset` (current EOF of source JSONL) and updated `entry_count` in `observation_state` via `db.update_observer_offset()`.

  **Step 7 — Return summary:** Report what was extracted: "Processed N messages. Found X decisions, Y TODOs, Z observations." Used by the skill and CLI for user feedback.

  **Existing infrastructure to use:**
  - `db.py`: `observation_state` table + all helper functions (migration 004)
  - `prompts/observer.md`: the prompt file
  - `indexer.py`: `parse_messages()` for JSONL parsing / chunk merging logic (reuse or adapt)
  - `db.OBS_DIR`: `~/.config/threadhop/observations/` path constant

  _(ADR-010, ADR-018, ADR-019)_

- [x] **19. Incremental observer processing (byte offset per session)** *(blocked by: #18)*
  **DONE.** `observer.watch_session()` is the watch-mode layer on top of `observe_session()`. Runs an initial catch-up extraction, then polls `source_path.stat().st_size` and re-invokes the observer whenever the file has grown past the recorded cursor. Polling is the ADR-015 fallback; the per-iteration `sleep_fn` is the swap point where task #34 can plug in fsevents/kqueue without breaking the contract. Failures are tallied and never raised, so transient Haiku errors don't kill the watcher. Tests in `tests/test_observer.py::TestWatchSession`. _(ADR-010, ADR-015, ADR-019)_

- [ ] **34. Build background observer sidecar (`threadhop observe`)** *(blocked by: #18, #19)*
  Background process mode for the observer (ADR-015). `threadhop observe --session <id>` watches the session's JSONL via fsevents (macOS) with polling fallback. Batches new messages (~3-4 trigger extraction). Records PID in `observation_state.observer_pid`. Exits when Claude Code session ends or `--stop` is sent. _(ADR-015)_

- [ ] **35. Observer stop/resume lifecycle** *(blocked by: #34, #44)*
  Stop mechanisms: `threadhop observe --stop [--session <id>]` sends SIGTERM to recorded PID. `threadhop observe --stop-all` stops all running observers. Observer handles SIGTERM gracefully: flushes pending observations, updates byte offset, sets `status = 'stopped'`. Resume: reads `source_byte_offset`, processes only new bytes. Stale PID detection via `kill -0 $PID` — corrects status to 'stopped' if process dead. Entry points for enabling: (1) manual `threadhop observe --session <id> &`, (2) Claude Code hook in `.claude/settings.json`, (3) `threadhop config set observe.enabled true`. _(ADR-015, ADR-019)_

- [ ] **20. Implement `threadhop todos [--project]` CLI query** *(blocked by: #18, #19)*
  Runs observer for unprocessed messages, then prints open TODOs from per-session observation files. Filterable by `--project`. _(ADR-010, ADR-011, ADR-019)_

- [ ] **21. Implement `threadhop decisions [--project]` CLI query** *(blocked by: #18, #19)*
  Same pattern as `todos` but filters for `type: decision`. _(ADR-010, ADR-011)_

- [ ] **22. Implement `threadhop observations [--project]` CLI query** *(blocked by: #18, #19)*
  Unfiltered dump of all observations, newest first. Reads from per-session files in `~/.config/threadhop/observations/`. Output format stays grep/jq-friendly. _(ADR-010, ADR-011, ADR-019)_

- [x] **45. Implement `threadhop conflicts [--project]` CLI query** *(blocked by: #18, #19)*
  **DONE.** `threadhop conflicts [--project <name>] [--resolved]` now refreshes observed sessions via observer + reflector, scans `observations/*.jsonl` for `type: "conflict"` entries, enriches them with the referenced decisions, filters by project via SQLite session mapping, and can mark the displayed conflicts reviewed without mutating the append-only JSONL. Tests in `tests/test_conflicts.py`. _(ADR-020)_

---

## Phase 4: Skill Plugin + TUI Observation Indicator
_Five skills for in-session use (ADR-012, ADR-016). TUI observation indicator
and transcript header (ADR-021)._

- [ ] **23. Research Claude Code skill plugin packaging**
  Determine how Claude Code skill plugins are distributed — directory of `.md` files in `~/.claude/skills/`? npm/pip package? Verify the plugin contract before implementing skills. _(Open Question Q4)_

- [ ] **24. Implement `/threadhop:tag <status>` skill** *(blocked by: #16, #23)*
  Instant, no LLM. Detects current session ID from process context, shells out to `threadhop tag <status>`, confirms: "Tagged this session as <status>". Third of three tag entry points. _(ADR-012, ADR-013)_

- [ ] **25. Implement `/threadhop:context` skill** *(blocked by: #12, #23)*
  Instant, no LLM. Reads clipboard via `pbpaste`, detects ThreadHop source labels, presents the content as a clearly bounded context block in the current conversation. Bridges TUI visual selection → Claude Code injection. _(ADR-012)_

- [ ] **26. Implement `/threadhop:handoff <id> [--full]` skill** *(blocked by: #18, #23)*
  Runs observer as underlying function (ADR-018). Checks `observation_state` for target session. If observations exist: reads per-session observation file, catches up on new bytes if source JSONL grew. If NO observations: runs observer on full session from byte 0 first. Then formats observations into handoff brief (~30-50 lines). Short observation sets format directly; large sets use Haiku sub-agent for polish. `--full` flag produces comprehensive handoff with rationale and excerpts. No separate "compress raw JSONL" path — observer is always the first step. _(ADR-012, ADR-016, ADR-018)_

- [ ] **40. Implement `/threadhop:observe` skill** *(blocked by: #23, #34)*
  Instant, no LLM (spawns background process). Per-session opt-in for observation (ADR-016). Detects current session ID, checks `observation_state.observer_pid` for existing observer. Spawns `threadhop observe --session <id> &` in background. Observer performs retroactive catch-up from `source_byte_offset` (or byte 0 for new), then switches to watch mode. Reports summary: "47 messages processed — 5 decisions, 3 TODOs, 1 ADR. Watching for new messages." _(ADR-015, ADR-016, ADR-019)_

- [ ] **41. Implement `/threadhop:insights` skill** *(blocked by: #23, #40)*
  Instant, no LLM. Pull-based context injection (ADR-016). Reads per-session observation file `observations/<session_id>.jsonl` — single file contains all types including `type: "conflict"` entries from reflector (ADR-020). Formats grouped by type. Returns nothing useful if session was never observed. _(ADR-016, ADR-020)_

- [ ] **46. TUI observation indicator — 🗒 icon in session list** *(blocked by: #44)*
  Add notepad icon (🗒 or fallback `≡` / `[O]`) next to session name when `observation_state.entry_count > 0`. Checked during existing 5s refresh cycle. Icon is independent of process state circles (◐ ● ○) — additive, not replacement. _(ADR-021)_

- [ ] **47. TUI transcript header for observed sessions** *(blocked by: #44)*
  When viewing a transcript with observations, show one-line header above first message: `─── 🗒 12 observations · ~/.config/threadhop/observations/abc123.jsonl ───`. Positioned above transcript, below session title. Does NOT interfere with search bar (bottom). Static, informational, copyable. _(ADR-021)_

- [ ] **48. TUI observation keybindings (o/O)** *(blocked by: #46, #34)*
  `o` on observed session: copy observation file path to clipboard, show notification. `o` on unobserved session: offer to start observing (y/n). `O` on stopped+observed session: resume observation from last byte offset. _(ADR-021)_

---

## Phase 5: Project Memory + Bookmarks
_Cross-session knowledge persistence (beyond the raw observations log)._

- [ ] **27. Build bookmark system** *(blocked by: #1, #10)*
  Add `bookmarks` table to schema. Toggle bookmark from message selection mode with `space`. Support labels and tags (JSON array). Build bookmark browser panel in TUI.

- [ ] **28. Build project memory ledger** *(blocked by: #1)*
  Add `memory` table to schema. Support typed entries: `decision | todo | done | adr | observation`. Append-only, filterable by project/type/date. Manual entry from TUI (type + text). Distinct from per-session observation files: this is for curated/explicit entries with `source: 'explicit'`. _(ADR-005)_

- [ ] **29. Add explicit annotation detection** *(blocked by: #18, #28)*
  Recognize `ADR:`, `DECISION:`, `TODO:` markers in conversations and offer to append them to the memory ledger automatically with `source: 'auto'`.

- [ ] **30. Project memory markdown rendering** *(blocked by: #18, #28)*
  Render project memory (observations + explicit ledger entries) as markdown for injection into new sessions. Used when a future skill/CLI wants to hand the current project's accumulated context to a fresh session.

---

## Phase 6: Reflector — Conflict Detection + Fuzzy Search
_Contradiction detection across sessions. Reflector writes `type: "conflict"` entries
to the SAME per-session observation JSONL (ADR-020, ADR-022). Triggered by observer
process — not independent. Uses `reflector_entry_offset` for incremental processing._

- [ ] **49. Create reflector prompt** *(prereq for: #31)*
  Write `~/.config/threadhop/prompts/reflector.md`. Constrains Haiku: append-only, one JSON line per conflict, dedup check against existing conflicts (same `refs` pair + `topic` = skip). Input shape: recent decisions from current session + all decisions from other sessions in same project. Output: `type: "conflict"` entries with `refs`, `topic`, `text` fields. _(ADR-022)_

- [x] **31. Build conflict detection reflector core function** *(blocked by: #18, #49)*
  **DONE.** `reflector.py` now runs the second `claude -p --model haiku --permission-mode acceptEdits` pass against recent current-session decisions and peer decisions from other same-project observation files, appends `type: "conflict"` lines to the current session's observation JSONL, and advances `reflector_entry_offset` / `entry_count` in SQLite. Tests in `tests/test_reflector.py`. _(ADR-015, ADR-020, ADR-022)_

- [ ] **36. Observer-triggered reflector in background mode** *(blocked by: #31, #34)*
  The observer process owns the reflector lifecycle (ADR-022). After each observer extraction, checks if `entry_count - reflector_entry_offset >= 5`. If yes, spawns reflector `claude -p` call. No separate PID, no separate daemon — when observer stops, reflector stops. When observer resumes, reflector resumes from its own `reflector_entry_offset`. _(ADR-015, ADR-020, ADR-022)_

- [ ] **38. TUI conflict notification** *(blocked by: #31)*
  Surface unreviewed `type: "conflict"` entries in the TUI sidebar or status bar. Badge count or indicator when new conflicts are detected. Press a key to open conflict detail view. _(ADR-015, ADR-020)_

- [ ] **39. Observation condensation (secondary reflector goal)** *(blocked by: #31)*
  When per-session observation files exceed a size threshold, merge related decisions, archive completed TODOs, produce condensed summaries. Keep source links back to original observations. Forward-only: condensation appends summary entries, does not delete originals.

- [ ] **32. Trigram-based fuzzy search for typo tolerance** *(blocked by: #14)*
  Add trigram tokenizer as a secondary FTS table. Fall back to trigram search when FTS5 prefix returns zero results. Handles spelling mistakes (e.g., "retr" matches "retry"). _(ADR-007)_

---

## Phase 7: Rendering Performance
_Scheduled after the observer ships and cross-session query surfaces
(#14, #20-22, #45) expose enough rows to matter. Backend perf (#50,
#51) lands in Phase 2 — rendering perf lands here._

- [ ] **52. Row-window virtualisation of TranscriptView** *(blocked by: #51)*
  Replace the `VerticalScroll`-of-all-messages with a custom virtual-scroll container (`VirtualTranscript(ScrollView)`) that mounts only the visible range + overscan buffer and recycles widget instances from per-type pools (`UserMessage`, `AssistantMessage`, `ToolMessage`). Start with fixed row height + overflow ellipsis; expand-on-focus handles tall messages. Single biggest rendering-perf win — makes long sessions render in constant cost. _(ADR-025, PERFORMANCE.md)_

- [ ] **53. Migrate data-heavy DataTables to textual-fastdatatable** *(blocked by: #14; also benefits #20, #21, #22, #45 if those land in the TUI)*
  Swap Textual's `DataTable` for [`textual-fastdatatable`](https://github.com/tconbeer/textual-fastdatatable) at every heavy surface — search results panel, cross-session query views, observation browsers. Light surfaces (settings, help) keep the stock `DataTable`. Add `pyarrow` + `textual-fastdatatable` to the PEP 723 deps. Add `db.fetch_arrow()` helper for SQLite → Arrow conversion so query pipelines can feed the table directly. _(ADR-026, PERFORMANCE.md)_
