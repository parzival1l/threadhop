# Task 054 — Transcript harness adapters

**Reference:** [ADR-028 in `DESIGN-DECISIONS.md`](./DESIGN-DECISIONS.md#adr-028-harness-adapter-seam-single-concrete-adapter)

Read ADR-028 first, then read this doc. ADR-028 is about the existing
`threadhop_core/harness/` seam for invoking `claude -p` during
observation, reflection, and handoff generation. This task is about a
different problem: ingesting session transcripts from multiple external
harnesses such as Claude Code, Codex CLI, Gemini CLI, Cline, Cursor,
Aider, and OpenCode. The naming overlap is real, so this plan uses the
phrase **transcript harness adapter** deliberately to keep the scope
clear.

---

## Objective

Move ThreadHop from a Claude Code transcript browser into a
multi-harness transcript system without rewriting search, observation,
handoff, or the UI for each provider. The goal is not to make every
harness behave identically at the storage layer. The goal is to let each
harness keep its own file format and storage path while ThreadHop owns a
single normalized session and message model above that layer.

At the end of this task, ThreadHop should be able to discover sessions
from more than one harness, parse them through harness-specific adapter
code, normalize them into the same SQLite-backed model, and present them
through the same TUI, CLI, and future local web UI. A user should be
able to search across Claude Code and another supported harness in one
query, browse both in one session list, and use the same bookmarks,
observations, and handoff workflows wherever the source transcript shape
contains enough information to support them.

---

## Intention

The intention of the transcript harness adapter work is to isolate the
parts of ThreadHop that are truly provider-specific. Those parts are
session discovery, transcript parsing, tool-call normalization,
message-role mapping, and any per-provider cleaning rules needed to turn
raw transcript bytes into the cleaned conversation view that ThreadHop
indexes and renders. Everything above that seam should become
provider-agnostic. Search should not know whether a row came from Claude
Code or OpenCode. The observer should not need a second implementation
for Codex. The future fork and merge-back work should attach to the
normalized session graph, not to one vendor's raw JSONL shape.

This task is also meant to stop provider growth from spreading conditionals
through the codebase. Right now the repo is still shaped around Claude
Code as the one source of truth. If multi-harness support is added by
sprinkling `if provider == ...` checks through the indexer, TUI, CLI,
and observation pipeline, the product will become harder to reason about
with every new source. The adapter boundary exists to keep that entropy
contained. Adding a new harness should feel like adding one new module
and wiring it into discovery, not reopening every feature layer.

---

## End Product

The end product is a ThreadHop core with two clear levels. The lower
level consists of one transcript harness adapter per source system.
Each adapter knows where that harness stores sessions, how to enumerate
them, how to parse its transcript format, how to identify the session
and project, and how to map raw records into ThreadHop's canonical
session and message shapes. The upper level consists of the existing
ThreadHop features operating only on canonical data: SQLite indexing,
FTS search, observation extraction, handoff generation, bookmarking,
status tagging, export, and UI rendering.

In the finished system, every indexed session row carries enough source
metadata to stay explainable. Users should be able to tell which harness
produced a session, where it came from on disk, and which features are
fully supported for that source. The app should degrade honestly when a
harness does not provide something Claude Code does provide. For
example, if a harness lacks stable tool-result structure or active
session detection hooks, ThreadHop should still index and display the
conversation while marking advanced capabilities as unavailable rather
than faking parity.

The end product is therefore not just “more parsers.” It is a provider-
agnostic data model, a stable adapter seam, and a capability-aware user
experience that can grow to multiple harnesses without diluting the core
product.

---

## What This Task Includes

This task includes extracting the existing Claude Code transcript logic
behind a formal transcript adapter seam and treating Claude as the first
real implementation of that seam. It includes defining the canonical
session and message model that all adapters must produce, including the
minimum metadata required by the existing ThreadHop surfaces: session
id, provider, project, cwd when known, timestamps, role, text, tool
artifacts when available, and enough stable identity to support
incremental indexing and bookmarks.

It also includes deciding how ThreadHop expresses harness capabilities.
Some features are universal, such as transcript browsing and text
search. Some are conditional, such as active-session detection,
reply-in-place, or exact tool-call rendering. The adapter layer needs a
small capability surface so the rest of the app can ask what is safe to
offer for a given session instead of inferring it from provider names.

This task includes a migration path for the current Claude-only code.
The Claude parser and discovery logic should become the baseline adapter
without changing user-visible Claude behavior. Multi-harness support
should begin by proving that the seam can hold Claude cleanly, then add
the second adapter that makes the abstraction real.

---

## What This Task Does Not Include

This task does not require a language port, a UI rewrite, or an OpenCode
integration inside their repository. It does not require every harness
to support every ThreadHop feature on day one. It also does not require
live multi-user collaboration, LAN-hosted shared editing, or two-person
continuation of a single active chat session. Those are separate product
problems and should be evaluated after the transcript model is stable.

It also does not replace ADR-028's existing `threadhop_core/harness/`
module. That module remains the outbound LLM invocation seam for
observer, reflector, and handoff prompts. This task introduces a second
seam on the inbound side: transcript discovery and normalization.

---

## Proposed Shape

The clean shape for this work is a new transcript-facing adapter layer
owned by ThreadHop itself. Each adapter should expose three responsibilities
in practice: discover sessions, describe session metadata, and parse a
session into canonical messages. The rest of the application should stop
reading provider-specific transcript formats directly. The indexer, TUI,
CLI queries, and future web server should all consume the normalized
result.

The first milestone is not “support all providers.” The first milestone
is “Claude Code now goes through the same adapter contract that future
providers will use.” Once that is true, the second milestone is to add a
second concrete adapter, because that is the point where the seam stops
being hypothetical. Codex CLI and OpenCode are good candidates for that
second adapter because they are structurally close to the use case that
already made ThreadHop valuable: local coding-agent transcripts with
tool activity and persistent session files.

After the second adapter exists, the rest of the roadmap becomes much
clearer. Search improvements, fork lineage, merge-back summaries, and a
local web UI all benefit from one normalized session graph more than
from any one provider-specific integration.

---

## Acceptance Criteria

This task is complete when ThreadHop has an explicit transcript harness
adapter seam, Claude Code is migrated onto it with no user-facing
regression, and adding a second harness no longer requires changes
across unrelated feature layers. At that point, the codebase should have
one place to add provider-specific discovery, one place to add
provider-specific parsing, and one normalized model that the rest of the
app trusts.

From a user perspective, the feature is successful when ThreadHop can
show sessions from multiple harnesses in one product, search across them
with the same interface, and surface the origin of each session clearly.
From a maintainer perspective, the feature is successful when adding the
next harness feels incremental instead of architectural.
