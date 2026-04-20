---
description: Tag the current Claude Code session with a status. Valid statuses are enumerated in the argument hint so users discover them without memorising the set.
argument-hint: "<backlog|in_progress|in_review|done|archived>"
disable-model-invocation: true
allowed-tools: Bash(threadhop:*)
---

!`threadhop tag $ARGUMENTS`
