# ThreadHop

Browse coding-agent sessions and carry useful context between them.

The working Python application is preserved in [`legacy/`](legacy/README.md).
The next implementation will start with a small TypeScript command-line tool.
There is no TypeScript implementation yet.

## Repository map

| Path | Purpose |
| --- | --- |
| `legacy/` | Python application, tests, Claude plugin, prompts, and historical documentation |
| `docs/research/` | Transcript-format research; observations, not a mandated architecture |
| `.github/workflows-disabled/` | Parked validation and release workflows; CI and publishing are disabled in this branch |
| `.claude-plugin/marketplace.json` | Marketplace discovery pointing to `legacy/plugin` |
| `threadhop` | Compatibility symlink to `legacy/threadhop` |
| `install.sh` | Existing public installer; continues installing the Python application |

Keep one TypeScript package at the repository root when implementation begins.
Add source and tests as they become useful; no empty server, UI, or plugin
packages are needed for the first command.

## Run the Python application

```bash
./threadhop
./legacy/threadhop peek <session> --last 1
```

The launcher uses `uv` and its inline dependencies. Existing installed symlinks
to the repository's `threadhop` path continue to resolve to the Python app.
The application's existing user data location is unchanged.

Run the legacy tests with:

```bash
uv run --with pytest --with rich --with textual --with watchdog --with pydantic \
  python -m pytest -q legacy/tests
```

## Next milestone

Build a TypeScript CLI that reads an explicitly supplied Claude Code transcript
and prints its last main-agent turn. A turn starts with a human prompt and
continues through its associated last assistant response; tool cycles do not
start new turns.

Claude, Codex, OpenCode, and Cursor remain the intended source scope. Add the
second source after the first command works. Pi, the new UI, a persistent
server, and a plugin framework are deferred.
