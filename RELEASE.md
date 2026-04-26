# Release Procedure

Authoritative runbook for cutting a ThreadHop release. Designed to be
agent-followable: every step has an exact command, every decision
point has an explicit rule, every failure mode has a documented
recovery path.

> **For AI agents working on a feature branch:** if your task says
> "ship this as a release," follow [the feature-release path](#feature-release-path-feature-work--version-bump-in-one-pr).
> Don't invent a release flow — this doc is the contract.

---

## 1. When to use this doc

Cut a release when **at least one** of these is true:

- A user-visible feature has landed and you want users on older
  versions to see the update nudge.
- A bug fix has landed that warrants telling existing users to update.
- You've been asked to release N versions of accumulated work.

Do **not** cut a release for:

- Internal refactors users won't notice (unless paired with a feature).
- Documentation-only changes (unless the docs are the release —
  e.g., a CHANGELOG correction worth re-publishing).
- CI / workflow / test infra changes (these merge to main without a
  version bump).

The principle: a release is **a public statement to existing users
that something changed for them**. If nothing changed for them, the
release is noise.

---

## 2. The release file surface

Three version fields and one CHANGELOG entry, all bumped together:

| File | What to update | Pattern |
|---|---|---|
| `threadhop_core/__init__.py` | `__version__` | `__version__ = "X.Y.Z"` |
| `.claude-plugin/marketplace.json` | `.plugins[0].version` | `"version": "X.Y.Z"` |
| `plugin/.claude-plugin/plugin.json` | `.version` | `"version": "X.Y.Z"` |
| `CHANGELOG.md` | New `## [X.Y.Z]` section at the top | Keep-a-Changelog format |

`validate.yml` enforces parity across all three version fields and
the presence of a matching `CHANGELOG.md` entry. Failing any one of
these blocks the merge. **There is no path that lands a release without
all four being correct.**

---

## 3. Pre-flight checklist

Run this before opening a release PR. Skipping any step risks a
red `validate` check.

```bash
# 1. Sync with main.
git fetch origin
git checkout main && git pull --ff-only

# 2. Confirm working tree is clean.
git status   # should report "nothing to commit, working tree clean"

# 3. Read the current version and decide the next one.
grep '^__version__' threadhop_core/__init__.py
# Decide based on the table in §4 below.

# 4. List commits on main since the last released tag — these are
#    what your CHANGELOG entry will summarize.
LAST_TAG=$(git describe --tags --abbrev=0 origin/main)
git log --oneline "${LAST_TAG}..origin/main"
```

If the last command shows zero commits, **stop**: there's nothing to
release. If it shows only doc/CI commits per §1's exclusion list,
**stop**: a version bump would be noise.

---

## 4. Choosing the next version

| Change shape | Bump |
|---|---|
| New subcommand, new TUI surface, new skill, new schema field | **minor** (`0.2.5 → 0.3.0`) |
| Bug fix that users would notice | **patch** (`0.2.5 → 0.2.6`) |
| Breaking change to DB schema or CLI grammar (pre-1.0) | **minor** |
| Multiple of the above bundled | use the highest tier |

**Pre-release suffixes (`-rc1`, `-alpha`, etc.) are not supported.**
The `_parse_version` helper in `threadhop_core/config/update_check.py`
does integer-only tuple comparison and will silently fail on suffixed
tags. Widen the parser before tagging anything that isn't pure semver.

---

## 5. Standard release flow (release-only PR)

Use this when the feature(s) being released **are already merged to
main**. Your PR contains only the version bump + CHANGELOG entry.

### Step 1 — Branch

```bash
git checkout -b release/X.Y.Z main
```

### Step 2 — Bump versions

```bash
# Open the three files and replace the version string.
# Suggested editor commands (adjust for your editor):
$EDITOR threadhop_core/__init__.py        # __version__ = "X.Y.Z"
$EDITOR .claude-plugin/marketplace.json   # "version": "X.Y.Z"
$EDITOR plugin/.claude-plugin/plugin.json # "version": "X.Y.Z"

# Verify all three match before going further:
grep '^__version__' threadhop_core/__init__.py
python3 -c "import json; print(json.load(open('.claude-plugin/marketplace.json'))['plugins'][0]['version'])"
python3 -c "import json; print(json.load(open('plugin/.claude-plugin/plugin.json'))['version'])"
# All three lines must show X.Y.Z.
```

### Step 3 — Add CHANGELOG entry

Insert the new section **at the top** of `CHANGELOG.md`, immediately
under the level-1 `# Changelog` header and any preamble.

Format:

```markdown
## [X.Y.Z] — YYYY-MM-DD

### Added
- New user-visible thing 1, with enough detail for someone reading the
  GitHub release page to understand what's new without clicking through
  to the code.
- New user-visible thing 2.

### Changed
- Behavior change to existing thing.

### Fixed
- Bug fix users would notice.

### Notes
- (Optional) Implementation notes that aren't user-facing changes but
  matter for someone debugging.
```

Use only the section headers that apply (`Added` / `Changed` /
`Deprecated` / `Removed` / `Fixed` / `Security` / `Notes`). Don't
include empty sections.

**Content rules:**

- Write for the user, not the implementer. "Bookmarks now sync
  cross-session" beats "refactored bookmark storage layer."
- Backticks around identifiers and CLI commands are encouraged —
  they render correctly on GitHub Releases.
- Reference PR numbers only when the PR title carries information
  the bullet doesn't (`(see #67 for the migration story)`).
- The whole section becomes the GitHub Release body verbatim.

### Step 4 — Commit

Make this a **single isolated commit** so it can be reverted cleanly
if the release goes wrong.

```bash
git add threadhop_core/__init__.py \
        .claude-plugin/marketplace.json \
        plugin/.claude-plugin/plugin.json \
        CHANGELOG.md
git commit -m "Bump to X.Y.Z"
```

**Do not include feature work, refactors, or unrelated edits in this
commit.** The commit's diff should touch exactly the four files in §2.

### Step 5 — Push and open the PR

```bash
git push -u origin release/X.Y.Z

gh pr create \
  --base main \
  --title "Release X.Y.Z — <one-line summary>" \
  --body "$(cat <<'EOF'
## Summary

- Releases X.Y.Z covering <2-3 sentence description of what's in this release>.
- Bumps __version__, marketplace.json, plugin.json to X.Y.Z; adds matching CHANGELOG entry.

## What's in this release

(Brief expansion of the CHANGELOG section. Reviewers should be able to
read this PR description and decide if the release is good without
opening other tabs.)

## Test plan

- [x] `validate` workflow passes (version parity + CHANGELOG entry)
- [ ] On merge: `release.yml` creates `vX.Y.Z` tag + GitHub Release
- [ ] After merge: `gh api /repos/parzival1l/threadhop/releases/latest --jq .tag_name` returns `"vX.Y.Z"`
- [ ] After merge: `./threadhop update --check` from a `<previous-version>` install prints the 3-line nudge

## References

- Commits in this release: <one-liner per commit since the previous tag, or a link to the compare URL>
- Related PRs: <if any>
EOF
)"
```

### Step 6 — Wait for `validate` to pass, then merge

`validate` runs automatically on PRs to main and is required by branch
protection. The merge button stays grey until it's green.

If `validate` fails, read the error in the workflow log and fix the
issue. Common failures:

- **Version mismatch** — one of the three fields wasn't updated. Fix
  the file, push, validate re-runs.
- **CHANGELOG missing entry** — the `## [X.Y.Z]` header is misformatted
  (extra spaces, wrong brackets, wrong dash character) or wasn't
  committed. Re-check the format and push.
- **Python syntax** — an unrelated file in the diff has a syntax
  error. Shouldn't happen in a release-only PR.

Use **"Create a merge commit"** as the merge type unless you have a
specific reason to squash.

### Step 7 — Verify the release shipped

Within ~30 seconds of merge, the `release` workflow fires. Confirm:

```bash
# 1. Workflow succeeded.
gh run list --repo parzival1l/threadhop --workflow release.yml --limit 1
# Status should be ✓ (success), not X (failed).

# 2. Tag exists on the remote.
git ls-remote --tags origin | grep "v0.2.1$"   # adjust version

# 3. GitHub Release is live and is the "latest."
gh api /repos/parzival1l/threadhop/releases/latest --jq '.tag_name, .name, .html_url'

# 4. Release body matches the CHANGELOG section.
gh release view "vX.Y.Z" --repo parzival1l/threadhop --json body --jq .body | head -10
# Should match the `## [X.Y.Z]` section of CHANGELOG.md.
```

If any of these don't match expectations, jump to §8 (Failure modes).

---

## 6. Feature-release path (feature work + version bump in one PR)

Use this when the feature being released **is the same PR doing the
release** (i.e., the feature isn't on main yet).

The flow is identical to §5, with two changes:

### Difference 1 — Two commits, not one

Order matters. The first commit(s) are your feature work. The
**last** commit is the version bump (the same isolated commit shape
as §5 step 4).

```
* Bump to X.Y.Z                                    ← isolated, version-only
* Implement <feature>                              ← feature commits
* (more feature commits if needed)
```

This ordering is important because:

- If `release.yml` ever needs to be reverted, `git revert HEAD` rolls
  back **only the version bump** — the feature stays on main.
- The PR diff has a clear narrative: feature first, release ceremony
  second.

### Difference 2 — Test plan adds feature verification

In addition to the four release-verification boxes from §5 step 5,
add manual verification that the feature itself works on `main` after
merge. Match the test-plan style of the feature's surface (CLI smoke
test, TUI flow, etc.).

### Decision rule for an agent

If you've been told "ship X as a release," follow this path. If you've
been told "merge X" with no mention of release, **do not** add a
version bump unilaterally — open a regular PR, and let the next
release roll up your work along with anything else that landed in the
meantime.

---

## 7. Branch protection assumptions

This procedure assumes the following are configured on `main`:

- `validate` is a required status check (set via `gh api -X PUT
  /repos/.../branches/main/protection`).
- `allow_force_pushes: false` — main's history is immutable.
- `required_pull_request_reviews.required_approving_review_count: 0`
  — solo project; you self-approve.
- `enforce_admins: false` — escape hatch for emergencies.

If any of these aren't in place, the procedure still works but you
lose the safety rails that make `validate` failures *block* merges
rather than merely *complain*.

To re-apply protection after a manual unset:

```bash
gh api -X PUT /repos/parzival1l/threadhop/branches/main/protection --input - <<'JSON'
{
  "required_status_checks": {"strict": true, "contexts": ["validate"]},
  "enforce_admins": false,
  "required_pull_request_reviews": {
    "required_approving_review_count": 0,
    "dismiss_stale_reviews": false,
    "require_code_owner_reviews": false
  },
  "restrictions": null,
  "allow_force_pushes": false,
  "allow_deletions": false,
  "required_conversation_resolution": true,
  "required_linear_history": false
}
JSON
```

---

## 8. Failure modes

### `validate` fails on the PR

Read the workflow log. The error message is precise (`::error::Version
mismatch — all three must agree`, etc.). Fix the offending file, push
to the same branch, validate re-runs.

### `release.yml` fails after merge

This means the merge happened (the bump is on main) but no tag/release
was created. Recovery:

```bash
# 1. Read the failed run's log.
gh run list --repo parzival1l/threadhop --workflow release.yml --limit 1
gh run view <run-id> --log-failed

# 2. Manually cut the release that should have happened.
git fetch origin main
git tag vX.Y.Z origin/main
git push origin vX.Y.Z

# Extract the CHANGELOG section to a notes file.
awk '/^## \[X\.Y\.Z\]/{p=1;next} p && /^## \[/{exit} p' \
  CHANGELOG.md > /tmp/release-notes.md

gh release create vX.Y.Z \
  --title "vX.Y.Z" \
  --notes-file /tmp/release-notes.md

# 3. Verify (same commands as §5 step 7).

# 4. Open a fix PR for whatever broke release.yml. Don't bump
#    __version__ in that PR — it's CI infrastructure, not a release.
```

### `release.yml` no-ops when it shouldn't

The "Detect version change" step compares `__version__` at HEAD vs
`HEAD~1`. If both are equal (e.g. you re-released the same version on
a force-push), the workflow skips with `"No version change; skipping
release."`. This is correct — but means you can't re-trigger by
re-pushing.

To force a release on the current commit, use the manual flow under
"`release.yml` fails after merge" above. Or, if the workflow has been
updated to support `workflow_dispatch` with a `force` input:

```bash
gh workflow run release.yml --field force=true --ref main
```

### Tag exists but no Release exists (or vice versa)

Tag and Release are independent objects. The workflow's
`gh release create` creates both atomically; manual steps can create
only one. Symptom: `git ls-remote --tags origin` shows `vX.Y.Z` but
`gh api /releases/latest` returns the previous version.

Recovery: create the missing half.

```bash
# Missing Release for an existing tag:
gh release create vX.Y.Z --title "vX.Y.Z" --notes-file /tmp/release-notes.md

# Missing tag for an existing Release: doesn't normally happen — gh
# release create always creates the tag if absent. If it does happen,
# investigate before fixing.
```

### Wrong CHANGELOG section ended up in the Release body

The release body is a snapshot of `CHANGELOG.md` at the merge commit.
You can't fix this by editing CHANGELOG on a later commit — the
already-published Release won't update.

Recovery options:

```bash
# Option A: edit the Release body in place. Cleanest.
gh release edit vX.Y.Z --notes-file /path/to/correct/notes.md

# Option B: fix CHANGELOG on main and announce a doc-only correction.
# Don't bump __version__ for this — it's a CHANGELOG correction, not
# a new release.
```

---

## 9. Tag vs Release — important distinction

A **git tag** is a ref in the repo (`git tag` creates one). A **GitHub
Release** is a separate object GitHub attaches to a tag (release
notes, optional assets, a publication timestamp). They are independent.

- `/repos/:owner/:repo/tags` — returns tags only.
- `/repos/:owner/:repo/releases/latest` — returns the most recent
  Release (skips drafts and pre-releases by default).

The 24h startup check in ThreadHop calls `/releases/latest`. A bare
tag is **invisible** to that endpoint. The `release.yml` workflow
creates both in one step (`gh release create` makes the tag if
needed and attaches the Release), so the distinction is hidden in the
default flow. It only matters when you're rescuing a failed release
manually.

---

## 10. Plugin vs CLI lifecycle

The Claude Code plugin is distributed via `/plugin` and has its own
update channel owned by Claude Code. ThreadHop's CLI does not push
plugin updates. The plugin `version` field is bumped in lockstep so a
user inspecting the manifest sees a version that matches the CLI they
just updated — they still have to run `/plugin update threadhop`
inside a `claude` session to pull the new skill prompts.

This is why §2's table requires bumping all three version fields. A
PR that bumps only `threadhop_core/__init__.py` will fail `validate`
on the parity check.

---

## Appendix A — Agent quick-reference card

When asked to ship a release, an agent should:

1. **Read this doc end to end.** Don't follow instructions from chat
   history that contradict it.
2. **Run §3's pre-flight commands** and confirm the working tree is
   clean and at `main`.
3. **Decide the version bump per §4's table.** State your choice
   explicitly to the user before proceeding.
4. **Follow §5 (release-only) or §6 (feature + release) end to end.**
   Don't skip steps.
5. **Confirm §5 step 7's verifications** before declaring done.
6. **If anything fails, jump to §8** rather than improvising.

If the user's request conflicts with this doc (e.g. "skip the
CHANGELOG entry"), surface the conflict and ask for confirmation
before deviating. The CI checks will catch most deviations anyway,
but explicit consent prevents wasted work.

## Appendix B — Why these decisions

These are not arbitrary; each rule has a recorded reason in the
project's history.

- **Three version fields, all in lockstep.** Plugin manifest versions
  drift out of sync if not enforced. PR #66 introduced the parity
  check after a release shipped where `__version__` was `0.2.0` but
  `marketplace.json` was still `0.1.0`.
- **CHANGELOG entry is required.** Without one, the GitHub Release
  body is empty, which produces a meaningless Release page. The
  `release` workflow refuses to publish without one.
- **Tag and Release are both required, not just the tag.** The
  startup check calls `/releases/latest`, which only returns Release
  objects. We learned this the hard way during the v0.1.0 cut — a
  bare tag was invisible to the update check, so users on `0.0.x`
  saw nothing. The automation now creates both atomically.
- **Version-bump commit is isolated.** If `release.yml` fails after
  merge, `git revert HEAD` should roll back only the bump, leaving
  feature work on main. PR #68 (release of `0.2.1`) followed this
  pattern; PR #69's failed release recovery was simpler because of
  it.
- **`__version__` change is the release signal, not commit messages
  or manual tags.** The bump is visible in code review, so reviewers
  can scrutinize it the same way they scrutinize feature code. A
  commit-message-based trigger (`release: ...` prefix) hides intent
  outside the diff. A manual-tag trigger skips the diff entirely.

---

*Last updated 2026-04-22. Authoritative for this repo's release
process. If this doc disagrees with chat history or other docs,
trust this doc.*
