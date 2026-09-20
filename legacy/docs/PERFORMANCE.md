# ThreadHop — Performance Design

Design decisions for the four near-term performance initiatives. Extracted
from the TUI redesign exploration on 2026-04-19 — the broader catalogue
of UI / UX ideas lives in [UI-IMPROVEMENTS.md](UI-IMPROVEMENTS.md);
these four are the ones scheduled for work.

ADR numbering continues the global sequence from
[DESIGN-DECISIONS.md](DESIGN-DECISIONS.md) so task references stay
unambiguous.

Status: **Design complete, implementation not started.**

---

## Table of Contents

- [Ordering & Rationale](#ordering--rationale)
- [ADR-023: Event-driven session discovery via fsevents](#adr-023-event-driven-session-discovery-via-fsevents)
- [ADR-024: Defer transcript parse until focus with lazy tail load](#adr-024-defer-transcript-parse-until-focus-with-lazy-tail-load)
- [ADR-025: Row-window virtualisation of TranscriptView](#adr-025-row-window-virtualisation-of-transcriptview)
- [ADR-026: Arrow/Parquet-backed DataTable via textual-fastdatatable](#adr-026-arrowparquet-backed-datatable-via-textual-fastdatatable)

---

## Ordering & Rationale

Two of the four are **backend / data-layer** changes that land alongside
the indexer and observer work in Phase 2. The other two are
**rendering-layer** changes that land after the observer ships and the
cross-session query surfaces (#14, #20, #21, #22, #45) expose tables
large enough to matter.

| # | ADR | Layer | Phase | Depends on |
|---|-----|-------|-------|-----------|
| 50 | ADR-023 | Backend — discovery | Phase 2 | #1 (SQLite), #8 (indexer) |
| 51 | ADR-024 | Backend — parse | Phase 2 | #8 (indexer) |
| 52 | ADR-025 | Rendering — transcript | Phase 7 | #51 |
| 53 | ADR-026 | Rendering — tables | Phase 7 | #14 (search) |

Backend-first because the rendering work assumes fresh, lazily-parsed
data. Virtualising a widget whose data source still does a synchronous
full-file parse just moves the stutter.

---

## ADR-023: Event-driven session discovery via fsevents

**Context:** `_gather_session_data()` runs in a background worker every
5 seconds. It scans `~/.claude/projects/**/*.jsonl`, reads the first 100
lines of each file for metadata, and rebuilds the session list. The
work is O(sessions) whether anything changed or not. Worst case the UI
lags up to 5 s behind Claude Code actually writing a message. Related
pieces already in flight: task #9 (incremental FTS indexing by byte
offset) and task #34 (observer sidecar watching a single file via
fsevents).

**Decision:** Replace the 5 s poll with a single `watchdog` observer
rooted at `~/.claude/projects`. On file-system events
(`FileModifiedEvent`, `FileCreatedEvent`, `FileDeletedEvent`), dispatch
a Textual `post_message` that the UI handles to update just the
affected session row. Polling is retained only as a cold-start path
(initial discovery on app launch).

**Rationale:**
- macOS `fsevents` (and Linux `inotify`) give sub-100 ms latency — the
  UI feels as live as Claude Code itself
- CPU at idle drops to zero — no more 5-second wake-ups
- The checkpoint logic task #9 already needs (per-file byte offset) is
  the right shape for incremental work triggered by events rather than
  polling
- `watchdog` transparently abstracts fsevents / kqueue / inotify, so
  the macOS-only concern only affects which event backend we pin at
  install time
- Existing session-detection (`ps` / `lsof` for active `claude`
  processes) stays on its own cadence — that's a different signal

**Rejected:**
- Keeping the 5 s poll and only cutting the per-file work. Still burns
  a wakeup; still has up-to-5-second lag; doesn't fix the idle CPU cost
  enough to justify staying on polling
- Inlining fsevents handling into the observer sidecar (task #34).
  That covers a single observed session; this ADR is about the
  **global** session-list refresh the TUI runs whether or not any
  observer is attached

**Implementation sketch:**
- Add `watchdog` to the `uv run --script` PEP 723 dependency block
- New module `fs_watch.py`: wraps `watchdog.observers.Observer`, emits
  typed Textual messages (`SessionFileChanged`, `SessionFileCreated`,
  `SessionFileDeleted`) on the UI thread
- `ClaudeSessions.on_mount()` starts the observer; `on_unmount()`
  stops it
- `_gather_session_data()` retained only for first-boot discovery;
  per-session state is updated by message handlers
- Incremental parse: each event carries a path → look up
  `index_state.byte_offset` → `seek()` → parse only the tail → update
  metadata + FTS row. Always parse up to the last `\n` (guard against
  partial flushes)

**Cost:** Small. New dependency (`watchdog` is well-known). Main
engineering is the partial-line guard and making sure the event stream
and the cold-start scan don't race.

**Reference idea:** [UI-IMPROVEMENTS.md — Performance Patterns](UI-IMPROVEMENTS.md#performance-patterns)
("Incremental re-index on fsevents"). Inspiration: lnav's byte-offset
checkpointing — [lnav performance](https://docs.lnav.org/en/latest/performance.html).

---

## ADR-024: Defer transcript parse until focus with lazy tail load

**Context:** `load_transcript()` opens the selected session's JSONL,
parses every line, and synchronously mounts a `UserMessage` /
`AssistantMessage` / `ToolMessage` widget for each. For a 50 MB session
this blocks the UI for visible seconds. The app already does a cheap
version of this on discovery (only reading the first 100 lines for
metadata), but the full parse on selection is the actual felt-latency
cost.

**Decision:** Keep discovery cheap (unchanged). On session focus, read
the **tail** of the file first (~64 KB), parse backwards from EOF,
mount the visible range immediately, then stream earlier messages in
from a background worker. Cache parsed message lists per session in an
LRU; invalidate when ADR-023 fires a file-change event for that
session.

**Rationale:**
- Users almost always want the most recent messages when they open a
  session — the tail-first read delivers those in tens of milliseconds
  regardless of file size
- The background fill keeps "scroll up to older messages" working
  without blocking the initial paint
- The LRU makes session-hopping feel instant after the first visit —
  paired with ADR-025's virtualisation this eliminates the "opening a
  big session hangs the TUI" failure mode
- Piggybacks on ADR-023's event stream for cache invalidation

**Rejected:**
- Pre-parsing all sessions in the background on startup. Wastes
  memory on sessions the user never opens and moves the cost rather
  than removing it
- Mounting widgets lazily *inside* `TranscriptView` without changing
  the parse model. The parse itself is the hot path — widget mount is
  a separate concern handled by ADR-025

**Implementation sketch:**
- Refactor `load_transcript()` to return an async iterator of message
  dicts rather than a list
- New helper `parse_tail(path, max_bytes=65536) -> list[dict]`: opens
  the file, seeks to `max(0, filesize - max_bytes)`, advances to the
  next `\n`, parses from there to EOF
- Spawn a `@work` background task that parses the remaining head (byte
  0 → tail-start) and yields message dicts as they resolve
- `TranscriptCache` — simple LRU keyed by `session_id`, value is
  `list[dict]`. Invalidated by `SessionFileChanged` messages from
  ADR-023
- `TranscriptView.load(session_id)` consumes the iterator and renders
  as messages arrive (today: mount each; after ADR-025: update the
  virtual window)

**Cost:** Medium. Mostly a refactor of `load_transcript` into an async
pipeline and teaching `TranscriptView` to accept incremental appends.
Low risk because the render logic itself doesn't change.

**Reference idea:** [UI-IMPROVEMENTS.md — Performance Patterns](UI-IMPROVEMENTS.md#performance-patterns)
("Defer heavy parse until focus"). Inspiration: k9s's resource-detail
fetch-on-drill-in pattern.

---

## ADR-025: Row-window virtualisation of TranscriptView

**Context:** Today `TranscriptView` mounts one widget per message. A
4-hour session with 3k messages produces 3k live widgets — each with
CSS, event handlers, and layout cost. Textual re-computes layout for
all of them on resize, theme change, and some scroll events. This is
the dominant cost for long sessions even after ADR-024 makes the
initial parse fast, because the widget mount loop has its own
bottleneck.

**Decision:** Replace the current `VerticalScroll`-of-all-messages
with a custom virtual-scroll container that mounts only the **visible
range + a small overscan buffer**. As the user scrolls, recycle widget
instances — the same N widgets get their content swapped rather than
new mounts.

**Rationale:**
- Rendering cost becomes constant in the session's length — a
  3k-message transcript behaves identically to a 100-message one
- Jump-to-message (e.g., from search results in #14, or timeline
  scrubbing if ever built) becomes O(1) instead of O(N) mounts
- Idiomatic in modern TUIs: Textual's own `DataTable` virtualises the
  same way, ratatui lists do it natively, TanStack Virtual is the
  web-world equivalent. We aren't inventing anything novel
- Enables the density toggle (UI-IMPROVEMENTS.md pattern #6) cleanly
  — compact rows and expanded rows both fit the same virtual-row API

**Rejected:**
- Using Textual's `DataTable` as-is to render messages. Message
  bodies aren't tabular — they have varying heights, inline markdown,
  selection state. `DataTable` is the wrong shape
- Fixing this with Textual's `OptionList`. Similar mismatch — no
  per-row-widget escape hatch, limited styling
- Deferring mount until scroll (lazy-append). Still ends up with N
  widgets when the user reaches the bottom, which is the common case

**Implementation sketch:**
- New widget `VirtualTranscript(ScrollView)`:
  - Holds the parsed message list as plain data (not widgets)
  - Maintains a small widget pool (`UserMessage`, `AssistantMessage`,
    `ToolMessage` instances) — one pool per message type
  - On `scroll` / `resize`, computes visible index range +
    ±OVERSCAN_ROWS buffer, pulls widgets from the pool, applies
    message data, mounts at the correct y-offset
  - Recycles widgets that fall out of range back into their pool
- Start with **fixed row height** (measured once per message type) +
  overflow ellipsis. Variable-height support lands in a follow-up —
  the density toggle handles most of the tall-message cases by letting
  the user collapse them
- Expand-on-focus: when a row receives focus, it can grow to full
  height and the container recomputes offsets below

**Cost:** Non-trivial. Not a library drop-in — this is a custom
scroll container. Budget a focused week. Single biggest performance
win in the roadmap.

**Reference idea:** [UI-IMPROVEMENTS.md — Performance Patterns](UI-IMPROVEMENTS.md#performance-patterns)
("Row-window virtualisation for the transcript"). Inspiration: Textual
`DataTable` internals, ratatui lists, TanStack Virtual (web).

---

## ADR-026: Arrow/Parquet-backed DataTable via textual-fastdatatable

**Context:** Textual's stock `DataTable` keeps every row as Python
objects in memory. Fine at a few hundred rows, painful past a few
thousand. ThreadHop's session list is small, but the cross-session
query surfaces landing in Phase 3 (tasks #20 `todos`, #21 `decisions`,
#22 `observations`, #45 `conflicts`, and the observation CLI queries
generally) and the full-text search results in task #14 will routinely
produce 10k–300k rows once real usage accumulates.

**Decision:** Adopt
[`textual-fastdatatable`](https://github.com/tconbeer/textual-fastdatatable)
as a drop-in replacement for `DataTable` at every data-heavy surface —
search results, cross-session query output, and (later) any Phase 3
CLI view ported into the TUI. Keep the stock `DataTable` for trivial
tabular surfaces where the row count is bounded (settings panes, help
overlay listings).

**Rationale:**
- Arrow's columnar layout means memory and sort/filter scale with the
  *projection size*, not the raw row count — idiomatic for FTS hits
  and cross-session aggregates
- SQLite → Arrow is a one-call conversion via `pyarrow.Table.from_pydict`
  on `fetchall()` output, so query pipelines stay straightforward
- The library is a direct `DataTable` API match — cutover is mostly
  import changes
- Parquet support means we can later materialise expensive queries
  (e.g., trigram-augmented search results in task #32) as on-disk
  tables the TUI reads lazily

**Rejected:**
- Staying on Textual's `DataTable` and paging queries manually. Moves
  complexity into every call site; doesn't fix sort/filter perf
- Building our own Arrow-backed table widget. The library already
  exists, is maintained, and matches the API we want

**Implementation sketch:**
- Add `pyarrow` and `textual-fastdatatable` to the PEP 723 dependency
  block (note: `pyarrow` is a ~50 MB wheel — acceptable for a local
  dev tool)
- Swap imports at every heavy surface (search panel, `todos` /
  `decisions` / `observations` / `conflicts` views when they come to
  the TUI). Light surfaces keep `DataTable`
- Query helpers in `db.py` grow a `fetch_arrow()` sibling to
  `fetch_all()` for callers that want Arrow straight out of SQLite
- Verify macOS wheels cover both Intel and Apple Silicon (they do as
  of `pyarrow` 13+)

**Cost:** Small — dependency weight and a straightforward API swap.
Main risk is the wheel size; mitigated by the fact that this is a
local dev tool, not a shipped binary.

**Reference idea:** [UI-IMPROVEMENTS.md — Performance Patterns](UI-IMPROVEMENTS.md#performance-patterns)
("Arrow/Parquet-backed DataTable"). Library:
[textual-fastdatatable](https://github.com/tconbeer/textual-fastdatatable).
