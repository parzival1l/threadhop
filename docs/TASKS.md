# ThreadHop — Implementation Task List

Extracted from [DESIGN-DECISIONS.md](DESIGN-DECISIONS.md) on 2026-04-14.

---

## Phase 1: SQLite Foundation + Session Tags + Archive
_Immediate value, enables all future features._

- [ ] **1. Create SQLite DB module (init, migrate, query helpers)**
  Create SQLite DB at `~/.config/threadhop/sessions.db` with WAL mode. Include schema for `settings`, `sessions` tables. Write init and query helper functions. _(ADR-001)_

- [ ] **2. Migrate config.json → SQLite** *(blocked by: #1)*
  One-time migration on first run: move session names, session order, last-viewed timestamps from config.json into the sessions table. Keep config.json only for app-level settings (theme, sidebar_width). _(ADR-001)_

- [ ] **3. Add session status field + grouped display** *(blocked by: #1)*
  Add `status` field to session model with values: `active | in_progress | in_review | done | archived`. Render sessions grouped by status in the TUI sidebar. _(ADR-004)_

- [ ] **4. Implement status cycling keybinds (s/S)** *(blocked by: #3)*
  `s` cycles status forward (active → in_progress → in_review → done), `S` cycles backward. Manual reorder works within each status group.

- [ ] **5. Implement archive (a) + archive toggle (A)** *(blocked by: #3)*
  `a` sets session status to archived. `A` toggles visibility of archived sessions. Archived sessions hidden by default. _(ADR-004)_

- [ ] **6. Implement sidebar resize ([/])**
  Add `[` and `]` keybindings to shrink/grow sidebar. Min 20, max 60, step 4. Persist width in config. Update `grid-columns` CSS dynamically via `self.styles.grid.columns`. _(ADR-010)_

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
  Press `y` on selected messages to copy to clipboard. Format includes source labels: `[From "session name" — ~/cwd — timestamp]`. _(ADR-008)_

- [ ] **13. Temp file export (e) → /tmp/threadhop/** *(blocked by: #10)*
  Press `e` to export selected messages to `/tmp/threadhop/<session_id>-<timestamp>.md`. Display full path in TUI after export. Auto-cleaned on OS reboot. _(ADR-008)_

- [ ] **14. Real-time search panel (/) with FTS5 prefix matching** *(blocked by: #8, #9)*
  Press `/` to open search input. FTS5 prefix matching per keystroke (e.g., `rate* lim*`). Results show message snippet, session name, project, timestamp. Navigate with `j`/`k`, `Enter` to jump to source. Filter syntax: `project:`, `user:`, `assistant:`. _(ADR-002, ADR-007)_

- [ ] **24. Data model hardening: CHECK constraints + Pydantic schemas** *(blocked by: #1; prereq for: #8)*
  Add a migration introducing `CHECK (status IN ('active','in_progress','in_review','done','archived'))` on the `sessions` table so the enum is enforced at the DB layer, not just the app. Design Pydantic models as the validation boundary for JSONL transcript parsing — user/assistant messages, `tool_use` blocks, `tool_result` blocks — so the indexer in #8 folds over typed instances instead of raw dicts and malformed lines fail loudly. Use `Literal` types for enums (session status, message role, memory `type`). Define `Session`, `Message`, `Bookmark`, `MemoryEntry` shapes alongside their table migrations so the Python types and SQL schemas evolve together. Add `pydantic` to the script's PEP 723 dependency block. _(ADR-001, ADR-003, ADR-004)_

---

## Phase 3: Skill Plugin
_Only skills that genuinely need an LLM or benefit from mid-conversation invocation._

- [ ] **15. Research Claude Code skill plugin packaging**
  Determine how Claude Code skill plugins are distributed — directory of .md files in `~/.claude/skills/`? npm/pip package? Verify the plugin contract before implementing skills. _(Open Question Q4)_

- [ ] **16. Implement handoff skill (/threadhop:handoff)** *(blocked by: #15)*
  Read JSONL for given session_id, parse to clean (role, text) pairs, strip system-reminders, abbreviate tool calls. Spawn sub-agent for transcript compression. Inject compressed brief (~30-50 lines) into current session. _(ADR-006)_

- [ ] **17. Implement memory injection skill (/threadhop:memory)** *(blocked by: #15, #19)*
  Read project memory from SQLite (or rendered .md cache), format as structured context with section headers, inject directly. No sub-agent, no LLM needed. _(ADR-006)_

---

## Phase 4: Project Memory + Bookmarks
_Cross-session knowledge persistence._

- [ ] **18. Build bookmark system**
  Add bookmarks table to schema. Toggle bookmark from message selection mode with `space`. Support labels and tags (JSON array). Build bookmark browser panel in TUI.

- [ ] **19. Build project memory ledger**
  Add memory table to schema. Support typed entries: `decision | todo | done | adr | observation`. Append-only, filterable by project/type/date. Manual entry from TUI (type + text). Rendered as markdown for injection. _(ADR-005)_

- [ ] **20. Add explicit annotation detection** *(blocked by: #19)*
  Recognize `ADR:`, `DECISION:`, `TODO:` markers in conversations and offer to append them to the project memory ledger automatically.

---

## Phase 5: Auto-Observer + Reflector
_Automatic knowledge extraction._

- [ ] **21. Build auto-observer**
  Background observer that detects new messages in active sessions during refresh cycle. Auto-extract observations (pattern matching first, `claude -p` later). Append to memory ledger with `source: "auto"`.

- [ ] **22. Build reflector** *(blocked by: #21)*
  Periodic condensation of old observations. Archive completed TODOs, merge related decisions. Keeps the memory ledger manageable over time.

- [ ] **23. Trigram-based fuzzy search for typo tolerance** *(blocked by: #14)*
  Add trigram tokenizer as a secondary FTS table. Fall back to trigram search when FTS5 prefix returns zero results. Handles spelling mistakes (e.g., "retr" matches "retry"). _(ADR-007)_

---

## Dependency Graph (critical path)

```
#1 SQLite DB ──┬──> #2 Migration ──> #7 Tests
               ├──> #3 Status ──┬──> #4 Keybinds (s/S)
               │                └──> #5 Archive (a/A)
               └──> #24 Data models ──> #8 Indexer ──> #9 Incremental ──> #14 Search ──> #23 Fuzzy
                                                                      └──> #14 Search
#6 Sidebar resize (independent)

#10 Selection ──┬──> #11 Range select
                ├──> #12 Clipboard copy
                └──> #13 Temp export

#15 Skill research ──┬──> #16 Handoff skill
                     └──> #17 Memory skill (also needs #19)

#19 Memory ledger ──> #20 Annotation detection
#21 Auto-observer ──> #22 Reflector
```
