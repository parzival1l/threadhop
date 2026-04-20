# ThreadHop plugin

Claude Code plugin that exposes the ThreadHop CLI as in-session entry
points. Three things ship together under one namespace:

| Invocation | Shape | What it does |
|------------|-------|--------------|
| `/threadhop:handoff <session_id> [--full]` | Skill (model-in-the-loop) | Runs `threadhop handoff`, frames the brief, suppresses auto-action on TODOs |
| `/threadhop:observe` | Command (thin `!`cmd`` wrapper) | Starts the observer for the current Claude Code session; lifetime bound to that session |
| `/threadhop:tag <status>` | Command (thin `!`cmd`` wrapper) | Tags the current session; argument-hint enumerates valid statuses |
| `/threadhop:bookmark [--note <text>]` | Command (thin `!`cmd`` wrapper) | Bookmarks the latest message in the current session; optional free-text note; writes to the shared `bookmarks` SQLite table that the TUI also reads |

Tagging and bookmarking also remain available as `!threadhop tag <status>`
and `!threadhop bookmark` (bash passthrough, zero LLM turn). The
slash-command forms are the discoverable aliases — Claude Code's `/`
picker renders the argument-hint so users don't have to memorise valid
options (statuses for tag; the `--note` flag for bookmark).

## Bookmark targeting — where the note goes

`/threadhop:bookmark` calls bare `threadhop bookmark` and inherits its
behaviour exactly:

- **Session**: auto-detected by walking the parent process tree for the
  `claude` ancestor (same mechanism as `threadhop tag`).
- **Message**: defaults to the latest indexed message in that session.
- **Note**: stored in `bookmarks.note` in `~/.config/threadhop/sessions.db`.
  Blank/whitespace-only notes collapse to `NULL` via
  `db._normalize_bookmark_note`.
- **Idempotency**: the `bookmarks` table has `UNIQUE(message_uuid)`, so
  bookmarking the same message twice updates the existing row rather
  than creating a duplicate.

The TUI (selection-mode `Space`/`L`), the CLI (`threadhop bookmark …`),
the bash passthrough (`!threadhop bookmark`), and this plugin command
all write through the same `db.upsert_bookmark` primitive. One `bookmarks`
table, one source of truth.

## Dependency: the ThreadHop CLI must be on PATH

The plugin is **not** self-contained. It calls bare `threadhop`, which
must resolve from the user's `$PATH`. Install the app separately:

```bash
# via pipx (recommended once on PyPI)
pipx install threadhop

# via repo clone + PATH (for development)
git clone https://github.com/parzival1l/threadhop
export PATH="$(pwd)/threadhop:$PATH"

# verify
threadhop --help
```

Decoupling the plugin from the app means the app can repackage
(pipx → brew → uv tool → …) without re-releasing the plugin.

## Layout

```
plugin/
├── .claude-plugin/plugin.json   # manifest: name=threadhop, version=0.1.0
├── skills/
│   └── handoff/
│       └── SKILL.md             # real skill — rich instructions, model frames the brief
└── commands/
    ├── bookmark.md              # !`threadhop bookmark`
    ├── observe.md               # !`threadhop observe`
    └── tag.md                   # !`threadhop tag` with discoverable argument-hint
```

## What is intentionally NOT shipped

- **`/threadhop:insights`** — would surface captured observations back
  into the Claude Code session that captured them, re-introducing the
  facts the observer was distilling out of that context. Observations
  are for *other* sessions (via `/threadhop:handoff`) or for the user
  to review outside the session (TUI, `threadhop observations <id>`
  CLI). There is no in-session viewer.
- **`/threadhop:context`** — clipboard-to-markdown wrapping is not
  worth a plugin command.
- **`/threadhop:observe --stop` / `--stop-all`** — the observer is
  bound to the lifetime of the Claude Code session that started it. No
  in-session stop command is needed. `threadhop observe --stop-all`
  remains available on the CLI for orphan cleanup from a terminal.

## Observer lifecycle

1. User runs `/threadhop:observe` inside a Claude Code session.
2. `threadhop observe` detects the Claude Code session ID and starts
   the sidecar in watch-mode.
3. The observer stays bound to that Claude Code session's process.
   When the Claude Code session ends, the observer ends.
4. Captured content lives at
   `~/.config/threadhop/observations/<session_id>.jsonl`. Consume it
   via the TUI, the `threadhop observations|decisions|todos|conflicts`
   CLI subcommands, or from a different Claude Code session via
   `/threadhop:handoff <session_id>`.

## Local install for development

```bash
claude --plugin-dir "$(pwd)/plugin"
# then, inside the session:
/threadhop:tag in_progress
/threadhop:bookmark --note "this answer is worth remembering"
/threadhop:observe
/threadhop:handoff <some_other_session_id>
```

## Publishing

Add a `.claude-plugin/marketplace.json` at the repo root declaring
this plugin, push to a public Git remote, and users can
`/plugin marketplace add github:parzival1l/threadhop` followed by
`/plugin install threadhop`.
