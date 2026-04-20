# Skill packaging research (resolves Q4)

Answers to _Open Question Q4_ in `DESIGN-DECISIONS.md`: how Claude Code
plugins and their in-session entry points are distributed and loaded.

**TL;DR:** ThreadHop ships as a **single plugin** (a directory with a
`.claude-plugin/plugin.json` manifest) containing a *hybrid* of one skill
and two slash commands, all under the `/threadhop:` namespace:

- `/threadhop:handoff` — **skill** (`plugin/skills/handoff/SKILL.md`).
  Model-in-the-loop because the brief wants framing, error judgment,
  and "don't auto-act on TODOs" behaviour that a pure CLI relay can't
  provide.
- `/threadhop:observe` — **command** (`plugin/commands/observe.md`).
  One-line `!`cmd`` wrapper over `threadhop observe`.
- `/threadhop:tag` — **command** (`plugin/commands/tag.md`). One-line
  `!`cmd`` wrapper; its `argument-hint` enumerates valid statuses so
  users discover them from the `/` picker without memorising.

The plugin is **not self-contained** — it calls bare `threadhop` from
`$PATH`. Users install the app (e.g. `pipx install threadhop`) and the
plugin (`/plugin install threadhop`) as two separate steps; this
decouples the two release cycles and keeps the plugin packaging-agnostic.

Tagging also remains available as a zero-LLM bash passthrough
(`!threadhop tag <status>`). The slash-command form's advantage is
discoverability — the argument-hint shows the valid status set inline.

---

## Three surfaces, different jobs

Claude Code overloads the word "skill". The three artifacts that live
inside a plugin are **not** interchangeable:

| Artifact | Path inside plugin | Who invokes it | Purpose |
|----------|--------------------|----------------|---------|
| Plugin manifest | `.claude-plugin/plugin.json` | Claude Code on load | Declares name/version, namespaces everything inside |
| Slash command | `commands/<name>.md` | **User** types `/plugin:name` | Side-effectful in-session entry point |
| Skill | `skills/<name>/SKILL.md` | **Model** (autoload by description, or `/plugin:name`) | Reusable knowledge / procedure the model can consult |

The Phase-4 design (ADR-012, ADR-016) calls every entry point a "skill"
colloquially. In Claude Code terms, the right shape depends on whether
the model adds value at invocation time:

- **Skill** when the model earns its turn — framing output, handling
  errors, making judgment calls (e.g. handoff: decides how to render
  the brief, whether to surface stderr diagnostics, when to stop
  before auto-acting on TODOs).
- **Command** with `!`cmd`` pre-injection when the LLM has nothing to
  frame — the CLI's stdout *is* the answer (e.g. observe, tag).

A single plugin can ship both shapes under one namespace. That's what
ThreadHop does, and what the OpenAI Codex plugin installed on this
machine does (it has both a `commands/` and a `skills/` directory
under one plugin.json).

Docs:
- Plugins: <https://code.claude.com/docs/en/plugins>
- Skills: <https://code.claude.com/docs/en/skills>
- Slash commands: <https://docs.claude.com/en/docs/claude-code/slash-commands>

## Directory layout

```
plugin/
├── .claude-plugin/
│   └── plugin.json                 # required manifest
├── skills/
│   └── handoff/
│       └── SKILL.md                # /threadhop:handoff — rich, model-framed
├── commands/
│   ├── observe.md                  # /threadhop:observe — thin !`cmd`
│   └── tag.md                      # /threadhop:tag — thin !`cmd` with argument-hint
└── README.md
```

No `bin/` directory. The plugin relies on `threadhop` being on `$PATH`
(see "The plugin ↔ app relationship" below).

Scope decisions made against ADR-016's original four-skill plan:

- **`/threadhop:insights` dropped.** Surfacing captured observations
  back into the same Claude Code session that captured them
  re-introduces the facts the observer was distilling out — it defeats
  the observer's purpose. Observations are meant for *other* sessions
  (consumed via `/threadhop:handoff`) or for the user to review outside
  the session (TUI, CLI subcommands). There is no in-session viewer.
- **`/threadhop:context` dropped.** Clipboard-to-markdown wrapping is
  not worth a plugin command.
- **`/threadhop:observe --stop` / `--stop-all` not surfaced.** The
  observer's lifetime is tied to the Claude Code session that started
  it — when that session's process exits, the observer exits. No
  in-session stop command is needed. `threadhop observe --stop-all`
  remains on the CLI for orphan cleanup from a terminal.
- **Hybrid shape for the three entry points.** Handoff ships as a
  skill because the model's framing work is real value; observe and
  tag ship as commands because they're thin CLI relays. All three sit
  under one plugin so users get a single install and the `/threadhop:`
  namespace prefix.

## `plugin.json` shape

The only required fields are `name`, `version`, `description`. `author`
is optional but recommended. Example from the installed OpenAI Codex
plugin (`~/.claude/plugins/cache/openai-codex/codex/1.0.2/.claude-plugin/plugin.json`):

```json
{
  "name": "codex",
  "version": "1.0.2",
  "description": "Use Codex from Claude Code to review code or delegate tasks.",
  "author": { "name": "OpenAI" }
}
```

The plugin's `name` is what prefixes every slash command it ships:
`/<plugin-name>:<command-file-stem>`. So `commands/handoff.md` inside a
plugin named `threadhop` is invoked as `/threadhop:handoff`.

## Slash-command frontmatter

Confirmed against `codex/commands/cancel.md`, `setup.md`, `rescue.md`:

```yaml
---
description: Generate a handoff brief for a past session
argument-hint: "<session_id> [--full]"
disable-model-invocation: true
allowed-tools: Bash(threadhop:*), Read
---
```

| Key | Required | Purpose |
|-----|----------|---------|
| `description` | yes | One-liner shown in `/` picker and to the model |
| `argument-hint` | no | Autocomplete hint after the command |
| `disable-model-invocation` | no — default `false` | `true` = only the user can invoke via slash; the model cannot auto-call it. Set this for all ThreadHop commands (they have side effects). |
| `allowed-tools` | no | Whitelist of tools Claude may use while the command runs. Supports bash-pattern matchers like `Bash(threadhop:*)`. |
| `context` | no | `fork` runs the command in a sub-context (used by codex's `rescue.md` to keep a large Codex response out of the main thread). |

Note: the frontmatter key for the **skill** equivalent is spelled
differently — `user-invocable: false` (see
`codex/skills/codex-cli-runtime/SKILL.md`). Commands default to
user-invocable and opt *out* via `disable-model-invocation: true`; skills
default to model-invocable and opt *out* via `user-invocable: false`. Do
not mix up the keys.

## Arguments

Two substitutions are available inside a command body:

- `$ARGUMENTS` — everything after the command name, verbatim
- `$1`, `$2`, … — positional splits on whitespace

If the body references neither, Claude Code appends
`ARGUMENTS: <raw input>` to the end of the rendered prompt.

## The `!`cmd`` pre-execution trick

**This is the most important finding for ThreadHop.** A line whose
first character is `!` followed by a backticked command is executed by
the harness *before* the model sees the prompt, and the stdout replaces
the placeholder. The command runs in the user's shell with
`allowed-tools` enforcing the permission check.

Reference implementation — `codex/commands/cancel.md` in its entirety:

```markdown
---
description: Cancel an active background Codex job in this repository
argument-hint: '[job-id]'
disable-model-invocation: true
allowed-tools: Bash(node:*)
---

!`node "${CLAUDE_PLUGIN_ROOT}/scripts/codex-companion.mjs" cancel $ARGUMENTS`
```

That's the whole file. No model-side reasoning — Claude just relays the
CLI's stdout back as the turn's output.

`${CLAUDE_PLUGIN_ROOT}` resolves to the installed plugin directory, so
bundled scripts are reachable regardless of install path. ThreadHop's
commands **do not** use this — they call bare `threadhop` from `$PATH`.
See "The plugin ↔ app relationship" below for why.

```markdown
!`threadhop observe $ARGUMENTS`
```

This pattern is a perfect fit for our use case: the hard work (observer
catch-up, reflection, formatting) already lives in the CLI and is tested
in isolation. The slash command is a thin wrapper that reuses it.

## Distribution

Two official paths:

1. **Marketplace** — publish a git repo containing
   `.claude-plugin/marketplace.json` at the root. Users then run
   `/plugin marketplace add <url>` followed by `/plugin install <name>`.
   Example marketplace from
   `~/.claude/plugins/cache/inngest-agent-skills/.../marketplace.json`:

   ```json
   {
     "name": "inngest-agent-skills",
     "metadata": { "description": "..." },
     "owner": { "name": "Inngest, Inc." },
     "plugins": [
       { "name": "inngest-skills", "source": "./", "skills": ["./skills/..."] }
     ]
   }
   ```

2. **Direct path** — during development, point Claude at an unpacked
   plugin with `--plugin-dir <path>` or symlink into
   `~/.claude/plugins/<name>/`. No manifest signing or npm publish step.

Docs: <https://code.claude.com/docs/en/plugin-marketplaces>.

## The plugin ↔ app relationship (Model B)

There are three ways a plugin can reach its underlying binary. We chose
the decoupled one:

| Model | Plugin contains | App installed separately? | Called as |
|-------|-----------------|---------------------------|-----------|
| A. Bundled | A copy of the CLI at `bin/threadhop` | No | `${CLAUDE_PLUGIN_ROOT}/bin/threadhop` |
| **B. PATH-dependent (chosen)** | **Just `.md` files — no binary** | **Yes — `pipx`, `uv tool`, `brew`** | **`threadhop` (bare, from `$PATH`)** |
| C. Plugin is the app | Everything, including Python modules | No | `${CLAUDE_PLUGIN_ROOT}/bin/threadhop` |

Why Model B:

- **Decoupled release cycles.** `pipx install threadhop` to v0.3.0 next
  month doesn't force a plugin re-release. The plugin (still v0.1.0)
  keeps working because it calls bare `threadhop`.
- **Packaging-agnostic.** Whether ThreadHop lands on PyPI, Homebrew,
  `uv tool install`, or a plain `install.sh` — the plugin doesn't
  care. The install story lives entirely on the app side.
- **One source of truth.** The CLI a user runs in a terminal is
  literally the same binary `/threadhop:handoff` shells out to. No
  "the plugin has an old bundled copy" bug class.
- **The TUI lives on its own.** ThreadHop's TUI is a first-class
  feature. Users install the app even if they never touch Claude Code.
  Model A would force them to install it twice (once standalone, once
  bundled in the plugin).

Installation story under Model B:

```bash
# 1. install the app (TUI + CLI on PATH)
pipx install threadhop            # or: uv tool install threadhop, brew install threadhop, etc.
threadhop --help                  # verify it's on PATH

# 2. install the plugin (slash commands in Claude Code)
/plugin marketplace add github:parzival1l/threadhop
/plugin install threadhop
```

Two commands, two concerns separated cleanly.

## ThreadHop plan

Phase 4 ships as one plugin `threadhop`. The plugin directory lives in
this repo under `plugin/` and points at bare `threadhop` from `$PATH`:

- `plugin/.claude-plugin/plugin.json`
- `plugin/skills/handoff/SKILL.md` — model-framed handoff brief (task #26, merged)
- `plugin/commands/observe.md` — `!`threadhop observe`` wrapper
- `plugin/commands/tag.md` — `!`threadhop tag`` wrapper with discoverable `argument-hint`

Each file is either a 93-line skill (handoff — where the model earns
its turn) or a ~8-line command (observe, tag — pure CLI relay).

## Verifying locally

```bash
# from the repo root
claude --plugin-dir "$(pwd)/plugin"
# then in the session:
/threadhop:hello
```

`hello` shells out to `echo`, so it works before the ThreadHop binary
is bundled. When handoff is ready:

```bash
/threadhop:handoff abc123
```

## Anti-patterns

- **Don't inject observation content back into the generating session.**
  Pulling observations (decisions, TODOs, conflicts) into the same
  Claude Code session that captured them re-introduces the facts the
  observer was distilling out of that context — the cost/benefit of
  running an observer disappears. Observations are for consumption
  *elsewhere*: by another Claude Code session via `/threadhop:handoff`,
  by the user in the ThreadHop TUI, or via CLI subcommands in a
  terminal. This is why there is no `/threadhop:insights`.
- **Don't surface observer stop/stop-all as a slash command.** The
  observer's lifetime follows the Claude Code session that started it.
  Surfacing a stop command suggests manual lifecycle management where
  none exists, and adds clutter to the `/` picker. Orphan cleanup stays
  on the CLI (`threadhop observe --stop-all`).
- **Don't put user-facing entry points in `skills/`.** Skills are
  model-invoked by description and can fire without the user typing
  anything. Side-effectful operations (observer spawn, handoff
  generation, tagging) belong in `commands/` with
  `disable-model-invocation: true`.
- **Don't do the work inside the command body with Markdown + Bash
  tool calls** when a single `!`cmd`` line would do — that costs an LLM
  turn for something the CLI could have done in milliseconds.
- **Don't bundle the `threadhop` binary inside the plugin.** The plugin
  depends on `$PATH` (Model B above). Bundling couples plugin and app
  release cycles, forces users to install the CLI twice, and creates a
  "the plugin has a stale copy" bug class.
- **Don't swap `disable-model-invocation` and `user-invocable`.** The
  keys look similar but live on different artifacts (command vs skill)
  and have inverted defaults.
