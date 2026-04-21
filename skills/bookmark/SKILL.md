---
name: bookmark
description: Record a categorized ThreadHop bookmark against a live Claude Code session message, with optional note text and current-session auto-detection.
---

# /threadhop:bookmark

You have been invoked to save part of the current or a named Claude Code
session into ThreadHop's categorized bookmark store.

## Arguments

Invoke this skill as:

```bash
/threadhop:bookmark <message_uuid> --category <name> [--note <text>] [--session <id>]
```

- `<message_uuid>` is required. It must be a ThreadHop-visible message UUID.
- `--category <name>` is required.
- `--note <text>` is optional.
- `--session <id>` is optional. If omitted, `threadhop bookmark add` will try to
  auto-detect the current Claude session from the terminal process tree.

Do not guess missing required values. If `<message_uuid>` or `--category` is
missing, ask the user for it.

## What You Do

1. Run the ThreadHop CLI:

   ```bash
   threadhop bookmark add --message <message_uuid> --category <name> [--note <text>] [--session <id>]
   ```

2. Surface the CLI confirmation directly to the user.

3. If the category does not already exist, ThreadHop will auto-create it with
   no research prompt. Mention that briefly so the user knows a future
   `threadhop bookmark category set-prompt ...` step may be needed before
   background research can run.

## Error Handling

- If the CLI says the session could not be auto-detected, tell the user to rerun
  with `--session <id>`.
- If the CLI says the transcript or message UUID could not be found, pass that
  through plainly. Do not fabricate a different target.
- Any other non-zero exit: show stderr and stop.

## Constraints

- Do not edit `sessions.db` or JSONL transcript files directly.
- Do not implement your own bookmark write path. Always go through
  `threadhop bookmark add`.
- Do not run the research step automatically. This skill only records the
  bookmark.
