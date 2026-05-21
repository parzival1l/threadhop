# ThreadHop Rust TUI Port — Design Spec

## 1. Title, Date, Status

- **Title:** ThreadHop Rust TUI Port — Design Spec
- **Date:** 2026-05-20
- **Status:** Approved — ready for implementation planning
- **Branch:** `migration/rust-port`

## 2. Goal

Port the ThreadHop TUI from Python/Textual to Rust/ratatui so session switching
stops paying the cost of mounting and tearing down thousands of widgets. The
Python observer, reflector, CLI subcommands, harness, and plugin keep running
unchanged; the Rust binary lives next to them and reads/writes the same
`~/.config/threadhop/sessions.db`. The port is the wedge — once the TUI is on
ratatui and we have the data layer in Rust, future phases can absorb the rest
of the codebase one subsystem at a time.

## 3. Non-Goals

- **No port of the observer or reflector.** They keep running as Python sidecar
  processes invoked via `claude -p` (ADR-018, ADR-020). The Rust TUI only
  *reads* observation JSONL files.
- **No port of CLI subcommands.** `threadhop tag`, `bookmark`, `todos`,
  `decisions`, `observations`, `conflicts`, `observe`, `handoff`, `update`,
  `changelog`, `future`, `config` stay in Python.
- **No port of the harness or plugin.** `threadhop_core.harness.claude` and the
  `/threadhop:*` skills/commands stay in Python.
- **No replacement of the Python entrypoint.** `./threadhop` keeps working;
  the Rust binary ships side-by-side as `./threadhop-rs`.
- **No pixel-perfect visual mimicry.** The design directive is "same
  information, fresh layout" — designer freedom to play to ratatui's
  strengths.
- **No schema migrations from Rust.** Only Python writes migrations; Rust
  refuses to start against an unrecognized schema version.
- **No new features at the port boundary.** Anything not already in the Python
  TUI is out of scope until the port lands.

## 4. Architecture Overview

A Cargo workspace at `rust/` (currently empty / not yet created) with two
crates:

- **`threadhop-core`** — pure data + I/O. No TUI imports. Owns SQLite access,
  JSONL parsing, FTS queries, observation-file reads, macOS session
  detection, theme JSON loading. This crate is also the future Rust home for
  whatever the CLI/observer eventually consume; designing it as a clean
  library now keeps options open.
- **`threadhop-tui`** — ratatui + crossterm + tokio. Pulls in `threadhop-core`,
  owns the event loop, background workers, screens, widgets, and keymap.

Workspace tree (target layout — file creation happens during implementation,
not as part of this spec):

```
rust/
├── Cargo.toml
├── threadhop-core/
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── paths.rs
│       ├── models.rs
│       ├── db.rs
│       ├── jsonl.rs
│       ├── fts.rs
│       ├── observations.rs
│       ├── session_detect.rs
│       └── theme.rs
└── threadhop-tui/
    ├── Cargo.toml
    └── src/
        ├── main.rs
        ├── app.rs
        ├── event.rs
        ├── keys.rs
        ├── workers/{session_scanner.rs, active_detector.rs, fs_watcher.rs}
        ├── screens/{main.rs, search.rs, bookmark.rs, kanban.rs, help.rs, label_prompt.rs, confirm.rs}
        └── widgets/{session_list.rs, transcript.rs, find_bar.rs, digest_bar.rs, contextual_footer.rs}
```

A wrapper script `./threadhop-rs` at the repo root invokes
`rust/target/release/threadhop-tui` and forwards args. Building is opt-in
(`cargo build --release` from `rust/`); the Python entrypoint never touches
the Rust binary.

The core architectural shift driving the port: **ratatui is immediate-mode**.
Every frame redraws the visible viewport from scratch against an in-memory
`Buffer`. There are no mounted widgets to destroy when switching sessions —
the active-session pointer flips and the next frame draws the new content. A
session swap is a `Vec` index change, not a tree mutation.

## 5. `threadhop-core` Module Reference

**`paths`** — Resolves the canonical filesystem locations the app reads:
`~/.config/threadhop/sessions.db`, `~/.config/threadhop/config.json`,
`~/.config/threadhop/observations/`, `~/.claude/projects/`. Public API:
free functions returning `PathBuf`. Honors `HOME` only — no XDG fallback
(macOS-only project). One small helper to glob `~/.claude/projects/**/*.jsonl`.

**`models`** — Plain Rust structs mirroring the Pydantic models in
`threadhop_core/models.py`. `Session`, `Message`, `Bookmark`, `MemoryEntry`,
`Observation`, `UserTranscriptLine`, `AssistantTranscriptLine`,
`TranscriptLine` (enum over the two), plus the content-block enums
(`TextBlock`, `ToolUseBlock`, `ToolResultBlock`). The `Literal` aliases in
Python (`SessionStatus`, `MessageRole`, `MemoryType`, `MemorySource`,
`BookmarkKind`) become Rust enums with
`#[serde(rename_all = "snake_case")]`. JSONL lines use `serde(rename_all = "camelCase")`
plus explicit `#[serde(rename = "...")]` where the JSONL field name
deviates (e.g. `parentUuid`, `sessionId`, `isSidechain`, `toolUseResult`).
A `parse_transcript_line(raw: &[u8]) -> Option<TranscriptLine>` mirrors the
Python parser — logs and skips on validation failure, returns `None` for
`summary` / `ai-title` / `custom-title` / `system` / `meta` / unknown types.

**`db`** — Thin wrapper around `rusqlite`. Opens the shared database in WAL
mode with `journal_mode=WAL`, `synchronous=NORMAL`, `busy_timeout=5000`.
Verifies the schema version on open via a `schema_version_check()` that
reads the `settings` table (or `PRAGMA user_version` — whichever Python is
using) and refuses to start if it is newer than the version this binary
was compiled against. Exposes typed query helpers for the read paths
(`list_sessions`, `messages_for_session`, `bookmarks_for_session`,
`recent_searches`, `session_by_id`) and the three write paths
(`upsert_bookmark` / `delete_bookmark`, `update_session_status`,
`insert_recent_search`). Every write call is wrapped with a single retry
on `SQLITE_BUSY`; a second failure returns an error that surfaces in the
digest bar.

**`jsonl`** — Port of `threadhop_core.indexer.parse_byte_range`. Same
contract: take a byte slice, produce a `Vec<CleanedMessage>` with ADR-003
chunk merging, `<system-reminder>` stripping, tool-result-line skipping,
and tool_use abbreviation. This is the cleaned-transcript view the TUI
renders. Tests share fixtures with the Python test suite — same JSONL in,
equivalent output out. Also exposes `read_session_metadata(jsonl_path)`
which reads the first 100 lines to extract `cwd`, project, first-message
timestamp (matches the Python `_gather_session_data` head scan).

**`fts`** — FTS5 query builder. Two entry points: `prefix_search(query,
filters) -> Vec<SearchHit>` against `messages_fts`, and (Tier 2,
optional in MVP) `trigram_fallback(query)` against `messages_fts_trigram`
when prefix yields zero rows. Parses `project:foo`, `user:`, `assistant:`
modifiers from the raw query string. Returns hits with surrounding
context (snippet + 60 chars on either side) and the message UUID for the
jump-to-message action.

**`observations`** — Read-only access to
`~/.config/threadhop/observations/<session_id>.jsonl`. Streams the file
with a buffered reader and yields a `Vec<Observation>` shaped union:
`Decision`, `Todo`, `Done`, `Conflict`, `Note`, `Adr` — matching the
shapes the Python observer/reflector write. Also exposes
`latest_summary(session_id)` for the digest bar (newest decision / open
todo count / unresolved conflict count). Never writes; the TUI displays
read state for conflicts by joining against `conflict_reviews` from
`db`. Setting conflict review state goes through Python — the TUI links
out to `./threadhop conflicts --resolved` rather than writing the row
itself (decision rationale in §9).

**`session_detect`** — macOS process detection ported from
`threadhop_core/session/`. Two shell-outs: `ps -eo pid,args` to find
`claude` processes, then `lsof -a -d cwd -p <pid>` to resolve each
process's working directory. Matches PIDs to session IDs by inspecting
args (resume-by-id) or CWD (active in this project's session dir).
Returns a `Vec<ActiveSession>` with `pid`, `session_id` (when known),
`cwd`, `started_at`. Pure subprocess + parsing — no platform abstraction
because the project is macOS-only.

**`theme`** — Loads the OpenCode theme JSON. Public API: a `Theme` struct
with named color slots (background, foreground, role-user, role-assistant,
role-tool, accent, dim, ...) and a `load_theme(path)` that deserializes
the vendored JSON into the struct. The TUI translates `Theme` slots to
ratatui `Style` values at render time. Theme data is configuration, not
behavior — no hot reload.

## 6. `threadhop-tui` Module Reference

**`main`** — Entry point. Parses args (`--project`, `--session`, `--days`,
`--help`), opens the DB via `threadhop_core::db`, runs the schema version
check, initializes the terminal (`enable_raw_mode`, alt screen,
`EnterAlternateScreen`, mouse capture off), constructs the `App`, runs
the event loop, and restores the terminal on exit (including on panic
via a guard).

**`app`** — Top-level `App` struct holding the visible state: `sessions:
Vec<SessionRow>`, `selected_session_idx`, `transcript_lines:
Vec<RenderedLine>`, `transcript_scroll`, the active screen
(`Screen::Main | Search | Bookmark | Kanban | Help | LabelPrompt |
Confirm`), the digest-bar payload, the contextual-footer payload, any
banner/error message, and handles to the worker channels. Owns the
`render(frame: &mut Frame)` method that lays out the main surface and
delegates the active screen / modals. Holds an `EventHandler` and
processes one of: `Event::Key`, `Event::Resize`, `Event::Worker(...)`,
`Event::ModalResult(...)`, `Event::Tick`. State transitions are
synchronous; long work is delegated to workers.

**`event`** — The `tokio::select!` loop. Four sources: a crossterm
event stream (keys/resize), a render-tick timer (~16ms / ~60fps), the
worker MPSC channel (background results), and the modal-result channel
(modal screens push their outcome here when they close). Yields a single
`Event` enum each iteration; the `App` dispatches on it.

**`keys`** — Declarative key registry mirroring
`threadhop_core.tui.keybindings.COMMAND_REGISTRY`. A `Command` enum
(`NextSession`, `PrevSession`, `OpenSearch`, `ToggleBookmark`,
`OpenBookmarkBrowser`, `OpenKanban`, `OpenHelp`, ...) plus a scoped
key-to-command map per screen. The contextual footer reads from this
registry so labels stay consistent with bindings.

**`workers::session_scanner`** — Background task that scans
`~/.claude/projects/**/*.jsonl` every 5s, reads each file's first 100
lines via `jsonl::read_session_metadata`, joins against the `sessions`
table, and emits a `WorkerEvent::SessionsRefreshed(Vec<SessionRow>)`. The
App diffs against its current `Vec` and replaces in place — no widget
churn because there are no widgets.

**`workers::active_detector`** — Background task that runs
`session_detect::scan_active()` every 5s on a separate cadence from the
file scanner, emits `WorkerEvent::ActiveSessionsRefreshed(Vec<ActiveSession>)`.
The App merges active markers into its `sessions` vector.

**`workers::fs_watcher`** — Watches the currently-selected session's
JSONL file for size changes (and optionally the observation file).
Emits `WorkerEvent::SessionGrew { session_id, new_bytes }` so the
transcript view can incrementally re-render. MVP uses polling at 1Hz
on the active session only — `notify` backend lives in §14 open
questions.

**`screens::main`** — The two-pane layout: session list on the left,
transcript on the right, digest bar across the top of the transcript
pane, contextual footer along the bottom. This is the always-rendered
backdrop; modal screens draw on top of it.

**`screens::search`** — Modal full-text search. Input box at top,
results list below. Each keystroke debounces ~30ms then calls
`fts::prefix_search`. Enter on a hit closes the modal and emits a
`ModalResult::JumpToMessage { session_id, message_uuid }` which the
App uses to switch the active session and scroll the transcript to
that message.

**`screens::bookmark`** — Modal browser over the `bookmarks` table.
Lists bookmarks newest-first with session label + note + snippet.
Enter jumps to the bookmarked message (same jump-to-message action as search). `d`
deletes a bookmark with a `Confirm` modal interstitial.

**`screens::kanban`** — Modal status board grouping sessions by
`status` column (`active | in_progress | in_review | done | archived`).
`h/l` moves between columns, `j/k` moves within a column, Enter
switches to that session, `t` opens the label prompt to change status.

**`screens::help`** — Context-aware overlay reading from `keys`. Shows
bindings relevant to the current scope (main vs each modal). Driven
by the same registry the contextual footer uses.

**`screens::label_prompt`** — Single-line input modal used to set a
session's custom name or status. Returns `ModalResult::LabelEntered(String)`.

**`screens::confirm`** — Generic yes/no modal. Used for bookmark
deletion and any other destructive action.

**`widgets::session_list`** — Renders the session list pane. One line
per session: status icon (`◐ working / ● active / ○ inactive`) +
optional observation indicator + custom name (falls back to first user
message) + age. Highlights the selected row. Stateless render against
the App's `sessions` vector.

**`widgets::transcript`** — Renders the cleaned transcript for the
selected session. Reads the session's JSONL bytes (via `jsonl`),
applies `parse_byte_range`, formats each message into ratatui `Line`s
with role-colored left gutters (user / assistant / tool). Supports
scroll, find-in-transcript highlighting, bookmark markers, and the
"jump to message UUID" entry point used by search and the bookmark
browser. **This widget is where the speed win lives**: a session
switch swaps the cached `Vec<RenderedLine>` and the next frame draws
the visible window — no per-message widget construction.

**`widgets::find_bar`** — In-transcript find. Activated with `/` while
on the main screen (distinct from cross-session search, which is its
own modal). Highlights matches in the rendered transcript and lets
the user `n`/`N` to step through them.

**`widgets::digest_bar`** — Horizontal status strip across the top of
the transcript pane: newest decision (truncated), open-TODO count,
unresolved-conflict count, last-modified timestamp, observer-active
indicator. Sourced from `observations::latest_summary` plus DB joins.

**`widgets::contextual_footer`** — Bottom strip showing the bindings
relevant to the current scope. Reads from `keys::COMMAND_REGISTRY`,
filtered to the active screen. Matches the Python footer's content
even if the visual treatment differs.

## 7. Crate Dependencies

| Crate | Key deps | Why |
|-------|----------|-----|
| `threadhop-core` | `rusqlite` (bundled SQLite, `serde_json` feature) | DB access; `bundled` avoids depending on system libsqlite version |
| `threadhop-core` | `serde`, `serde_json` | JSONL line parsing + theme JSON |
| `threadhop-core` | `thiserror` | Per-module error enums |
| `threadhop-core` | `time` or `chrono` | Timestamp parsing for JSONL `timestamp` ISO-8601 strings |
| `threadhop-core` | `dirs` | `~` expansion for HOME-based paths |
| `threadhop-core` | `tracing` | Structured logging at parse boundaries |
| `threadhop-tui` | `ratatui` | Immediate-mode TUI rendering |
| `threadhop-tui` | `crossterm` | Terminal backend + event stream |
| `threadhop-tui` | `tokio` (`rt-multi-thread`, `macros`, `sync`, `time`, `process`) | Async runtime for `select!` + workers + subprocess for `ps`/`lsof` |
| `threadhop-tui` | `tokio-util` | `EventStream` adapter for crossterm |
| `threadhop-tui` | `anyhow` | Top-level error type at TUI boundaries |
| `threadhop-tui` | `tracing-subscriber` | Log sink (file in `~/.config/threadhop/logs/`) |
| `threadhop-tui` | `clap` (derive) | Argv parsing for `--project`, `--session`, `--days` |
| dev / tests | `insta` | Snapshot tests on rendered ratatui `Buffer`s |
| dev / tests | `tempfile` | Throwaway SQLite + JSONL fixtures |

## 8. Event Loop / Data Flow

The event loop is a `tokio::select!` over four sources, polling whichever
fires first:

1. **Crossterm event stream** — `Event::Key`, `Event::Resize`. Dispatched
   to the active screen's key handler, which mutates `App` state and
   optionally produces a `Command` (e.g. `OpenSearch`).
2. **Render tick** — ~16ms interval. On each tick the App calls
   `terminal.draw(|frame| app.render(frame))`. Rendering is pure — it
   reads state and writes to ratatui's `Buffer`. No I/O.
3. **Worker channel (MPSC)** — `WorkerEvent` variants from the
   session-scanner, active-detector, and fs-watcher tasks. The App
   merges results into its state (`sessions` vec, active markers,
   transcript-grew notifications).
4. **Modal-result channel** — modal screens push a `ModalResult` when
   they close (jump-to-message, label-entered, confirm-yes, etc.). The
   App dispatches on the variant to mutate state and pop back to the
   main screen.

**The session-switch fast path:** when the user presses `j`/`k` on the
session list, the App mutates `selected_session_idx`. On the next render
tick the `transcript` widget reads
`app.sessions[selected_session_idx].cached_render` (or loads + caches
it if cold) and writes to the frame buffer. There are no widgets to
destroy, no children to remount, no virtual DOM diff. This is the
entire point of the port.

**Render caching:** Each `SessionRow` carries `Option<RenderedTranscript>`.
First selection loads bytes, parses via `jsonl::parse_byte_range`, and
caches the resulting `Vec<RenderedLine>`. Subsequent selections are
O(1). Cache is invalidated when fs_watcher reports growth on that
session. An LRU cap (say 16 sessions) keeps memory bounded.

## 9. SQLite Sharing Policy

The Rust binary and Python share `~/.config/threadhop/sessions.db`.
Coexistence rules:

**Rust READS from:**
`sessions`, `messages`, `messages_fts`, `messages_fts_trigram`,
`bookmarks`, `index_state`, `observation_state`, `conflict_reviews`,
`recent_searches`, `settings`.

**Rust WRITES to (three tables only):**
- `bookmarks` — insert / delete from the bookmark toggle on the
  transcript view and the bookmark browser's delete affordance.
- `sessions` — `UPDATE` of `status`, `custom_name`, `sort_order`,
  `last_viewed` columns. Never `INSERT`/`DELETE`.
- `recent_searches` — `INSERT` on every committed FTS query (after
  Enter in the search modal, not per-keystroke).

**Rust NEVER WRITES to:**
- `messages`, `messages_fts`, `messages_fts_trigram` — owned by the
  Python indexer.
- `observation_state`, `conflict_reviews` — owned by the Python
  observer/reflector and the `conflicts --resolved` CLI subcommand.
- `index_state`, `settings` — written only by Python migrations and
  CLI commands.
- Schema migrations — Rust never runs them.

**Schema version handshake.** On startup, Rust reads the schema version
(via the same mechanism Python uses to track applied migrations — to be
finalized during Phase 1 by inspecting `settings` / `PRAGMA user_version`).
If the version is *newer* than the version this Rust binary was compiled
against, Rust prints a one-line "your Python schema is ahead — rebuild
the Rust binary with `cargo build --release` in `rust/`" and exits non-zero.
If the version is *older* (Python hasn't been run since an upgrade),
Rust prints "run `./threadhop` once to apply pending migrations" and
exits non-zero. The Rust binary never runs migrations itself.

**WAL mode.** Rust sets WAL on open. WAL is idempotent at the SQLite
level — if Python already enabled it, this is a no-op. WAL allows
concurrent readers and one writer, which is exactly the
Python-observer-plus-Rust-TUI case.

**Write-conflict behavior.** All Rust writes go through a small
`with_busy_retry` helper: try, on `SQLITE_BUSY` sleep 50ms and retry
once, on a second failure return the error. The error bubbles up to the
App which surfaces a transient banner in the digest bar ("Save failed —
DB busy, try again"). The user's input is not lost; the action is just
not committed.

## 10. Error Handling

- Each `threadhop-core` module defines its own `thiserror` enum
  (`DbError`, `JsonlError`, `FtsError`, `ObservationError`,
  `SessionDetectError`, `ThemeError`). Functions return `Result<T, ModuleError>`.
  Modules never panic on bad input — malformed JSONL, busy DB, missing
  files all return typed errors.
- `threadhop-tui` uses `anyhow::Result<()>` at the App / main boundary.
  Internal helpers may still use specific error types where they help.
- **Workers never panic.** Each background task is wrapped so a panic
  becomes a `WorkerEvent::Error(String)` instead of taking down the
  runtime. The App surfaces these via the digest-bar banner.
- **Terminal restoration on panic.** A `Drop` guard on a tiny
  `TerminalGuard` struct in `main` calls `disable_raw_mode` and
  `LeaveAlternateScreen` even if the App panics, so the user's
  terminal isn't wrecked.
- Logs go to `~/.config/threadhop/logs/threadhop-rs.log` via
  `tracing-subscriber` (file appender, daily rotation, INFO by default,
  DEBUG with `-v`).

## 11. Testing Strategy

- **`threadhop-core` unit tests** live alongside each module
  (`#[cfg(test)] mod tests`). Use `tempfile::tempdir()` for SQLite +
  JSONL fixtures. Share fixture inputs with the Python test suite where
  possible — copy a couple of representative JSONL files into
  `rust/threadhop-core/tests/fixtures/` and assert
  `jsonl::parse_byte_range` produces output equivalent to the Python
  `parse_byte_range` (round-trip via a small Python script that dumps
  expected JSON during fixture setup).
- **`threadhop-tui` snapshot tests** use `insta` against rendered
  ratatui `Buffer`s. Build a deterministic App state from a fixture,
  call `app.render(frame)` with a fixed-size backend buffer, dump the
  buffer to text, snapshot. Covers the session list, transcript
  rendering, digest bar, each modal screen.
- **Integration tests** live in `rust/threadhop-tui/tests/` (or a
  workspace-level `tests/` directory) and drive the App with synthetic
  `Event::Key` sequences against an ephemeral DB seeded from fixtures.
  Verify: opening search → typing → results appear → Enter switches
  session and scrolls to the hit; toggling a bookmark commits to DB and
  appears in the browser modal; switching sessions does not leak memory
  across 100 swaps; schema version mismatch exits non-zero with the
  expected stderr message.
- **No GUI-level / golden-image testing.** Snapshot-on-`Buffer` is the
  equivalent and is text-diffable.

## 12. Phase Plan

Each phase ends with a working binary. Phase N+1 builds on N without
breaking the previous milestone.

**Phase 1 — Workspace + `threadhop-core` data layer.**
Stand up the Cargo workspace at `rust/`. Implement `paths`, `models`,
`db` (read paths only + schema version check + WAL setup), `jsonl`
(`parse_byte_range` + `read_session_metadata`), `fts` (prefix search),
`observations` (read), `session_detect`, `theme` (load JSON). Full unit
test coverage with shared JSONL fixtures. No TUI yet — `cargo test`
green is the milestone.

**Phase 2 — Bare ratatui app: main screen, session list, transcript.**
First ship-able milestone. `threadhop-tui` skeleton: `main`, `app`,
`event`, `keys`, the session-scanner and active-detector workers,
`screens::main`, `widgets::session_list`, `widgets::transcript`,
`widgets::contextual_footer`. Read-only — no writes, no modals. The
binary launches, lists sessions, switches between them instantly,
renders transcripts. This is where we prove the perf claim.

**Phase 3 — FTS search modal + find-in-transcript.**
Add `screens::search`, `widgets::find_bar`, the `recent_searches`
write path. Per-keystroke debounced prefix search, jump-to-hit, project
and role filters. Find-bar (`/`) on the main screen for in-transcript
matching.

**Phase 4 — Bookmarks + tag/status writes.**
Add `screens::bookmark`, `screens::label_prompt`, `screens::confirm`.
Wire the bookmark toggle on the transcript view, the bookmark browser
with delete-confirm, the label-prompt for custom name / status. First
phase that writes to the DB; exercises `with_busy_retry` and the
digest-bar error banner.

**Phase 5 — Kanban + digest bar + observation reads + conflict viewer.**
Add `screens::kanban`, `widgets::digest_bar`, integrate
`observations::latest_summary`. Display conflict markers on the session
list and in the digest bar; clicking through to conflict details opens
a read-only viewer that shells out to `./threadhop conflicts --resolved`
for the resolution action.

**Phase 6 — Polish.**
Load and apply the OpenCode theme. Bring `widgets::contextual_footer`
and `screens::help` to parity with the Python registry. Add the
schema-version mismatch UX, the panic guard for terminal restoration,
file logging via `tracing-subscriber`. Build a release binary; wire
the `./threadhop-rs` wrapper script.

**Phase 7 — Semantic search (planned, out of current MVP scope).**
Add context-driven search: "in what conversation did we talk about X"
where X is a phrase, not a term. This is *deferred* until after the MVP
ships, but the search-layer modules in Phases 2–3 must be designed with
a clean seam so Phase 7 doesn't require rewriting `fts.rs` or the search
modal.

*Architecture (intended):*
- **Vector store:** `sqlite-vec` extension, embedded in the same
  `sessions.db` Rust already opens. No new daemon. Both Python and Rust
  load the extension at connection time.
- **Embedding model:** Local ONNX inference via the `ort` Rust crate.
  Default model: `bge-small-en-v1.5` (~33MB, fast on Apple Silicon).
  Avoids API roundtrips and per-query cost.
- **Retrieval:** *Hybrid* — FTS5 retrieves the top ~50 lexical candidates,
  vector cosine reranks to top ~10. Best precision + recall for chat
  queries that mix terms and intent.
- **Indexing:** Extend the Python observer pipeline (out of Rust scope)
  to emit embeddings for new messages into a `message_embeddings` table.
  Rust reads that table; never writes to it.
- **New Rust module:** `threadhop-core::search_semantic` (sibling of
  `fts.rs`). Adds a `search_semantic(query) -> Vec<Hit>` function and a
  `search_hybrid(query) -> Vec<Hit>` that composes FTS + vec rerank.
- **TUI surface:** The existing search modal grows a mode toggle
  (`f` lexical / `s` semantic / `h` hybrid). No new modal needed; the
  result-rendering and jump-to-message flow are reused.

*Seam requirements for Phases 2–3 (must be in place before Phase 7):*
1. `fts::search()` returns a `Vec<Hit>` where `Hit` carries
   `message_uuid`, `session_id`, `snippet`, `score`. The
   `search_hybrid()` function composes the same `Hit` type.
2. The search modal must not assume FTS-only — its query interface
   takes a `dyn SearchProvider` so Phase 7 can swap implementations
   without touching screen code.
3. Message-row queries must always return the canonical `message_uuid`;
   never an FTS-internal rowid that the vector layer can't join on.

*Alternative direction (worth noting, not chosen yet):* A
conversational interface where the user asks "where did we talk about
saving?" and Claude reads top-K FTS hits and answers in prose with
citations. Leverages the existing harness. Compatible with the
embedding path — both can ship.

## 13. Development Workflow

The "fresh layout" decision means visual feel is a live design loop, not
something to settle from screenshots. During Phases 2-6, keep a
long-running shell open with the binary under `cargo watch`:

```
cd rust && cargo watch -x 'run -p threadhop-tui --release'
```

(Or, if `cargo-watch` isn't installed, the equivalent: a monitored
background process the implementation agent restarts on each
meaningful change.) Iterate on layout, spacing, color application, and
behavior with live visual feedback. Snapshot tests catch regressions;
the live binary catches "this feels off." For the implementation agent
this means launching the binary as a background process early and
re-checking it after each substantive change rather than waiting for a
batch of changes to accumulate.

Release builds are recommended even during development because ratatui
is fast enough that a debug build's frame budget can mask perf
regressions. Compile time stays manageable because the workspace is
small.

## 14. Open Questions / Future Work

Explicitly deferred — not blockers for the port, but worth flagging
before they become surprise scope:

- **Keybinding chord support.** The Python registry is single-key; the
  Rust keymap should leave room for chords (`g g`, `<leader>x`) without
  designing them in v1.
- **Terminal-image rendering.** Any image content in transcripts (rare
  but exists) is currently rendered as a placeholder. Whether to use
  `ratatui-image` or `viuer` or skip entirely is unresolved.
- **`notify` vs polling for the fs watcher.** MVP polls the active
  session at 1Hz; `notify` would give us instant updates at the cost of
  a non-trivial dep. Decide after Phase 2 perf measurements.
- **Schema migrations while Rust is running.** If Python applies a
  migration mid-session, Rust holds stale prepared statements and may
  hit confusing errors. MVP behavior: don't try to recover — surface a
  "Python upgraded the DB, please restart" banner and degrade to
  read-only until restart. Worth revisiting if it bites in practice.
- **Future port path for observer / reflector / CLI / harness.** Once
  the TUI port stabilizes, the obvious next moves are (a) port the
  CLI subcommands to Rust against the existing `threadhop-core` (low
  risk, already shaped for it), and (b) decide whether the observer
  stays Python (because it shells to `claude -p` and that's
  language-agnostic) or moves. Plugin stays as-is regardless.
- **Packaging.** How users install the Rust binary is undecided.
  Options: `cargo install --path rust/threadhop-tui`, Homebrew tap,
  prebuilt release artifacts attached to GitHub releases. The Python
  install path (`uv tool install` / repo clone) isn't affected.
- **Schema version mechanism.** Phase 1 needs to finalize whether
  Rust reads the version from `settings`, `PRAGMA user_version`, or a
  dedicated migration-tracking row. Pick whichever Python authoritatively
  uses; if Python uses more than one, pick one and stop using the other
  on the Python side too.
- **Mouse support.** Disabled in MVP. Selection-by-mouse and click-on-
  session would be nice but introduce a non-trivial state machine.

## 15. References

ADRs in `docs/DESIGN-DECISIONS.md` that this port must respect:

- **ADR-001** — SQLite over JSON for metadata storage. Rust opens the
  same DB; never reverts to JSON.
- **ADR-003** — Chunk merging for assistant messages. The Rust
  `jsonl::parse_byte_range` must produce output equivalent to the
  Python implementation; tests share fixtures.
- **ADR-004** — Conductor-style status tags. The `SessionStatus` enum
  and the CHECK constraint in `_migration_006_sessions_status_check`
  encode the same invariant; Rust's `SessionStatus` enum is the third
  copy and must stay in lockstep.
- **ADR-006** — Strict LLM vs instantaneous boundary. The Rust TUI
  inherits the "instantaneous in the TUI, LLM stays in skills" rule.
  Bookmarks/tags/search are Rust-native; handoff stays Python.
- **ADR-007** — Real-time search architecture. Rust implements the same
  FTS5 prefix-matching design and leaves trigram fallback for later.
- **ADR-018** — Observer as core function. The Rust TUI never invokes
  `claude -p`; the Python observer keeps that role.
- **ADR-019** — Per-session observation files with SQLite state tracking.
  Rust reads the JSONL; the state row in `observation_state` is
  Python-owned.
- **ADR-020** — Unified observation JSONL (observer + reflector share
  one file). Rust treats the file as append-only and read-only; it
  surfaces decisions, TODOs, and conflicts from the same stream.
- **ADR-028** — Harness adapter seam. Unaffected by the port (Rust
  never invokes the harness), but worth noting because the future
  "port the observer to Rust" question lands here.
