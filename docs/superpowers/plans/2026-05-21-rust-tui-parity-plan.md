# ThreadHop Rust TUI Visual-Parity Plan vs Python Textual

**Goal:** Bring the Rust/ratatui TUI to 1:1 visual parity with the Python
Textual reference, prioritising the perceptual gaps that account for the
"underwhelming" feel of the current MVP. This is a *plan document* only —
no Rust code is touched in this commit.

**Reference points:** Python TUI under `threadhop_core/tui/` (CSS in
`threadhop_core/tui/css/`, widgets in `threadhop_core/tui/widgets/`); Rust
port under `rust/threadhop-tui/src/`.

---

## 1. Executive Summary

The Rust port has every functional surface — sidebar, transcript, find
bar, search modal, bookmark browser, kanban, conflict viewer, label
prompt, confirm modal, help overlay, contextual footer. By feature count
the port is **~85% complete**. By *perceived quality*, the remaining
**15% is the most visible portion**: soft-wrap continuation, code-block
syntax highlighting, tool-message folding, motion (scroll easing + modal
fade), and the right-column session digest. Their absence is what is
producing the "underwhelming" reaction despite multiple polish passes.

This plan delivers parity in **Phase 0 + five lettered phases (A–E)**, with
an additional **Phase A.5** inserted after the first user-driven
re-diagnosis pass. Phase 0 lands keybinding parity (the Rust port
invented its own bindings during phase planning; this restores Python as
source of truth). Phase A.5 is the digest-bar root-cause fix — the bar is
still invisible in the live binary after the cosmetic pass in `3836d09`,
and the diagnosis below points to a population gap rather than a render
bug. Phases A–E then follow the original impact-ordered sequence.
The single largest lever is **Phase A**: replacing `Paragraph::wrap` in
the transcript with a custom line-shaper that re-emits the role gutter
`▌` on every wrapped line. Today the gutter only marks the first row of
each body line — once a long paragraph soft-wraps to four rows, the
gutter disappears on rows 2–4 and the role accent that defines
ThreadHop's visual language is gone. Fixing this alone closes the gap
the user most often notices.

The biggest *structural* finding: the Rust **`DigestBarWidget` is a
single-line horizontal aggregator at the top of the screen**, while the
Python `SessionDigestBar` is a **multi-section right-column panel**
(`grid-columns: 36 1fr 36`, see `tui/css/app.tcss:4`). These are
different products. Phase C reconciles them — the right-column panel is
the canonical surface and is what the user sees in the Python build.

Rough effort (sub-agent-hours, parallel-where-possible):

| Phase | Surfaces | Estimate |
|---|---|---|
| 0 — Keybinding parity (Python as source of truth) | keys.rs, footer hints, help overlay | 0.5–1 h |
| A — Line-shaper (transcript) | transcript, find overlay, message cursor | 4–6 h |
| A.5 — Digest bar root-cause fix | screens/main.rs, event.rs, app.rs init | 1–2 h |
| B — Markdown upgrade (syntect + tables + blockquotes) | transcript | 3–4 h |
| C — Right-column digest panel + tool fold + CommandPill | digest, main layout, transcript | 4–6 h |
| D — Animation primitives (scroll ease, modal fade) | app + every modal | 3–5 h |
| E — Mouse + polish round 3 | sidebar, footer, modals | 2–3 h |
| **Total** | | **~18–27 h** |

**Not in scope:** reactive/declarative layout, pixel-exact hex matching,
reply-input parity (the Rust port is intentionally read-only — see
`RUST-PORT-STATUS.md`), mobile / web export.

---

## 2. Per-Surface Gap Audit

### 2.1 Transcript pane

Python: `tui/widgets/transcript.py` + `app.tcss:147-186`. Rust:
`widgets/transcript.rs`.

| Feature | Python | Rust today | Gap | Fix |
|---|---|---|---|---|
| Role accent | `border-left: thick $accent / $success / round $panel-lighten-2` | per-line `▌` gutter span | gutter lost on soft-wrap continuations | **A** |
| Row background | `$primary-background 15%` (user), surface (asst), tinted (tool) | RGB tint per body line | wrapped rows fall back to terminal bg | **A** |
| Markdown body | `rich.markdown.Markdown(...)` (fences, headers, tables, blockquotes, links) | hand-rolled inline parser only | no syntax highlight, no tables, no blockquotes | **B** |
| Fenced code | Pygments via Rich | flat `code_bg` rect | no language coloring | **B (syntect)** |
| Tool message | `ToolMessage` widget, `border: round $panel-lighten-2`, batched `⚙ … / ↳ …` | inline muted gutter, no border, no fold | tool spam dominates long sessions; no rounded box; no fold | **C** |
| CommandPill | one-line `▶ /foo` / `✦ skill loaded: …`, no border, dim | folded into transcript as plain user msg | slash commands visually equal real turns | **C** |
| Selection mode | `m` enters; `tint: $warning 8%`, `border-left: thick $warning`; `v` range | `J/K` moves cursor; only header bolded/reversed | no bg tint, no `v` range, no `y` copy | **A (cursor) + E (range/copy)** |
| Find-in-page | `_rebuild_widget_with_highlights` swaps md for plain Text | inline span run with theme.warning bg | parity (Rust arguably better — keeps md context) | nil |
| Smooth scroll | Textual eases `scroll_to(y=…)` ~100ms | `app.scroll: u16` jumps | jarring | **D** |
| Observation header | `ObservationInfoHeader` — dim row `─── 🗒 N obs · ~/path ───` | not rendered | header missing | **C** |
| Scrollbar | thin, `$border-blurred` → `$accent` on drag | none (Paragraph doesn't draw one) | affordance missing | **D (optional)** |

### 2.2 Session list / sidebar

Python: `widgets/session_list.py` + `app.tcss:31-243`. Rust:
`widgets/session_list.rs`.

| Feature | Python | Rust today | Gap | Fix |
|---|---|---|---|---|
| Panel border | `solid $panel` → `solid $accent` on focus-within | static `Block::borders` w/ theme color | no focus elevation | **E** |
| Status group dividers | `SessionStatusHeader` rows (`── Backlog`, `── In Progress`) on `$panel-darken-1`, bold | flat list | grouping missing | **E** |
| Spinner | 4-frame circle | 8-frame Braille | Rust is ahead | nil |
| Row classes | `.unread { color: $warning; bold }`, `.active { $accent }`, `.working { $success }`, `.archived { $text-muted; italic }` | only `is_working` drives status icon color | unread/archived classes missing | **E** |
| Scrollbar | thin, `$border-blurred` | none | missing | **D (optional)** |

### 2.3 Right-column session digest (the structural divergence)

Python: `widgets/session_digest_bar.py` + `css/session_digest.tcss`.
Rust: `widgets/digest_bar.rs`.

| Feature | Python | Rust today | Gap | Fix |
|---|---|---|---|---|
| Layout | right column, 36-cells wide, full panel height (3rd grid column) | single line at top of screen | **structural** | **C** |
| Identity block | title bold, slug muted, chips `⎇ branch · duration` | session name + status glyph | most fields missing | **C** |
| Recap | section header + N `recap-band` rows with left accent bar | not rendered | missing | **C** |
| Outputs | `↗ PR #N`, `✎ N files` | not rendered | missing | **C** |
| Context | `120k / 200k · 60% used`, input/output totals, `cache 95% hit`, model chips | not rendered | missing | **C** |
| Footer | divider + perm-mode/version chips + `claude -r <short>…` resume | not rendered | missing | **C** |
| Aggregate (current Rust line) | n/a — Python has no analog | counts row at top of screen | useful but in the wrong place — eats the row that should be empty, takes the 36-col real estate that belongs to the digest panel | **C (relocate to status line / footer)** |

The Rust horizontal aggregator is *valuable* — it's the live observation
heartbeat. Phase C **keeps it**, but relocates to a status band tied to
the ContextualFooter, freeing the right column for the real digest.

### 2.4 Contextual footer

Python: `widgets/contextual_footer.py` + `css/contextual_footer.tcss`.
Rust: `widgets/contextual_footer.rs`. Mostly parity; only the `$boost`
elevation tint is missing — **E**.

### 2.5 Find bar

Python: `widgets/find_bar.py` + `app.tcss:65-102`. Rust:
`widgets/find_bar.rs`. Parity on layout / match counter / persistence.
Missing: `$boost`→`$surface` focus tint, the `[×]` close-glyph hover
state (no mouse) — **E**.

### 2.6 Search modal

Python: `screens/search.py` + `css/search.tcss`. Rust: `screens/search.rs`.

| Feature | Python | Rust today | Gap | Fix |
|---|---|---|---|---|
| Backdrop | `$background 70%` semi-transparent dim | `Clear` opaque wipe | no dim, modal feels detached | **D (alpha)** |
| Container | 90%×85%, `$panel` bg, no border, padding `1 2`; inner widgets `transparent` | centered with rounded border | border-vs-borderless mismatch | **D/E** |
| Filter pills | parsed `project:` / `user:` / `since:` modifiers rendered as inline pills | pills rendered | parity | nil |
| Result rows | snippet + `session · project · ts` meta | similar | parity | nil |

### 2.7 Bookmark browser

Python: `screens/bookmark.py` + `css/bookmark.tcss`. Rust:
`screens/bookmark_browser.rs`. Mirrors search modal — same backdrop /
border divergence — **D/E**. Row shape is parity.

### 2.8 Kanban

Python: `screens/kanban.py` + `css/kanban.tcss`. Rust: `screens/kanban.rs`.

| Feature | Python | Rust today | Gap | Fix |
|---|---|---|---|---|
| Card border | `tall $primary-darken-2`, hover `tall $accent`, selected `heavy $warning + $warning 25% bg + $warning 8% tint` | bordered, selected reverse-styled | tint missing; selected reads as inverted not "highlighted" | **E** |
| Title vs meta | two-widget vertical (title flex, meta pinned to last row) | single block; long titles push meta off | overflow risk | **C/E** |
| Hover | `tall $accent` | no mouse | missing | **E (mouse)** |

### 2.9–2.11 Confirm / Label prompt / Help overlay

Python: `screens/{confirm,label_prompt,help}.py`. Rust mirror exists for
each. Parity on layout. **Help** is missing scope grouping (the
`.help-scope` rule in `css/help.tcss`) — flat list today, should group
by `Scope` — **E**.

### 2.12 Conflict viewer

No Python TUI surface (CLI-only). Rust is ahead. No work.

### 2.12.1 Keybinding parity audit (Python = source of truth)

Python's command surface is centralised in
`threadhop_core/tui/keybindings.py::COMMAND_REGISTRY` plus per-screen
`BINDINGS = [...]` blocks (kanban/bookmark/search/label_prompt/confirm/
help). The Rust port wrote its own table from scratch in
`rust/threadhop-tui/src/keys.rs` and the two have drifted — most visibly
the kanban opener (`t` in Rust, `B` in Python's App-level binding) and
the conflict viewer opener (`c` in Rust — Python has no TUI surface for
this at all). Side-by-side mapping below; **Match** column: ✓ identical,
≈ near-match (extra alt key on one side only), ✗ drift, ⊘ Rust-only
(Python has no analog), ◌ Python-only (Rust unimplemented).

#### Global / App-level

| Action | Python key | Rust key today | Match | Fix |
|---|---|---|---|---|
| Help overlay | `?` | `?` | ✓ | — |
| Quit | `q` | `q` (+ `Ctrl-c`) | ≈ | keep `Ctrl-c` as Rust-only convenience; matches `KeyboardInterrupt` muscle memory |
| Refresh sessions | `r` | — | ◌ | bind `r` → `RefreshSessions` (no-op stub OK until worker plumbed) |
| Next theme | `t` | — (Rust uses `t` for Kanban) | ✗ | **conflict** — see Kanban row |
| Prev theme | `T` | — | ◌ | bind `Shift+T` → `ThemeBack` |
| Open search | `/` | `/` | ✓ | — |
| Find in transcript | `Ctrl-f` | `f` | ✗ | rebind to `Ctrl-f`; keep `f` as alt (terminal multiplexers may eat `Ctrl-f`, but Python parity wins) |
| Browse bookmarks | `b` | `Shift+B` (Rust `b` = toggle bookmark on cursor) | ✗ | **conflict** — Python's `b` is "browse bookmarks" via App; Rust hijacked `b` for `ToggleBookmark` on the message cursor. Per "Python = source of truth": rebind Rust `b` → browse, move toggle to `Space` (matches Python's `space` in `SCOPE_SELECTION`) |
| Shrink sidebar | `[` | — | ◌ | bind `[` |
| Grow sidebar | `]` | — | ◌ | bind `]` |
| Kanban (App-level) | `Shift+B` | `t` | ✗ | rebind Rust kanban → `Shift+B`; frees `t` for theme cycle |

#### Session list (sidebar focus)

| Action | Python key | Rust key today | Match | Fix |
|---|---|---|---|---|
| Next session | `j` | `j` (+ `Down`) | ✓ | — |
| Previous session | `k` | `k` (+ `Up`) | ✓ | — |
| Focus transcript | `l` / `Right` | — (Rust has no scope toggle) | ◌ | introduce a `transcript`-focus scope or treat as no-op for now |
| Reply to session | `Enter` | `Enter` (Confirm — opens) | ≈ | Rust port is read-only by design (per Non-Goals); Enter opens transcript; label OK |
| Rename session | `n` | — | ◌ | bind `n` (LabelPrompt rename mode) |
| Copy resume command | `g` | `g` (Rust: ScrollTop) | ✗ | **conflict** — Python uses `g` for copy resume; Rust uses `g` for "scroll to top". Vim users will expect `g`/`G`. Open question below — for now, propose Python parity (`g` = copy resume) and move "scroll top" to `gg` (double-tap, Vim-style) |
| Observe / copy obs path | `o` | — | ◌ | bind `o` |
| Resume observation | `O` | — | ◌ | bind `Shift+O` |
| Cycle status forward | `s` | `s` (Rust: OpenLabelPrompt) | ✗ | rebind `s` → cycle, move label-prompt opener to `Shift+S` (matches Python's `S = cycle backward`; both go to LabelPrompt in Rust as a single mode-toggle modal — acceptable simplification) |
| Cycle status backward | `S` | `Shift+S` (OpenLabelPrompt) | ✗ | see above; bind `Shift+S` → cycle-back |
| Archive session | `a` | — | ◌ | bind `a` |
| Toggle archived view | `A` | — | ◌ | bind `Shift+A` |
| Move session down | `J` / `Shift+Down` | `Shift+J` (MoveCursorDown — in-transcript msg) | ✗ | **conflict** — Python: `J` reorders sessions; Rust: `J` moves message cursor. Resolve by moving Rust msg-cursor to `Ctrl+J` / `Ctrl+K` |
| Move session up | `K` / `Shift+Up` | `Shift+K` (MoveCursorUp) | ✗ | as above |

#### Transcript (focus)

| Action | Python key | Rust key today | Match | Fix |
|---|---|---|---|---|
| Focus session list | `h` / `Left` | — | ◌ | tied to scope toggle (above) |
| Scroll page up | `PageUp` | `PageUp` / `Ctrl-u` | ≈ | Rust has extra Vim `Ctrl-u`; keep |
| Scroll page down | `PageDown` | `PageDown` / `Ctrl-d` | ≈ | keep |
| Scroll to top | `Home` | `g` | ✗ | bind `Home` (additional to whatever `g` resolves to) |
| Scroll to bottom | `End` | `Shift+G` | ✗ | bind `End` |
| Enter selection mode | `m` | — (Rust has no selection mode yet) | ◌ | deferred until Phase A; Phase 0 just reserves the binding |

#### Selection mode (transcript widget-local, Python)

Rust has no analog yet. Phase A introduces it; Phase 0 registers the
scope and labels so the footer/help reflect the design:

| Action | Python key | Fix in Phase 0 |
|---|---|---|
| Next/prev message | `j`/`k` (also `down`/`up`) | reserve in `Scope::Selection` |
| Toggle range select | `v` | reserve |
| Copy selection | `y` | reserve |
| Export to /tmp | `e` | reserve |
| Toggle bookmark on selected message | `space` | reserve — and rebind Rust's main-screen `b` away from this |
| Edit bookmark note | `L` | reserve |
| Exit selection | `m` / `Esc` | reserve |

#### Find bar (open)

| Action | Python key | Rust key today | Match | Fix |
|---|---|---|---|---|
| Next match | `Enter` / `Down` | `Enter` / `n` | ≈ | Rust uses Vim `n`/`Shift+N`; Python uses `Enter` / arrow. Keep both — `n` is widely expected. Add `Down`/`Up` aliases |
| Prev match | `Up` (or app-wide `Shift+N`) | `Shift+N` | ≈ | add `Up` alias |
| Close find | `Esc` | `Esc` | ✓ | — |

#### Search modal

| Action | Python key | Rust key today | Match | Fix |
|---|---|---|---|---|
| Navigate results | `Up`/`Down` / `Ctrl-n` / `Ctrl-p` | `Up`/`Down` | ≈ | add `Ctrl-n`/`Ctrl-p` aliases |
| Jump through results | `PageUp` / `PageDown` | — | ◌ | bind |
| Open result | `Enter` | `Enter` | ✓ | — |
| Clear query / history | `Ctrl-x` | — | ◌ | bind |
| Close | `Esc` | `Esc` | ✓ | — |

#### Bookmark browser modal

| Action | Python key | Rust key today | Match | Fix |
|---|---|---|---|---|
| Navigate | `Up`/`Down` / `Ctrl-n` / `Ctrl-p` | `j`/`k` | ✗ | add `Up`/`Down` aliases (keep `j`/`k`) |
| Jump to message | `Enter` | `Enter` | ✓ | — |
| Edit note | `L` | — | ◌ | bind `Shift+L` |
| Delete bookmark | `d` | `d` | ✓ | — |
| Close | `Esc` | `Esc` | ✓ | — |

#### Kanban modal (Python only; Rust ahead with conflict viewer)

| Action | Python key | Rust key today | Match | Fix |
|---|---|---|---|---|
| Prev column | `Left` | `h` | ≈ | add `Left` alias |
| Next column | `Right` | `l` | ≈ | add `Right` alias |
| Prev card (row) | `Up` | `k` | ≈ | add `Up` alias |
| Next card (row) | `Down` | `j` | ≈ | add `Down` alias |
| Move card left | `Shift+Left` | — | ◌ | bind |
| Move card right | `Shift+Right` | `m` (Rust KanbanMoveItem) | ✗ | rebind to `Shift+Right`; keep `m` as undocumented alt |
| Open card | `Enter` | `Enter` | ✓ | — |
| Close | `Esc` | `Esc` | ✓ | — |

#### Confirm / LabelPrompt / Help — small surfaces

Confirm parity is clean: `y`/`n`/`Enter`/`Esc` match exactly.
LabelPrompt: Python uses just `Enter`/`Esc`; Rust adds `j`/`k`/`Tab` for
the in-modal status list — those are Rust-only extensions consistent
with the merged modal design. Acceptable.
Help: Python uses `Esc`/`?`/`q`; Rust uses `Esc`/`?`. Add `q` to Rust to
match (closes overlay, doesn't quit App while overlay is up).

#### Rust-only bindings (no Python analog)

- `Ctrl-c` → Quit (kept, see above)
- `c` on MainScreen → OpenConflictViewer (Rust feature ahead of Python; keep)

#### Totals

- **~46 distinct Python actions across all scopes**; ~28 Rust bindings registered today.
- **Direct matches (✓)**: 11
- **Near-matches (≈, harmless extra/alt key)**: 13
- **Drifts (✗) requiring rebind**: 11
- **Python-only (◌, Rust unimplemented)**: 14
- **Rust-only (⊘)**: 2 (kept)

Phase 0 (§4 below) rebinds the 11 ✗ rows and adds the 14 ◌ rows where
feasible. The remaining ◌ rows (selection mode, scope-aware
`h`/`l` focus toggle) are deferred to Phase A where they have a real
handler to bind to.

### 2.13 Main screen layout

Python: `app.tcss:1-20` Screen grid `grid-size: 3 2; grid-columns: 36 1fr
36`. Rust: `screens/main.rs` vertical `{1-row digest, content, 1-row
footer}`, content is `sidebar(36) + transcript`. **Structural
divergence** — **C**.

---

## 2.14 Digest bar re-diagnosis (the bar is still invisible)

Commit `3836d09` added (a) a pre-tint loop in `DigestBarWidget::render`
that paints every cell of `area` with `theme.background_panel` before
the `Paragraph` writes its glyphs, and (b) an empty-state branch in
`build_line_with_now` that emits a session-name + status-glyph + age +
`"no observations yet"` hint when `summary` is `None`. The user reports
the bar still isn't visible in the live binary. The cosmetic patch was
correct on paper but didn't fix the live failure — so the cause sits
upstream of the render path. Four hypotheses, ranked by evidence:

### H1 — Population gap on initial render (**most likely**)

`app.digest_summary_cache` is only written by
`event.rs::handle_event::WorkerEvent::TranscriptRefreshed` (line 191) and
by an explicit insert in `app.rs` test seeding (line 2342). The
**first** `TranscriptRefreshed` doesn't fire until the fs-watcher worker
finishes the initial parse for the auto-selected session — which can
take a noticeable fraction of a second on a large project. Before that
event lands, the App enters its first render pass with:

- `app.selected_session_id = Some(sid)` (auto-selected at `app.rs:299`)
- `app.digest_summary_cache.get(sid) = None`
- `selected_item` resolves correctly, so `session_display_name` is `Some(name)`
- `context` is populated

So the widget hits the empty-state branch — which **does** render the
session name and `"no observations yet"`. That should be visible. Why
isn't it?

**Secondary cause inside H1:** the empty-state spans are correct but the
**panel background tint is so close to the canvas bg** that the row
elevation is invisible (see H4). Combined with `last_active_at` and the
status glyph both rendering as muted DarkGray on near-black, the row
reads as "blank" to the eye even though every cell carries content.

**Test path:** `screens::main::tests::digest_bar_first_row_has_visible_content_even_without_observations`
(already added in `3836d09`) passes — it only checks for the
session-name substring, which IS in the buffer. That's why the unit
test green-lit a still-broken UI.

**Proposed fix:** make the empty-state row carry a *visually distinct*
glyph that doesn't depend on background contrast — e.g., reverse-video
the session name when `summary` is `None`, or paint the row in
`theme.accent` foreground (not `text_muted`) so it's unmistakably
present. Land a frame-buffer test that asserts at least one cell on
row 0 has a non-default `fg` color, not just non-whitespace content.

### H2 — Pre-tint vs Paragraph bg-style interaction

The render impl pre-tints every cell with `panel_bg`, then renders the
`Paragraph` with `Style::default().bg(panel_bg)`. For cells where the
Paragraph writes only spaces (the leading ` ` in `format!(" {glyph} ")`),
the cell gets a glyph of `" "` with bg = panel_bg. That's correct. But
ratatui's `Buffer::set_string` (which `Paragraph` calls internally) does
not write to cells beyond the rendered text length — those cells keep
the pre-tinted bg but **also keep the leftover symbol from a previous
frame** if the buffer wasn't cleared. Confirmed harmless on first
render (buffer initialised to spaces) but could matter when the bar's
text shrinks between frames.

**Test path:** add a regression that draws frame A with a long summary,
then frame B with `summary = None`, and asserts row 0 contains no
trailing glyphs from frame A.

**Proposed fix:** in `Widget::render`, after the pre-tint loop and
before the Paragraph, also write a `" "` symbol to every cell so stale
glyphs are wiped. Cheap, deterministic.

### H3 — Modal painting over row 0

The modals (bookmark_browser, label_prompt, kanban, conflict_viewer,
search, confirm, help) all use `screens::*::centered_rect(W%, H%, frame.area())`
with the full frame area as the parent. `centered_rect(95, 90, ...)` for
kanban produces a rect starting at roughly `y = frame.height * 0.05` —
which for an 80×24 terminal is `y = 1`, **not overlapping the digest at
y = 0**. But for a `92, 88` modal at 80×24 the rect could start at
`y = 1` and overlap a 2-row digest. Today the digest is 1 row so we're
fine, but the modals use `Clear` (an opaque wipe) inside their `draw`
which means **the row underneath the modal's rect goes blank**. If the
modal rect ever extends to `y = 0`, the digest gets wiped — and there's
no signal forcing a redraw of the digest after the modal closes (the
main loop redraws every frame at 60 fps, so this is actually fine on
close, but flicker is possible).

**Test path:** open each modal, snapshot frame, assert row 0 still has
the session name.

**Proposed fix:** if any modal's `centered_rect` can land on `y = 0`,
clamp its `y` to `>= 1` so the digest row is always preserved.
Currently a no-op (all modals stay below row 0) but worth a static
assertion.

### H4 — `background_panel` ≈ `background` (low elevation)

From `threadhop-core/src/theme.rs`: `default_dark` has
`background = "#0a0a0a"`, `background_panel = "#141414"`. The hex
distance is **10/255 per channel** — about 4% luminance lift. On many
terminal emulators (especially those with a "true black" background set
to `#000000` via theme override, ignoring the app's color) that
luminance bump is below the JND (just-noticeable-difference) and the
panel reads as "same color as canvas". Combined with the muted-DarkGray
glyph color in the empty-state path, the row looks blank.

**Test path:** none from code alone — requires a side-by-side photo
or `vhs` capture against the user's actual terminal theme.

**Proposed fix:** lift `background_panel` to at least `#1c1c1c` (≈11%
luminance bump) in `default_dark`, and similarly bump `default_light`'s
panel down by ~5%. This is a theme tweak, not a widget fix, but it's
what actually drives perceived elevation across every panel that uses
`background_panel`.

### Diagnosis ranking

**H1 + H4 are the most likely combined cause.** The cosmetic fix in
`3836d09` proved the render path renders *something*; the unit test
proves the content is *in the buffer*; the user reports they can't
*see* it. That trio narrows to a perceptual-contrast failure (H4),
amplified by an empty-state that uses muted colors precisely when the
user has the least context to confirm the bar exists (H1).

Phase A.5 (§4 below) addresses both. H2 and H3 stay on the suspect list
for regression coverage; neither is the primary bug.

## 3. Cross-Cutting Concerns

**3.1 Alpha blending (`$color 15%`).** Textual computes
`bg.mix(fg, 0.15)`; Rust uses raw RGB. Add
`blend(fg, bg, alpha) -> Color` in `threadhop-core/src/theme.rs`. Land
in **A** — the line-shaper needs it for selection tints and modal
backdrops.

**3.2 Focus management.** ratatui has no auto-focus. Extend
`app.scope: Scope` to drive panel borders (sidebar vs transcript). One
`border_style()` helper per panel. **E**.

**3.3 Animations.** Add `threadhop-tui/src/anim.rs` with
`Tween { from, to, started_at, duration, easing }`, sampled per frame
(the App already redraws at ~60 fps). Drives scroll easing
(`scroll_target` + tween), modal fade-in (backdrop alpha ramp). **D**.

**3.4 Markdown via `syntect`.** Standard Rust syntax-highlighter, gives
language-aware fenced-code rendering equivalent to Rich + Pygments.
Bundle a small subset (rust/python/ts/js/sh/json/md/toml/yaml). Wire
into `transcript.rs::md::render` at the `in_fence` branch. **B**.

**3.5 Soft-wrap gutter (the big one).** Today
`Paragraph::new(lines).wrap(Wrap { trim: false })` re-wraps `Line`s at
runtime, **losing the `▌` gutter** on every continuation row. Fix: stop
using `Paragraph::wrap`. Replace with a shaper that takes role +
row_bg + text, breaks into width-aware visual rows (`unicode_width` is
already a dep), and emits `[gutter_span, " ", text_span]` per visual
row with row bg applied. **A**.

**3.6 Mouse support.** crossterm already delivers `MouseEvent`s; we
ignore them. Adding routing enables click-to-select sidebar, click `×`
to close find bar, click kanban card, scroll-wheel transcript. **E**.

---

## 4. Phased Plan

Each phase ends with a working binary. `cargo test --workspace` must pass
at every phase boundary. Parallel sub-agents flagged where independent.

### Phase 0 — Keybinding parity (Python = source of truth)

**Goal:** Rust bindings match Python bindings 1:1 where Python has them;
Rust-only bindings (`Ctrl-c`, conflict viewer `c`) survive as documented
extensions. Footer hints and help overlay re-derive from the updated
table.

**Surfaces:** `keys.rs` (rewrite), `widgets/contextual_footer.rs`
(reads `commands_for_scope` — should auto-update), `screens/help.rs`
(reads the same registry), inline doc-comments referencing old keys.

**Tasks:**
1. *(Solo)* Rewrite `MAIN_SCREEN_BINDINGS`, `BOOKMARK_BROWSER_BINDINGS`,
   `KANBAN_BINDINGS`, etc. from the §2.12.1 mapping table. Specifically:
   - `b` → `OpenBookmarkBrowser` (was `ToggleBookmark`)
   - `Space` → `ToggleBookmark` (new)
   - `Shift+B` → `OpenKanban` (was unbound; `t` is freed)
   - `t` → cycle theme forward (new); `Shift+T` → backward
   - `g` → `CopyResumeCommand` (new); `Home` → `ScrollTop`
   - `End` → `ScrollBottom`; remove `g`/`Shift+G` as scroll bindings (keep `gg` as Vim double-tap optional follow-up)
   - `s` → `CycleSessionStatus` (was `OpenLabelPrompt`); `Shift+S` → cycle-back
   - `Ctrl+J`/`Ctrl+K` → `MoveCursorDown`/`Up` (was `Shift+J`/`K`)
   - `Shift+J`/`Shift+K` → `MoveSessionDown`/`Up` (new)
   - add `r`/`n`/`o`/`Shift+O`/`a`/`Shift+A`/`[`/`]` as no-op-stub bindings so the footer/help reflect them; real handlers land in their owning phase.
2. *(Parallel with 1)* Add `Scope::Selection` to the enum; reserve all
   selection-mode bindings as footer-hint-only entries (real handlers
   in Phase A).
3. *(After 1)* Update inline doc comments in `keys.rs` that reference
   the old key letters (notably the `// Phase 4 Wave 2` block on
   `b`/`Shift+B`/`s`/`Shift+S`).
4. *(After 1)* Add a snapshot test that dumps every `(Scope, KeyEvent)
   → Command` pair, sorted, and compares against a checked-in
   `tests/fixtures/keybindings.snap`. Drift is caught on the next run.

**Verification:**
- `cargo test -p threadhop-tui keys` passes; new snapshot test green.
- Side-by-side smoke: open the Python TUI and the Rust binary against
  the same project; verify `b`, `Shift+B`, `Shift+B` (kanban), `s`,
  `Space`, `Shift+J/K`, `[`/`]` produce the same modal/action in both.
- Help overlay manually opened in Rust; the scope-grouped key columns
  match the Python overlay's labels.

**Effort:** 0.5–1 h. Single sub-agent dispatch.

### Phase A — Custom line-shaper

**Goal:** role gutter `▌` and row bg tint reach every visible row of
every message, including soft-wrapped lines. Selection mode lands.

**Surfaces:** `widgets/transcript.rs`, `widgets/find_bar.rs` (overlay
rebuilds on shaped lines), `app.rs` (selection state).

**Tasks:**
1. *(Solo)* Add `theme::blend(fg, bg, alpha)` with clamp tests.
2. *(Solo)* `transcript::shape_message(msg, theme, width) ->
   Vec<Line<'static>>` — pure, unit-testable; emits gutter on every
   visual row; replace `Paragraph::wrap` with `Paragraph::new(shaped)`
   (no wrap).
3. *(After 2; parallel with 4, 5)* Re-port find-bar overlay onto shaped
   lines.
4. *(After 2; parallel with 3, 5)* Selection mode (`m / v / j / k / y /
   e / space / L / Esc`), `tint(warning, 0.08)` via `blend`.
5. *(After 2; parallel with 3, 4)* Verify message cursor (`J/K`) still
   bolds the header row correctly post-shaping.

**Verification:** new `shape_emits_gutter_on_every_visual_row` test on
`TestBackend` at narrow width; insta snapshot of a 5-msg transcript at
width 60; manual smoke against a long assistant message.

**Effort:** 4–6 h. One sub-agent for #1–2; up to three parallel for
#3/#4/#5.

### Phase A.5 — Digest bar root-cause fix

**Goal:** digest bar is *visibly distinct* in the live binary across
every initial-load state: no session selected, session selected with no
cache yet, session selected with cache populated. Solves the failure
mode the cosmetic patch in `3836d09` didn't.

**Surfaces:** `widgets/digest_bar.rs`, `screens/main.rs` (test
additions), `threadhop-core/src/theme.rs` (panel-bg luminance bump).

**Tasks:**
1. *(Solo)* **Empty-state visibility (H1).** Make the empty-state branch
   render in `theme.accent` foreground (not muted DarkGray) so the row
   pops even when the panel bg is close to the canvas bg. Bold the
   session name; keep the "no observations yet" hint in muted but at
   the *end* of the line so it doesn't carry the contrast burden.
2. *(Parallel with 1)* **Panel bg elevation (H4).** Bump
   `default_dark.background_panel` from `#141414` to `#1c1c1c`
   (≈11% luminance lift; still subtle, but past the JND threshold on
   most terminals). Mirror in `default_light`. Capture a side-by-side
   screenshot at the phase boundary so the change is auditable.
3. *(Parallel with 1, 2)* **Stale-glyph wipe (H2).** In
   `Widget::render`, after the pre-tint loop, also write a `" "` symbol
   to every cell of `area` so a shrinking line never leaves stale
   glyphs from a previous frame. ≤5 lines, deterministic.
4. *(After 1)* **Frame-capture regression test.** New test in
   `screens::main::tests` that:
   - constructs an App with one session, **no** `digest_summary_cache`
     entry, and a populated `sidebar`;
   - draws a frame at 120×24;
   - asserts row 0 contains a cell whose `fg` is the theme `accent`
     (i.e., the session name span really did render in accent color);
   - asserts row 0 contains the substring `"no observations yet"` (the
     hint reaches the buffer);
   - asserts row 0 has at least 8 non-whitespace cells.
5. *(After 1, parallel with 4)* **Modal-overlap clamp (H3).** Static
   assertion in `screens/main.rs::tests`: every `centered_rect(_, _,
   frame.area())` returns a rect with `y >= 1` so the digest row is
   never wiped by a modal. Currently true by accident; the test pins
   it.

**Verification:**
- New test in #4 passes.
- Live binary smoke: launch Rust TUI on a project with no observations
  yet — confirm the bar is **visibly distinct** (the user is the final
  signal; if it still reads as blank after H1+H4 land, escalate to
  re-painting the row in `theme.surface` or adding a 1-cell top/bottom
  border).
- `cargo test -p threadhop-tui` passes.

**Effort:** 1–2 h. Single sub-agent dispatch.

### Phase B — Markdown upgrade

**Goal:** syntect fences, tables, blockquotes.

**Surface:** `widgets/transcript.rs::md`.

**Tasks:**
1. *(Solo)* Add `syntect` (default-fancy features off, embedded subset
   of syntaxes). Wire into `in_fence`. Bundle `base16-ocean.dark` as a
   starter theme that aligns with OpenCode.
2. *(Parallel with 1)* Simple table parser (`| col | col |` + `|---|`).
   Column-aligned spans clamped at terminal width.
3. *(Parallel with 1, 2)* Blockquote — `>` prefix → left `│` accent in
   `text_muted` + indent.

**Verification:** snapshot tests for a Rust fence, a 2×3 table, a
blockquote. Manual: open a code-heavy session transcript.

**Effort:** 3–4 h. Up to three parallel.

### Phase C — Right-column digest + tool fold + CommandPill

**Goal:** the right 36-col digest panel matches Python sections; tool
messages collapse into `▶ N tool calls` (toggle `o`); CommandPill
restored; observation header rendered.

**Surfaces:** `widgets/digest_bar.rs` (rewrite into
`session_digest.rs`), `screens/main.rs` (layout), `widgets/transcript.rs`
(fold + pill + obs header).

**Tasks:**
1. *(Solo)* Layout shift: `screens/main.rs` from vertical band to
   `grid-columns: 36 1fr 36`. Move current horizontal aggregator into a
   status line on the ContextualFooter so observation counts stay
   visible.
2. *(After 1; parallel with 3, 4, 5)* New `SessionDigestPanel` —
   `Vec<DigestBlock>` model: `Identity / Recap / Outputs / Context /
   Footer`. Pure-render; App passes `ObservationSummary` +
   `SessionDigest` (both already exist in `threadhop-core`).
3. *(Parallel with 2, 4, 5)* Tool fold — `app.expanded_tools:
   HashSet<usize>`; collapsed = `▶ N tool calls` summary; expanded =
   batched render; `o` toggles on cursor.
4. *(Parallel with 2, 3, 5)* CommandPill — detect `command` /
   `skill_load` roles; render as dim one-liner, no gutter, indent 2.
5. *(Parallel with 2, 3, 4)* Observation header — first row of the
   transcript pane when observations exist for the session.

**Verification:** main-screen snapshot at 160×40; tool-fold round-trip
snapshots; visual diff against a Python screenshot at the same dims.

**Effort:** 4–6 h. One sub-agent for #1; up to three parallel for
#2/#3/#4 (with #5 trailing).

### Phase D — Animation primitives

**Goal:** scroll easing, modal fade-in. Motion is the cheapest signal
of polish at this stage.

**Surfaces:** `app.rs` event loop, every modal draw, `transcript`
render.

**Tasks:**
1. *(Solo)* `threadhop-tui/src/anim.rs`: `Tween`, `Easing { Linear,
   EaseOutCubic, EaseInOutCubic }`, `Clock` newtype for test injection.
2. *(After 1)* `app.scroll: u16` → `scroll_current: f32 +
   scroll_target + Option<Tween>`. Sample per frame, render uses
   rounded current.
3. *(After 1; parallel with 2)* Modal fade — each modal stamps
   `opened_at: Instant`; `draw()` blends backdrop alpha 0→0.7 over 80ms
   via `blend(theme.background, theme.fg, alpha)`.
4. *(After 1; parallel with 2, 3, optional)* Thin scrollbar on
   transcript + sidebar (1-cell), idle `border_blurred`, active
   `accent`.

**Verification:** `Tween::value` boundary tests; manual: confirm scroll
feels smooth at 60fps.

**Effort:** 3–5 h.

### Phase E — Mouse + polish round 3

**Goal:** mouse-driven interactions plus the last visual nits — focus
borders, status group dividers, kanban tint, help scope groups, sidebar
row classes.

**Surfaces:** `app.rs`, `widgets/session_list.rs`, `widgets/find_bar.rs`,
`widgets/contextual_footer.rs`, `screens/help.rs`, `screens/kanban.rs`.

**Tasks:**
1. *(Solo)* Mouse dispatch — enable `EnableMouseCapture` in
   `terminal_guard.rs`; route `MouseEvent::Down(Left, col, row)`
   through a scope-aware hit-test. Each focusable widget exposes
   `hit_test(rect, col, row) -> Option<HitAction>`.
2. *(Parallel)* Sidebar status group dividers — port
   `SessionStatusHeader`, `panel_darken bg, bold text, height 2`.
3. *(Parallel)* Session-list row classes — `text_muted+italic` for
   `archived`, `warning+bold` for `unread`, `accent` for
   `active && !working`, `success` for `working`.
4. *(Parallel)* Panel focus border — `sidebar` and `transcript` borders
   switch from `theme.panel` to `theme.accent` based on `app.scope`.
5. *(Parallel)* Help screen scope grouping — render each `Scope` as
   a section header (dim italic, 1-row top margin), per
   `css/help.tcss::.help-scope`.
6. *(Parallel)* Kanban selected-card tint —
   `blend(theme.warning, panel, 0.25)` bg + `heavy` border +
   `blend(theme.warning, _, 0.08)` row tint.
7. *(After 1)* Find-bar `×` hover — bg `blend(error, panel, 0.15)` →
   `0.30` on hover.

**Verification:** snapshot of help with scope groups; snapshot of
kanban with one selected card; manual click-through.

**Effort:** 2–3 h. One sub-agent for #1; up to six parallel after.

---

## 5. Open Questions for the User

1. **Keybinding tie-breakers (Phase 0).** Where Python has multiple
   bindings for the same action OR uses bindings that conflict with
   conventional Rust/Vim TUI patterns, which takes precedence — Python
   parity or convention?
   - Concrete case: Python's `g` is "copy resume command"; the Rust
     port and most Vim-flavoured TUIs use `g`/`Shift+G` for
     "scroll top/bottom". Phase 0 proposes **Python parity** (`g` =
     copy resume, `Home`/`End` for scroll) but a sensible
     counter-proposal is to keep `g`/`G` for scroll (Vim) and move
     copy-resume to `y` (which is unused at the App level) — which is
     more important: muscle-memory port from the Python TUI users
     already have, or alignment with the Vim conventions that Rust TUI
     users expect?
   - Same tension on `b`: Python uses it for "browse bookmarks" from
     anywhere; Rust grabbed it for `ToggleBookmark` on the message
     cursor. Phase 0 hews to Python (b = browse, Space = toggle).
     Confirm?
2. **Digest bar emptiness threshold (Phase A.5).** When a session has
   zero observations and the observer hasn't run, what should the bar
   show?
   - (a) Nothing — gracefully fade to a thin 1-row separator that just
     elevates the panel bg.
   - (b) Session header alone — name + status glyph + age, no hint
     text.
   - (c) Hint — name + status + age + "no observations yet" muted
     suffix (current behaviour post-`3836d09`).
   - Phase A.5 assumes **(c) with accent-color name** so the bar
     always carries useful identification, but the user may prefer the
     quieter (b) once observation pipelines run reliably.
3. **Mouse support — in or out?** ~200 LOC + crossterm flag. Proposed
   default: **in**, gated by `--no-mouse`.
4. **Animation policy.** Some users find scroll easing distracting on
   slow SSH. Proposed default: **on**, with `THREADHOP_NO_ANIM=1`
   override.
5. **Syntect theme.** Bundle `base16-ocean.dark` (aligns with OpenCode)
   or load the user's active OpenCode theme JSON and synthesize on the
   fly? Proposed: **base16-ocean.dark** now, dynamic synth as a
   follow-up.
6. **Tool fold default state.** Python always expands. Phase C
   proposes collapsed-by-default with `o` to expand — cleans long
   sessions but hides the trail. Proposed: **expanded by default**
   (match Python), `o` toggles to collapsed.
7. **Right column on narrow terminals.** Below ~120 cols the `36 1fr
   36` grid is awkward. Auto-collapse below a threshold (proposed:
   110 cols)?

---

## 6. Non-Goals

- Reactive/declarative layout system. ratatui's immediate-mode model is
  fine; matching CSS *semantics* would 10× the surface area.
- Pixel-exact hex matching. The OpenCode theme defines the palette; we
  match systems (alpha mix, focus elevation, role accents), not
  individual `$primary-darken-2` values.
- Reply-input parity. Rust port is the browser; sending stays Python's
  job.
- Touchscreens, web export, mobile.
- A full CSS-equivalent flexbox engine. Phase A's shaper handles the
  transcript; other surfaces keep `Paragraph` because their content
  doesn't carry per-line styling that wraps.

---

## 7. Verification Strategy

1. **Side-by-side screenshots per surface.** Python via `vhs`; Rust via
   `TestBackend` frame dump. Compare in
   `docs/parity/2026-05-21-screenshots/` at each phase boundary.
2. **Frame-buffer regression tests.** Each phase adds ≥1 `insta`
   snapshot per touched surface; regenerated on phase-boundary commit.
3. **Smoke matrix.** `rust/scripts/parity-smoke.sh` opens a fixture DB
   under `expect`, drives the same 30 keypresses
   `RUST-PORT-STATUS.md` references, dumps the final frame, diffs
   against a checked-in golden.
4. **Ultimate signal: live driving.** After each phase, user opens the
   binary against their real DB, drives 5 minutes of real work, reports
   any "feels off" reactions. Iterate inside the phase; do not start
   the next until the user signs off.

## 8. Why this order

- **0 first** — keybindings are how every other phase is exercised; let the
  user smoke-test phases A–E against their muscle memory rather than the
  Rust port's invented bindings. Trivially small, blocks nothing else.
- **A.5 inserted between A and B** — the digest-bar fix sits *after* A
  because A introduces the row-bg blending primitive (`theme::blend`)
  the digest panel will eventually reuse, but A.5 is small enough to
  land as a follow-up without waiting for the full A phase to settle.
  If A is delayed, A.5 can ship standalone — its panel-bg tweak is
  decoupled.
- **A first** — the transcript is where the user spends 90% of attention,
  and the soft-wrap gutter is the single biggest perceptual gap. Fixing
  it sells the whole port.
- **B second** — given A's shaper, B is purely "fill better content into
  the shaper" with no rework risk.
- **C third** — the layout shift is the largest visual restructuring and
  benefits from a stable transcript surface.
- **D fourth** — animations layer on stable layouts. Doing D before C
  forces re-tuning timings.
- **E last** — mouse + nits are independent and benefit from stable
  layouts to hit-test against.

Parallelism cap: **3 sub-agents at once** per phase. Beyond that,
`app.rs` and `screens/main.rs` become merge serialization points.
Phases themselves are sequential — do not start Phase B until A merges.

*End of plan.*
