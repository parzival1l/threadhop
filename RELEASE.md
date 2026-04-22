# Release Discipline

ThreadHop ships as a git checkout. `threadhop update` and the 24-hour
startup version check both compare `__version__` in the `threadhop`
script against the latest GitHub **Release** (not a bare tag — see note
below) on the repo. Without a matching Release, the check silently
no-ops. Keep Releases and `__version__` in lockstep so users actually
see the nudge.

## Automated flow (default)

Releases are cut automatically by `.github/workflows/release.yml`
whenever `__version__` changes on `main`. You don't run `git tag` or
`gh release create` by hand — the workflow does both, using the
matching `CHANGELOG.md` section as the release body.

A regular release PR looks like this:

1. Branch from `main`:
   ```bash
   git checkout main && git pull
   git checkout -b release/0.3.0
   ```
2. Bump the three version fields together:
   - `__version__` in `threadhop`
   - `.plugins[0].version` in `.claude-plugin/marketplace.json`
   - `.version` in `plugin/.claude-plugin/plugin.json`
3. Add a new `## [0.3.0] — YYYY-MM-DD` section at the top of
   `CHANGELOG.md` (Keep-a-Changelog style — Added / Changed /
   Fixed / Removed). This becomes the Release body verbatim.
4. Commit and open a PR to `main`:
   ```bash
   git add -u CHANGELOG.md
   git commit -m "Bump to 0.3.0"
   gh pr create --base main --title "Release 0.3.0"
   ```
5. Merge once `validate` is green. The `release` workflow fires on
   push-to-main, detects the version change, refuses if `v0.3.0`
   already exists, extracts the CHANGELOG section, and creates the
   tag + GitHub Release.

That's it. No manual tag step. No forgotten GitHub Release creation.

### What `validate` catches on each PR

`.github/workflows/validate.yml` provides the `validate` status check
that branch protection requires before any PR can merge to `main`:

- Python syntax for `threadhop` and `tui.py`
- Version parity — `threadhop` / `marketplace.json` / `plugin.json`
  must all report the same version
- `CHANGELOG.md` must have a `## [X.Y.Z]` entry matching the current
  `__version__`
- `pytest` runs if `tests/` has content

Failure on any one of these blocks the merge.

### What `release` catches on push to main

- Duplicate tag — refuses to overwrite an existing `vX.Y.Z`
- Missing CHANGELOG section — refuses to cut a release without
  notes for that version

Both failures surface as a red workflow badge on the commit; they do
not re-introduce the bad state on main (the merge already happened),
but they make the release visibly broken so you notice.

## When to bump which component

| Change | Bump |
|---|---|
| New subcommand, new TUI surface, new skill | minor (`0.2.0 → 0.3.0`) |
| Bug fix that users would notice | patch |
| Breaking change to the DB schema or CLI grammar (pre-1.0) | minor |

Pre-release suffixes (`-rc1`, etc.) are not wired into `_parse_version`,
so don't tag them — the startup check will treat the tag as malformed
and silently fall back. Widen the parser first if pre-releases become
useful later.

## Manual release (fallback)

If the workflow breaks or you need to release offline, the manual
steps are:

```bash
# 1-4 as above (bump versions, update CHANGELOG, merge PR).
# Then, manually:
git fetch origin main
git tag v0.3.0 origin/main
git push origin v0.3.0

# Critical: create the Release, not just the tag. /releases/latest
# serves only Releases, so a bare tag is invisible to the update check.
gh release create v0.3.0 \
  --title "v0.3.0" \
  --notes "$(awk '/## \[0.3.0\]/,/^## \[0\./{if(!/^## \[0\.[^3]/)print}' CHANGELOG.md)"
```

## Tag vs Release — why both matter

A **git tag** is a ref in the repo (`git tag`). A **GitHub Release** is
a higher-level object GitHub attaches to a tag (release notes, optional
assets, a timestamp). The two are independent — creating a tag does
**not** auto-create a Release.

The startup check calls `/repos/:owner/:repo/releases/latest`, which
only returns Release objects. A bare tag returns 404 at that endpoint.
The automation in `release.yml` creates both in one step
(`gh release create vX.Y.Z …` behind the scenes creates the tag if it
doesn't exist and attaches the Release), so the distinction is
invisible in the default flow. It only matters when debugging or doing
a manual release.

## Plugin vs CLI lifecycle

The Claude Code plugin is distributed via `/plugin` and has its own
update channel owned by Claude Code. ThreadHop's CLI does not push
plugin updates. The plugin `version` field is bumped here so a user
inspecting the manifest sees a version that matches the CLI they just
updated — they still have to run `/plugin update threadhop` inside a
`claude` session to pull the new skill prompts.
