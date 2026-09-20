# ThreadHop — Design Decisions & Implementation Plan

Extracted from design discussion on 2026-04-14.
Revised 2026-08-27: observer/reflector architecture dropped (ADR-029);
replaced by the borrow spectrum — peek / search / prepare / receive
(ADR-030–ADR-033).
Status: **Design complete, implementation not started.**

---

## Table of Contents

- [Decisions (ADRs)](#decisions-adrs)
- [Implementation Plan](#implementation-plan)
- [Schema](#schema)
- [Plugin Architecture](#plugin-architecture)
- [TODO](#todo)
- [Open Questions](#open-questions)

---

## Decisions (ADRs)

### ADR-001: SQLite over JSON for metadata storage

**Context:** The app currently uses `~/.config/threadhop/config.json` for
all persistent state (theme, session names, ordering, last_viewed). New features
(FTS search, bookmarks, tags, project memory) require richer query patterns.

**Decision:** Migrate to SQLite at `~/.config/threadhop/sessions.db`.
Keep `config.json` only for app-level settings (theme, sidebar_width).

**Rationale:**
- FTS5 requires SQLite — no way around this
- Tags/bookmarks need relational queries ("all bookmarks tagged 'decision' from project X")
- Append-only memory ledger needs filtering by type, project, date
- WAL mode handles concurrent reads (TUI + skill plugin)
- One-time migration from config.json on first run

**Rejected:** Keeping JSON alongside SQLite (two sources of truth).

---

### ADR-002: SQLite FTS5 with porter stemming over embeddings

**Context:** Need full-text search across all session transcripts.

**Decision:** Use SQLite FTS5 with `porter unicode61` tokenizer.

**Rationale:**
- Personal-scale data (hundreds of sessions, not millions)
- Users remember keywords, not abstract concepts
- Zero infrastructure — SQLite is Python stdlib
- Sub-millisecond queries
- Deterministic, explainable results

**Revisit when:** Keyword search demonstrably fails on actual usage patterns.

---

### ADR-003: Chunk merging for assistant messages in FTS index

**Context:** Claude Code writes assistant responses as multiple JSONL lines
sharing the same `message.id` but each with a unique `uuid`. Lines 9-11 of a
typical session might all be `msg_01McJaN...` — text block, tool_use, more tool_use.

**Decision:** Group consecutive assistant lines by `message.id`, concatenate
their text content, index as one row. Use the first `uuid` as PK.

**Rationale:**
- Search results should be logical messages, not streaming chunks
- A tool call and its surrounding text are one thought unit
- User messages are always one line = one row (no grouping needed)

---

### ADR-004: Conductor-style status tags for sessions

**Context:** Sessions are currently a flat list sorted by manual ordering or
modification time. No way to track work status.

**Decision:** Add a `status` field to sessions with values:
`active` (default) | `in_progress` | `in_review` | `done` | `archived`

Sessions display grouped by status. Manual reorder works within each group.
Archived sessions are hidden by default (toggle with `A`).

**Rationale:**
- Matches the plan → implement → verify workflow
- Archived sessions declutter the list without deleting data
- Status groups provide natural visual organization

---

### ADR-005: Append-only JSONL ledger for project memory

**Context:** Need a sequential, chronological record of decisions, TODOs, and
work completed — distinct from CLAUDE.md which is a snapshot/declarative doc.

**Decision:** Store project memory in the SQLite `memory` table with typed
entries (decision, todo, done, adr, observation). Entries are append-only.
Rendered as markdown for injection into new sessions.

**Rationale:**
- CLAUDE.md is the wrong shape — it's a snapshot that gets rewritten
- The ledger answers "what happened and when" not "what are the rules"
- Cross-project by default, filterable by project
- Same SQLite DB, no separate files to manage

---

### ADR-006: Strict LLM vs instantaneous boundary

**Context:** Early design mixed LLM-powered skills and instant operations
without a clear boundary. For example, search and context insertion were
proposed as Claude Code skills — but search must be per-keystroke instant,
and context insertion is a visual selection in the TUI, not a typed command
with opaque message ranges.

**Decision:** Draw a hard line between what requires an LLM call and what
must be instantaneous:

| Operation | Type | Where it lives |
|-----------|------|----------------|
| Search across sessions | Instantaneous (FTS) | TUI |
| Message selection + copy | Instantaneous | TUI |
| Message export to temp file | Instantaneous | TUI |
| Bookmark a message | Instantaneous | TUI |
| Tag a session | Instantaneous | TUI |
| Handoff brief generation | LLM call (sub-agent) | Skill plugin |
| Project memory injection | File read (no LLM) | Skill plugin |

**Rationale:**
- Search must update results on every keystroke — an LLM round-trip is absurd
- Context insertion requires visual message selection — you can't type "15-25"
  without seeing the messages first, so the TUI handles selection and the
  clipboard/temp-file handles transport
- Only handoff (transcript compression) genuinely needs an LLM
- Skills are for operations that benefit from being invoked mid-conversation
  without switching to the TUI (handoff, memory injection)

**Rejected:** `/threadhop:insert-context <id> [range]` as a skill
(range numbers are opaque without visual context).
**Rejected:** `/threadhop:search <query>` as a skill (too slow, wrong UX).

---

### ADR-007: Real-time search architecture in TUI

**Context:** Need to search across all session transcripts from the TUI.
Must be per-keystroke instant — results update as you type, like a fuzzy finder.

**Decision:** Two-tier search powered by the FTS index:

**Tier 1 (v1): FTS5 prefix matching — instant, per-keystroke**
- User presses `/` (or a search keybind) → search input appears
- Each keystroke queries FTS5 with prefix matching:
  `messages_fts MATCH 'rate* lim*'` as user types "rate lim"
- Results displayed in a panel: matching message snippets with session name,
  project, and timestamp
- Navigate results with `j`/`k`, press `Enter` to jump to that message
  in the source transcript
- FTS5 prefix queries are sub-millisecond on personal-scale data
- Porter stemmer handles word forms ("running" matches "run")

**Tier 2 (future): Fuzzy matching for typos**
- Add trigram tokenizer (`tokenize='trigram'`) as a secondary FTS table
- Trigram matching handles spelling mistakes: "retr" matches "retry"
- Fall back to trigram search when FTS5 prefix returns zero results
- Alternatively: compute Levenshtein distance on FTS5 results for reranking

**Scope:** Searches all indexed sessions by default. Filter by project with
a modifier (e.g., `project:atlas rate limiting`). Filter by role with
`user:` or `assistant:` prefix.

**Rationale:**
- FTS5 prefix matching is the simplest path to per-keystroke search
- SQLite runs the query in C — Python overhead is just the binding call
- The index is already built by the background refresh cycle
- No regex needed for v1 — FTS5 tokenization handles word boundaries
- Regex can be offered as an advanced mode later (`/regex:pattern`)

---

### ADR-008: Context export via clipboard + temp files

**Context:** Users need to carry messages from one session into another.
Two transport mechanisms, both instantaneous (no LLM).

**Decision:**

**Clipboard copy (primary):**
- Select messages in TUI → press `y` → formatted text copied to clipboard
- Paste into any Claude Code session, T3 Code, or any other tool
- Format includes source labels:
  ```
  [From "API contracts" — ~/agent-atlas — 2026-04-12 10:30]
  User: What about rate limiting?
  Claude: Two options: leaky bucket vs token bucket...
  ```

**Temp file export (for larger selections):**
- Select messages → press `e` (export) → written to temp directory
- Path: `/tmp/threadhop/<session_id>-<timestamp>.md`
- NOT stored in any repo directory — these are ephemeral reference files
- TUI displays the full path after export
- User references from Claude Code: `Read /tmp/threadhop/abc123-20260414.md`
- Temp files are auto-cleaned on OS reboot (standard /tmp behavior)

**Rationale:**
- Clipboard for quick grabs (1-5 messages)
- Temp file for larger context blocks (10+ messages) that would be unwieldy
  as clipboard paste
- Temp directory avoids polluting any repo or config directory
- Full absolute paths so any session on the machine can reference them
- No LLM needed — this is a copy/format operation

**Rejected:** Storing exports in the repo structure.
**Rejected:** Storing exports in `~/.config/threadhop/` (not temp data).

---

### ADR-009: Stay with Python + Textual

**Context:** Considered Rust (ratatui) and TypeScript (Ink) as alternatives.

**Decision:** Stay with Python + Textual.

**Rationale:**
- App is I/O bound (file reads, subprocess calls), not CPU bound
- Textual provides complete widget system — ratatui would require hand-rolling
- SQLite FTS runs in C regardless of host language
- Single-file deployment via `uv run --script` is a major UX advantage
- Only benefit of Rust: ~5ms startup vs ~200ms (not perceptible in TUI)

---

### ADR-010: Observer-first architecture

**Status:** Superseded by ADR-029 (2026-08-27). Established the observer
(Haiku via `claude -p`) as the system core, with TUI and CLI as consumers
of typed observation JSONL. Body removed — see git history.

---

### ADR-011: Dual-mode CLI (TUI + subcommands)

**Context:** ThreadHop was initially TUI-only. With the observer-first
architecture, users need CLI access to observations, tagging, and handoffs
without launching the TUI.

**Decision:** Single executable, two modes:

```bash
# No subcommand = TUI mode
threadhop
threadhop --project atlas --days 7

# With subcommand = CLI mode
threadhop todos                        # list all open TODOs
threadhop todos --project atlas        # filtered by project
threadhop decisions                    # all decisions
threadhop observations                 # everything
threadhop tag backlog                  # tag current session
threadhop tag in_review --session abc  # tag specific session
threadhop handoff abc123               # generate handoff brief
```

**Session detection for `threadhop tag`:** When called without `--session`,
detects the current session by scanning `ps` for claude processes in the
current terminal. Same detection logic the TUI already uses.

**Rationale:**
- One executable, no separate CLI tool to install
- Subcommand pattern is familiar (git, docker, etc.)
- CLI queries trigger the observer, so observations are always fresh
- Tags can be set from any terminal tab without switching to the TUI

---

### ADR-012: Two skills — context, handoff (tagging uses bash passthrough, not a skill)

**Amended 2026-08-27 (ADR-029, ADR-031, ADR-032):** both skills are gone.
`/threadhop:handoff` is superseded by `threadhop prepare` / `threadhop
receive` (one LLM call, at prepare — ADR-032). `/threadhop:context`
survives as the `/threadhop:copy` command. The plugin surface is now
commands-only, zero skills (see [Plugin Architecture](#plugin-architecture)).
The principle below stands: never spend an LLM turn on a one-shot write —
the `!` passthrough remains the tagging path.

**Context:** Need to interact with ThreadHop from within a Claude Code session
without switching to the TUI or a terminal. Earlier design had three skills
including `/threadhop:tag`; that skill was dropped in favor of the `!`
bash passthrough (see ADR-013 evolution note) — skills invoke the LLM,
which is pure overhead for a one-shot SQLite write.

**Decision:** Two Claude Code skills with distinct roles, plus the `!`
passthrough for tagging:

| Surface | What it does | Uses LLM? |
|---|---|---|
| `!threadhop tag <status>` (bash passthrough) | Tags current session | No — direct shell invocation, no turn |
| `/threadhop:context` | Formats clipboard content as sourced context | No — reads `pbpaste`, formats |
| `/threadhop:handoff <id>` | Compresses a full session into a brief | Yes — sub-agent with Haiku |

**`!threadhop tag <status>`** — zero LLM turn:
1. Claude Code's `!` prefix runs the command directly in the host shell
2. `threadhop tag` auto-detects the current session id from its process ancestry
3. Prints one tight line: `✓ tagged <short-id> as <status>`

**`/threadhop:context`** — instant, no LLM:
1. Reads clipboard (`pbpaste`) containing messages copied from TUI
2. Detects ThreadHop source labels in the content
3. Presents as a clearly bounded context block in the conversation
4. The model can now work with the injected context

**`/threadhop:handoff <id> [--full]`** — LLM call:
1. Reads the JSONL transcript for the given session
2. Default: sub-agent generates a structured brief (~30-50 lines)
3. `--full`: sub-agent produces comprehensive handoff with rationale and excerpts
4. Injects the brief into the current conversation

**Rationale:**
- Tag: the `!` bash passthrough is zero-LLM, already supported by
  Claude Code, and surfaces in `!`-history autocomplete after first use.
  No reason to burn a skill on it.
- Context: bridges TUI (visual selection) to Claude Code (injection)
- Handoff: the only one that needs an LLM, clearly separated

**Rejected:**
- Merging context and handoff into one skill (different mechanisms,
  different cost profiles).
- A dedicated `/threadhop:tag` skill: a skill invokes the LLM — pure
  overhead for a one-shot SQLite write. Users who want slash-style
  ergonomics can install the optional `UserPromptSubmit` hook documented
  in the README. Trade-off accepted: no `/` autocomplete discoverability.

---

### ADR-013: Session tagging from three entry points

**Context:** Session tags (backlog, in_progress, in_review, done, archived)
need to be settable from multiple places depending on the user's context.

**Decision:** Three entry points, one database:

| Entry point | How | When you'd use it |
|---|---|---|
| ThreadHop TUI | Press `s` to cycle status | Triaging multiple sessions |
| Terminal CLI | `threadhop tag backlog` | Quick tag from another tab |
| In-session bash passthrough | `!threadhop tag backlog` from inside Claude Code | Mid-conversation, without leaving |

All three write to the same SQLite `sessions` table. The TUI reflects
changes from CLI/passthrough on the next 5s refresh.

**Rationale:**
- Different moments call for different interfaces
- Shared database means no sync issues
- The in-session entry point is the `!` bash passthrough — zero LLM turn,
  instantaneous, already built into Claude Code. An optional
  `UserPromptSubmit` hook gives `/tag <status>` ergonomics for users who
  want it (documented in README).

**Evolution:** Earlier revisions of this ADR proposed a `/threadhop:tag`
skill as the in-session entry point. A skill invokes the LLM — pure
overhead for a one-shot SQLite write. The `!` passthrough delivers the
same outcome with no model call, and its history is surfaced in
Claude Code's `!`-history autocomplete after first use. Trade-off: no
`/` autocomplete discoverability. Mitigated by a README section and the
optional hook.

---

### ADR-014: Sidebar resize via keybindings

**Context:** Session list is hardcoded at 36 characters (`grid-columns: 36 1fr`).
No way to resize from the UI.

**Decision:** Add `[` and `]` keybindings to shrink/grow sidebar (min 20, max 60,
step 4). Persist width in config. Consider Textual `Splitter` widget later for
drag-to-resize.

---

### ADR-015: Background observer-reflector as sidecar process

**Status:** Superseded by ADR-029 (2026-08-27). Background observer +
reflector sidecar processes watching session JSONL in real time
(Mastra-inspired). Body removed — see git history.

---

### ADR-016: Per-session opt-in trigger and pull-based context injection

**Status:** Superseded by ADR-029 (2026-08-27). Per-session opt-in
observation (`/threadhop:observe`) and pull-based injection
(`/threadhop:insights`). Body removed — see git history.

---

### ADR-017: Context-aware discoverability via modal help and shared command metadata

**Context:** ThreadHop already has multiple interaction surfaces with
different affordances: the global search modal, the persistent in-transcript
find bar, the stock footer, and transcript-local selection mode. The current
footer only exposes a small subset of bindings, while other commands live in
widget-local `on_key` handlers or transient notifications. That was fine when
the app was smaller, but it does not scale now that bindings are focus-aware,
mode-specific, and sometimes conflicting.

An always-on footer that tries to show every key all the time would turn into
noise and still be incomplete. The app needs one discoverability surface that
answers "what can I do from here?" without flattening all contexts together.

**Decision:** Add a context-aware help overlay, using the same full-app modal
pattern as search, and back it with a shared command metadata registry.

**UI model:**
- Keep the footer minimal and contextual. It remains a compact reminder of the
  highest-value actions currently available, not the source of truth for every
  binding.
- Add a global help overlay that takes over the app like search does and
  groups commands by scope: global app, session list, transcript, selection
  mode, reply input, and search/find.
- The help overlay may optionally expose executable actions later, but v1 is
  discoverability-first rather than a general command palette.
- The help trigger must remain separate from handoff naming. Do not hardcode
  `H` as the permanent key for help.

**Architecture model:**
- Define command metadata in one shared registry rather than duplicating key
  descriptions across `Footer`, modal help text, README tables, and ad hoc
  notifications.
- The registry must support context predicates so commands can be shown only
  when relevant (for example: transcript focused, selection mode active, find
  bar open).
- Widget-local commands still own their behaviour, but they also register
  discoverability metadata so they stop being invisible to the rest of the UI.
- Footer rendering and help-overlay rendering should both read from this same
  metadata source.

**Rationale:**
- Search already established the right interaction precedent for a full-app
  overlay in this TUI.
- Context-aware discoverability matches the app's actual behaviour; a flat
  list of bindings does not.
- A shared registry prevents docs and UI surfaces from drifting apart as more
  commands are added.
- Leaving the help key unresolved avoids creating unnecessary coupling with the
  future handoff shortcut work.

**Rejected:** Expanding the footer into a permanent wall of bindings.
**Rejected:** Maintaining help text separately in code, docs, and notifications.

---

### ADR-018: Observer as core function — `claude -p` invocation, not API

**Status:** Superseded by ADR-029 (2026-08-27). Observer as the single
core function behind all entry points, invoked as `claude -p --model
haiku --permission-mode acceptEdits`. Body removed — see git history.

---

### ADR-019: Per-session observation files with SQLite state tracking

**Status:** Superseded by ADR-029 (2026-08-27). Per-session observation
files plus the `observation_state` SQLite table (byte offsets, PID
lifecycle, stop/resume). Body removed — see git history.

---

### ADR-020: Unified observation JSONL — observer and reflector share one file

**Status:** Superseded by ADR-029 (2026-08-27). Observer and reflector
appending to one per-session observation JSONL, conflicts as
`type:"conflict"` entries. Body removed — see git history.

---

### ADR-022: Reflector implementation — prompt, invocation, and state tracking

**Status:** Superseded by ADR-029 (2026-08-27). Reflector as a second
`claude -p` call spawned by the observer, with entry-offset tracking
and conflict dedup. Body removed — see git history.

---

### ADR-021: Observation indicator in TUI session list + transcript header

**Status:** Superseded by ADR-029 (2026-08-27). 🗒 observation indicator
in the session list, transcript header, and `o`/`O` keybindings. Body
removed — see git history.

---

### ADR-027: Update lifecycle — `threadhop update`, `changelog`, `future`, and 24h startup check

**Context:** ThreadHop is distributed as a git checkout (via `install.sh`
curl-bash or manual clone) with no package manager brokering versions.
Once installed, nothing tells the user that new commits exist on `main`,
and there is no built-in command to pull them — the only path today is
re-running the curl-bash installer, which is undiscoverable and relies
on the user remembering the URL. The Claude Code plugin surface has its
own update channel (owned by Claude Code's `/plugin` subsystem, out of
scope here). This ADR defines the CLI-side update story only.

**Decision:** Introduce three CLI subcommands plus a startup version
check:

1. **`threadhop update [--to <ref>] [--check]`** — pull latest `main` by
   default, or roll back to a specific git ref with `--to`, or report
   without pulling with `--check`.
2. **`threadhop changelog`** — print the repo's `CHANGELOG.md`, falling
   back to fetching `raw.githubusercontent.com/.../main/CHANGELOG.md` if
   the local file is missing (e.g. the user is on a version that
   predates the changelog surface).
3. **`threadhop future`** — print the top five entries from a new
   `ROADMAP.md` at repo root. Unconditional top-5; no tagging, filtering,
   or priority scoring.
4. **Startup version check** — at `main()` entry (CLI) and `on_mount`
   (TUI), cheaply compare the installed `__version__` against the latest
   GitHub release tag. If newer, print a nudge (CLI) or toast (TUI).

The mechanism is entirely explicit: no background auto-update, no daemon.
Users only get new code by running `threadhop update` (or re-running the
installer), and they only see version nudges when they're actively
invoking the tool.

**Subcommand shapes:**

```bash
threadhop update                   # git fetch && git reset --hard origin/main
threadhop update --check           # compare versions, print, exit 0
threadhop update --to v0.1.0       # roll back to a tag, branch, or SHA
threadhop changelog                # print CHANGELOG.md (paginated via less if TTY)
threadhop future                   # print top 5 ROADMAP.md entries
```

No flag-form alias (`threadhop --update`) — the subcommand shape is
required because `--to <ref>` needs an argument, and flag-with-argument
grammar is awkward.

**Startup check semantics — four gates before printing anything:**

```
1. TTY gate       stdout.isatty() AND stderr.isatty() must be True.
                  Protects pipelines (threadhop observations | jq).
2. Context gate   Must NOT be running inside a Claude Code session.
                  Detected via parent-process walk (same helper as
                  `_resolve_cli_session` from task #17). Protects plugin
                  invocations and `!threadhop` bash passthrough.
3. Cache gate     ~/.cache/threadhop/last_check mtime must be older than
                  24 hours. On pass, touch the file.
4. Env gate       $THREADHOP_NO_UPDATE_CHECK must not be set (opt-out).
```

Only when all four pass does the tool perform a 1-second-timeout HTTP
GET against `api.github.com/repos/parzival1l/threadhop/releases/latest`.
Any network error, JSON error, or unparseable tag format is swallowed
silently — a version check must never break the CLI.

**Notification shape — CLI:**

```
ThreadHop 0.2.0 is available (you have 0.1.0).
  What's new:  threadhop changelog
  Update:      threadhop update
```

Three lines, stderr, printed exactly once per session (first CLI command
of the day after cache expiry). No plugin-update hint — plugin lifecycle
is Claude Code's responsibility.

**Notification shape — TUI:**

```python
# In ClaudeSessions.on_mount():
if info := _check_for_update():
    self.notify(
        f"ThreadHop {info.latest} available — run `threadhop update`.",
        title="Update available",
        severity="information",
        timeout=10,
    )
```

Transient toast in the top-right corner (Textual's native `notify`
pattern). Auto-dismiss after 10 seconds. Does not resize the layout or
steal focus.

**Where the notification does NOT appear:**

- Inside Claude Code plugin commands (`/threadhop:tag`, etc.) — context
  gate suppresses it, because a plugin user may fire many commands
  within a session and the 24h cache only arms once per session.
- Inside `!threadhop …` bash passthrough in Claude Code — same gate.
- In non-TTY contexts (CI, `threadhop ... | jq`, etc.) — TTY gate.
- When `$THREADHOP_NO_UPDATE_CHECK=1` — env gate.

**`threadhop future` — format contract:**

`ROADMAP.md` at repo root, format:

```markdown
# ThreadHop Roadmap

- #NN — Short description line that renders well in CLI.
- #NN — Another item.
...
```

Parser rules:
- Lines matching `^- #(\d+) — (.+)$` become roadmap entries.
- The first five matching lines are printed, in file order.
- Everything else (headers, prose, blank lines) is ignored.

Output shape:

```
ThreadHop — what's coming up:

  #NN  Short description line that renders well in CLI.
  #NN  Another item.
  ...

Full roadmap:
  https://github.com/parzival1l/threadhop/blob/main/ROADMAP.md
```

Implementation: read from the installed repo's `ROADMAP.md`. No network
call. Users on pinned old versions see that version's roadmap; that's
acceptable because `threadhop update` brings the roadmap forward.

**`threadhop changelog` — format contract:**

`CHANGELOG.md` at repo root, Keep-a-Changelog style:

```markdown
# Changelog

## [0.2.0] — 2026-05-01
### Added
- `threadhop update` subcommand
- 24h startup version check

## [0.1.0] — 2026-04-20
- Initial release.
```

No parsing — the file is printed verbatim through `less -R` if stdout
is a TTY, raw otherwise. Users can read per-version sections by
scrolling.

**Version comparison — simple tuple compare:**

```python
def _parse_version(v: str) -> tuple[int, ...]:
    return tuple(int(x) for x in v.lstrip("v").split("."))
```

Assumes `major.minor.patch` with integer components. Tag names with
suffixes (`v0.2.0-rc1`) will raise; the caller treats that as "check
failed" and falls back silently. Acceptable until the project actually
ships pre-releases.

**Plugin updates — explicitly out of scope:**

Claude Code's `/plugin` subsystem owns plugin lifecycle. Users update
the plugin via `/plugin update threadhop` (or whatever Claude Code's
equivalent is). ThreadHop's CLI does not try to push plugin updates,
does not warn plugin users about CLI updates (they see the notice when
they next run the CLI directly), and does not synchronize the two
surfaces' versions. The plugin's own `version` field in
`.claude-plugin/marketplace.json` is bumped manually on each release as
part of the CLI release discipline.

**Release discipline — what the maintainer does on each release:**

1. Update `CHANGELOG.md` with a new version header and bullet list.
2. Bump `__version__` in `threadhop`.
3. Bump the plugin `version` field in `.claude-plugin/marketplace.json`
   and `plugin/.claude-plugin/plugin.json`.
4. Commit everything.
5. Cut a git tag: `git tag v0.2.0 && git push --tags`.
6. (Optional later) GitHub Releases auto-populate from tags.

Without step 5, the startup check has nothing to compare against and
silently degrades to no-op. This is acceptable: missing tags means no
false-positive "update available" notifications, just no notifications
at all.

**Rationale:**
- **Re-running curl-bash works but is undiscoverable** — users install
  once, never see the URL again. A `threadhop update` surface gives
  them an ergonomic second update path without replacing the first.
- **Startup check is the ambient discovery channel** — users don't have
  to remember to check for updates; the tool reminds them once per day,
  unobtrusively. 24h cooldown bounds network traffic and prevents
  notification fatigue.
- **Suppress inside Claude Code** because plugin users may invoke
  commands dozens of times per day; a 24h cache still means dozens of
  plugin messages per year get an unsolicited upgrade nudge. Cleaner to
  only nudge when they're deliberately at the CLI.
- **Rollback via `--to`** honors the user's stated "users can go back
  to older versions if they want" requirement. Covers bug-regression
  scenarios at zero extra cost.
- **Top-5 roadmap with simple format** is intentionally dumb — it's a
  view into an existing file, not a ranking engine. If the format ever
  needs to get smarter (priorities, categories), it can grow later.
  Keeping the contract narrow means `ROADMAP.md` stays maintainable by
  hand.
- **Plugin updates delegated to Claude Code** because cross-tool
  lifecycle coordination is a rat-hole. Claude Code has first-party
  update machinery; ThreadHop should not try to replicate or wrap it.

**Rejected:**
- **Background auto-update** — reputation-shredding antipattern; users
  hate when their CLI version silently changes.
- **Flag form `threadhop --update`** — can't carry `--to <ref>` cleanly.
- **Notification inside Claude Code plugin invocations** — would pollute
  hundreds of plugin messages per week for active users.
- **Preemptive plugin-update hint in the CLI notification** — two
  surfaces, two update channels; co-mixing them in one message adds
  noise more than clarity.
- **Using `docs/TASKS.md` as the roadmap source** — `TASKS.md` is being
  removed from `main` (maintained out-of-band going forward). A
  dedicated `ROADMAP.md` with a narrow format contract is more stable.
- **SHA-based "did main change?" comparison** — loses semantic
  versioning and gives no changelog anchor. Requires users to trust
  that any commit is a valid "update."
- **Notification in a persistent footer badge** — competes with
  `ContextualFooter` (ADR-017) and stays visible forever. A transient
  toast is temporally scoped to the session it actually matters for.
- **Updating CLI and plugin in lockstep via `threadhop update`** —
  reaches across to Claude Code's plugin cache, which is not ours to
  touch.

---

### ADR-028: Harness adapter seam (single concrete adapter)

**Status:** Accepted (2026-04-26)

**Context:** ThreadHop shells out to `claude -p` from three sites — the
observer (`threadhop_core/observation/observer.py`), the reflector
(`threadhop_core/observation/reflector.py`), and handoff brief
generation (`threadhop_core/handoff.py`). *(Editorial note, 2026-08-27:
ADR-029 removed all three of those call sites; `run_claude_p` is now
called from exactly one site — `threadhop prepare` (ADR-032). The seam
survives unchanged and is where a second adapter will land.)* Originally each call site
carried its own `subprocess.run` block, its own argv assembly, its own
prompt-path resolution, and its own quirks (working directory, timeout,
env munging). Adding a second LLM CLI (e.g. `codex`, `gemini`) under
this layout would mean three parallel duplications times N adapters —
untenable at the first migration.

The package reorganisation (Phases 1-4) made the duplication acutely
visible because each module suddenly computed `parents[N]` differently
to find the bundled `prompts/` directory after moving deeper into the
tree.

**Decision:** Unify the subprocess invocation and prompt-template
loading behind a thin harness module, but do **not** introduce a
`Harness` Protocol or registry yet:

1. **`threadhop_core/harness/claude.py::run_claude_p()`** — the single
   entry point for invoking `claude -p`. Owns argv construction, model
   selection, working-directory normalisation, timeout, stdout/stderr
   capture, and returns a frozen `HarnessResult` dataclass whose field
   names mirror `subprocess.CompletedProcess` (`returncode`, `stdout`,
   `stderr`).
2. **`threadhop_core/harness/prompts.py::load_prompt()`** — reads
   bundled prompt templates from the repo's top-level `prompts/`
   directory using a single, package-anchored path resolution. The
   three call sites no longer compute `parents[N]` themselves.
3. **No `Harness` Protocol, no registry, no factory.** There is one
   concrete adapter today. Per the "one adapter = hypothetical seam,
   two adapters = real seam" rule, the abstraction is premature: its
   shape would be guesswork without a second adapter to constrain it.

**Rationale:**
- The natural Protocol shape is *already* the public signature of
  `run_claude_p(prompt, *, model, ...) -> HarnessResult`. When a
  second adapter (`codex.py`, `gemini.py`) lands, the Protocol can be
  extracted directly from this signature with no design churn.
- `HarnessResult` mirrors `subprocess.CompletedProcess` deliberately
  so existing tests that mock `subprocess.run` keep working without
  rewrites — the seam is invisible to test fixtures.
- Centralising prompt-path resolution removes a class of
  package-relocation footguns. Future package moves only update one
  file (`harness/prompts.py`) instead of three.
- Adding the second adapter is now a parallel-file change
  (`harness/codex.py` with `run_codex(prompt, *, model, ...)`,
  `harness/gemini.py`, etc.) plus a thin selection layer at the call
  sites — env var, config key, or per-session preference — wired in
  at the moment of the second adapter, not speculatively now.

**Rejected:**
- **Build the `Harness` Protocol now, with one implementation.**
  Speculative generality. The Protocol's method names, exception
  shape, and capability negotiation surface (e.g. does `codex`
  support `--permission-mode`? does `gemini` accept stdin prompts?)
  are guesswork without a second concrete adapter to constrain them.
  Better to ship the Protocol when it has two callers to satisfy.
- **Keep the three duplicated `subprocess.run` blocks until a second
  adapter arrives.** Rejected: the duplication was already a nuisance
  for prompt-path resolution after the package reorg, and re-paying
  the cleanup cost three times during the second-adapter migration is
  worse than paying it once now.
- **Stuff the harness into a class hierarchy with abstract methods.**
  Python protocols + module-level functions are simpler, friendlier
  to mocks, and have zero import-time cost.

**Revisit when:** A second LLM CLI adapter is added. At that point,
extract `Harness` Protocol from `run_claude_p`'s signature, add a
registry (env-var or config-driven), and update the three call sites
to ask the registry for the active adapter. The seam is already in
the right place; it just becomes load-bearing.

---

### ADR-029: Drop the observer/reflector architecture — lazy compression at transfer time

**Status:** Accepted (2026-08-27). Supersedes ADR-010, ADR-015, ADR-016,
ADR-018, ADR-019, ADR-020, ADR-021, ADR-022. Amends ADR-012, ADR-028.

**Context:** The observer ran Haiku every ~3-4 messages — roughly 50
background `claude -p` calls per 200-message session — paid speculatively
against the bet that the resulting observations would be queried later.
Actual usage disproved the bet: observations were rarely queried, and
handoffs are occasional events, not a continuous need. The architecture
also carried real operational weight: sidecar processes, PID lifecycle,
watch mode, stop/resume semantics, a companion reflector, and a state
table — all serving queries the user never makes.

**Decision:** Remove the observer/reflector architecture wholesale:

- Observer and reflector processes, prompts, and watch mode
- Per-session observation files (`~/.config/threadhop/observations/`)
- The `observation_state` table (dropped by migration — ADR-033)
- `/threadhop:observe` and the observer-backed `/threadhop:handoff`
- `/threadhop:insights`
- The `todos` / `decisions` / `conflicts` / `observations` CLI queries

New invariant: **every LLM call must be user-intent-gated.** The only LLM
call in the system is one compression call at `threadhop prepare` time
(ADR-032), made at the moment the user proves intent to transfer.

**Rationale:**
- 1 call at proven intent vs ~50 speculative calls per session — the
  economics only work if observations are queried often; they weren't
- No background processes: no PID lifecycle, no watch mode, no stale-PID
  detection, no reflector cadence — an entire failure-mode class deleted
- What is knowingly lost: the typed decision ledger, conflict detection,
  and insights. Lookup needs are served by FTS search over raw
  transcripts instead (ADR-002, ADR-031) — the transcripts were always
  the source of truth; observations were a derived cache

**Rejected:**
- Keeping the observer as a dormant opt-in — dead code with a
  maintenance bill and a standing temptation to re-grow
- Cheaper/batched observation — reduces the multiplier, keeps the
  speculation

---

### ADR-030: Exchange as the retrieval and windowing unit

**Status:** Accepted (2026-08-27). Extends ADR-003.

**Context:** Pulling context out of another session needs bounds. ±N
message windows are arbitrary — they split thoughts mid-stream and drag
in unrelated neighbours. Embedding-based semantic boundary detection
(TextTiling-style) solves that properly but is overkill at personal
scale and drags model weight into a zero-LLM path.

**Decision:** The retrieval unit is the **exchange** — one user turn plus
all assistant/tool activity until the next user turn. Exchanges are
computed structurally at parse time; no DB schema change is required
(they may later be stamped as `exchange_id` in the `messages` table when
FTS lands — see Q8). `peek` windows, `--grep` results, and `prepare`
tails are all exchange-bounded.

**Rationale:**
- In agent chats, each user prompt almost always opens a topic — the
  exchange is a natural semantic unit obtained for free
- Zero model cost, deterministic, explainable — the same properties that
  won FTS over embeddings in ADR-002
- Extends ADR-003 one level up: ADR-003 merges streaming chunks into
  logical messages; ADR-030 groups logical messages into logical topics

**Rejected:** ±N message windows (arbitrary boundaries). TextTiling /
embedding boundary detection (model weight for a problem the transcript
structure already solves).

---

### ADR-031: The borrow spectrum — peek / search / transfer

**Status:** Accepted (2026-08-27).

**Context:** Codex CLI's #-mention (PR #17358) injects prior
conversations verbatim — user/assistant messages only, tool calls and
system prompts stripped — as a hidden remembered-context packet. It
validates raw injection as a mechanism, but re-creates the context-fill
problem by injecting whole threads. Handoff-style compression is the
opposite extreme: an LLM call even when the user just wants to *look at*
something. Neither extreme matches how borrowing actually happens.

**Decision:** Three tiers, each matched to a question:

| Question | Command | LLM cost |
|---|---|---|
| "Show me that part of chat A" | `threadhop peek <session> [--last N / --range A:B / --grep X]` | 0 |
| "Where did we discuss X?" | `threadhop search <query> [--project] [--json]` (FTS5) | 0 |
| "Continue this work over there" | `threadhop prepare` → `threadhop receive <ticket>` | exactly 1, at prepare |

`--grep` results return whole exchanges (ADR-030), not the full thread —
scoped borrowing beats Codex's whole-conversation injection. Verbatim
output strips tool results, sidechains, and system-reminders, and
includes source labels (session name, project, timestamp) in the ADR-008
format.

**Rationale:**
- Most borrowing is lookup, not continuation — lookup must cost zero
- The tier boundary is the user's question, not an implementation detail
- Every tier is a plain CLI command, so all three work as `!threadhop …`
  passthroughs from inside any chat

**Rejected:** Making transfer the only door (rebuilds handoff friction
for what is usually a lookup). Injecting whole conversations (Codex's
flaw — the mechanism validated, the context-fill problem kept).

---

### ADR-032: prepare/receive transfer tickets

**Status:** Accepted (2026-08-27).

**Context:** Continuation transfers need whole-conversation context, but
"last N messages verbatim" alone loses the arc, and a fresh summary of
everything is the old handoff cost paid every time. There is also a
moving-target problem with lazy pulls: if chat B pulls "the last 3
exchanges" from chat A while chat A keeps working, the referent changes
between glance and paste.

**Decision:** Transfer is a two-command flow around a frozen ticket.

**`threadhop prepare [--session id] [--tail N=3] [--tail-budget chars=8000] [--model haiku]`**
— run from (or for) chat A:

1. Auto-detects the current session when `--session` is omitted (same
   parent-process walk as `threadhop tag`)
2. Splits the transcript into **head** (everything but the last N
   exchanges) and **tail** (the last N exchanges verbatim, capped at the
   token budget, tool results stripped) — exchange-bounded per ADR-030
3. Makes ONE `claude -p` call (via harness `run_claude_p`, ADR-028,
   prompt at `prompts/prepare.md`) producing a narrative head summary:
   goal, current state, decisions with rationale, open items, files
   touched
4. Writes a frozen ticket to `~/.config/threadhop/transfers/tk_<8hex>.md`
5. Prints a paste-ready line:
   `Paste in the target chat: !threadhop receive tk_xxxx`

**`threadhop receive <ticket>`** — prints the ticket verbatim. Zero LLM.
Works pasted into any tool with a shell — Claude Code, Codex, anything.

**Rationale:**
- prepare pins a snapshot at the moment of intent — no moving target;
  the ticket says the same thing tomorrow
- All cost lives in prepare; receive is a file read
- No ID archaeology — prepare auto-detects the current session, and the
  ticket ID travels in one paste-ready line
- Summary-head + verbatim-tail is the established compaction pattern
  (Claude Code `/compact`, Mastra OM): the head carries narrative, which
  compresses well; the tail carries working state, which compresses badly

**Rejected:** Pull-from-target ("chat B fetches from chat A") — moving
target. Fully verbatim tickets — context fill with no arc. Fully
summarized tickets — destroys the working state the target needs
verbatim.

---

### ADR-033: Byte-offset summary caching at prepare time

**Status:** Accepted (2026-08-27).

**Context:** Re-preparing the same long-running session should not
re-summarize from byte 0. Incremental processing via byte offsets was
the observer's one genuinely good trick (former ADR-019) — salvaged
here without the background calls that came with it.

**Decision:** New SQLite table:

```sql
CREATE TABLE transfer_state (
    session_id          TEXT PRIMARY KEY,
    source_byte_offset  INTEGER NOT NULL DEFAULT 0,
    cached_summary      TEXT,
    updated_at          REAL
);
```

On re-prepare:
- Source JSONL grew → summarize only the new exchanges and merge with the
  cached summary. Still one call — the merge is part of the same prompt
  (`prompts/prepare.md` receives the cached summary + the new exchanges)
- Source unchanged → reuse the cached summary, zero LLM calls

The `observation_state` table is dropped by the same migration.

**Rationale:**
- A 400-message session re-prepared after 20 new messages costs one
  small call, not one giant one
- The cache is a pure optimization — deleting a row only makes the next
  prepare slower, never wrong

**Rejected:** Caching per-exchange summaries (more rows, no cheaper —
the merge call dominates). Keeping `observation_state` for its offset
column (wrong shape, dead columns).

---

### ADR-034: Vector/hybrid search tier — rejected pending evidence

**Status:** Accepted (2026-08-27). Reaffirms and extends ADR-002.

**Context:** The borrow-spectrum proposal considered a hybrid
FTS+embedding search tier with semantic-boundary chunking — "RAG without
generation." Retrieval quality would likely improve on concept-shaped
queries ("that auth discussion") where keyword search misses.

**Decision:** Rejected for now.

- Embeddings are not model-free: a local encoder is ≈100MB of weights,
  an embedding pass per indexed message, and heavier deps in a
  `uv run --script` single file
- FTS v1 has not shipped — ADR-002's revisit clause ("when keyword
  search demonstrably fails") has zero evidence either way
- Exchange chunking (ADR-030) already fixes the bounds problem
  structurally, which was half of what semantic chunking promised

**Revisit trigger:** Log searches that return zero results or are
retried with reworded queries. If frequent, add a local encoder +
`sqlite-vec` as a fallback/rerank tier fused with RRF — an additive
change on top of FTS, not a redesign.

**Rejected (for now):** Hybrid FTS+vector at v1 (cost before evidence).
Embedding-based chunking (ADR-030 covers it structurally).

---

## Implementation Plan

### Phase 1: SQLite Foundation + Session Tags + Archive
_Immediate value, enables all future features._

1. Create SQLite DB initialization with schema (sessions, settings tables)
2. One-time migration from config.json
3. Add `status` field to session model
4. Render sessions grouped by status in the TUI
5. Keybindings: `s`/`S` cycle status, `a` archive, `A` toggle archive view
6. Sidebar resize (`[`/`]`)
7. Update CLAUDE.md with new architecture

### Phase 2: FTS Index + Message Selection + Search
_Enables instant search and cross-session context sharing. All TUI features._

1. Add messages table + FTS5 virtual table
2. Build indexer: parse JSONL, merge assistant chunks, strip system-reminders
3. Incremental indexing via index_state table (byte offset tracking)
4. Piggyback indexing on the 5s refresh cycle
5. Add message selection mode (`m` to enter, `j`/`k` between messages)
6. Range selection (`v` + movement)
7. Copy selected messages with source labels to clipboard (`y`)
8. Export selected messages to temp file (`e`) → `/tmp/threadhop/<id>.md`
9. Real-time search panel (`/`):
   - FTS5 prefix matching, results update per keystroke
   - Results show: message snippet, session name, project, timestamp
   - `j`/`k` to navigate results, `Enter` to jump to source transcript
   - Filter syntax: `project:atlas`, `user:`, `assistant:`
10. Future: trigram-based fuzzy search for typo tolerance
11. Context-aware help overlay + shared command metadata registry:
   - Full-app discoverability overlay, modeled on the search modal
   - Footer stays minimal/contextual instead of listing every keybinding
   - One registry feeds footer hints, help content, and future docs sync
   - Trigger key intentionally left open; do not assume `H`
12. Exchange grouping in the indexer (ADR-030): stamp or derive the exchange
    for each message so search results, peek windows, and prepare tails all
    share the same unit (materialize vs derive: Q8)
13. `threadhop search <query> [--project] [--json]` CLI subcommand over the
    same FTS index — the one-shot, scriptable counterpart to the TUI panel

### Phase 3: Borrow CLI — peek / search / prepare / receive
_The borrow spectrum (ADR-031). Zero-LLM peek and search; exactly one LLM
call at prepare (ADR-032), cached across re-prepares (ADR-033)._

1. Add argparse subcommand routing: no subcommand = TUI, with subcommand = CLI
2. Implement `threadhop tag <status> [--session <id>]`
   - Auto-detect session from current terminal when `--session` omitted
3. Exchange parser (ADR-030): one user turn + all assistant/tool activity
   until the next user turn, computed at parse time
4. `threadhop peek <session> [--last N] [--range A:B] [--grep X]`:
   - Verbatim, exchange-bounded output (`--grep` returns whole exchanges)
   - Strips tool results, sidechains, system-reminders
   - Source labels (session name, project, timestamp) per ADR-008
5. `threadhop search <query> [--project] [--json]` over the Phase 2 FTS index
6. `threadhop prepare [--session id] [--tail N=3] [--tail-budget chars=8000] [--model haiku]`:
   - Head/tail split per ADR-032; ONE `claude -p` via harness
     `run_claude_p` (ADR-028) with `prompts/prepare.md`
   - `transfer_state` caching: summarize only new bytes, merge with the
     cached summary in the same call (ADR-033)
   - Write frozen ticket to `~/.config/threadhop/transfers/tk_<8hex>.md`
   - Print the paste-ready `!threadhop receive tk_xxxx` line
7. `threadhop receive <ticket>` — print ticket verbatim, zero LLM
8. Transfers directory + ticket format
9. Migration: drop the `observation_state` table (ADR-033)

### Phase 4: Plugin refresh
_One plugin, six commands, zero skills (ADR-029, ADR-031, ADR-032). Every
command body is a single `!threadhop …` line the harness pre-executes (Q4)._

1. Commands: `tag`, `bookmark`, `copy`, `peek`, `prepare`, `receive` — all
   `!threadhop …` pre-executed passthroughs with `argument-hint`
   frontmatter for `/` picker discoverability
2. Remove the `handoff` skill and the `observe` command from the plugin
3. All six remain available as raw `!threadhop …` passthroughs for
   zero-turn invocation
4. README: document the borrow spectrum and the prepare → receive flow

### Phase 5: Project Memory + Bookmarks
_Cross-session knowledge persistence._

1. Add bookmarks table to schema
2. Bookmark action from message selection mode (`space` to toggle)
3. Bookmark browser panel in TUI
4. Explicit annotation detection: recognize "ADR:", "DECISION:", "TODO:" markers
   in conversations and write directly to the `memory` table (ADR-029 —
   observations are gone)
5. Memory rendering: generate project memory markdown from the `memory`
   table for injection

---

## Schema

```sql
-- Location: ~/.config/threadhop/sessions.db

-- App-level settings (replaces most of config.json)
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL  -- JSON-encoded
);

-- Session metadata
CREATE TABLE sessions (
    session_id    TEXT PRIMARY KEY,
    session_path  TEXT NOT NULL,
    project       TEXT,
    cwd           TEXT,
    custom_name   TEXT,
    status        TEXT DEFAULT 'active',
        -- active | in_progress | in_review | done | archived
    sort_order    INTEGER,
    last_viewed   REAL,
    created_at    REAL,
    modified_at   REAL
);

-- Message index (for FTS and message selection)
CREATE TABLE messages (
    uuid          TEXT PRIMARY KEY,
    session_id    TEXT NOT NULL,
    parent_uuid   TEXT,
    role          TEXT NOT NULL,      -- 'user' | 'assistant'
    timestamp     TEXT NOT NULL,
    session_path  TEXT NOT NULL,
    line_number   INTEGER NOT NULL,   -- for jump-to-source
    cwd           TEXT,
    text          TEXT NOT NULL,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);

-- Full-text search
CREATE VIRTUAL TABLE messages_fts USING fts5(
    text,
    content='messages',
    content_rowid='rowid',
    tokenize='porter unicode61'
);

-- Incremental index tracking
CREATE TABLE index_state (
    session_path  TEXT PRIMARY KEY,
    last_offset   INTEGER NOT NULL,  -- byte offset into JSONL
    last_modified REAL NOT NULL
);

-- Bookmarks
CREATE TABLE bookmarks (
    id            INTEGER PRIMARY KEY,
    message_uuid  TEXT NOT NULL,
    note          TEXT,
    kind          TEXT NOT NULL DEFAULT 'bookmark',
        -- bookmark | research (task #59 later generalizes this)
    tags          TEXT,               -- JSON array
    created_at    REAL NOT NULL,
    FOREIGN KEY (message_uuid) REFERENCES messages(uuid)
);

-- Project memory ledger
CREATE TABLE memory (
    id            INTEGER PRIMARY KEY,
    project       TEXT NOT NULL,
    type          TEXT NOT NULL,
        -- decision | todo | done | adr | observation
    text          TEXT NOT NULL,
    session_id    TEXT,
    source        TEXT DEFAULT 'explicit',  -- explicit | auto
    resolved      INTEGER DEFAULT 0,        -- for TODOs: 0=open, 1=done
    created_at    REAL NOT NULL
);

-- Prepare-time summary cache (ADR-033)
CREATE TABLE transfer_state (
    session_id          TEXT PRIMARY KEY,
    source_byte_offset  INTEGER NOT NULL DEFAULT 0,  -- where prepare last read in source JSONL
    cached_summary      TEXT,                        -- head summary from the last prepare
    updated_at          REAL,
    FOREIGN KEY (session_id) REFERENCES sessions(session_id)
);
```

---

## Plugin Architecture

### Principle: one plugin, six commands, zero skills

Everything in-session is a slash command whose body is a single
`!threadhop …` line that the harness pre-executes before the model sees
the prompt (Q4) — the CLI does the work, the model relays stdout. There
are no skills: a skill spends an LLM turn framing output, and after
ADR-029 the only LLM spend in the system is the one call inside
`threadhop prepare` (ADR-032). The TUI handles everything visual and
instantaneous; the CLI handles queries, tagging, and the borrow spectrum
(ADR-031).

### Plugin: `threadhop`

One plugin, six commands under the `/threadhop:` namespace. The former
`handoff` skill and `observe` command are removed (ADR-029). The plugin
calls bare `threadhop` from `$PATH` — the app is installed separately
(Model B).

```
plugin/
├── .claude-plugin/plugin.json           # manifest: name=threadhop
└── commands/
    ├── tag.md                           # /threadhop:tag — !`threadhop tag` + argument-hint
    ├── bookmark.md                      # /threadhop:bookmark — !`threadhop bookmark`
    ├── copy.md                          # /threadhop:copy — clipboard → labelled context block
    ├── peek.md                          # /threadhop:peek — !`threadhop peek`
    ├── prepare.md                       # /threadhop:prepare — !`threadhop prepare`
    └── receive.md                       # /threadhop:receive — !`threadhop receive`
```

All six also work as raw `!threadhop …` bash passthroughs for
zero-LLM-turn invocation. The slash forms' advantage is discoverability
through the `argument-hint` frontmatter shown in the `/` picker.

### What lives where

| Feature | Lives in | Why |
|---------|----------|-----|
| Search | TUI + CLI (`threadhop search`) | Per-keystroke instant in the TUI; one-shot FTS query from any terminal or `!` passthrough (ADR-031) |
| Message select + copy | TUI | Visual selection, clipboard transport |
| Message export to .md | TUI | Visual selection, writes to /tmp |
| Bookmark ingest | TUI + CLI + `!` bash passthrough + `/threadhop:bookmark` | Four entry points, one `bookmarks` table — all write through `db.upsert_bookmark` via the same normalization |
| Tag session | TUI + CLI + `!` bash passthrough + `/threadhop:tag` | Four entry points, one DB (ADR-013) |
| Clipboard context injection | `/threadhop:copy` | Bridges TUI visual selection into the current chat |
| Peek at another session | CLI + `/threadhop:peek` | Zero LLM, exchange-bounded verbatim output (ADR-030, ADR-031) |
| Search from inside a chat | `!threadhop search` | Zero LLM, FTS5 over raw transcripts (ADR-031) |
| Prepare a transfer ticket | CLI + `/threadhop:prepare` | The system's only LLM call, at proven intent (ADR-032) |
| Receive a transfer ticket | CLI + `/threadhop:receive` | Zero-LLM file read — works in any tool with a shell |

### Tag entry point 3: `!threadhop tag <status>` (bash passthrough, zero LLM turn)

Not a skill — Claude Code's `!` prefix runs the command directly in the
host shell. See ADR-013 for why tagging was moved off the skill plane.

```
User (in Claude Code): !threadhop tag backlog

1. Claude Code runs `threadhop tag backlog` in the host shell (no model turn)
2. threadhop auto-detects the current session id by walking the parent
   process tree for its `claude` CLI ancestor (task #17)
3. ThreadHop CLI writes the tag to SQLite
4. Prints one tight line: "✓ tagged <short-id> as backlog"
```

On detection failure the command exits `2` with the helpful error from
`_resolve_cli_session()` and makes no DB write.

Optional: a `UserPromptSubmit` hook can provide `/tag <status>` ergonomics
— documented in README. Hooks do not appear in `/` autocomplete or `/help`.

### Command: `/threadhop:peek` (zero LLM)

Scoped borrowing — look at part of another session without paying for
compression. `--grep` returns whole exchanges (ADR-030), not the thread.

```
User (in any chat): !threadhop peek api-contracts --grep "rate limiting"

1. CLI resolves the session (name or id) and finds matching exchanges
2. Prints them verbatim — tool results, sidechains, and system-reminders
   stripped; source labels (session, project, timestamp) included
3. The output lands in the conversation; the model can work with it
```

### Command: `/threadhop:prepare` (the one LLM call)

```
User (in chat A): !threadhop prepare

1. Auto-detects the current session; splits the transcript into head +
   last 3 exchanges (tail), per ADR-032
2. transfer_state cache: only new bytes since the last prepare are
   summarized, merged with the cached summary in the same call (ADR-033)
3. ONE `claude -p --model haiku` via harness `run_claude_p` with
   `prompts/prepare.md`
4. Writes ~/.config/threadhop/transfers/tk_<8hex>.md
5. Prints: Paste in the target chat: !threadhop receive tk_<8hex>
```

### Command: `/threadhop:receive` (zero LLM)

```
User (in chat B — Claude Code, Codex, any tool with a shell):
  !threadhop receive tk_3f9a1c2e

1. Reads the frozen ticket file
2. Prints it verbatim — narrative head summary + verbatim tail with
   source labels
3. The target model now has the transfer context
```

### Context flows (TUI clipboard, prepare → receive)

Flow 1 — visual grab (TUI → clipboard → `/threadhop:copy`):

```
1. Open ThreadHop TUI
2. Navigate to source session, view transcript
3. Enter message select mode (m)
4. Select messages visually (j/k to move, v for range)
5. Press y → copied to clipboard with source labels
6. Switch to the target chat
7. /threadhop:copy → clipboard content formatted and injected
```

Flow 2 — continuation transfer (prepare → receive, ADR-032):

```
Chat A:  !threadhop prepare
         → one Haiku call → ~/.config/threadhop/transfers/tk_3f9a1c2e.md
         → prints: Paste in the target chat: !threadhop receive tk_3f9a1c2e

Chat B:  !threadhop receive tk_3f9a1c2e
         → ticket printed verbatim: summary head + verbatim tail. Zero LLM.
```

For larger visual exports:
```
5. Press e → exported to /tmp/threadhop/<id>-<ts>.md
6. In the target chat: "Read /tmp/threadhop/..."
```
---

## TODO

### Immediate (Phase 1)
- [ ] Create SQLite DB module (init, migrate, query helpers)
- [ ] Migrate config.json → SQLite (one-time, on first run)
- [ ] Add session status field + grouped display
- [ ] Implement status cycling keybinds (`s`/`S`)
- [ ] Implement archive (`a`) + archive toggle (`A`)
- [ ] Implement sidebar resize (`[`/`]`)
- [ ] Write tests for DB migration

### Next (Phase 2)
- [ ] Build JSONL indexer with chunk merging
- [ ] Implement incremental indexing (byte offset tracking)
- [ ] Add message selection mode to TUI (`m` to enter, `j`/`k` between messages)
- [ ] Add range selection (`v` + movement)
- [ ] Clipboard copy with source labels (`y`)
- [ ] Temp file export (`e`) → `/tmp/threadhop/`
- [ ] Real-time search panel (`/`) with FTS5 prefix matching
- [ ] Per-keystroke result updates in search
- [ ] Jump-to-source from search results (`Enter`)
- [ ] Search filter syntax: `project:`, `user:`, `assistant:`
- [ ] Context-aware help overlay + shared command metadata registry
- [ ] Exchange grouping in the indexer (ADR-030, Q8)
- [ ] `threadhop search <query> [--project] [--json]` CLI subcommand

### Phase 3: Borrow CLI — peek / search / prepare / receive (ADR-030–ADR-033)
- [ ] Add argparse subcommand routing (no subcommand = TUI)
- [ ] Implement `threadhop tag <status> [--session <id>]`
- [ ] Session auto-detection from current terminal (ps/lsof)
- [ ] Exchange parser (ADR-030)
- [ ] `threadhop peek <session> [--last N] [--range A:B] [--grep X]` — exchange-bounded, stripped, source-labelled
- [ ] `threadhop search <query> [--project] [--json]` CLI over the FTS index
- [ ] `threadhop prepare` — head/tail split, ONE `claude -p` via harness `run_claude_p` (ADR-032)
- [ ] `prompts/prepare.md` prompt template
- [ ] `transfer_state` caching: new-bytes-only summarization + cached-summary merge (ADR-033)
- [ ] `threadhop receive <ticket>` — verbatim print, zero LLM
- [ ] Transfers directory: `~/.config/threadhop/transfers/`
- [ ] Migration: drop `observation_state` (ADR-033)

### Phase 4: Plugin refresh (ADR-029, ADR-031, ADR-032)
- [ ] Six commands, zero skills: `tag`, `bookmark`, `copy`, `peek`, `prepare`, `receive` — all `!threadhop …` pre-executed passthroughs
- [ ] Remove the `handoff` skill and `observe` command from the plugin
- [ ] `argument-hint` frontmatter for all six commands
- [ ] README: borrow spectrum + prepare → receive flow

### Phase 5: Memory + Bookmarks
- [ ] Build bookmark system (shared ingest primitive + TUI/browser surfaces)
- [ ] Explicit annotation detection (ADR:, DECISION:, TODO: markers) — writes directly to the `memory` table
- [ ] Project memory markdown rendering from the `memory` table

---

## Open Questions

> These need resolution before or during implementation.

### Q1: Custom status tags or fixed set?
Current design uses a fixed set: `active | in_progress | in_review | done | archived`.
Should users be able to define custom tags? Custom tags add flexibility but
complicate the UI (keybind cycling, group headers, color coding).
**Leaning:** Fixed set for v1. Custom tags as a later enhancement.

### Q2: Project memory — per-project or per-feature?
We discussed both. Per-project is simpler (project name is already in session
metadata). Per-feature requires explicit tagging of sessions to features.
**Leaning:** Per-project for v1. Feature concept layered on top later — a feature
is essentially a tag that groups sessions and memory entries across projects.

### Q3: Observer trigger — when does auto-observation run?
**Superseded by ADR-029 (2026-08-27).** The observer is removed — there is
no auto-observation to trigger. Body removed — see git history.

### Q4: Skill plugin packaging — **RESOLVED (2026-04-19)**
See [`skill-packaging.md`](skill-packaging.md) for the full writeup.
Summary: ThreadHop ships as a **plugin** — a directory with
`.claude-plugin/plugin.json` — containing **slash commands** under
`commands/<name>.md`, not "skills". Each command body is a single
`` !`${CLAUDE_PLUGIN_ROOT}/bin/threadhop <subcommand> $ARGUMENTS` `` line
that the harness pre-executes before Claude sees the prompt, so the CLI
does the work and the model just relays stdout. Distribution is via
`/plugin marketplace add <git-url>` + `/plugin install threadhop`, or
`--plugin-dir <path>` during development. Boilerplate lives under
[`plugin/`](../plugin) in this repo.

### Q5: FTS indexing — index tool results or not?
Current design: index only user + assistant text. Tool results are huge (file
contents, command output) and would dominate search with noise.
**Leaning:** Skip tool results for v1. Add opt-in tool result indexing if users
find themselves wanting to search "what was the output of that command."

### Q6: Handoff sub-agent model
**Superseded by ADR-029 (2026-08-27).** Observer-backed handoff is removed;
the one LLM call is `threadhop prepare` (default Haiku, `--model` flag —
ADR-032). Body removed — see git history.

### Q7: Push/mailbox — should `threadhop send` exist?
Should `threadhop send <session> "note"` exist — push a note into another
session's mailbox, with pull-based delivery into the target chat — or is
push out of scope? prepare/receive covers continuation and peek covers
lookup; a mailbox adds a delivery-state machine for an unproven need.
**Leaning:** out of scope until a concrete workflow demands it.

### Q8: Materialize exchange_id or derive at parse time?
Should `exchange_id` be stamped into the `messages` table at index time,
or always derived structurally at parse time (ADR-030)? Stamping makes
exchange-grouped search results a plain GROUP BY; deriving keeps the
schema smaller and leaves the parser as the single source of truth.
**Leaning:** derive for v1; stamp when FTS result grouping demonstrably
needs it.

---

## What We Learned

### JSONL message structure is richer than expected
Every message line has native `uuid`, `parentUuid`, `sessionId`, `timestamp`,
`cwd`, `isSidechain`. No synthetic IDs needed for the index. Assistant messages
come as multiple lines sharing the same `message.id` but unique `uuid`s — these
must be merged for search.

### The 36-char sidebar is hardcoded in CSS
`grid-columns: 36 1fr` at line 408. Textual supports dynamic CSS manipulation
via `self.styles.grid.columns`, so resize keybindings are straightforward.

### Message widgets are non-interactive
`UserMessage`, `AssistantMessage`, `ToolMessage` (lines 159-171) are bare
`Static` subclasses. Adding message selection requires either:
- Making them focusable/highlightable (Textual supports this via CSS classes)
- Or overlaying a selection cursor that tracks position independently

### config.json is the only persistence
No database, no cache. Everything is in-memory during runtime, persisted to a
single JSON file. Migration to SQLite is a clean cut — one-time import, then
JSON for app settings only.

### The app has 26 keybindings already
Modal (focus-aware). Available keys for new features: `m`, `s`, `a`, `b`, `v`,
`y`, `[`, `]`, `f`, `p`, `1-4`, `space`. Sufficient for everything planned.
