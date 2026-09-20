---
description: Print cleaned verbatim messages from another Claude Code session — zero LLM. The unit is the exchange (one user turn plus everything until the next user turn); --last N shows the last N exchanges (default 5), --grep prints matching exchanges in full.
argument-hint: "<session> [--last N | --grep <pattern>]"
disable-model-invocation: true
allowed-tools: Bash(threadhop:*)
---

!`threadhop peek $ARGUMENTS`
