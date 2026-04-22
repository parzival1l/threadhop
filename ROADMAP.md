# ThreadHop Roadmap

What's coming up. Tracked as narrow deliverables; anything bigger lives
in `docs/DESIGN-DECISIONS.md` as an ADR. The top five entries also
surface via `threadhop future` — keep that subset meaningful.

Format contract (parsed by `threadhop future`):

```
- #NN — Short description line that renders well in CLI.
```

Lines that don't match the `- #NN — …` shape are ignored by the parser.
Headers and prose are free to evolve without breaking the CLI output.

## Next up

- #50 — Event-driven session discovery via fsevents (retire the 5s poll).
- #51 — Lazy tail-load transcript parsing for instant session focus.
- #42 — Context-aware help overlay with a shared command registry.
- #32 — Trigram-based fuzzy search for typo tolerance on top of FTS5.
- #38 — TUI conflict notifications when the reflector flags a contradiction.
- #52 — Row-window virtualisation of TranscriptView for very long sessions.
- #39 — Observation condensation: merge related decisions, archive done TODOs.
- #53 — Migrate data-heavy DataTables to textual-fastdatatable.

## Later

- Codex session support (currently Claude Code only).
- Broader bookmark categories beyond the `bookmark` / `research` split.
- GitHub Releases automation — auto-populate release notes from
  `CHANGELOG.md` on tag push.
