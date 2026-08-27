---
description: Build a frozen transfer ticket for continuing this session's work in another chat. One Haiku call summarizes the conversation head; the last N exchanges (default 3) are kept verbatim. Prints a paste-ready `!threadhop receive tk_<id>` line for the target chat.
argument-hint: "[--session <id>] [--tail N]"
disable-model-invocation: true
allowed-tools: Bash(threadhop:*)
---

!`threadhop prepare $ARGUMENTS`
