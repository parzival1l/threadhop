# ThreadHop repository guidance

The repository is transitioning from Python to TypeScript incrementally.

- Use `AGENTS.md` for repository and directory-level agent instructions
  everywhere in this repository. Do not create `CLAUDE.md` instruction files.
- `legacy/` contains the runnable Python application, its tests, plugin, and
  historical design/release documents. Read `legacy/AGENTS.md` for that code;
  its paths are relative to `legacy/` unless stated otherwise.
- The first TypeScript goal is a file-based CLI that prints the last main-agent turn.
- `docs/research/` collects evidence. It does not mandate a shared model.
- One root TypeScript ESM package is configured with strict typechecking,
  Vitest, and stable Effect 3 (including Effect Schema). `src/` holds source and
  `tests/` holds TypeScript tests. Use `npm ci` and `npm run check`.
- The first helper is `src/session-label.ts`; parsing and the CLI are not
  implemented yet. Avoid speculative packages and frameworks.
- `src/conversation.ts` defines the initial text-facing session/message/turn
  schemas for peek. Native transcript decoding is a later lesson; do not treat
  these small values as a lossless schema for every provider's data.
- Keep the existing Python app usable while developing its successor. Root
  launch/install and marketplace discovery paths are compatibility surfaces.
- CI and publishing are disabled; workflows are parked in `.github/workflows-disabled/`. Do not re-enable them without an explicit request. Python tests live in `legacy/tests/`.
- Do not alter real transcript files or the user's application database in
  tests. Use small sanitized fixtures and temporary paths.
- Preserve the user's agreed scope: main-agent peek/bookmarks, turns counted
  from human prompt through last assistant response, and no Pi support yet.
