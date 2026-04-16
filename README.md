# ThreadHop

Cross-session context manager for Claude Code.

![ThreadHop](assets/demo.png)

A terminal app that turns isolated Claude Code sessions into a connected workspace — browse transcripts, search across sessions, and carry context between them.

## Install

Requires macOS and [uv](https://github.com/astral-sh/uv). That's it.

```bash
ln -s /path/to/threadhop/threadhop ~/bin/threadhop
```

## Usage

```bash
threadhop                              # all sessions
threadhop --project myproject          # filter by project
threadhop --days 7                     # last 7 days only
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

## Roadmap

- Cross-session search (SQLite FTS5, per-keystroke)
- Range selection (`v`), clipboard copy (`y`), and temp-file export (`e`) with source labels
- Project memory — append-only ledger of decisions, TODOs, ADRs
- Handoff generation — compress a session into a brief via `/threadhop:handoff`
- Codex support

See [docs/DESIGN-DECISIONS.md](docs/DESIGN-DECISIONS.md) for the full architecture.

## Docs

- [Origin & Attribution](docs/ORIGIN.md)

## License

MIT. Originally forked from [thomasrice/claude-sessions](https://github.com/thomasrice/claude-sessions).
