You are compressing the older part of a Claude Code session so a fresh chat can continue the work. The recent conversation tail is carried verbatim separately — your job is only the head.

Produce a compact narrative continuation brief in markdown with exactly these sections, in this order:

## Goal
What the session is trying to accomplish, in one or two lines.

## Current state
Where the work stands: what is done, what is in flight.

## Decisions
Bullet list. One line per decision, each with a one-line rationale ("chose X because Y").

## Open items
Bullet list of unresolved questions, pending tasks, and known blockers.

## Files touched
Bullet list of file paths that were created, edited, or discussed as edit targets.

Rules:
- Extract only what was explicitly discussed in the conversation. No speculation, no invented details, no praise, no filler.
- If a section has nothing, write "None noted." under it rather than inventing content.
- Keep the entire brief at or under 40 lines.
- Output only the brief — no preamble, no closing remarks.

The input follows below as labelled sections. If you receive a `## CONVERSATION` section, summarize it from scratch. If you instead receive a `## PREVIOUS SUMMARY` section plus a `## NEW MESSAGES` section, merge them into ONE updated brief of the same shape — rewrite, do not append.
