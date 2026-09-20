# Transcript formats and T3 Code: research inventory

Research date: 2026-09-20. Status: research inventory with user-confirmed initial
scope below; not an approved schema or implementation plan.

## Initial scope — user decisions

- Initial source scope: Claude Code, Codex, OpenCode, and Cursor. Pi is deferred;
  its research below is retained only as background.
- `peek --last 1` means one turn: the last human prompt through its associated
  last assistant response. Intervening tool cycles do not create new turns.
- Peek and bookmarks cover main-agent messages only. Subagent messages are out
  of scope for these features initially.
- Collect evidence and reuse existing work before inventing abstractions.
  A universal shared model is not a prerequisite.
- Branch reconstruction, exact model-context replay, and a general framework
  for unknown events are deferred. They do not block the basic version.

The practical goal is listing sessions, reading turns, copying selected text,
and bookmarking main-agent messages. Internal parsing details are engineering
responsibilities, not a questionnaire the user must finish first.

## Purpose and scope

Collect the formats and implementation differences before choosing ThreadHop's
architecture. A shared model is an option to evaluate, not a requirement or an
approved decision. Potential consumers include session listing, peek, message
export/copy, bookmarks, search, MCP, and a future UI.

Research covers Claude Code, Codex, OpenCode, Pi, and Cursor. Local inspection
is read-only and reports structural information rather than personal transcript
content. Upstream branch URLs are moving references. Pin source revisions and
record fixture versions before implementing adapters. Observed storage details
are not promises of permanent compatibility.

## Claude Code

Official documentation describes project transcript JSONL, nested subagent
transcripts, and separate tool-result files. Discovery must handle configurable
roots and superseded files.

Sources:
- [Directory layout](https://code.claude.com/docs/en/claude-directory)
- [Subagent transcripts](https://code.claude.com/docs/en/sub-agents#resume-subagents)

Local structural sample: 54 files, including 19 subagent files, with reported
producer versions spanning 2.1.149–2.1.263. These details are observations:

- Distinct record UUIDs can share an assistant `message.id`. A record is not a
  whole logical message. Repeated usage snapshots must not be summed per row.
- Subagents can share the parent's `sessionId`; `agentId` and stream provenance
  distinguish their conversation. Session ID alone is not a sufficient key.
- A `user` record can contain a tool result rather than a human prompt.
- Content includes text, thinking, tool use, tool results, and images.
- Top-level `attachment` can mean injected context such as reminders; it is not
  synonymous with a user-uploaded file.
- `parentUuid` and observed continuation references carry relationships that
  flattening would discard.

Exact compaction disk records were not present in this bounded local sample.
Hook documentation must not be substituted for evidence of a disk schema.

## Codex

Rollouts contain several record families, including metadata, response items,
events, turn context, and usage. Current persistence policy distinguishes legacy
history from paginated history with completed turn items. Corresponding event
and response records can describe the same content; normalization needs an
adapter-specific selection/reconciliation policy, not text-based deduplication.

Sources:
- [Persistence policy](https://github.com/openai/codex/blob/main/codex-rs/rollout/src/policy.rs)
- [Rollout recorder](https://github.com/openai/codex/blob/main/codex-rs/rollout/src/recorder.rs)
- [Content and response models](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/models.rs)

Structural inspection of 20 recent and 20 earlier local files found versions
0.105–0.112 and 0.155 alpha variants. Older response records lacked native item
IDs; recent examples had them. A universal required native message ID would
exclude supported history.

Keep commentary/final phase when available, tool call/output links, native turn
IDs when available, and distinctions between readable and opaque reasoning.
Source file identity and thread identity differ: current recorder supports
history bases, forks, and replacement rollouts. Preserve provenance when one
normalized item draws from multiple source records.

Unresolved: exact legacy correspondence rules, full revert/compaction replay,
and all history-base cases. These need targeted fixtures before implementation.

## OpenCode

V2 source describes SQLite `session_v2` and `session_message` projections.
Messages have IDs, session ownership, explicit sequence ordering, type, and JSON
payload. Assistant content contains ordered text, reasoning, and tools. Tool
state changes and streaming can update existing messages.

The shared ingestion contract must support updates, not just appended records.
Use a consistent read-only database view or a versioned API; pagination alone
does not guarantee a complete change feed.

Sources:
- [SQL schema](https://github.com/anomalyco/opencode/blob/v2/packages/core/src/session/sql.ts)
- [Message schemas](https://github.com/anomalyco/opencode/blob/v2/packages/schema/src/session-message.ts)
- [Message updater](https://github.com/anomalyco/opencode/blob/v2/packages/core/src/session/message-updater.ts)
- [Store](https://github.com/anomalyco/opencode/blob/v2/packages/core/src/session/store.ts)

Child-session relationships and fork ancestry have separate representations.
Compaction changes effective model context; it does not make original history
and effective context the same view. Files can carry MIME and origin metadata.

Sources:
- [Session relationships](https://github.com/anomalyco/opencode/blob/v2/packages/schema/src/session.ts)
- [History projection](https://github.com/anomalyco/opencode/blob/v2/packages/core/src/session/history.ts)
- [Prompt attachments](https://github.com/anomalyco/opencode/blob/v2/packages/schema/src/prompt.ts)

V1 uses a different message/part representation. The older file named
`message-v2.ts` does not establish that it represents OpenCode 2.0. This research
does not verify a universal beta database filename; discovery needs validation
against the installed release and configuration.

## Pi — deferred, background research only

Local inspection found 48 version-3 files under `~/.pi/agent/sessions/`.
Observed message roles were user, assistant, and tool result. Model and
thinking-level changes also appeared; compaction/custom records were not
observed locally in this sample.

Upstream defines a session header and parent-linked entries. Branches are paths
through that tree, not simply contiguous ranges of file lines. A branch change
can exist only in memory until another append, so a passive reader cannot
always establish the live selected branch. Compaction-aware context is a
separate projection. Extension state and custom messages have different context
participation; visibility does not imply participation. Opening old sessions
through Pi's manager can migrate/rewrite them, so use a read-only reader.

[Session manager](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/src/core/session-manager.ts)

Assistant tool calls link to separate tool-result messages through call IDs.
Content can include text, thinking, and images. Message time and entry time have
different representations and should not be silently conflated. Model usage and
upstream response ID are metadata, not durable local entry identity.

[AI message types](https://github.com/badlogic/pi-mono/blob/main/packages/ai/src/types.ts)

Runtime streaming events are distinct from persisted messages: normal messages
are appended on message-end. A disk reader cannot promise token-level updates.

[Persistence integration](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/src/core/agent-session.ts)

Illustrative tree: human entry `a` has assistant/tool descendants `b → c`, and
another descendant `d` starts a different path from `a`. Reading `a,b,c,d` as one
linear conversation would combine alternatives.

## Cursor

Official documentation describes local desktop SQLite history and Markdown
export. CLI output documentation describes an interface, not a comprehensive
contract for local database contents.

Sources:
- [Desktop history](https://docs.cursor.com/en/agent/chat/history)
- [CLI usage](https://docs.cursor.com/en/cli/using)
- [CLI output formats](https://docs.cursor.com/en/cli/reference/output-format)

Local findings below are undocumented, version-sensitive observations:

- Desktop `globalStorage/state.vscdb` contains `ItemTable`, `cursorDiskKV`, and
  `composerHeaders`; workspace databases also contain composer metadata.
- Composer records reference ordered bubble IDs. Bubble records can include
  text, thinking, attachments, timestamps, tool calls/results/status, and usage.
  Preserve native composer, bubble, and tool-call IDs when available.
- Checkpoints examined described file state/diffs. Their existence is not
  evidence of conversation branches or compaction semantics.
- CLI `~/.cursor/chats/…/store.db` uses blob and metadata tables. Sampled blobs
  mix JSON messages and opaque binary data. Complete reconstruction was not
  established; one database could not be opened read-only and was left alone.
- 184 JSONL files were found under project `agent-transcripts` directories.
  Bounded samples included role/message/content records and other shapes.
  Sampled records lacked top-level durable message IDs/timestamps. Do not infer
  the full fidelity of desktop storage from a transcript projection.

Desktop storage, CLI storage, and transcript exports need distinct source-kind
handling, even if implemented within one Cursor adapter package. Represent
missing data and unknown records honestly. Mutable database rows require
reconciliation; transcript projections need file-specific cursors and rewrite
handling. Full branch and compaction reconstruction remains unresolved.

The SDK documents a configurable JSONL local store. This is a further source
surface, not proof that desktop or CLI uses it by default.

[SDK documentation](https://cursor.com/docs/sdk/typescript)

## T3 Code comparison

Inspected the adjacent checkout at commit `b73232bdd` (2026-08-12), whose
upstream remote points to `pingdotgg/t3code`, and checked the current upstream
driver list. Local drivers: Codex, Claude, Cursor, Grok, OpenCode. Current
upstream also includes Antigravity. Neither inspected list includes Pi.

[Current driver registry](https://github.com/pingdotgg/t3code/blob/main/apps/server/src/provider/builtInDrivers.ts)

T3 has multiple shared contracts rather than one exhaustive transcript model:

- `ProviderAdapterShape` describes runtime operations: start/send/interrupt,
  approvals, snapshots, and event streams.
- `ProviderRuntimeEvent` uses Effect schemas for shared event kinds and IDs,
  with optional provider-native references and raw payloads.
- `OrchestrationMessage` is a smaller client-facing shape: identity, role,
  text, attachments, turn reference, streaming flag, and timestamps. Activity
  and other orchestration state live outside this simple message record.
- Snapshot turn items can remain `unknown`; not every provider object is
  forced into the same deep structure at every boundary.

The inspected adapters integrate live protocols: Codex app-server, Claude SDK,
Cursor ACP, and OpenCode SDK. Cursor's `readThread` returns tracked session
turns; OpenCode's implementation calls `session.messages`. This is not evidence
of a universal importer for arbitrary pre-existing on-disk histories.

Local reference files (relative to the adjacent `t3code` repository):
- `docs/internals/providers.md`
- `apps/server/src/provider/Services/ProviderAdapter.ts`
- `packages/contracts/src/providerRuntime.ts`
- `packages/contracts/src/orchestration.ts`
- `apps/server/src/provider/Layers/CursorAdapter.ts`
- `apps/server/src/provider/Layers/OpenCodeAdapter.ts`

Lesson to evaluate: share contracts where consumers need consistent behaviour,
while retaining provider-specific data elsewhere. T3's full orchestration and
checkpoint machinery serves agent control; it is not automatically needed for
ThreadHop's historical retrieval use case.

## Optional future considerations — not prerequisites

These are proposals for discussion, not implementation decisions:

1. Separate source records from normalized conversation items. Preserve one or
   more source references for each logical item.
2. Give source instance, session, execution/subagent scope, message, and tool
   invocation distinct identities. Do not use screen position as identity.
3. Separate role from origin: user-shaped source data may be tool output or
   injected context, not a human prompt.
4. Preserve structured content and derive cleaned text for each consumer.
5. Preserve ordering and lineage independently of wall-clock timestamps.
6. Define a current conversation path separately from complete source history
   and the context a provider actually sent to its model.
7. Treat exchanges as derived selections. Native turns can inform them but
   should not silently dictate an identical meaning across providers.
8. Represent unavailable information explicitly. Missing completion evidence
   is not proof that a tool failed or is still running.
9. Decide whether a bookmark follows the latest message state, saves a snapshot,
   or both. A stable target plus a saved excerpt can reveal later changes.
10. Preserve source kind and available fidelity. An exported transcript may
    support text retrieval without supporting exact timestamps or tool state.

Effect Schema can express validated tagged variants and readonly data after
these meanings are agreed. It cannot decide the semantics for us. Source
reading and persistence belong outside pure conversation transformations.

## Reuse before rebuilding

The Python implementation already shares cleaned-row iteration between copy
and exchange loading (`copier.py` and `exchanges.py`). Its behavior and fixtures
are useful references for a TypeScript implementation; a port still requires
implementation work and is not direct reuse of Python functions.

T3's contracts and adapter mappings are precedents to inspect or selectively
adapt. Its live-provider orchestration is not a drop-in importer for arbitrary
historical transcripts. Do not bring its server, reactors, and checkpointing
machinery into ThreadHop solely to retrieve conversation text.

Implement one retrieval path first, then reuse it for peek and copy. Bookmark
references should resolve to those same main-agent messages. Add abstractions
only where the second source or a real consumer demonstrates a need.

For the initial reader, assemble fragmented messages without duplicating them,
skip unneeded event kinds with diagnostics, and leave native source data
untouched. These are modest adapter behaviours, not a reason to design a
universal history engine. Unsupported history layouts can be reported as such
until their support is required.
