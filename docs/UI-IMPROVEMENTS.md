# ThreadHop — UI / UX Improvement Ideas

Captured from a design exploration on 2026-04-19. Survey of modern TUI
patterns (agentic CLIs, transcript/log viewers, dashboard TUIs, Charm /
ratatui aesthetics) to inform a radical next-gen redesign.

Status: **Ideas only — not scheduled.** Pull items into `TASKS.md` when
promoted.

---

## Table of Contents

- [Borrowable Patterns (12)](#borrowable-patterns-12)
- [Aesthetic Moves Worth Stealing](#aesthetic-moves-worth-stealing)
- [Performance Patterns](#performance-patterns)
- [Three-Thing vNext](#three-thing-vnext)
- [Sources](#sources)

---

## Borrowable Patterns (12)

### 1. Transient menus (Magit)

Prefix key → floating popup of grouped commands **with togglable flags**
before you fire. Not a palette — a stateful command-builder. Maps
naturally to `handoff --include-sidechains --from=<uuid>`.
**Textual difficulty:** Medium (`ModalScreen` + grid; the infix flag
state is the hard bit).

### 2. Hotlist / activity strip (WeeChat)

One-line strip showing only sessions that changed since last look,
coloured by urgency. `Alt+A` cycles. Replaces the 36-col sidebar when
you have 20+ sessions.
**Easy.**

### 3. SQL-over-transcripts ad-hoc view (lnav)

Press `;` → SQL prompt queries JSONL as virtual tables. Example:
`SELECT sessionId, count(*) FROM tool_call WHERE tool='Bash' AND ts > today()-7`.
Already have SQLite + FTS; mostly schema work.
**Medium.**

### 4. Threaded message tree pane (NeoMutt)

Collapsible ASCII tree in a left gutter mirroring the transcript.
Sidechains render as `├─ subagent(researcher): …` under their parent.
Different from a full DAG — it is a gutter, not a canvas.
**Medium.**

### 5. Timeline rail with live-preview filter (lnav 0.14)

Top/bottom strip that is both scrubber and filter. Drag handles →
matching lines dim in-place before you commit. "Show only user messages
3–5 pm."
**Medium.**

### 6. Density toggle (Crush)

One binding flips the entire UI between verbose and one-line-compact.
Crush ships `compact_mode`. 10× scrolling speed-up.
**Easy.**

### 7. btop-style toggleable regions

Overview screen with N panels, digit keys `1`–`5` hide/show each.
Sparkline-per-project, open TODOs, blocked-on-user sessions.
**Easy–medium.**

### 8. Zoomable time-series sparkline (zenith)

`+`/`-` zooms time axis above the transcript. Click-scrub ties sparkline
position to transcript scroll. See the 3-hour gap or 40-tool-call burst.
**Medium.**

### 9. Chrome-style session tabs (opencode proposal)

Persistent top tabs with per-tab status (spinner / attention / error /
done). Preserves scroll position unlike list selection. Hybrid with #2:
tabs for pinned, hotlist for the long tail.
**Easy.**

### 10. Subagent split panel (opencode proposal)

When a session has sidechains, peel them into a right-side panel —
"what is the subagent doing now." Currently they are inlined and
visually indistinguishable.
**Medium.**

### 11. Regex-capture-groups-as-columns (lnav)

User types a regex with named captures → lnav materialises it as a SQL
table. For ThreadHop: `TODO: (?<task>.+)`,
`` ```(?<lang>\w+)\n(?<code>.+?)``` ``. Save, reuse, query across
sessions. More powerful than tags.
**Medium.**

### 12. Discoverability popup (Helix / which-key)

Press `space` → floating menu of every valid next binding,
context-aware. Replaces the static help overlay (ADR-017) with a
keyboard tutor that teaches as you use it. The command registry already
has the data.
**Easy.**

---

## Aesthetic Moves Worth Stealing

- **Glamour-rendered markdown** for assistant messages (Textual 5's
  `Markdown` widget). Real code-fence syntax highlighting.
- **Single accent gradient** (Crush charmtone) instead of per-message
  background tints. Less palette sprawl.
- **Nerd Font glyphs** for density —  /  /  / . Gate behind a
  setting.
- **Chrome reduction** (Helix) — kill borders, use 1-px dividers +
  background tint.
- **Contextual bottom key-hint bar** (k9s, Crush) that shrinks as focus
  changes.
- **Kitty graphics protocol** for real line charts + inline avatars on
  iTerm2 / Kitty / WezTerm / Ghostty. Textual cannot render inline
  raster — shell out to `kitty +kitten icat`. Unicode fallback required.
- **Turn on Textual smooth pixel scrolling** (2.0+). Near-zero-code
  perceptual win.

---

## Performance Patterns

- **SQLite virtual tables over raw JSONL** (lnav). Index by byte offset
  + timestamp; do not ingest row-per-message. See
  https://docs.lnav.org/en/latest/performance.html.
- **Arrow / Parquet-backed DataTable** via
  [`textual-fastdatatable`](https://github.com/tconbeer/textual-fastdatatable).
  Drop-in, loads 300k rows instantly.
- **Row-window virtualisation for the transcript.** Mount only visible
  range + buffer, reuse widgets. Current `VerticalScroll` mounts
  everything — the single biggest perf win for long sessions.
- **Incremental re-index on fsevents** — parse only the bytes after the
  last checkpoint.
- **Defer heavy parse until focus** (k9s). Session list stays cheap;
  full JSONL loads only on selection.
- **Cap indexer CPU** (btop) at a fixed fraction. A laggy TUI is more
  noticeable than slightly-slower search.

---

## Three-Thing vNext

Minimum set that delivers the biggest felt improvement:

1. Virtualise transcript rendering + turn on Textual smooth scroll.
2. Replace the sidebar with Chrome tabs (#9) + WeeChat hotlist (#2).
3. Add the `:` SQL prompt over a JSONL virtual table (#3) — unlocks #5,
   #8, #11 almost for free.

## The Hot Take

ThreadHop vNext is **lnav for AI transcripts**: single scroll surface
warpable with a `:` SQL prompt, a scrubbable / filterable timeline rail
up top, a collapsible thread-tree in the left gutter (not a separate
DAG), a Magit-style transient menu for `handoff` / `tag` / `fork` with
their flags, and a hotlist strip + Chrome tabs for multi-session
presence without a permanent 36-col sidebar. Storage: virtual tables
over JSONL — never re-parse on launch. Trade-off: the most beautiful
version assumes Kitty / iTerm2 / WezTerm; Unicode fallbacks required.
If aesthetics truly dominate, the honest answer is a Bubble Tea rewrite
in Go — but ~90% of the functional ideas ship in Textual today.

---

## Turn-as-unit transcript rendering

Captured 2026-04-20 alongside the bookmark-FK crash fix.

### What already landed (interim fix)

The crash was resolved by two small id-layer changes in the renderer
(`threadhop._parse_messages`) plus a defensive guard in
`bookmark_toggle_selection`:

- Consecutive assistant JSONL lines sharing a `message.id` now merge
  into one assistant tuple tagged with the first chunk's uuid —
  matching the indexer's rule (ADR-003).
- `tool_use` widgets and `tool_result` widgets now inherit the
  enclosing turn's canonical uuid instead of their own JSONL line's
  uuid. `toolUseResult` user lines no longer flush the pending
  assistant turn — they're mid-turn events.
- `bookmark_toggle_selection` pre-checks the FK target with a SELECT,
  dedups duplicate uuids within a selection, and shows a "skipped
  (not indexed yet)" toast instead of crashing on any remaining
  mismatch.

Net effect: bookmarks now work on any widget inside an assistant turn
(prose, tool call, tool result) because they all resolve to the same
turn id — the one the indexer actually stores. This is
**turn-as-unit semantics applied only to the identity layer**;
rendering is still per-widget.

### What's still open (visual refactor)

**The unit of meaning in a Claude conversation is the turn, not the
JSONL event.** Today the renderer walks the file line by line and
emits one visible block per event — assistant prose, each tool use,
each tool result — so a single conversational turn can explode into
six or seven stacked blocks. In selection mode each fragment is a
separate navigation stop, which makes long sessions feel bloated and
makes range-selection for copy / export / bookmark awkward. Search,
export, and the handoff skill all really want turn-shaped data too.

The proposed structural change: treat everything between two genuine
human messages as **one selectable block**. Internally the block
still shows its structure (prose, dim `⚙ Reading …` lines for tool
calls, notable tool results), but outwardly it is one unit with one
id — the id of the turn's first assistant event, which is already
what the indexer stores.

### Open design questions (park until we pick this up)

1. **Turn boundary.** Strictly "next genuine human message," or do
   we also break on a sidechain spawn, session resume, or long idle
   gap? Current instinct: only genuine human input ends a turn;
   everything else stays inside the block. Explicitly call out
   **sidechain spawns** — they're the only in-file event that marks
   "main turn paused for something on the side," and their rendering
   is already called out separately in pattern #10 (subagent split
   panel). If we keep sidechains inside the parent turn, the split
   panel still works; if we break turns on sidechain spawn, the
   parent turn has to be reassembled for export.

2. **Tool result inclusion.** Keep the current filter (show
   "Found 12 files", "+3/-1 lines", errors; drop bare success) and
   fold survivors into the turn block. Errors probably always visible.

3. **Visual structure.** Flat flowing section (matches current
   aesthetic, less code) vs. subtly-bordered card with collapsible
   tool details (buys "hide tool calls" toggle, adds work). Ties into
   density-toggle (#6).

4. **Search jump target.** When an FTS hit lands inside a tool-call
   snippet, jump to the enclosing turn block and highlight within —
   same mechanism as jumping to a snippet inside a long prose
   message, just widened.

### Scope implications if/when adopted

- Renderer: replace per-line emission with a turn composer that
  buffers events until the next genuine user message.
- Indexer: already merges assistant text + tool-call abbreviations;
  consider folding surviving tool-result summaries so the row text
  matches what the user sees inside the turn block. Row identity
  (first-assistant-event uuid) is unchanged.
- Selection / bookmarks / copy / export: one widget = one target, so
  these simplify rather than complicate.
- Live-turn edge case: when the assistant is mid-turn during a
  refresh, the block is incomplete and trailing events keep
  arriving. The renderer must re-compose in-progress turns without
  flickering or losing scroll position. This is where the 5-second
  refresh interacts — design for it, don't discover it.

### Payoff

- Selection mode matches the user's mental model ("one reply = one
  thing"), not the file format's event model.
- Observer / reflector / handoff skill already think in turns and
  decisions — aligning the renderer and indexer closes the last
  semantic seam across the app.
- The bookmark FK can never drift out of sync again, because only
  one id per turn is ever exposed.

---

## Sources

Agentic CLI / TUI:
- [Crush (Charm)](https://github.com/charmbracelet/crush),
  [TUI arch](https://deepwiki.com/charmbracelet/crush/5.1-tui-architecture),
  [Styling](https://deepwiki.com/charmbracelet/crush/5.8-styling-system)
- [OpenCode TUI docs](https://opencode.ai/docs/tui/),
  [multi-session tabs](https://github.com/anomalyco/opencode/issues/16047),
  [subagent panel](https://github.com/anomalyco/opencode/issues/15223)
- [Codex CLI](https://developers.openai.com/codex/cli/features),
  [DeepWiki](https://deepwiki.com/openai/codex/4-developer-guide)
- [Claude Code statusline](https://code.claude.com/docs/en/statusline)
- [Aider commands](https://aider.chat/docs/usage/commands.html)
- [pi-tool-display](https://github.com/MasuRii/pi-tool-display)

Log / transcript viewers:
- [lnav features](https://lnav.org/features),
  [performance](https://docs.lnav.org/en/latest/performance.html),
  [UI](https://docs.lnav.org/en/latest/ui.html)
- [jless](https://jless.io/), [fx](https://fx.wtf/)
- [NeoMutt](https://neomutt.org/guide/),
  [Aerc](https://blog.sergeantbiggs.net/posts/aerc-a-well-crafted-tui-for-email/)
- [WeeChat guide](https://weechat.org/files/doc/devel/weechat_user.en.html)
- [tut](https://github.com/RasmusLindroth/tut)

Dashboard / monitor:
- [btop review](https://www.both.org/?p=9668)
- [k9s overview](https://www.x-cmd.com/install/k9s/)
- [lazygit keybindings](https://github.com/jesseduffield/lazygit/blob/master/docs/keybindings/Keybindings_en.md)
- [tig manual](https://jonas.github.io/tig/doc/manual.html)
- [zenith](https://terminaltrove.com/zenith/)

Editor patterns:
- [Helix Way](https://dev.to/rajasegar/the-helix-way-36mh)
- [which-key / wf.nvim](https://github.com/Cassin01/wf.nvim)
- [Telescope](https://github.com/nvim-telescope/telescope.nvim)
- [Magit Transient](https://github.com/magit/transient)

Rendering / aesthetics:
- [Lipgloss](https://github.com/charmbracelet/lipgloss),
  [Glamour](https://github.com/charmbracelet/glamour)
- [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/),
  [rasterm](https://github.com/BourgeoisBear/rasterm),
  [term-image](https://pypi.org/project/term-image/)
- [Textual smooth scroll](https://textual.textualize.io/blog/2025/02/16/smoother-scrolling-in-the-terminal-mdash-a-feature-decades-in-the-making/),
  [Sparkline](https://textual.textualize.io/widgets/sparkline/),
  [Markdown](https://textual.textualize.io/widgets/markdown/)
- [textual-fastdatatable](https://github.com/tconbeer/textual-fastdatatable)
- [awesome-ratatui](https://github.com/ratatui/awesome-ratatui),
  [awesome-tuis](https://github.com/rothgar/awesome-tuis)
