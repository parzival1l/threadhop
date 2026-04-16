# ThreadHop — Implementation Task List

Extracted from [DESIGN-DECISIONS.md](DESIGN-DECISIONS.md). Last reconciled 2026-04-15.

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

- [ ] **33. Data model hardening: CHECK constraints + Pydantic schemas** *(blocked by: #1; prereq for: #8)*
  Add a migration introducing `CHECK (status IN ('active','in_progress','in_review','done','archived'))` on the `sessions` table so the enum is enforced at the DB layer, not just the app. Design Pydantic models as the validation boundary for JSONL transcript parsing — user/assistant messages, `tool_use` blocks, `tool_result` blocks — so the indexer in #8 folds over typed instances instead of raw dicts and malformed lines fail loudly. Use `Literal` types for enums (session status, message role, memory `type`). Define `Session`, `Message`, `Bookmark`, `MemoryEntry` shapes alongside their table migrations so the Python types and SQL schemas evolve together. Add `pydantic` to the script's PEP 723 dependency block. _(ADR-001, ADR-003, ADR-004)_

---

## Phase 3: CLI Subcommands + Observer
_Observer-first architecture. CLI access to observations without launching the TUI._

- [ ] **15. Add argparse subcommand routing**
  No subcommand → TUI mode (preserve existing behaviour, including `--project` / `--days` flags). With subcommand → CLI mode. Shared arg parsing for `--project`, `--session`. _(ADR-011)_

- [ ] **16. Implement `threadhop tag <status> [--session <id>]` CLI** *(blocked by: #1, #3, #15)*
  Writes status to the same SQLite `sessions` table the TUI reads. Second of three tag entry points. _(ADR-011, ADR-013)_

- [ ] **17. Session auto-detection from current terminal** *(blocked by: #16)*
  When `threadhop tag` is called without `--session`, detect the current session ID by scanning `ps` for `claude` processes in the current terminal's process tree. Reuse the detection logic the TUI already runs in `_get_active_claude_sessions()`.

- [ ] **18. Build Haiku observer** *(blocked by: #8)*
  Process unindexed conversation chunks through Haiku. Extract typed observations (`todo | decision | done | adr | question | blocker`). Prompt: extract only explicitly discussed items, do not infer. Append to `~/.config/threadhop/observations.jsonl`. _(ADR-010)_

- [ ] **19. Incremental observer processing (byte offset per session)** *(blocked by: #18)*
  Track per-session byte offsets so the observer only re-processes new conversation. Observer triggers on CLI query or TUI launch — not a daemon, not a hook. _(ADR-010)_

- [ ] **20. Implement `threadhop todos [--project]` CLI query** *(blocked by: #18, #19)*
  Runs observer for unprocessed messages, then prints open TODOs from `observations.jsonl`. Filterable by `--project`. _(ADR-010, ADR-011)_

- [ ] **21. Implement `threadhop decisions [--project]` CLI query** *(blocked by: #18, #19)*
  Same pattern as `todos` but filters for `type: decision`. _(ADR-010, ADR-011)_

- [ ] **22. Implement `threadhop observations [--project]` CLI query** *(blocked by: #18, #19)*
  Unfiltered dump of all observations, newest first. Output format should stay grep/jq-friendly. _(ADR-010, ADR-011)_

---

## Phase 4: Skill Plugin
_Three skills for in-session use — tag, context, handoff._

- [ ] **23. Research Claude Code skill plugin packaging**
  Determine how Claude Code skill plugins are distributed — directory of `.md` files in `~/.claude/skills/`? npm/pip package? Verify the plugin contract before implementing skills. _(Open Question Q4)_

- [ ] **24. Implement `/threadhop:tag <status>` skill** *(blocked by: #16, #23)*
  Instant, no LLM. Detects current session ID from process context, shells out to `threadhop tag <status>`, confirms: "Tagged this session as <status>". Third of three tag entry points. _(ADR-012, ADR-013)_

- [ ] **25. Implement `/threadhop:context` skill** *(blocked by: #12, #23)*
  Instant, no LLM. Reads clipboard via `pbpaste`, detects ThreadHop source labels, presents the content as a clearly bounded context block in the current conversation. Bridges TUI visual selection → Claude Code injection. _(ADR-012)_

- [ ] **26. Implement `/threadhop:handoff <id> [--full]` skill** *(blocked by: #23)*
  The only LLM-powered skill. Reads JSONL for given session, parses to clean (role, text) pairs, strips system-reminders, abbreviates tool calls. Spawns sub-agent (Haiku default) to compress into a ~30-50 line brief. `--full` flag uses a stronger model for comprehensive handoff with rationale and excerpts. _(ADR-006, ADR-012)_

---

## Phase 5: Project Memory + Bookmarks
_Cross-session knowledge persistence (beyond the raw observations log)._

- [ ] **27. Build bookmark system** *(blocked by: #1, #10)*
  Add `bookmarks` table to schema. Toggle bookmark from message selection mode with `space`. Support labels and tags (JSON array). Build bookmark browser panel in TUI.

- [ ] **28. Build project memory ledger** *(blocked by: #1)*
  Add `memory` table to schema. Support typed entries: `decision | todo | done | adr | observation`. Append-only, filterable by project/type/date. Manual entry from TUI (type + text). Distinct from observations.jsonl: this is for curated/explicit entries with `source: 'explicit'`. _(ADR-005)_

- [ ] **29. Add explicit annotation detection** *(blocked by: #18, #28)*
  Recognize `ADR:`, `DECISION:`, `TODO:` markers in conversations and offer to append them to the memory ledger automatically with `source: 'auto'`.

- [ ] **30. Project memory markdown rendering** *(blocked by: #18, #28)*
  Render project memory (observations + explicit ledger entries) as markdown for injection into new sessions. Used when a future skill/CLI wants to hand the current project's accumulated context to a fresh session.

---

## Phase 6: Reflector + Fuzzy Search
_Condensation and search resilience — add only when volume demands it._

- [ ] **31. Build reflector** *(blocked by: #18)*
  Periodic condensation of old observations via Haiku. Archive completed TODOs, merge related decisions, produce condensed summaries. Keep source links back to original observations. Keeps the memory ledger manageable over time.

- [ ] **32. Trigram-based fuzzy search for typo tolerance** *(blocked by: #14)*
  Add trigram tokenizer as a secondary FTS table. Fall back to trigram search when FTS5 prefix returns zero results. Handles spelling mistakes (e.g., "retr" matches "retry"). _(ADR-007)_

---

## Dependency Graph (critical path)

```
#1 SQLite DB ──┬──> #2 Migration ──> #7 Tests
               ├──> #3 Status ──┬──> #4 Keybinds (s/S)  ─────────┐
               │                └──> #5 Archive (a/A)            │
               ├──> #33 Data models ──> #8 Indexer ──> #9 Incremental ──> #14 Search │ ──> #32 Fuzzy
               │                                      └──> #18 Observer ──> #19 Incremental
               │                                                            ├──> #20 todos
               │                                                            ├──> #21 decisions
               │                                                            ├──> #22 observations
               │                                                            ├──> #29 Annotation detection
               │                                                            ├──> #30 Memory rendering
               │                                                            └──> #31 Reflector
               ├──> #27 Bookmarks (also needs #10)
               └──> #28 Memory ledger ──┬──> #29 Annotation detection
                                        └──> #30 Memory rendering

#6 Sidebar resize (independent)

#10 Selection ──┬──> #11 Range select
                ├──> #12 Clipboard copy ──> #25 /threadhop:context (also needs #23)
                ├──> #13 Temp export
                └──> #27 Bookmarks (also needs #1)

#15 Subcommand routing ──> #16 tag CLI ──> #17 Auto-detect session
                                        └──> #24 /threadhop:tag skill (also needs #23)

#23 Skill packaging research ──┬──> #24 /threadhop:tag
                                ├──> #25 /threadhop:context
                                └──> #26 /threadhop:handoff

Entry points for session tagging (ADR-013): #4 (TUI) · #16 (CLI) · #24 (skill) — all write to the same sessions table.
```
