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
