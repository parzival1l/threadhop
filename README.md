# ThreadHop

Browse coding-agent sessions and carry useful context between them.

The working Python application is preserved in [`legacy/`](legacy/README.md).
The TypeScript successor currently contains project tooling, a tested
session-label helper, validated conversation values, and a read-only Claude
transcript peek CLI.

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
npm run build          # Compile src/ into ignored dist/
```

This is one private ESM package. TypeScript checks only `src/`, `tests/`, and
the Vitest configuration. Vitest executes tests in Node and does not replace
typechecking. Effect 3 is pinned to the stable release line; Effect Schema is
included in that dependency. Pure helpers do not need an Effect wrapper.

Start by reading `src/session-label.ts` and `tests/session-label.test.ts`:
an optional title is trimmed, with the session ID as the fallback. Local imports
use `.js` extensions to match Node ESM conventions; TypeScript and Vitest resolve
them to the corresponding `.ts` source files during development. Typechecking
does not emit files; the build command emits JavaScript into `dist/`. CLI tests
rebuild that output on each suite run, including watch mode.

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

## Try TypeScript peek

```bash
npm run --silent cli -- peek tests/fixtures/claude-session.jsonl --last 1
npm run --silent cli -- peek /path/to/main-session.jsonl --last 3
```

Peek reads an explicitly supplied Claude Code transcript. A turn starts with a
human prompt and continues through its associated last assistant response; tool
cycles do not start new turns. The default is one turn. Tool output, injected
skill/command text, and marked subagent records are omitted. A prompt waiting
for a reply is printed as-is.
An image/document-only prompt is shown as an attachment placeholder so it still
starts its own turn; attachment contents are not rendered.

The executable writes conversation text to stdout and skipped-record diagnostics
to stderr. Exit codes: `0` for success/help, `1` for unreadable files or no
main-agent turns, `2` for invalid command arguments. Use `--help` for usage.

This first reader loads one file into memory. It does not discover sessions or
reconstruct conversation branches. Leading assistant-only output is skipped
with a diagnostic because it has no human prompt to start a turn. Use a main
session transcript, not a subagent transcript or another provider's export.

Claude, Codex, OpenCode, and Cursor remain the intended source scope. Only Claude
is implemented in this CLI so far. Pi, the new UI, a persistent server, and a
plugin framework are deferred.
