# ThreadHop Rust Core — Multi-Harness Transcript Adapters

**Goal:** Make `rust/threadhop-core` (and therefore `./threadhop-rs`) read
sessions from **Claude Code, Codex CLI, OpenCode, pi, Gemini CLI and
t3code** through one `TranscriptAdapter` seam, instead of being wired to
`~/.claude/projects` everywhere. This is a *plan document* — no Rust code
is touched in this commit.

**Reference points:** `docs/task-054-transcript-harness-adapters.md`
(product intent, written against the Python core), ADR-028 (the *outbound*
`harness.rs` seam — different problem, deliberately kept separate),
`rust/threadhop-core/src/{jsonl,models,session_detect,paths,exchanges}.rs`
and the TUI's `workers/{session_scanner,fs_watcher,active_detector}.rs`.

---

## 1. Executive Summary

Today the Rust core has **no discovery module** and **no indexer**. The
Claude Code coupling lives in five places:

| Where | Coupling |
|---|---|
| `paths.rs:30` `claude_projects_dir()` | the only transcript root; called directly by scanner, watcher, `app.rs:2091`, `session_detect.rs:165` |
| `jsonl.rs` `parse_byte_range` / `read_session_metadata` | untyped `serde_json::Value` lookups on `sessionId`, `parentUuid`, `isSidechain`, `toolUseResult`, `message.id`, `message.content[]` |
| `exchanges.rs` `read_clean_rows` | a *third* independent parser over the same keys, with per-line byte offsets |
| `session_detect.rs` | `claude` argv shape, cwd → dir-slug → newest jsonl |
| TUI `session_scanner.rs`, `fs_watcher.rs`, `app.rs::derive_project_for_session` | walk `<root>/<slug>/<id>.jsonl`, `project` = parent dir name |

None of `Session`, `SessionMetadata`, `SessionListItem` or `CleanedMessage`
carries a provider. Python owns the SQLite schema (v11); Rust only reads
`sessions`/`messages` and writes bookmarks/status/custom_name/transfer_state.

The plan: introduce `threadhop_core::transcript` with a `Provider` enum, a
`SessionRef` (provider + id + locator + cwd), a `Capabilities` struct and a
`TranscriptAdapter` trait; move Claude behind it with **zero behaviour
change** (golden test stays green); then add five adapters, each a single
file plus fixtures. The TUI stops calling `claude_projects_dir()` and asks
the adapter registry instead. Everything above the seam keeps consuming
`Vec<CleanedMessage>`.

Harness survey (on this machine, 2026-09-08):

| Harness | Root | Format | Local data? |
|---|---|---|---|
| Claude Code | `~/.claude/projects/<slug>/<id>.jsonl` | JSONL, `parentUuid` tree, `message.id` chunks | yes |
| Codex CLI | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` + `archived_sessions/` (+ `state_5.sqlite` index) | JSONL `{timestamp,ordinal,type,payload}` | yes (505 + 77 files) |
| OpenCode | `~/.local/share/opencode/opencode.db` | SQLite; v1 `session/message/part` **and** v2 `session_v2/session_message` both live | yes (`storage/*.json` is dead legacy) |
| pi | `~/.pi/agent/sessions/--<cwd>--/<ts>_<uuid>.jsonl` | JSONL, true tree (`id`/`parentId`), header line | yes (46 sessions) |
| Gemini CLI | `~/.gemini/tmp/<hash>/chats/session-*.jsonl` | JSONL, shape-discriminated records, `$set`/`$rewindTo` | **no** — build from upstream source |
| t3code | `~/.t3/userdata/state.sqlite` | SQLite, event-sourced → `projection_*` tables; wraps claude/codex/opencode | yes (487 MB) |

---

## 2. Canonical model

### 2.1 `Provider`

```rust
#[derive(Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider { ClaudeCode, Codex, OpenCode, Pi, Gemini, T3Code }
impl Provider {
    pub fn slug(self) -> &'static str;   // "claude_code" … stable, used in DB + CLI flags
    pub fn label(self) -> &'static str;  // "Claude", "Codex", "OpenCode", "pi", "Gemini", "T3"
    pub fn glyph(self) -> &'static str;  // one-cell sidebar badge
}
```

### 2.2 `SessionRef` — what discovery returns

```rust
pub struct SessionRef {
    pub provider: Provider,
    pub session_id: String,            // provider-native id, unprefixed
    pub locator: SourceLocator,        // how `load` finds the bytes
    pub cwd: Option<String>,           // real path, not a slug
    pub project: Option<String>,       // display/filter key — see §2.5
    pub title: Option<String>,         // first user text (50-char cap) or provider title
    pub created_at: Option<f64>,
    pub modified_at: Option<f64>,      // drives sort + "unread"
    pub parent_session: Option<String>,// subagent → parent (Claude agent-*, OpenCode parent_id, Gemini subagent dir)
    pub linked: Option<(Provider, String)>, // t3code thread → underlying provider session
}

pub enum SourceLocator {
    File(PathBuf),                       // Claude, Codex, pi, Gemini
    Sqlite { db: PathBuf, key: String }, // OpenCode, t3code
}
```

`SessionRef` supersedes `jsonl::SessionMetadata` (kept as a thin
Claude-internal type). Composite identity is `(provider, session_id)`;
ids are unprefixed so existing Claude bookmarks/status rows keep matching.

### 2.3 `Capabilities`

```rust
pub struct Capabilities {
    pub active_detection: bool,  // adapter can say "a process is driving this session"
    pub working_detection: bool, // adapter can say "…and it's mid-turn"
    pub tool_results: bool,      // tool rows carry a result snippet
    pub usage: bool,             // assistant rows carry token usage
    pub branching: bool,         // session may contain abandoned branches
    pub reply: bool,             // ThreadHop can send a message into it (Claude only, for now)
    pub borrow_surface: bool,    // peek/prepare work on this session (§5 Phase 8)
    pub watch: WatchMode,        // File | SqliteFile | Poll(Duration)
}
```

The TUI/CLI ask `adapter.capabilities()` rather than matching on
`Provider` — per task-054, no `if provider == …` outside `transcript/`.

### 2.4 `CleanedMessage` — kept, minimally extended

`CleanedMessage` is already provider-neutral in shape (role string, text,
timestamp, parent, tool_name, usage). Changes:

- Add `pub provider: Provider` so the transcript widget can theme by source
  without a session lookup.
- Formalise `role` values as constants in `transcript::role` (`user`,
  `assistant`, `tool`, `command`, `skill_load`, **new:** `system`,
  `reasoning_summary`). Still a `String` for TUI compatibility.
- `MessageUsage` stays Anthropic-shaped; adapters map Codex/OpenAI
  (`input_tokens`/`output_tokens`), pi (`usage.input/output`), Gemini
  (`tokens.input/output/cached`) and OpenCode (`tokens.*`) into it.
- Tool result convention unchanged: `"\n↳ <120-char snippet>"` appended to
  the tool row. Adapters where call+result are one record (OpenCode,
  Gemini) emit the tool row already suffixed.
- `uuid` must be unique within a session and stable across re-parses
  (bookmarks key on it). Adapters that lack native ids (Gemini toolCalls,
  t3 activities) derive `sha1(session_id + native_id + kind)`.

### 2.5 `project`

Today `project` is the Claude dir slug (`-Users-alice-alpha`) and is used
as an FTS filter and sidebar group. Decision: **`project` = Claude-style
slug of `cwd` for every provider**, computed by one helper
`transcript::project_slug(cwd)`. This keeps the existing `sessions.project`
column, FTS `--project` filter and the sidebar grouping working unchanged,
and means a Codex session and a Claude session in the same repo group
together. Providers with no cwd (t3 threads without worktree) fall back to
their native project title.

---

## 3. The seam — `threadhop_core::transcript`

```
rust/threadhop-core/src/transcript/
  mod.rs          Provider, SessionRef, SourceLocator, Capabilities, TranscriptAdapter, registry(), project_slug
  claude_code.rs  wraps existing jsonl.rs + session_detect.rs (no logic moved in Phase 1)
  codex.rs
  opencode.rs
  pi.rs
  gemini.rs
  t3code.rs
  process.rs      shared ps/lsof plumbing lifted from session_detect.rs (ProcessTable, cwd_of_pids)
```

```rust
pub trait TranscriptAdapter: Send + Sync {
    fn provider(&self) -> Provider;
    fn capabilities(&self) -> Capabilities;
    /// Paths the fs watcher should observe (dirs for File adapters, the db + -wal for Sqlite).
    fn watch_roots(&self) -> Vec<PathBuf>;
    /// Cheap enumeration: head-scan / index query only. Must not parse whole transcripts.
    fn discover(&self) -> Result<Vec<SessionRef>, TranscriptError>;
    /// Re-resolve a single session (used by watcher on change + by CLI `--session`).
    fn describe(&self, session_id: &str) -> Result<Option<SessionRef>, TranscriptError>;
    /// Full parse into canonical rows, in display order.
    fn load(&self, r: &SessionRef) -> Result<Vec<CleanedMessage>, TranscriptError>;
    /// Which discovered sessions have a live process. Default: empty.
    fn active(&self, procs: &ProcessTable, refs: &[SessionRef]) -> Vec<ActiveSession> { vec![] }
}

pub fn registry() -> &'static [Box<dyn TranscriptAdapter>];          // all six, in fixed order
pub fn adapter(p: Provider) -> &'static dyn TranscriptAdapter;
pub fn discover_all() -> Vec<SessionRef>;                             // concat + sort by modified_at desc
pub fn enabled_providers() -> Vec<Provider>;                          // settings key `transcript.providers`, default all
```

Adapters are stateless singletons; caching (e.g. OpenCode's open db
handle, Codex's `state_5.sqlite` index) lives in a `OnceCell` inside the
adapter. `discover()` is called every 5 s by the scanner, so each adapter
documents its cost budget (target < 50 ms warm).

### Error policy

One adapter failing (db locked, malformed line, missing root) **never**
blanks the session list. `discover_all()` logs per-adapter errors via
`tracing::warn!` and continues. `load()` errors surface in the transcript
pane as a single `system` row ("Codex adapter: …"), matching how the TUI
already shows missing files.

---

## 4. Per-adapter mapping

### 4.1 Claude Code (`claude_code.rs`) — Phase 1, zero behaviour change
- `discover`: existing one-level walk from `session_scanner::list_session_files_blocking`, moved into core. `project` = parent dir name (already the slug). `parent_session` from `agent-*.jsonl` sibling/subdir.
- `load`: `jsonl::parse_byte_range(bytes, Some(id))`, then stamp `provider`.
- `active`: existing `session_detect` logic, refactored to take `ProcessTable` (pure) so it's testable and shareable.
- Capabilities: everything `true`; `watch: File`.

### 4.2 Codex (`codex.rs`)
- `discover`: walk `sessions/**/rollout-*.jsonl` + `archived_sessions/*.jsonl`; read line 1 (`session_meta`) for `id`, `cwd`, `git.branch`, `originator`, `forked_from_id`. If `state_5.sqlite` exists, prefer its `threads` table (`rollout_path`, `cwd`, `title`, `first_user_message`, `archived`, `updated_at_ms`) — one query beats 580 file opens. Fall back to file scan if the sqlite is absent/locked.
- `load`: iterate `response_item`:
  - `message role=user` → `user` row **only if** not injected context (skip when `internal_chat_message_metadata_passthrough.content_item_kinds` marks it as environment/AGENTS.md; also skip `role=developer`).
  - `message role=assistant` → `assistant` row; `id` → `message_id`; `phase` kept in text? No — `phase=commentary` vs final both render as assistant.
  - `custom_tool_call` / `function_call` → `tool` row (`tool_name` = `name`; `uuid` = `call_id`; abbreviate via a Codex table: `exec`→command, `spawn_agent`→task_name, `apply_patch`→file list).
  - `*_output` → result snippet onto matching `call_id` row.
  - `reasoning` → skipped (encrypted).
  - `token_usage_record` → attach usage to the preceding assistant row.
  - `event_msg.task_complete.last_agent_message` is a duplicate → ignore.
- `active`: `ps` args contain `codex` **and not** `app-server` (ChatGPT.app host); cwd via lsof → newest rollout whose `session_meta.cwd` matches.
- Capabilities: `tool_results`, `usage`, `active_detection` true; `working_detection` true (task_started without task_complete in tail + mtime < 5 min); `branching` false (forks are separate files — surface via `linked`? No: use `parent_session` = `forked_from_id`).

### 4.3 OpenCode (`opencode.rs`)
- Read-only rusqlite handle with `?mode=ro&immutable=0`, busy timeout 200 ms (opencode is often running).
- `discover`: `SELECT id, directory, title, time_created, time_updated, parent_id FROM session_v2 UNION session` — dedupe by id, take max `time_updated`. `project` = slug(directory). `parent_session` = `parent_id`.
- `load`: for the id, read **both** generations and merge by created time:
  - v1: `message` rows (`data.role`, `data.time.created`) + their `part` rows ordered by `time_created`. `text` part → text; `reasoning` → skipped (or `reasoning_summary` row if setting on); `tool` part → `tool` row with `callID` as uuid, `state.input` abbreviated, `state.output` snippet appended, `state.status=error` prefix `✗`; `step-start/finish`, `snapshot`, `patch`, `file`, `compaction` → skipped (compaction → `system` row "context compacted").
  - v2: `session_message` rows; `type=user` → `data.text`; `type=assistant` → walk `data.content[]` with the same part mapping; `system|synthetic|model-switched|shell` → `system` rows; `compaction` → as above.
  - Tool names are lowercase (`bash`, `read`, `edit`, `glob`, `grep`, `webfetch`, `task`) → map to Claude-style casing before `abbreviate_tool_use` so digest's file-touch detection keeps working.
- `active`: process named `opencode`; cwd via lsof → sessions with that `directory`; pick newest. `working`: newest `session_message` is `user`, or last assistant `content[].tool.state.status ∈ {pending, running}`.
- `watch`: `SqliteFile` — watch `opencode.db-wal` mtime.
- Capabilities: all true except `reply`, `branching`.

### 4.4 pi (`pi.rs`)
- `discover`: walk `~/.pi/agent/sessions/*/*.jsonl` (dir names start with `--`, so use `Path` APIs, never shell). Header line gives `id`, `cwd`, `timestamp`. `title` from first `message.role=user` in head. Also honour `~/.prime/agent/sessions/*.jsonl` as the same format if present (flat layout) — cheap win, same code.
- `load`: build `HashMap<id, entry>`; find the **current leaf** = last appended entry; walk `parentId` to root; emit that path in order. Abandoned branches are not rendered (matches pi's own `/tree` semantics). `branch_summary`, `compaction` entries on the path → `system` rows. `model_change`, `thinking_level_change` → skipped.
  - `message.role=user` → `user`; `assistant` → `assistant` (`text` blocks joined; `thinking` skipped; each `toolCall` → `tool` row, uuid = `toolCall.id`); `toolResult` → snippet onto that row; `bashExecution` → `command` row; `custom*` → `system`.
  - `uuid` = entry `id`; `parent_uuid` = `parentId`; `message_id` = None; `usage` from `message.usage`.
- `active`: process `pi`; cwd → dir `--<encoded>--` → newest file.
- Capabilities: `branching` true, `tool_results`, `usage`, `active_detection` true; `working_detection` true (leaf is user message or assistant with `stopReason=toolUse` and no toolResult child).

### 4.5 Gemini CLI (`gemini.rs`) — from upstream source, synthetic fixture
- `discover`: `~/.gemini/tmp/*/chats/session-*.jsonl` (+ one level of subagent dirs). Header line (has `sessionId` + `projectHash`) → id, `startTime`, `kind`. `cwd`: reverse-map `projectHash` through `~/.gemini/projects.json`; else `directories[0]`; else None. Also read legacy single-object `session-*.json` if present.
- `load`: replay semantics matter here:
  - Keep an ordered `IndexMap<id, MessageRecord>`; a record whose `id` already exists **replaces** it (last wins).
  - `{"$set": {messages: [...]}}` → replace the whole map. Other `$set` keys → ignore.
  - `{"$rewindTo": id}` → truncate that id and everything after.
  - Then emit: `type=user` → `user`; `type=gemini` → `assistant` (+ `usage` from `tokens`, `model`), then one `tool` row per `toolCalls[]` (uuid = `call.id`, snippet from `result[].functionResponse`, `status≠success` → `✗`); `info|warning|error` → `system`.
- `active`: process `gemini`; cwd → `projects.json` → newest chat in that hash dir.
- Capabilities: `tool_results`, `usage`, `branching` (rewind) true; `working_detection` false (no reliable signal without a process).
- Because there is no local data, ship `tests/fixtures/gemini_session.jsonl` hand-built from `chatRecordingService.ts` shapes, and mark the adapter `experimental` in `Capabilities`/help text until validated on a real install.

### 4.6 t3code (`t3code.rs`)
- Read-only rusqlite on `~/.t3/userdata/state.sqlite`.
- `discover`: `projection_threads` JOIN `projection_projects` → id = `thread_id`, `title`, `cwd` = `worktree_path ?? workspace_root`, `created_at`, `modified_at` = max(`projection_thread_messages.created_at`). `linked` = (`provider_session_runtime.provider_name` mapped `claudeAgent→ClaudeCode`, `codex→Codex`, `opencode→OpenCode`, `resume_cursor_json.resume`). Archived threads (`archived_at` not null) get `status` hint → we don't own status; just include them.
- `load`: `projection_thread_messages` ordered by `created_at` → `user`/`assistant` rows (uuid = `message_id`, `message_id` = `turn_id`); interleave `projection_thread_activities` with `tone=tool` by `sequence`/turn: `tool.started` opens a `tool` row (uuid = `payload.toolCallId`, name = `payload.data.tool`), `tool.completed` appends snippet from `payload.detail`/`data.state.output`. `tone=error` → `system`. Streaming rows (`is_streaming=1`) rendered as-is (they are the latest projection).
- `active`: Electron process `T3 Code` running **and** `projection_thread_sessions.active_turn_id IS NOT NULL` (or `status=ready` with `provider_session_runtime.last_seen_at` < 60 s) for that thread. No lsof needed.
- **Dedup decision:** a t3 thread and its underlying Claude/Codex/OpenCode session will both appear. Phase 1 behaviour: show both, with the t3 row's `linked` rendered as a `↗ claude` hint in the sidebar. A `transcript.hide_linked_children` setting (default **off**) can collapse the underlying session. Revisit once real usage shows which view people want.
- Capabilities: `tool_results` true, `usage` false, `active_detection`/`working_detection` true, `borrow_surface` true; `watch: SqliteFile`.

---

## 5. Phased plan

Each phase is one PR, lands green, and is independently useful.

### Phase 0 — Fixtures & goldens
- Copy + redact one real session per provider from this machine into
  `rust/threadhop-core/tests/fixtures/<provider>/` (Codex rollout, OpenCode
  mini-db built via `sqlite3 .dump` of one session's rows into a fresh db,
  pi jsonl, t3 mini-db likewise). Hand-author Gemini.
- Add a `redact.sh` note in the fixtures README; strip absolute home paths
  → `/Users/x`, keep structure.
- Golden `expected.json` (`Vec<CleanedMessage>`) per fixture, generated by
  the adapter once written, reviewed by hand once.

### Phase 1 — Seam + Claude migration (no user-visible change)
1. `transcript/mod.rs` with `Provider`, `SessionRef`, `SourceLocator`,
   `Capabilities`, `TranscriptAdapter`, `registry()`, `project_slug()`.
2. `transcript/process.rs`: lift `parse_ps_pid_args_table`,
   `parse_lsof_multi`, and the `ps`/`lsof` runner from `session_detect.rs`
   into a shared `ProcessTable { pid → (args, cwd) }`. `session_detect.rs`
   becomes a thin re-export for compatibility.
3. `transcript/claude_code.rs` wrapping `jsonl` + Claude detect.
4. `CleanedMessage.provider` added; `jsonl::parse_byte_range` stamps
   `ClaudeCode`. Golden `sample_session_expected.json` regenerated (one
   extra field per row — verify no other diff).
5. TUI: `session_scanner.rs` → `transcript::discover_all()`;
   `fs_watcher.rs` → `watch_roots()` + `adapter(p).load(ref)`;
   `app.rs::derive_project_for_session` deleted (use `SessionRef.project`);
   `active_detector.rs` → `for a in registry() { a.active(&procs, &refs) }`.
   `SessionListItem` gains `provider`; `WorkerEvent::TranscriptRefreshed`
   carries the `SessionRef`.
6. `cli/mod.rs::display_name_and_project` → `adapter.describe()`.
7. `paths::claude_projects_dir()` becomes `pub(crate)` used only by
   `claude_code.rs`. Grep-gate: no `claude_projects_dir` outside
   `transcript/`.
- Exit: `cargo test` green, `./threadhop-rs` behaves identically,
  `sample_session_expected.json` diff is provider-field-only.

### Phase 2 — Codex adapter
- `codex.rs` per §4.2, fixture golden, sidebar badge appears, `--provider
  codex` filter on `threadhop-rs` + `search`/`peek` (where `borrow_surface`).
- Sidebar: provider glyph in the status column (`◐/●/○` stays; glyph goes
  after age, dimmed). Settings key `transcript.providers` to disable.

### Phase 3 — OpenCode adapter
- `opencode.rs` per §4.3 (v1 + v2 union). `WatchMode::SqliteFile` support
  in `fs_watcher.rs` (watch `-wal` mtime, debounce 500 ms).

### Phase 4 — pi adapter
- `pi.rs` per §4.4, including leaf-path linearisation test with a
  hand-built branching fixture.

### Phase 5 — Gemini adapter (experimental)
- `gemini.rs` per §4.5 with replay tests for `$set.messages` and
  `$rewindTo`. Help text marks it experimental.

### Phase 6 — t3code adapter
- `t3code.rs` per §4.6; `linked` hint in sidebar; `hide_linked_children`
  setting.

### Phase 7 — Persistence: `sessions.provider`
- Python `storage/db.py` migration 012: `ALTER TABLE sessions ADD COLUMN
  provider TEXT NOT NULL DEFAULT 'claude_code'`; bump
  `EXPECTED_SCHEMA_VERSION` to 12 in both cores; `models::Session.provider`.
- Rust `db::ensure_session_row(&SessionRef)` — inserts a minimal row
  (`session_path` = locator string, `project` = slug, `provider`) if
  missing, so **bookmarks, status tags, custom names and last_viewed work
  for every provider** without waiting for the Python indexer.
- Known gap, documented in help: FTS `search` only covers sessions the
  Python indexer has processed (Claude). Bringing the other five into the
  Python indexer, or porting the indexer to Rust, is a separate plan.

### Phase 8 — Borrow surface generalisation (optional, after 1–7)
- `exchanges.rs` currently re-parses raw Claude JSONL and keys
  `transfer_state.source_byte_offset` on byte offsets. Rebuild `Exchange`
  from `Vec<CleanedMessage>` and make the cursor a row ordinal (`u64`).
  For Claude this changes cursor semantics → `transfer_state` rows are
  reset once (cached summaries regenerate on next `prepare`). After this,
  `peek`/`prepare` work for any provider with `borrow_surface`.
- Until then, `borrow_surface = false` for non-Claude adapters and the CLI
  prints "not yet supported for <provider>".

### Phase 9 — `is_working` port (optional)
- Python's heuristic (mtime < 5 min + last row is user, or pending
  tool_use) implemented once over `Vec<CleanedMessage>` in `transcript/`,
  with per-adapter overrides where the store has a better signal
  (OpenCode `tool.state.status`, t3 `active_turn_id`, Codex
  `task_started`).

---

## 6. Cross-cutting concerns

- **Performance:** `discover()` runs every 5 s. Codex via `state_5.sqlite`
  and OpenCode/t3 via one indexed query are cheap; pi/Gemini are small
  file walks; Claude is unchanged. Cache `(path, mtime) → SessionRef` per
  adapter so unchanged files aren't re-read.
- **Locked databases:** OpenCode and t3 are usually running. Open
  read-only, `busy_timeout(200)`, treat `SQLITE_BUSY` as "keep last
  result".
- **Timestamps:** adapters normalise to ISO-8601 Z strings for
  `CleanedMessage.timestamp` (what `digest.rs` parses) and unix seconds
  `f64` for `SessionRef.{created,modified}_at`. Codex event times are unix
  **seconds**, OpenCode/pi-inner/t3-payload are **milliseconds** — one
  `transcript::time` helper, tested.
- **Tool-name casing:** `abbreviate_tool_use` and `digest.rs` file-touch
  detection match Claude names (`Read`, `Edit`, `Bash`). A single
  `canonical_tool_name(provider, raw) -> String` map in `transcript/`
  keeps the digest working for OpenCode (`bash`), Codex (`exec`), pi
  (`bash`), Gemini (`read_file`, `run_shell_command`).
- **No `Provider` matches outside `transcript/`:** enforce with a test
  that greps `threadhop-tui/src` and non-transcript core modules for
  `Provider::` and allows only `label()/glyph()/slug()` call sites.
- **Feature flag:** `transcript.providers` setting (JSON array of slugs)
  in the existing `settings` table; default all six. CLI `--provider`
  repeatable flag on `threadhop-rs`, `search`, `peek`, `prepare`.

---

## 7. Open questions

1. **t3code dedup** — show t3 threads *and* their underlying provider
   sessions (proposed default), or hide the child? See §4.6.
2. **pi abandoned branches** — render only the current leaf path
   (proposed), or also show abandoned branches folded under a `system`
   row?
3. **Reasoning/thinking** — currently dropped for Claude. Keep dropping
   for all providers (proposed), or add a `reasoning_summary` row type
   behind a setting?
4. **Gemini without local data** — acceptable to ship as experimental
   from source + synthetic fixture, or defer until you install Gemini CLI
   and produce one real session?
5. **Phase 7 schema bump** touches the Python core (migration 012). OK to
   include, or keep this plan Rust-only and accept that bookmarks/status
   won't work on non-Claude sessions until Python catches up?
6. **Codex injected context** — `response_item message role=user` includes
   AGENTS.md/environment blobs. Filter them out (proposed), or show them
   as folded `system` rows?

---

## 8. Non-goals

- Porting the Python indexer / FTS writer to Rust (separate plan).
- Reply-into-session for non-Claude harnesses.
- Reading OpenCode's dead `storage/*.json` layout.
- Windows/Linux process detection.
- Rendering Codex encrypted reasoning or Claude thinking blocks.

---

## 9. Verification

- Per-adapter golden test (`fixture → Vec<CleanedMessage>` snapshot).
- Claude golden unchanged modulo `provider` field (Phase 1 gate).
- Property test: every `CleanedMessage.uuid` unique within a session,
  every `timestamp` parses via `digest.rs`'s parser.
- Manual: `./threadhop-rs` on this machine shows Claude + Codex + OpenCode
  + pi + t3 sessions interleaved by `modified_at`, badges correct, opening
  each renders without a `system` error row.
- `cargo clippy -- -D warnings`, `cargo test --workspace`.

## 10. Why this order

Claude-first-with-zero-diff proves the seam holds before any new format
touches it (task-054's "two adapters make the seam real" rule). Codex is
next because it is closest to Claude (JSONL, tool call/result pairs,
active process). OpenCode forces the SQLite locator and watcher mode that
t3code then reuses. pi forces tree linearisation. Gemini is last among
file adapters because it's the only one without local data. Persistence
(Phase 7) is deferred until all adapters exist so the schema change is
made once with real requirements.
