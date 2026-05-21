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

This plan delivers parity in **five phases**, ordered by impact-per-effort.
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
| A — Line-shaper (transcript) | transcript, find overlay, message cursor | 4–6 h |
| B — Markdown upgrade (syntect + tables + blockquotes) | transcript | 3–4 h |
| C — Right-column digest panel + tool fold + CommandPill | digest, main layout, transcript | 4–6 h |
| D — Animation primitives (scroll ease, modal fade) | app + every modal | 3–5 h |
| E — Mouse + polish round 3 | sidebar, footer, modals | 2–3 h |
| **Total** | | **~16–24 h** |

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

### 2.13 Main screen layout

Python: `app.tcss:1-20` Screen grid `grid-size: 3 2; grid-columns: 36 1fr
36`. Rust: `screens/main.rs` vertical `{1-row digest, content, 1-row
footer}`, content is `sidebar(36) + transcript`. **Structural
divergence** — **C**.

---

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

1. **Mouse support — in or out?** ~200 LOC + crossterm flag. Proposed
   default: **in**, gated by `--no-mouse`.
2. **Animation policy.** Some users find scroll easing distracting on
   slow SSH. Proposed default: **on**, with `THREADHOP_NO_ANIM=1`
   override.
3. **Syntect theme.** Bundle `base16-ocean.dark` (aligns with OpenCode)
   or load the user's active OpenCode theme JSON and synthesize on the
   fly? Proposed: **base16-ocean.dark** now, dynamic synth as a
   follow-up.
4. **Tool fold default state.** Python always expands. Phase C
   proposes collapsed-by-default with `o` to expand — cleans long
   sessions but hides the trail. Proposed: **expanded by default**
   (match Python), `o` toggles to collapsed.
5. **Right column on narrow terminals.** Below ~120 cols the `36 1fr
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
