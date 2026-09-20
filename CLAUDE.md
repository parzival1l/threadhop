# ThreadHop repository guidance

The repository is transitioning from Python to TypeScript incrementally.

- `legacy/` contains the runnable Python application, its tests, plugin, and
  historical design/release documents. Read `legacy/CLAUDE.md` for that code;
  its paths are relative to `legacy/` unless stated otherwise.
- The first TypeScript goal is a file-based CLI that prints the last main-agent turn.
- `docs/research/` collects evidence. It does not mandate a shared model.
- TypeScript code is not scaffolded yet. Start with one root package when
  implementing the first command; avoid speculative packages and frameworks.
- Keep the existing Python app usable while developing its successor. Root
  launch/install and marketplace discovery paths are compatibility surfaces.
- CI and publishing are disabled; workflows are parked in `.github/workflows-disabled/`. Do not re-enable them without an explicit request. Python tests live in `legacy/tests/`.
- Do not alter real transcript files or the user's application database in
  tests. Use small sanitized fixtures and temporary paths.
- Preserve the user's agreed scope: main-agent peek/bookmarks, turns counted
  from human prompt through last assistant response, and no Pi support yet.
