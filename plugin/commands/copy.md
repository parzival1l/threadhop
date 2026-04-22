---
description: Copy the cleaned session transcript to the clipboard as markdown. No argument copies the last turn; a number copies the last N turns; `all` copies the whole session. Tool calls, sidechains, and system reminders are stripped.
argument-hint: "[N|all]"
disable-model-invocation: true
allowed-tools: Bash(threadhop:*)
---

!`threadhop copy $ARGUMENTS`
