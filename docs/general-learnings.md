# General Learnings — Cross-Session Context & Memory Patterns

Extracted from design discussion on 2026-04-14. These are not specific to the
ThreadHop repo — they apply to any multi-session AI workflow.

---

## 1. The Context Portability Problem

When working with AI coding assistants across multiple sessions, knowledge gets
trapped. A session at 40-50% context usage has accumulated decisions, rationale,
and working state that cannot be efficiently carried to a new session.

**Three failed approaches:**
- **Continue the bloated session** — quality degrades past ~40% context
- **Dump full markdown** — recreates the bloat in the new session
- **Manually summarize** — labor-intensive, lossy, breaks flow

**The right approach:** Structured compression via handoff briefs — 5-10% of
original context carrying ~80% of signal. Generated on-demand, not in advance.

## 2. Mastra Observational Memory Pattern

Reference: `observational-memory.mdx` (Mastra `@mastra/memory` docs)
Credit: [Mastra team](https://mastra.ai) — `@mastra/memory@1.1.0`

A three-tier memory system that applies beyond Mastra's framework:

```
Recent messages (full fidelity, current task)
    ↓ Observer (threshold-triggered, e.g. 30k tokens)
Observations (compressed, structured, append-only log)
    ↓ Reflector (threshold-triggered, e.g. 40k tokens)
Reflections (condensed patterns, merged items)
```

**Key design decisions in OM:**
- Observer runs in background, non-blocking (async buffering)
- Observations are append-only — never rewritten, only condensed by Reflector
- Resource scope enables cross-thread memory (experimental, can cause task bleed)
- Token-tiered model selection: cheaper models for small inputs, stronger for large
- Retrieval mode keeps observation-to-source links for recall

**Applicable insight:** This tiered compression model works for any system where
context accumulates over time. The Observer/Reflector split means you can start
with just observation (cheap) and add reflection later (when volume demands it).

**The inherent problem when applying to Claude Code:** Mastra's Observer/Reflector
are *inline agents* — they share the same process and memory as the primary agent
and can directly modify its context window (removing old messages, injecting
compressed observations). Claude Code is a black box: we can read its JSONL
transcript files but cannot modify the running agent's context. This makes the
inline approach impossible.

ThreadHop adapts the pattern as an *external sidecar* — the observer watches
transcript files post-hoc, and the reflector focuses on *conflict detection*
(finding contradictory decisions across sessions) rather than Mastra's original
goal of context compression. The value shifts from helping the *agent* stay
effective to helping the *human* understand what happened. See ADR-015 in
`DESIGN-DECISIONS.md` for the full architecture.

## 3. Feature-Scoped Shared Memory

The right unit for cross-session context is neither the session nor the project —
it's the **feature**. A feature spans multiple sessions (planning → implementation
→ verification), possibly multiple projects, possibly multiple AI models.

**Feature context contains:**
- Decisions made (with rationale)
- Current implementation state
- Open questions / unresolved threads
- Contributing sessions (with their roles: planning, implementation, review)

**This differs from CLAUDE.md:**

| CLAUDE.md | Feature Memory |
|-----------|----------------|
| Snapshot (declarative) | Timeline (sequential) |
| Rewritten on update | Append-only |
| "What are the rules?" | "What happened and when?" |
| Project-scoped | Feature-scoped (cross-project) |

## 4. The Verification Session Problem

A common workflow: Plan (Session 1) → Implement (Session 2) → Verify (Session 3,
often with a different model). Session 3 needs not just the plan artifact but the
**reasoning** behind it — rejected alternatives, constraints, tradeoffs.

That reasoning lives in Session 1's conversation and never makes it into the plan.
The handoff brief solves this by explicitly capturing decisions and rationale, not
just outcomes.

## 5. SQLite FTS vs Embeddings for Personal Tools

For personal-scale search across conversation transcripts (hundreds of sessions,
thousands of messages):

**SQLite FTS5 with porter stemming** is the right starting point:
- Keyword search with stemming ("running" matches "run")
- Zero infrastructure — SQLite is stdlib in Python
- Deterministic results — you know why something matched
- Sub-millisecond queries on personal-scale data
- `porter unicode61` tokenizer handles English + Unicode

**Embeddings become worth it only when:**
- Keyword search fails on actual usage patterns
- You need semantic similarity ("find discussions about error handling" matching
  "exception management")
- The dataset is large enough that keyword recall drops

For most personal tools, keyword search covers 90%+ of queries because you
remember *words* you used, not abstract concepts.

## 6. Language Choice for TUI Tools

For I/O-bound TUI applications (file reading, subprocess calls, widget rendering):

**Python + Textual** is the right default:
- Textual provides a complete widget system (layout, styling, events)
- Bottlenecks are I/O and subprocess calls — language doesn't help
- SQLite FTS queries run in C regardless of host language
- `uv run --script` gives near-instant startup with no install step
- Single-file deployment is a major UX advantage

**Rust** only makes sense if:
- Startup time matters at the <50ms level
- You're processing millions of records in a tight loop
- You want to distribute a single static binary

**TypeScript** only makes sense if:
- You plan to share code with a browser/Electron UI later
- Your team knows TS better than Python

## 7. Strict LLM vs Instantaneous Boundary in Tool Design

When building tools that mix AI-powered and non-AI operations, draw a hard
line between what needs an LLM call and what must be instantaneous:

**Instantaneous (TUI / local app):**
- Search — must update per-keystroke, FTS queries are sub-millisecond
- Visual selection — users select messages by seeing them, not by typing indices
- Copy/export — clipboard and file writes are instant
- Tagging/bookmarking — single-key state changes

**LLM-powered (skill/plugin):**
- Transcript compression / handoff — genuinely needs summarization
- Knowledge extraction — needs understanding, not just filtering

**The failure mode:** Making search or context insertion into an LLM skill.
Search needs per-keystroke feedback. Context insertion needs visual message
selection — you can't type "messages 15-25" without seeing them first, making
an opaque range parameter useless. These must be instant, visual, and local.

**The architecture:** The TUI and the skill plugin share the same database
but serve different moments. The TUI is for browsing, selecting, and instant
operations. Skills are invoked mid-conversation for operations that benefit
from LLM processing (handoff) or that inject pre-built context without
switching windows (project memory).

## 8. Context Transport: Clipboard vs Temp Files

Two complementary mechanisms for carrying messages between sessions:

- **Clipboard** for small grabs (1-5 messages): select, copy, paste into
  the other session. Include source labels so the receiving session knows
  where the context came from.

- **Temp files** for larger context blocks (10+ messages): export to
  `/tmp/<tool-name>/<id>.md`, display the full path, let the user
  reference it via file read in the receiving session.

Key: temp files go in `/tmp/`, NOT in the repo or config directory.
They are ephemeral references, auto-cleaned on reboot. The absolute path
makes them referenceable from any session on the machine.
