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

| Key | Action |
|-----|--------|
| `j`/`k` | Navigate sessions |
| `h`/`l` | Focus session list / transcript |
| `Enter` | Reply / Send message |
| `Escape` | Cancel reply |
| `g` | Copy session ID to clipboard |
| `n` | Rename session |
| `J`/`K` | Reorder sessions |
| `t`/`T` | Cycle themes |
| `r` | Refresh |
| `q` | Quit |

## Roadmap

- Cross-session search (SQLite FTS5, per-keystroke)
- Message selection, copy, and export with source labels
- Session tags — in progress / in review / done / archived
- Project memory — append-only ledger of decisions, TODOs, ADRs
- Handoff generation — compress a session into a brief via `/threadhop:handoff`
- Codex support

See [docs/DESIGN-DECISIONS.md](docs/DESIGN-DECISIONS.md) for the full architecture.

## Docs

- [Origin & Attribution](docs/ORIGIN.md)

## License

MIT. Originally forked from [thomasrice/claude-sessions](https://github.com/thomasrice/claude-sessions).
