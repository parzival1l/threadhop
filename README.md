# ThreadHop

Browse coding-agent sessions and carry useful context between them.

The working Python application is preserved in [`legacy/`](legacy/README.md).
The TypeScript successor currently contains project tooling, a tested
session-label helper, validated conversation values, and read-only Claude
transcript parsing. The CLI is the next learning step.

## Repository map

| Path | Purpose |
| --- | --- |
| `legacy/` | Python application, tests, Claude plugin, prompts, and historical documentation |
| `src/` | TypeScript source |
| `tests/` | TypeScript tests |
| `docs/research/` | Transcript-format research; observations, not a mandated architecture |
| `.github/workflows-disabled/` | Parked validation and release workflows; CI and publishing are disabled in this branch |
| `.claude-plugin/marketplace.json` | Marketplace discovery pointing to `legacy/plugin` |
| `threadhop` | Compatibility symlink to `legacy/threadhop` |
| `install.sh` | Existing public installer; continues installing the Python application |

## Work on TypeScript

Use Node.js 22.12+ on the 22.x line, 24.x, or 26+ and npm.

```bash
npm ci                 # Install the exact dependencies in package-lock.json
npm run typecheck      # Check types without executing code or emitting files
npm test               # Run the TypeScript tests once
npm run test:watch     # Re-run tests as you edit
npm run check          # Typecheck, then run tests
```

This is one private ESM package. TypeScript checks only `src/`, `tests/`, and
the Vitest configuration. Vitest executes tests in Node and does not replace
typechecking. Effect 3 is pinned to the stable release line; Effect Schema is
included in that dependency. Pure helpers do not need an Effect wrapper.

Start by reading `src/session-label.ts` and `tests/session-label.test.ts`:
an optional title is trimmed, with the session ID as the fallback. Local imports
use `.js` extensions to match Node ESM conventions; TypeScript and Vitest resolve
them to the corresponding `.ts` source files during development. No build output
is produced yet.

`src/conversation.ts` defines Effect schemas for a session reference, user and
assistant messages, and a turn. `tests/conversation.test.ts` contains a small
sample and accepted/rejected inputs. These are text-facing values for peek;
native records are handled separately by `src/claude-transcript.ts`. That adapter
groups main-agent text into turns, excludes tool results and subagents, and
returns diagnostics for skipped records. `src/turns.ts` selects and renders turns.

`skipLibCheck` skips checking dependency declaration files: Vitest's benchmark
dependency references a browser type. Our source and tests remain strictly
checked without adding browser globals to this Node project.

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
