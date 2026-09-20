# ThreadHop plugin

Claude Code plugin that exposes the ThreadHop CLI as in-session entry
points. Six commands ship together under one namespace — all thin
`!`cmd`` wrappers, zero model-in-the-loop:

| Invocation | What it does |
|------------|--------------|
| `/threadhop:peek <session> [--last N \| --grep <pattern>]` | Prints cleaned verbatim exchanges from another session; zero LLM |
| `/threadhop:prepare [--session <id>] [--tail N]` | Builds a frozen transfer ticket (one Haiku call summarizes the head, last N exchanges verbatim); prints a paste-ready receive line |
| `/threadhop:receive <ticket-id>` | Prints a transfer ticket verbatim into the current chat; zero LLM |
| `/threadhop:tag <status>` | Tags the current session; argument-hint enumerates valid statuses |
| `/threadhop:bookmark [--note <text>]` | Bookmarks the latest message in the current session; optional free-text note; writes to the shared `bookmarks` SQLite table that the TUI also reads |
| `/threadhop:copy [N\|all]` | Copies the cleaned transcript (last turn / last N turns / whole session) to the clipboard as markdown |

Everything also remains available as bash passthrough — e.g.
`!threadhop tag <status>`, `!threadhop peek <session>` — with zero LLM
turn. The slash-command forms are the discoverable aliases — Claude
Code's `/` picker renders the argument-hint so users don't have to
memorise valid options.

## Transfer flow

Carrying context from one chat to another takes three steps:

1. In chat A, run `/threadhop:prepare` (or `!threadhop prepare`). It
   writes a frozen ticket to `~/.config/threadhop/transfers/` and prints
   a line like `Paste in the target chat: !threadhop receive tk_ab12cd`.
2. Copy that printed receive line.
3. In chat B, paste `!threadhop receive tk_ab12cd`. The ticket text is
   printed verbatim into the new chat — works in any tool with a shell,
   not just Claude Code.

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
├── .claude-plugin/plugin.json   # manifest: name=threadhop
└── commands/
    ├── peek.md                  # !`threadhop peek`
    ├── prepare.md               # !`threadhop prepare`
    ├── receive.md               # !`threadhop receive`
    ├── bookmark.md              # !`threadhop bookmark`
    ├── copy.md                  # !`threadhop copy`
    └── tag.md                   # !`threadhop tag` with discoverable argument-hint
```

The plugin ships commands only — no skills.

## Local install for development

```bash
claude --plugin-dir "$(pwd)/plugin"
# then, inside the session:
/threadhop:tag in_progress
/threadhop:bookmark --note "this answer is worth remembering"
/threadhop:peek <some_other_session_id> --last 3
/threadhop:prepare
```

## Publishing

Add a `.claude-plugin/marketplace.json` at the repo root declaring
this plugin, push to a public Git remote, and users can
`/plugin marketplace add github:parzival1l/threadhop` followed by
`/plugin install threadhop`.
