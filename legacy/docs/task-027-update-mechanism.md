# Task 027 — Implement `threadhop update`, `changelog`, `future`, and the 24h startup check

**Reference:** [ADR-027 in `DESIGN-DECISIONS.md`](./DESIGN-DECISIONS.md#adr-027-update-lifecycle--threadhop-update-changelog-future-and-24h-startup-check)

Read ADR-027 end-to-end before starting. The ADR captures every design
decision and the rationale behind each one. This doc only adds the
concrete "what to build and how to verify it" layer.

---

## Objective

Give ThreadHop users a discoverable CLI-side update mechanism with an
ambient notification channel, plus a lightweight roadmap surface. No
code change to the Claude Code plugin — plugin lifecycle stays owned by
`/plugin`.

End state: a user who ran `curl ... install.sh | bash` a week ago runs
any `threadhop` command from their shell and sees a 3-line "update
available" notice once per 24 hours. They run `threadhop update` and
their install refreshes to `origin/main`. They run `threadhop
changelog` to see what changed and `threadhop future` to see what's
coming next.

---

## Deliverables (checklist for the picking-up chat)

### New files at repo root

- [ ] `ROADMAP.md` — top of the roadmap + a list of entries in the format
      `- #NN — description`. Seed with 5-8 items from existing internal
      task lists.
- [ ] `CHANGELOG.md` — Keep-a-Changelog style. Seed with `0.1.0 — current
      date` and an entry noting initial release (--version flag,
      did-you-mean, curl-bash installer, marketplace.json).

### New subcommands in `threadhop`

All three live behind new `subparsers.add_parser(...)` entries in
`build_parser()`. Follow the epilog/Examples pattern already established
for existing subcommands.

- [ ] `threadhop update` with flags `--to <ref>` and `--check`.
- [ ] `threadhop changelog` — no flags.
- [ ] `threadhop future` — no flags.

### Startup check

- [ ] New helper `_check_for_update()` in `threadhop`. Returns an object
      (or None) with `.latest` and `.current` fields.
- [ ] Called once at `main()` entry, before dispatch. Prints the 3-line
      notice to stderr if a newer version is available AND all four
      gates pass (see ADR-027 "Startup check semantics").
- [ ] Called once at `ClaudeSessions.on_mount()` in `tui.py`. On hit,
      fires `self.notify(...)` with the TUI toast shape from the ADR.

### Opt-out surface

- [ ] Respect `$THREADHOP_NO_UPDATE_CHECK`. If set to any non-empty
      value, skip the network call entirely.
- [ ] Document it in README.

### Version and release discipline (one-time scaffolding)

- [ ] Bump `__version__` in `threadhop` from `0.1.0` to `0.2.0` as part
      of this PR (new version = new feature set).
- [ ] Bump the `version` field in `.claude-plugin/marketplace.json` and
      `plugin/.claude-plugin/plugin.json` to `0.2.0` to match.
- [ ] Add a `CHANGELOG.md` entry for `0.2.0` listing the new commands.
- [ ] Add a short "Release discipline" section to `CLAUDE.md` (or a new
      `RELEASE.md`) capturing the six-step release flow from ADR-027 so
      future releases don't skip the tag.

### README updates

- [ ] New section under `## Install` or just below it titled "Updating
      ThreadHop" with the three commands and the opt-out env var.

---

## Files likely to change

| File | Scope |
|---|---|
| `threadhop` | ~150 lines: new subcommands, startup check, version bump, argparse wiring |
| `tui.py` | ~10 lines: `on_mount()` hook + import of `_check_for_update` |
| `README.md` | new "Updating ThreadHop" section |
| `CHANGELOG.md` | new file |
| `ROADMAP.md` | new file |
| `.claude-plugin/marketplace.json` | version bump |
| `plugin/.claude-plugin/plugin.json` | version bump |
| `CLAUDE.md` or `RELEASE.md` | release discipline note |

No changes to `install.sh` (already idempotent and unaffected).

---

## Acceptance criteria

Each item here should be a one-command verification.

### `threadhop update`
- [ ] `threadhop update --check` prints `ThreadHop X.Y.Z is up to date.`
      when on latest, or the 3-line "update available" notice when not.
      Exits 0 either way.
- [ ] `threadhop update` (no flags) runs `git fetch` + `git reset --hard
      origin/main` on the installed repo (`Path(__file__).resolve().parent`
      inside the script). Exits 0. Prints "Updated to `<new-version>`."
- [ ] `threadhop update --to v0.1.0` checks out the tag. Prints "Pinned
      to v0.1.0." Exits 0.
- [ ] `threadhop update --to <nonexistent-ref>` exits 1 with a clean
      error message mentioning the ref.
- [ ] When run against a non-git install (someone copied the script
      standalone), exits 1 with "Not a git checkout; re-run the
      installer instead."

### `threadhop changelog`
- [ ] `threadhop changelog` on an install with `CHANGELOG.md` present
      prints the file. Through `less -R` when stdout is a TTY; raw
      otherwise.
- [ ] `threadhop changelog` on an install without the file fetches from
      `raw.githubusercontent.com/.../main/CHANGELOG.md` (1s timeout).
      On network failure, prints "CHANGELOG not available offline. See
      https://github.com/parzival1l/threadhop/blob/main/CHANGELOG.md" to
      stderr and exits 1.

### `threadhop future`
- [ ] `threadhop future` prints the first five roadmap entries from
      `ROADMAP.md`. Output matches the shape in ADR-027.
- [ ] Fewer than 5 entries in the file: prints what's there, no error.
- [ ] Missing `ROADMAP.md`: prints a friendly message + the GitHub URL,
      exits 0 (not an error — just unavailable).

### Startup check (CLI)
- [ ] `threadhop todos` on a stale install (older `__version__` than
      latest tag) prints the 3-line notice once, then the normal output.
- [ ] Running it again within 24h: notice does not reprint.
- [ ] `touch -d '25 hours ago' ~/.cache/threadhop/last_check && threadhop
      todos`: notice reprints.
- [ ] `THREADHOP_NO_UPDATE_CHECK=1 threadhop todos`: no notice, no
      network call.
- [ ] `threadhop todos | cat`: no notice (stdout is not a TTY).
- [ ] From inside a Claude Code session (`!threadhop tag ...` or
      `/threadhop:tag`): no notice. Verified by running the command in
      a live session and confirming no update line appears.

### Startup check (TUI)
- [ ] Launch TUI on a stale install: toast appears in the top-right,
      fades after ~10s, does not resize the layout or steal focus.
- [ ] Relaunch TUI within 24h: toast does not reappear.

---

## Implementation hints

- **Context gate reuses existing helper.** The `_resolve_cli_session`
  function in `threadhop` (from task #17, per `CLAUDE.md`) already walks
  the parent-process tree to detect whether the caller is running inside
  a Claude Code session. Extract the "are we inside claude?" logic into
  a small `_invoked_from_claude_code() -> bool` helper and gate the
  notification on `not _invoked_from_claude_code()`.
- **Cache directory.** Use `~/.cache/threadhop/` (XDG-ish). Create it
  lazily on first check. The cache file contents don't matter — only
  mtime is consulted.
- **GitHub API.** Use `urllib.request` (stdlib). No new dependency. The
  URL is
  `https://api.github.com/repos/parzival1l/threadhop/releases/latest`.
  Send `Accept: application/vnd.github+json`. Parse `tag_name` (string
  like `"v0.2.0"`).
- **Version parsing.** See ADR-027 for the `_parse_version` helper.
  Wrap the comparison in a try/except so malformed tags silently skip.
- **Self-replacement gotcha.** `threadhop update` replaces `threadhop`
  on disk while it's running. On macOS/Linux this is safe (the running
  process's file has already been mmap'd), but only complete the git
  operations and exit immediately afterwards. Do not run further Python
  logic after the git reset — call `os._exit(0)` or just `return` from
  the function that dispatched it.
- **TUI notify timeout.** Textual's `notify` accepts `timeout=<seconds>`.
  10 seconds is right.
- **Pager for changelog.** `subprocess.run(["less", "-R"], input=content,
  text=True)` with a fallback to raw `print(content)` if `less` isn't
  on PATH.

---

## Out of scope

- **Auto-generated CHANGELOG** (e.g. via `git-cliff`) — manual entries
  are fine for now.
- **Signed releases / checksum verification** — pre-1.0, no threat model
  warrants it.
- **GitHub Releases automation** — a separate follow-up task. Initial
  tags can be cut manually with `git tag` + `git push --tags`.
- **Plugin update mechanism** — Claude Code owns this. See ADR-027
  "Plugin updates — explicitly out of scope."
- **Notification inside Claude Code plugin / `!bash` invocations** —
  explicitly suppressed by the context gate.
- **`threadhop update --all`** (update CLI + plugin in one step) —
  rejected in ADR-027 because plugin cache is Claude Code's surface.

---

## Open questions (for the picking-up chat to resolve)

1. **Changelog rendering fallback.** If `less` isn't on PATH, should we
   fall back to raw print, or try `more`, or exit with a friendly
   error? Recommend: raw print, always work.
2. **TUI toast placement customization.** Textual's default is
   top-right. Verify this looks right given ThreadHop's 2-column
   layout. If not, consider passing a `severity` that positions
   differently.
3. **Roadmap file location.** ADR-027 proposes repo root
   (`ROADMAP.md`). If the picking-up chat finds repo-root clutter, it
   can move to `docs/ROADMAP.md` and update the parser path.

---

## Rough effort estimate

| Chunk | Estimate |
|---|---|
| New subcommands + argparse wiring | 45 min |
| Startup check + all four gates | 45 min |
| TUI notify integration | 20 min |
| README + CHANGELOG + ROADMAP seeding | 30 min |
| Version bumps + release discipline doc | 15 min |
| Tests (unit for parsing, manual for end-to-end) | 60 min |
| **Total** | **~3.5 hours** |

Expect ~200 lines of diff when done. Single PR, single commit fine.
