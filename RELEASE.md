# Release Discipline

ThreadHop ships as a git checkout. `threadhop update` and the 24-hour
startup version check both compare `__version__` in the `threadhop`
script against the newest tag on the GitHub repo. Without a tag, the
check silently no-ops — not a false positive, just nothing at all. Keep
tags in lockstep with `__version__` so users see the nudge.

## Six-step release flow

Per [ADR-027](docs/DESIGN-DECISIONS.md#adr-027-update-lifecycle--threadhop-update-changelog-future-and-24h-startup-check):

1. Update `CHANGELOG.md` with a new `## [X.Y.Z] — YYYY-MM-DD` header and
   a bullet list of user-visible changes (Keep-a-Changelog style).
2. Bump `__version__` in `threadhop`.
3. Bump the `version` field in both
   `.claude-plugin/marketplace.json` and
   `plugin/.claude-plugin/plugin.json` to match.
4. Commit everything together so the version, changelog, and plugin
   manifests move as one unit.
5. Cut and push the tag:
   ```bash
   git tag v0.2.0
   git push --tags
   ```
6. (Optional later) Draft GitHub Release notes from the new CHANGELOG
   entry.

Steps 1–3 are reviewable in-PR. Step 5 is what arms the startup check;
don't skip it.

## When to bump which component

| Change | Bump |
|---|---|
| New subcommand, new TUI surface, new skill | minor (`0.1.0 → 0.2.0`) |
| Bug fix that users would notice | patch |
| Breaking change to the DB schema or CLI grammar (pre-1.0) | minor |

Pre-release suffixes (`-rc1`, etc.) are not wired into `_parse_version`,
so don't tag them — the startup check will treat the tag as malformed
and fall back silently. If pre-releases ever become useful, widen the
parser first.

## Plugin vs CLI lifecycle

The Claude Code plugin is distributed via `/plugin` and has its own
update channel owned by Claude Code. ThreadHop's CLI does not push
plugin updates. The plugin `version` field is bumped here so a user
inspecting the manifest sees a version that matches the CLI they just
updated — they still have to run `/plugin update threadhop` inside a
`claude` session to pull the new skill prompts.
