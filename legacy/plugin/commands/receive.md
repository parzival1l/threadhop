---
description: Print a ThreadHop transfer ticket verbatim so this chat can pick up where another session left off — zero LLM. Pass the ticket id printed by `threadhop prepare` (e.g. tk_ab12cd).
argument-hint: "<ticket-id>"
disable-model-invocation: true
allowed-tools: Bash(threadhop:*)
---

!`threadhop receive $ARGUMENTS`
