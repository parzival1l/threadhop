---
name: handoff
description: Produce a ThreadHop handoff brief for another Claude Code session so this conversation can pick up where that one left off. Pass the target session id; add --full for a comprehensive brief with transcript excerpts. Uses the ThreadHop observer + a Haiku sub-agent.
---

# /threadhop:handoff

You have been invoked to produce a **handoff brief** for a prior Claude
Code session, so the current conversation can carry its context forward.

## Arguments

The user invokes this skill as:

```
/threadhop:handoff <session_id> [--full]
```

- `<session_id>` — required. The Claude Code session UUID (or the
  first unambiguous prefix) the user wants to hand off from. Usually
  lifted from the ThreadHop TUI or `~/.claude/projects/`.
- `--full` — optional. Request a comprehensive brief with rationale,
  file/code references, and verbatim transcript excerpts. Without it,
  you get a short brief (~30-50 lines).

Parse those from `$ARGUMENTS` exactly. If `<session_id>` is missing,
stop and ask the user for it — do not guess.

## What you do

1. Run the ThreadHop `handoff` subcommand with the parsed arguments.
   It already handles every observer/observation detail per ADR-018:
     - Seeds the `sessions` row if the transcript exists.
     - Runs the observer to catch up incrementally (or extract from
       byte 0 if this session was never observed).
     - Reads the per-session unified observation JSONL
       (observer + reflector conflict entries, ADR-020).
     - Formats short sets directly; polishes large sets / `--full`
       through a Haiku sub-agent.

   ```bash
   threadhop handoff <session_id>          # short brief
   threadhop handoff <session_id> --full   # comprehensive brief
   ```

   The command prints the markdown brief to **stdout**. Status and
   diagnostic lines (e.g. "Polished 12 observations via Haiku", or the
   fallback reason if Haiku polish failed) go to **stderr** — surface
   those only if something noteworthy happened (fallback, error).

2. Present the brief to the user inside a clearly bounded context
   block so it's visually distinct from your own reasoning. Start with
   a one-line preface that names the session id and mode, then render
   the markdown the command emitted verbatim (do not paraphrase,
   truncate, or reflow).

   Example layout:

   ```
   Handoff brief for session abc12345 (short):

   <markdown stdout from `threadhop handoff`>
   ```

3. After the brief, do NOT automatically start acting on any TODOs
   it lists. Wait for the user to tell you what to do with the
   handed-off context. This skill only *loads* context; it does not
   *execute* on it.

## Error handling

- Exit code 1 (`no_source` — transcript not found): tell the user the
  session id couldn't be located and suggest they confirm it via the
  ThreadHop TUI or `ls ~/.claude/projects/`. Do not retry with a
  different id unless the user provides one.
- Exit code 0 with empty stdout (`no_observations`): the stderr message
  explains — usually the target session is too short or routine for the
  observer to find decisions/TODOs. Pass that message through to the
  user; do not fabricate a brief.
- Any other non-zero exit: show the stderr message verbatim. Do not
  retry silently.

## Constraints

- Do NOT try to read the raw JSONL transcript yourself. Always go
  through `threadhop handoff` — it owns the observer invocation,
  prompt template, and state tracking.
- Do NOT re-run the subcommand multiple times hoping for a better
  brief. The observer is deterministic-enough that a retry at the same
  state returns the same output.
- Do NOT edit `~/.config/threadhop/observations/*.jsonl` or the
  ThreadHop SQLite DB yourself. Those files are append-only and
  maintained by the observer/reflector (ADR-020).
