# Observer v2 — Design Notes

**Status:** Draft. Written before the v1 scrap so the rebuild has a starting
point. Branch `archive/observer-v1` snapshots the v1 code as it stood the day
this was written; `dev` will scrap observer + reflector after this doc lands.

**Goal of this document:** describe what the next observer should look like,
why the v1 design is being abandoned, and which patterns from Mastra's
observational-memory system are worth porting (and which aren't). It is not a
spec — open questions are flagged explicitly. Treat it as the brief for the
next attempt.

---

## 1. Why v1 is being scrapped

V1 is the pipeline rooted at `threadhop_core/observation/{observer,reflector}.py`
plus `prompts/{observer,reflector}.md`, the per-session JSONL file at
`~/.config/threadhop/observations/<session_id>.jsonl`, the `observation_state`
and `conflict_reviews` tables, and the CLI verbs `observe`, `observations`,
`decisions`, `todos`, `conflicts`. It works end to end on Sonnet, but four
problems made it not worth keeping:

1. **The model and the prompt fight each other.** The default model is Haiku
   (cheap, fast). The prompt is strict ("append only, no output if nothing
   qualifies") and asks for a 5-way type taxonomy with subtle boundaries
   (`decision` vs `adr`, `todo` vs `done`). On a transcript with explicit
   "## Decision" headers, Haiku extracts zero observations. Sonnet extracts
   eight. The taxonomy is too subtle for the cheap tier and the prompt is
   too long for the cheap tier; pick one to fix and the other still bites.

2. **The Write-tool dependency hides failures.** Observer invokes
   `claude -p --permission-mode acceptEdits` and tells the model to call the
   Write tool to append JSONL lines. When the model decides to answer inline
   instead of calling Write, the subprocess returns 0, observer advances the
   cursor, the user sees `"no new observations"`, and the actual model output
   is thrown away. There is no way to distinguish "found nothing" from
   "subprocess succeeded but the model just talked at us." Every recovery
   requires `--reset` and a fresh run.

3. **The taxonomy is over-engineered for the value it delivers.** Five output
   types (`decision` / `todo` / `done` / `adr` / `observation`) plus a sixth
   from the reflector (`conflict`). The CLI surfaces these as five separate
   commands (`todos`, `decisions`, `observations`, `conflicts`, plus the
   handoff brief that consumes all of them). In practice the user wants
   "show me what mattered in this session" — a single ranked stream. The
   typing buys queryability we never use, at the cost of a much harder
   prompt.

4. **The reflector is doing cross-session conflict detection on a base
   that doesn't reliably extract decisions in the first place.** It runs a
   second `claude -p` call per N new observations to compare this session's
   decisions against sibling sessions in the same project, and writes
   `type:conflict` entries. The cost-benefit is bad: another model call,
   another prompt to maintain, another append-only invariant to defend, on
   top of a decision-extraction layer that has its own failure modes. Every
   bug in v1 has been in the observer; the reflector compounds them.

The observer pipeline is a useful idea. The implementation in this branch
is not the right shape.

---

## 2. v2 principles

Working list. Stated as commitments so future-me has something to push back
against, not the loosest possible "we should consider X."

### 2.1 One observation type. No taxonomy.

Drop `decision` / `todo` / `done` / `adr` / `observation` / `conflict`. There
is one record type: an *observation*. Each observation is a short piece of
text the model thought was worth remembering, with a priority/importance
ranking and a timestamp.

If we later need queryability ("show me decisions"), the model can include
that as a tag inside the text or a free-form `tags` array — but the schema
doesn't enforce any of it, and the CLI surfaces a single `observations`
view, full stop.

This follows Mastra's lead (their observer ranks importance with
🔴/🟡/🟢/✅ instead of typing) and explicitly rejects the v1 typed-output
contract.

### 2.2 Drop the reflector entirely.

No cross-session conflict detection in v2. If we need it later, it should
be a separate, opt-in feature that runs on stored observations *after* the
observer has proven reliable for a while. Until then, `reflector.py`,
`prompts/reflector.md`, the `conflict_reviews` table, and the
`reflector_entry_offset` column all go.

### 2.3 SQLite, not JSONL files.

V1 used per-session JSONL files because the observer subprocess wrote
directly to disk (Write tool), and SQLite from a child Claude process is
awkward. V2 doesn't have the subprocess writing at all — we parse stdout —
so observations land directly in a single `observations` table.

This kills:
- The `~/.config/threadhop/observations/` directory entirely.
- `_count_obs_lines` / `_count_new_entries_by_type` and the line-count diff
  for "how many new entries did this run produce."
- `obs_path` columns and the path-shape parts of `observation_state`.
- The append-only invariant we had to defend with code (`ADR-020`); SQLite
  rows are by default not deleted, which gives us the same property without
  prose.

### 2.4 Parse stdout, not Write tool.

`run_claude_p()` already returns stdout. The v2 observer prompt asks the
model to respond with one JSON object per observation, one per line, and
nothing else. The Python side parses stdout, rejects malformed lines, and
inserts what survives.

This eliminates the entire class of "subprocess succeeded but wrote
nothing" bugs from v1. The minimum permission mode drops from `acceptEdits`
back to `default` — no file write happens in the child.

If the model rambles before the JSON, we strip everything before the first
`{` and after the last `}`; if it produces no parseable JSON at all, we log
the raw stdout to `observation_runs.raw_stdout` (see §4) so the user can
diagnose why nothing got extracted instead of staring at silence.

### 2.5 Sonnet by default. Haiku is opt-in for cheap experiments.

Empirically confirmed in the original chat: Haiku produces zero observations
on a transcript that Sonnet handles cleanly, with the same prompt. v1's
`model: str = "haiku"` default is the wrong tradeoff. v2 defaults to
`sonnet`; the `--model` flag stays for users who want to try cheaper tiers.

### 2.6 Smaller prompt.

V1's `prompts/observer.md` is ~110 lines of markdown including a 5-way
type table, type-boundary disambiguation, input-shape documentation, and
multi-turn handling notes. Mastra's observer prompt is ~6k chars and they
note it's expensive without prompt caching. We should aim *below* both
numbers.

Target: under 60 lines of prompt body. Trade detail for example density —
two or three concrete (input → output) pairs do more work than three
paragraphs of disambiguation.

### 2.7 Surface the model's output when nothing was extracted.

When `new_entries == 0`, the result struct must contain the raw stdout
(truncated at, say, 4 KB) so the caller can inspect what the model said.
V1 swallowed this. Future-us is going to want it the very first time
something doesn't work as expected.

### 2.8 Keep the cleaned-transcript view.

This part of v1 is correct and stays: the observer reads the source JSONL
through `indexer.parse_byte_range`, which strips `<system-reminder>`,
abbreviates tool calls, drops tool stdout, and merges streaming chunks.
The anti-pattern in CLAUDE.md ("don't feed the observer raw JSONL") is
correct and should be re-enforced in v2.

### 2.9 Keep `observation_state` (slimmer).

The byte-offset cursor is the right idea — without it we'd re-process the
whole transcript on every run. V2 keeps a per-session row with
`source_byte_offset` and `last_observed_at`, drops `obs_path`,
`reflector_entry_offset`, and `entry_count` (the latter becomes a `COUNT(*)`
on the `observations` table when needed).

`observer_pid` and `status` only matter if we keep the watch-mode sidecar.
See §6.

---

## 3. Out of scope for v2 (explicit non-goals)

- **Cross-session reflection / conflict detection.** Possibly a v3 feature.
- **Multi-tier importance encoding (🔴/🟡/🟢) at schema level.** A single
  integer `priority` column is fine; we do not adopt Mastra's emoji-tier
  vocabulary into the data model.
- **Token-threshold triggering.** Mastra runs the observer at ~30k
  message-tokens of new content; v2 starts with the simpler "manual run +
  optional sidecar that runs on every N new turns" and doesn't try to be
  clever about tokens until the simple version is shipping.
- **Compression ladder.** Mastra's reflector retries with progressively
  more aggressive compression hints when the consolidated output isn't
  smaller. Not relevant — we don't have a reflector and we're not
  consolidating.
- **The handoff skill consumes observations.** v2 should keep the
  `/threadhop:handoff` plugin command working. Whatever schema observations
  end up in, the handoff builder needs to read them. Don't break the skill.

---

## 4. Architecture sketch

Three pieces:

```
threadhop_core/
  observation/
    __init__.py
    observer.py         # observe_session() + watch_session() (if kept)
    parser.py           # parse_observation_lines(stdout) -> list[Observation]
  storage/
    db.py               # observations + observation_state schema
  cli/commands/
    observe.py          # `threadhop observe`
    observations.py     # `threadhop observations` (single view)
prompts/
  observer.md           # smaller, JSON-out, no taxonomy
```

Compared to v1: `reflector.py`, `queries.py`, `observer_state.py` (separate
file) collapse into `observer.py` + `parser.py`. `prompts/reflector.md`
gone. Five CLI commands collapse to two.

### Data flow (one observation pass)

1. Read `observation_state[session_id]` → byte cursor.
2. Read source JSONL from cursor to last newline.
3. `indexer.parse_byte_range(...)` → list of cleaned turns (same view as
   the TUI).
4. If `len(turns) < BATCH_THRESHOLD` → return `below_threshold`, don't
   advance cursor.
5. Format turns as a role-labelled transcript, splice into the prompt.
6. `run_claude_p(prompt, model="sonnet", permission_mode="default")` →
   `HarnessResult` with stdout.
7. `parser.parse_observation_lines(stdout)` → list of `Observation` (or
   empty list with the raw stdout retained).
8. Insert observations into the DB inside one transaction; advance the
   cursor in the same transaction.
9. Return a result struct that **always** includes `raw_stdout` so zero-
   result runs are debuggable.

### Schema sketch

```sql
CREATE TABLE observations (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT    NOT NULL,
    project         TEXT,                 -- denormalized from sessions
    text            TEXT    NOT NULL,
    priority        INTEGER NOT NULL DEFAULT 2,  -- 1=high, 2=med, 3=low
    tags            TEXT,                 -- optional JSON array, model-set
    source_ts       TEXT,                 -- ISO 8601, from the turn
    created_at      REAL    NOT NULL,     -- unix ts of insertion
    run_id          INTEGER,              -- FK to observation_runs(id)
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);

CREATE INDEX idx_observations_session ON observations(session_id, created_at);
CREATE INDEX idx_observations_project ON observations(project, created_at);

CREATE TABLE observation_state (
    session_id          TEXT PRIMARY KEY,
    source_path         TEXT NOT NULL,
    source_byte_offset  INTEGER NOT NULL DEFAULT 0,
    last_observed_at    REAL,
    -- sidecar fields, only if we keep watch mode (§6):
    observer_pid        INTEGER,
    status              TEXT NOT NULL DEFAULT 'idle'
                        CHECK (status IN ('idle','running','stopped'))
);

CREATE TABLE observation_runs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id      TEXT NOT NULL,
    started_at      REAL NOT NULL,
    finished_at     REAL,
    model           TEXT NOT NULL,
    new_entries     INTEGER NOT NULL DEFAULT 0,
    raw_stdout      TEXT,                 -- truncated to ~4 KB
    error           TEXT,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
```

Notes:
- `observation_runs` is the v2 answer to "what did the model actually say
  when nothing was extracted?" Every observe pass writes one row whether
  or not it produced observations. Cheap, append-only, and self-cleaning
  (we can vacuum runs older than N days).
- `priority` is an int rather than an enum/CHECK because we don't want to
  litigate the boundaries; the model writes 1/2/3 and we trust it.
- `tags` is unconstrained TEXT (JSON-encoded array). If we end up wanting
  queryability, a follow-up migration adds a `observation_tags` join
  table; we don't preempt it.
- All `Literal` types in `models.py` mirror SQL `CHECK`s per CLAUDE.md
  ("Don't add a DB enum-like column without matching `Literal`+CHECK").
  v2 has only one such column (`status` on `observation_state`), and it
  follows the rule.

### CLI surface

```
threadhop observe [--session ID] [--model M] [--once] [--reset]
threadhop observations [--session ID] [--project P] [--limit N]
```

That's it. `decisions`, `todos`, `conflicts` are gone. The handoff skill
reads `observations` directly with whatever filters it wants.

---

## 5. The observer prompt — sketch

Concrete enough to be a starting point. Not the final wording.

```markdown
# ThreadHop Observer

You read a chunk of a coding-session transcript and extract notable
observations. One observation per JSON line on stdout. Nothing else on
stdout — no preamble, no markdown, no explanation.

## Output

Each line is a single JSON object:

    {"text":"<one sentence>","priority":<1|2|3>,"tags":[...],"ts":"<ISO 8601>"}

- `text`: a self-contained sentence. Someone reading this in three weeks
  with no other context should understand what was decided/discovered.
- `priority`: 1 = decision or change of direction; 2 = useful context;
  3 = minor observation. When in doubt, 2.
- `tags`: short free-form strings. Optional. Use them when a tag would
  make the observation findable later (e.g. ["sqlite","schema"]).
- `ts`: timestamp from the transcript turn where the item was concluded.

If the chunk contains nothing worth extracting, output nothing. An empty
stdout is a valid result.

## Examples

Transcript:
    ### user · 2026-04-17T10:30:00Z
    Should we use REST or gRPC for the client API?

    ### assistant · 2026-04-17T10:30:30Z
    REST. Our client SDKs don't have working gRPC tooling on web.

Output:
    {"text":"Client API will use REST, not gRPC — gRPC tooling for web SDKs is unreliable.","priority":1,"tags":["api","rest"],"ts":"2026-04-17T10:30:30Z"}

Transcript:
    ### user · 2026-04-17T11:00:00Z
    Tests passing now?

    ### assistant · 2026-04-17T11:00:05Z
    Yes, all green.

Output:
    (empty — routine status check, nothing to remember)

## Input

The conversation is between <session_chunk> tags. Tool outputs are
already removed. Tool calls appear as one-line abbreviations like
`[Editing foo.py]` — these are evidence of what was done, not
observations themselves.
```

That's ~45 lines vs. v1's 110. The taxonomy disappeared; the example pair
does the work the table used to do.

---

## 6. Watch mode — keep, simplify, or drop?

V1 has a substantial watch-mode/sidecar layer:
`watch_session()`, `observe_sidecar()`, `_PollingWaitBackend`,
`_FSEventsWaitBackend`, PID tracking, SIGTERM handling, "flush on stop"
semantics. About half the LOC in `observer.py`.

Three options for v2, in order of preference:

1. **Drop it for v2.0; ship manual-only.** `threadhop observe --session
   ...` runs once. The TUI can run it on demand from the keybinding. No
   background process, no PID table, no FSEvents. This gets v2 shipped in
   a few hundred LOC.

2. **Keep a minimal polling watcher.** Single backend (poll), no PIDs in
   the DB, no flush-on-stop heuristic. The watcher exits when the session
   becomes inactive. ~100 LOC. Optional flag, not the default.

3. **Port the v1 sidecar verbatim.** Don't.

Recommend option 1 unless the handoff workflow really needs ambient
observation. The TUI already has a refresh loop that runs every 5s; a
"run observer when this session ticks past 3 new turns" hook on that loop
is much cheaper than a separate sidecar process and gives the user the
same UX.

---

## 7. What to steal from Mastra (and what not to)

Source files to read (links good as of 2026-04):

| What | Mastra path | Worth porting? |
|---|---|---|
| Observer prompt | `packages/memory/src/processors/observational-memory/observer-agent.ts` | **Read, don't copy.** Their prompt is 6k chars; ours should be smaller. Steal the JSON-on-stdout pattern. |
| Reflector prompt | `packages/memory/src/processors/observational-memory/reflector-agent.ts` | **Skip for v2.** No reflector. |
| Result types | `.../types.ts` | **Read.** Their "single observation" shape (no taxonomy) is what we're adopting. |
| Anti-degeneration | `detectDegenerateRepetition()`, `sanitizeObservationLines()` | **Port the simpler one.** Truncate any single observation to <2 KB at the parser layer. Skip the >40%-window-duplicate detector unless we hit the bug. |
| XML-output coercion + bullet-list fallback | (in their parser) | **Skip.** We're using JSON-per-line; the malformed-line discard behavior is enough. |
| Token-threshold triggering | `thresholds.ts` | **Skip for v2.** Add later if we have evidence the simple "every N turns" trigger is wrong. |
| Cache-TTL-aligned activation (`activateAfterIdle: '5m'`) | `observation-strategies/*` | **Note for later.** If we add a watcher in v2.1, align its idle threshold to the prompt-cache TTL (see Mastra's note: cheaper to wait past one cache miss and amortize than to pay a partial miss). |
| Compression ladder | reflector-only | **Skip.** No reflector. |
| Emoji-tier priorities | observer-agent.ts | **Inspired by, but use ints.** `priority: 1/2/3` in our schema; the model can think emoji and we map it. |
| User-assertion-precedence rule | reflector-agent.ts | **Skip with the reflector.** |

Two things Mastra does that we explicitly *don't*:

- **Append-only is dropped on their end** — their reflector rewrites the
  entire log. For our use case (knowing what was decided last Tuesday),
  rewriting history is a bug, not a feature. Our SQLite rows are
  effectively append-only because no code path deletes them.
- **Their observer prompt is huge.** Without prompt caching that's a real
  cost; with our subprocess model (no caching across calls), it would be
  worse. We aim small.

---

## 8. Open questions

Things to resolve before implementation, not while writing it.

1. **Should observations feed FTS5 search?** The existing `messages_fts`
   table indexes message bodies. Do we add observations to FTS so
   `threadhop search "rate limit"` finds the observation that mentions
   it? Cheap to do; risk is FTS noise from short observation text.
2. **Per-session vs per-project observations table.** v1 was per-session
   (one JSONL per session). The schema sketch above is one global table
   with a `session_id` column. Cleaner and lets the handoff skill query
   across sessions in a project trivially. Confirm.
3. **What does the TUI surface?** v1 had observation indicators in the
   sidebar and a kanban for decisions/todos. v2's single-stream design
   doesn't kanban — does the TUI just gain an "observations" panel or
   a per-message marker? Out of scope for the observer rebuild itself,
   but the kanban screen (`screens/kanban.py`) gets gutted either way.
4. **Sidecar trigger cadence (if we keep it).** "Every N new turns"
   (cheap) vs "every N new tokens" (Mastra-style). N=3 turns matched
   v1's `BATCH_THRESHOLD` and felt right; no reason to change without
   evidence.
5. **What does `--reset` mean?** v1 dropped the state row and deleted
   the JSONL. v2 should drop the state row and `DELETE FROM observations
   WHERE session_id = ?` in a transaction. Same name, same UX, simpler
   implementation.
6. **Plugin command.** `/threadhop:observe` exists. Trim it to match
   the v2 surface or rebuild from scratch? Probably trim; the command
   itself is thin.

---

## 9. Migration / rollout

There is no live data to migrate — observations are derived. The plan:

1. **`archive/observer-v1` (this branch)**: snapshot of v1 + this doc.
   Reference for the rebuild. Don't merge to dev.
2. **Scrap PR on dev**: removes everything listed in §1 ("what gets
   scrapped") plus the related ADRs in `docs/DESIGN-DECISIONS.md`
   (ADR-018 / 019 / 020). Updates `CLAUDE.md` to remove the observation
   pipeline sections. Cuts a release that bumps minor (the observer was
   a documented feature; removing it is a breaking change for anyone
   using the CLI verbs).
3. **v2 PR on dev**: implements the design above. Lands on a fresh ADR
   number. Re-introduces a single ADR for the v2 design, replacing the
   three v1 ADRs.

Between steps 2 and 3, dev has no observer at all. That's fine — the
TUI, search, bookmarks, tagging, and handoff skeleton all work without
it. Handoff briefs in that window will be missing the
"observations/decisions" sections; the brief should degrade cleanly
("no observations recorded for this session").

---

## 10. Reference: where v1 lives

This branch (`archive/observer-v1`) preserves v1 exactly as it stood when
the decision to scrap was made. To read the v1 implementation:

- `threadhop_core/observation/observer.py` — orchestrator + watch mode
- `threadhop_core/observation/reflector.py` — cross-session conflict pass
- `threadhop_core/observation/observer_state.py` — state-row helpers
- `threadhop_core/observation/queries.py` — JSONL readers for the CLI
- `prompts/observer.md`, `prompts/reflector.md` — the v1 prompts
- `threadhop_core/cli/commands/{observe,observations,decisions,todos,conflicts}.py`
- `threadhop_core/storage/db.py` — `observation_state`, `conflict_reviews`
- `obv_sample.jsonl` (repo root) — sample output that confirmed Sonnet
  could handle the v1 prompt where Haiku could not
- `docs/observational-memory.md` — v1 design notes
- `docs/DESIGN-DECISIONS.md` — ADR-018 (observer orchestrator),
  ADR-019 (per-session JSONL), ADR-020 (shared observer/reflector file)

The canonical exported chat that motivated this rewrite is preserved in
the user's threadhop session export at session ID
`12e5081f-e91c-41eb-9bb4-68b4c73f186c` (2026-04-17). Key points from
that conversation are summarized in §1 and §2 of this doc.
