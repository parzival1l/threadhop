# ThreadHop

Persistent, searchable, cross-session memory for Claude Code — a TUI, a CLI, and a Claude Code plugin that share one SQLite store.

![ThreadHop](assets/demo.png)

Each Claude Code session ships as an isolated JSONL transcript. ThreadHop indexes them into SQLite with FTS5, runs a Haiku-powered sidecar that extracts TODOs and decisions per session, a reflector that surfaces decision conflicts across sibling sessions, and a handoff skill that compresses a session into a brief. The TUI is the main browser; most day-to-day capture happens from inside the Claude Code chat via `!threadhop …` bash passthrough or the `/threadhop:*` plugin commands.

### What's in the box

- **TUI** — two-column browser over `~/.claude/projects/**/*.jsonl`, with FTS search, bookmarks, status tags, message-range selection, AI-generated session titles.
- **CLI** — `threadhop tag / bookmark / todos / decisions / observations / conflicts / observe`, all auto-detecting the current session from the parent process tree so they work inside a live `claude` chat.
- **Observer + reflector** — background extractors that append structured JSONL per session and cross-compare decisions across sessions in a project (ADR-018 – ADR-020).
- **Claude Code plugin** (`plugin/`) — `/threadhop:handoff` skill plus `/threadhop:observe`, `/threadhop:tag`, `/threadhop:bookmark` commands under the `/threadhop:` namespace.

## Install

macOS only. The installer handles everything — including installing [uv](https://github.com/astral-sh/uv) if you don't have it.

### Quick install (recommended)

```bash
curl -LsSf https://raw.githubusercontent.com/parzival1l/threadhop/main/install.sh | bash
```

This clones the repo to `~/.local/share/threadhop`, installs `uv` if missing, and symlinks `threadhop` into `~/.local/bin`. Re-run the same command any time to update.

### Manual install

If you'd rather not pipe curl to bash, the same three steps by hand:

```bash
# 1. Install uv (skip if you already have it)
brew install uv    # or: curl -LsSf https://astral.sh/uv/install.sh | sh

# 2. Clone and link
git clone https://github.com/parzival1l/threadhop.git ~/.local/share/threadhop
mkdir -p ~/.local/bin
ln -s ~/.local/share/threadhop/threadhop ~/.local/bin/threadhop

# 3. Make sure ~/.local/bin is on your PATH (add to ~/.zshrc if not)
export PATH="$HOME/.local/bin:$PATH"
```

Verify with `threadhop --version`.

### Claude Code integration

Once the CLI is installed, add the plugin to get the `/threadhop:*` slash commands and the handoff skill. From inside any `claude` session:

```
/plugin marketplace add parzival1l/threadhop
/plugin install threadhop@threadhop
```

That registers the four primitives — `/threadhop:handoff`, `/threadhop:tag`, `/threadhop:observe`, `/threadhop:bookmark` — persistently across all future sessions. The plugin is a thin wrapper over the CLI, so the `threadhop` command must already be on your `$PATH` (see above) for the slash commands to do anything.

**Skill-only alternative**: if you *only* want `/threadhop:handoff` and not the three commands, you can install just the skill via [Vercel's `skills` CLI](https://github.com/vercel-labs/skills):

```bash
npx skills add parzival1l/threadhop
```

**Dev / local-testing path**: if you've cloned the repo and want to load the plugin against a working-tree copy for one session, `claude --plugin-dir ~/.local/share/threadhop/plugin` loads it for that invocation only (no persistence).

## Usage

### TUI

```bash
threadhop                              # all sessions
threadhop --project myproject          # filter by project
threadhop --days 7                     # last 7 days only
```

### CLI

All subcommands accept `--project` and `--session`; without them they auto-detect the current session.

```bash
threadhop tag <status>            # backlog | in_progress | in_review | done | archived
threadhop bookmark [kind]         # bookmark | research — against the latest indexed message
threadhop todos                   # open TODOs extracted by the observer
threadhop decisions               # decisions extracted by the observer
threadhop observations            # raw observation JSONL, newest first
threadhop conflicts [--resolved]  # cross-session decision conflicts from the reflector
threadhop observe [--once|--stop|--stop-all] [--watch-backend auto|poll|fsevents]
```

## Keybindings

### Navigation

| Key | Action |
|-----|--------|
| `j` / `k` | Navigate sessions (in session list) |
| `h` / `l` | Focus session list / transcript |
| `left` / `right` | Focus session list / transcript |
| `PageUp` / `PageDn` | Scroll transcript |
| `Home` / `End` | Jump to top / bottom of transcript |

### Sessions

| Key | Action |
|-----|--------|
| `Enter` | Focus reply input (or send if already focused) |
| `/` | Focus reply input |
| `Alt+Enter` | Insert newline in reply |
| `Alt+j` / `Alt+k` | Navigate sessions while reply input is focused |
| `Escape` | Cancel reply / exit selection mode |
| `n` | Rename session |
| `g` | Copy `claude -r <id>` to clipboard |
| `J` / `K` | Reorder sessions (move up/down) |
| `Shift+Up` / `Shift+Down` | Reorder sessions (move up/down) |
| `s` / `S` | Cycle session status forward / backward |
| `a` | Toggle archive on selected session |
| `A` | Show / hide archived sessions |

### Message Selection (focus transcript first with `l`)

| Key | Action |
|-----|--------|
| `m` | Enter / exit selection mode (starts at last message) |
| `j` / `k` or `Down` / `Up` | Move selection between messages |
| `v` | Start / cancel range selection (anchor + extend) |
| `Escape` | Exit selection mode |

### Display

| Key | Action |
|-----|--------|
| `t` / `T` | Cycle theme forward / backward |
| `[` / `]` | Shrink / grow sidebar |
| `r` | Refresh session list |
| `q` | Quit |

## Tagging sessions from inside Claude Code

Use Claude Code's `!` bash passthrough to tag the current session without leaving the chat. The `!` prefix runs the command in the host shell — no LLM turn, instantaneous — and `threadhop tag` auto-detects the session by walking the parent process tree for its `claude` ancestor.

```
!threadhop tag in_review
```

Output:

```
✓ tagged 8f3b2a1c as in_review
```

Valid statuses: `active`, `in_progress`, `in_review`, `done`, `archived`.

The TUI reflects the new status on its next refresh (5s). From another terminal tab, pass the id explicitly instead: `threadhop tag in_review --session <id>`.

If detection fails (e.g. running outside a Claude Code terminal), the command exits `2` with a helpful error and no DB write.

### Optional: `/tag` via a UserPromptSubmit hook

Prefer a slash-style trigger? A Claude Code `UserPromptSubmit` hook can intercept `/tag <status>`, shell out to `threadhop tag`, and block the prompt from reaching the model. Note: hooks are not surfaced in `/` autocomplete or `/help` — `!threadhop tag` remains the recommended, discoverable surface. The hook below is purely for users who want the slash ergonomics.

1. Drop this script at `~/.claude/hooks/threadhop-tag.sh` and `chmod +x` it:

    ```bash
    #!/usr/bin/env bash
    INPUT=$(cat)
    PROMPT=$(printf '%s' "$INPUT" | jq -r '.prompt')
    if [[ "$PROMPT" =~ ^/tag[[:space:]]+([A-Za-z_]+)[[:space:]]*$ ]]; then
      STATUS="${BASH_REMATCH[1]}"
      threadhop tag "$STATUS" >&2
      exit 2   # blocks the prompt — it never reaches the model
    fi
    exit 0
    ```

2. Register it in `~/.claude/settings.json`:

    ```json
    {
      "hooks": {
        "UserPromptSubmit": [
          {
            "matcher": "",
            "hooks": [
              { "type": "command", "command": "~/.claude/hooks/threadhop-tag.sh" }
            ]
          }
        ]
      }
    }
    ```

The hook reads the prompt from stdin as JSON, matches `/tag <status>`, invokes `threadhop tag`, and exits `2` — which tells Claude Code to block the submission and show the stderr output to the user.

## Bookmarking from inside Claude Code

Use the same `!` bash passthrough pattern to bookmark the current conversation without opening the TUI first.

General keep-for-later bookmark against the latest message in the current session:

```bash
!threadhop bookmark
```

Research follow-up bookmark with a short note:

```bash
!threadhop bookmark research --note "compare retry strategies later"
```

Output:

```text
✓ bookmarked kind=research session=8f3b2a1c-... message=6d2e... role=assistant text="We should compare retry strategies later." note="compare retry strategies later"
```

Targeting rules:

- Session auto-detect works the same way as `threadhop tag`: inside a live Claude Code terminal it walks the parent process tree for the current `claude` session.
- Without `--message`, ThreadHop bookmarks the latest indexed message in that session.
- If you need a specific message, pass `--session <id> --message <uuid>`.
- Built-in classes are intentionally narrow for now: `bookmark` and `research`.

The ingest path is shared and deterministic: chat commands use the same bookmark primitive that future TUI actions can call later.

### Optional: `/bookmark` and `/research` via a UserPromptSubmit hook

Prefer slash-style triggers? This hook blocks the prompt before it reaches the model and shells out to `threadhop bookmark`.

1. Drop this script at `~/.claude/hooks/threadhop-bookmark.sh` and `chmod +x` it:

    ```bash
    #!/usr/bin/env bash
    set -euo pipefail
    INPUT=$(cat)
    PROMPT=$(printf '%s' "$INPUT" | jq -r '.prompt')
    if [[ "$PROMPT" =~ ^/bookmark([[:space:]]+(.*))?$ ]]; then
      NOTE="${BASH_REMATCH[2]-}"
      if [[ -n "$NOTE" ]]; then
        threadhop bookmark --note "$NOTE" >&2
      else
        threadhop bookmark >&2
      fi
      exit 2
    fi
    if [[ "$PROMPT" =~ ^/research([[:space:]]+(.*))?$ ]]; then
      NOTE="${BASH_REMATCH[2]-}"
      if [[ -n "$NOTE" ]]; then
        threadhop bookmark research --note "$NOTE" >&2
      else
        threadhop bookmark research >&2
      fi
      exit 2
    fi
    exit 0
    ```

2. Register it in `~/.claude/settings.json` the same way as the `/tag` example above.

This gives you two low-friction chat-side buckets now, while keeping the app-side bookmark model ready for later generalized categories.

## Observer Lifecycle

Start the background observer for the current Claude Code session from inside
the chat:

```bash
!threadhop observe
```

Or target a specific session from another terminal:

```bash
threadhop observe --session <id> &
```

Stop and resume use the persisted `observation_state` row. The observer handles
`SIGTERM` gracefully: it flushes any pending tail, advances
`source_byte_offset`, marks the row `stopped`, and exits cleanly. A later
`threadhop observe` resumes from the recorded byte offset instead of re-reading
the full transcript.

```bash
threadhop observe --stop
threadhop observe --stop --session <id>
threadhop observe --stop-all
```

### Optional: hook-driven auto-start

If you want observer auto-start on every prompt for sessions where it is
enabled, first persist the flag:

```bash
threadhop config set observe.enabled true
```

Then register a lightweight `UserPromptSubmit` hook that launches
`threadhop observe` in the background. The command is safe to run repeatedly:
it auto-detects the current session and exits immediately when an observer is
already running for that session.

1. Drop this script at `~/.claude/hooks/threadhop-observe.sh` and `chmod +x` it:

    ```bash
    #!/usr/bin/env bash
    set -euo pipefail

    if [[ "$(threadhop config get observe.enabled 2>/dev/null)" == "true" ]]; then
      threadhop observe >/dev/null 2>&1 &
    fi
    ```

2. Register it in `~/.claude/settings.json`:

    ```json
    {
      "hooks": {
        "UserPromptSubmit": [
          {
            "hooks": [
              { "type": "command", "command": "~/.claude/hooks/threadhop-observe.sh" }
            ]
          }
        ]
      }
    }
    ```

## Shipped recently

- SQLite + FTS5 backend with assistant-chunk merging (ADR-003).
- Observer sidecar with poll / fsevents watch backends and byte-offset resume (ADR-018, ADR-019).
- Reflector cross-session conflict detection, with `conflict_reviews` for resolution state (ADR-020).
- `/threadhop:handoff` skill plus `/threadhop:observe`, `/threadhop:tag`, `/threadhop:bookmark` plugin commands.
- Chat-side `!threadhop bookmark` / `tag` / `observe` with parent-process session auto-detect.
- Message-range selection (`v`), status cycling, archive toggle, day-scale age display, AI-generated session titles.

## Roadmap

- Phase 5 release polish — `marketplace.json`, interactive install verification, discoverability for `threadhop tag` no-args.
- Broader bookmark categories beyond the current `bookmark` / `research` split.
- Codex session support (currently Claude Code only).

See [docs/DESIGN-DECISIONS.md](docs/DESIGN-DECISIONS.md) for the full architecture and [docs/TASKS.md](docs/TASKS.md) for open work.

## Docs

- [Origin & Attribution](docs/ORIGIN.md) — what ThreadHop inherited from [thomasrice/claude-sessions](https://github.com/thomasrice/claude-sessions) and what's new
- [Design Decisions](docs/DESIGN-DECISIONS.md) — ADRs, schema, phase plan
- [Skill Packaging](docs/skill-packaging.md) — how the Claude Code plugin is wired
- [Observational Memory](docs/observational-memory.md) — observer + reflector internals
- [Performance](docs/PERFORMANCE.md)
- [UI Improvements](docs/UI-IMPROVEMENTS.md)

## License

MIT. Originally forked from [thomasrice/claude-sessions](https://github.com/thomasrice/claude-sessions).
