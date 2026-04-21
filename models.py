"""Pydantic models for ThreadHop — the validation boundary for JSONL
transcript parsing and the typed shapes that back SQLite rows.

Task #24 (data model hardening) introduces this module so:

- **Enums are enforced in two places.** Session status, message role, and
  memory type use :data:`typing.Literal` aliases here and CHECK constraints
  (or the equivalent) in ``db.py``. A bad value is a type error statically
  *and* a DB error at runtime.
- **JSONL parsing has a typed boundary.** The indexer (task #8) folds over
  instances of :class:`UserTranscriptLine` / :class:`AssistantTranscriptLine`,
  not raw dicts. Malformed or schema-drifted lines fail loudly via
  :func:`parse_transcript_line` (log + skip), so one bad line doesn't
  silently corrupt the search index.
- **Python types and SQL schemas evolve together.** Each DB-row model here
  (:class:`Session`, :class:`Message`, :class:`Bookmark`, :class:`MemoryEntry`)
  pairs with a SQL migration in ``db.py``. When a migration adds or renames
  a column, update the matching model in the same change.

See ADR-001 (SQLite storage), ADR-003 (assistant chunk merging), and ADR-004
(session status enum) in ``docs/DESIGN-DECISIONS.md``.
"""

from __future__ import annotations

import json
import logging
from typing import Annotated, Any, Literal, Union

from pydantic import BaseModel, ConfigDict, Field, ValidationError

log = logging.getLogger("threadhop.models")


# --- Enum-like Literal aliases -------------------------------------------
# `Literal` (not `StrEnum`) so mypy/pyright narrow on raw strings and
# Pydantic rejects unknown values at parse time without forcing callers
# to import/construct enum members.

SessionStatus = Literal[
    "active",
    "in_progress",
    "in_review",
    "done",
    "archived",
]
"""Kanban-style session status (ADR-004).

Enforced at the DB layer by the CHECK constraint in migration 006. Keep
this list and the CHECK in ``db._migration_006_sessions_status_check`` in
lockstep — they encode the same invariant on opposite sides of the wire.
"""

MessageRole = Literal["user", "assistant"]
"""The two roles we index for search. System/tool-result metadata rides
along on user lines but is not its own role in the indexed view."""

MemoryType = Literal["decision", "todo", "done", "adr", "observation"]
"""Typed entries in the project memory ledger (ADR-005)."""

MemorySource = Literal["explicit", "auto"]
"""Whether a memory entry came from a human annotation or the auto-observer."""

BookmarkKind = Literal["bookmark", "research"]
"""Built-in bookmark classes for the initial chat-ingest pathway.

Task #59 is expected to generalize this into SQL-backed category rows. Until
then, keep this Literal and the CHECK in ``db.py`` in lockstep.
"""


# --- DB row shapes -------------------------------------------------------
# One Pydantic model per SQL table. The model is the authoritative Python
# view of a row; callers should not pass raw dicts around the app.


class Session(BaseModel):
    """One row of the ``sessions`` table (migrations 001 + 006).

    Migration 006 adds a CHECK constraint on ``status`` that mirrors
    :data:`SessionStatus`.
    """

    model_config = ConfigDict(extra="forbid")

    session_id: str
    session_path: str
    project: str | None = None
    cwd: str | None = None
    custom_name: str | None = None
    status: SessionStatus = "active"
    sort_order: int | None = None
    last_viewed: float | None = None
    created_at: float | None = None
    modified_at: float | None = None


class Message(BaseModel):
    """One row of the ``messages`` table (migrations 002 + 003).

    Represents one *logical* message. For assistant messages this is the
    result of merging all JSONL chunks that share the same ``message_id``
    (ADR-003); ``uuid`` is the first chunk's UUID and becomes the PK.

    ``message_id`` (added in migration 003) stores the Claude API
    ``message.id`` shared across streaming chunks — needed for cross-batch
    chunk merging when an assistant response spans two refresh cycles.
    """

    model_config = ConfigDict(extra="forbid")

    uuid: str
    session_id: str
    role: MessageRole
    text: str
    timestamp: str | None = None
    cwd: str | None = None
    parent_uuid: str | None = None
    is_sidechain: bool = False
    message_id: str | None = None


class Bookmark(BaseModel):
    """One row of the categorized ``bookmarks`` table."""

    model_config = ConfigDict(extra="forbid")

    id: int | None = None  # autoincrement — None when inserting
    message_uuid: str
    session_id: str
    category_id: int
    note: str | None = None
    created_at: float
    researched_at: float | None = None


class BookmarkCategory(BaseModel):
    """One row of the ``bookmark_categories`` table."""

    model_config = ConfigDict(extra="forbid")

    id: int | None = None
    name: str
    research_prompt: str | None = None
    created_at: float
    updated_at: float


class MemoryEntry(BaseModel):
    """One row of the ``memory`` table (created by the migration for task #19).

    Append-only ledger (ADR-005). ``resolved`` is stored as 0/1 in SQL;
    exposed as ``bool`` here.
    """

    model_config = ConfigDict(extra="forbid")

    id: int | None = None
    project: str
    type: MemoryType
    text: str
    session_id: str | None = None
    source: MemorySource = "explicit"
    resolved: bool = False
    created_at: float


# --- JSONL content blocks ------------------------------------------------
# Claude Code transcripts put three block shapes inside ``message.content``.
# We validate them as a discriminated union on the ``type`` field so a
# mismatch surfaces as a ValidationError — not a silent AttributeError on
# ``.get("name")`` three frames deep in the indexer.
#
# ``extra='ignore'`` on blocks so fields Claude Code adds over time
# (``cache_control``, ``citations``, ...) don't reject otherwise-valid lines.


class TextBlock(BaseModel):
    """A plain-text content block. Indexed as search material."""

    model_config = ConfigDict(extra="ignore")

    type: Literal["text"]
    text: str


class ToolUseBlock(BaseModel):
    """An assistant tool invocation. ``input`` is tool-specific."""

    model_config = ConfigDict(extra="ignore")

    type: Literal["tool_use"]
    id: str
    name: str
    input: dict[str, Any] = Field(default_factory=dict)


class ToolResultBlock(BaseModel):
    """A tool result routed back to the model on a user line.

    ``content`` may be a plain string (most tools) or a list of inner
    blocks (e.g. multi-part outputs). We keep the list branch as opaque
    dicts — reaching further in is the indexer's job, and the shape
    varies per tool.
    """

    model_config = ConfigDict(extra="ignore")

    type: Literal["tool_result"]
    tool_use_id: str
    content: str | list[dict[str, Any]] = ""
    is_error: bool | None = None


ContentBlock = Annotated[
    Union[TextBlock, ToolUseBlock, ToolResultBlock],
    Field(discriminator="type"),
]
"""Discriminated union over the three block shapes. Pydantic picks the
right class by inspecting the ``type`` field, so callers get a concrete
subclass without writing their own branching."""


# --- JSONL message payloads ----------------------------------------------


class UserMessagePayload(BaseModel):
    """The ``message`` object on a user-type JSONL line.

    User content is either a plain string (normal prompts) or a list of
    blocks — the list form carries ``tool_result`` blocks routed back to
    the model.
    """

    model_config = ConfigDict(extra="ignore")

    role: Literal["user"]
    content: str | list[ContentBlock]


class AssistantMessagePayload(BaseModel):
    """The ``message`` object on an assistant-type JSONL line.

    Multiple consecutive assistant lines share the same ``id`` when the
    model streams text + tool_use in chunks — the indexer merges by this
    id (ADR-003). ``id`` is marked optional so that if Claude Code ever
    ships an assistant line without one, we don't drop the line entirely;
    the indexer falls back to grouping by the line's own ``uuid``.
    """

    model_config = ConfigDict(extra="ignore")

    id: str | None = None
    role: Literal["assistant"]
    model: str | None = None
    content: list[ContentBlock]


# --- JSONL transcript line wrappers --------------------------------------
# Every JSONL line carries threading/metadata the indexer needs (uuid,
# parentUuid, timestamp, ...). The ``type`` discriminator picks the
# payload shape.
#
# ``populate_by_name`` lets callers use snake_case or camelCase.
# ``extra='ignore'`` tolerates JSONL fields we don't model today
# (``gitBranch``, ``version``, ``requestId``, ``userType``, ...) without
# rejecting lines — Claude Code adds these over time and we don't want
# the parser to brittle-fail on additions.


class _TranscriptLineBase(BaseModel):
    model_config = ConfigDict(populate_by_name=True, extra="ignore")

    uuid: str
    parent_uuid: str | None = Field(None, alias="parentUuid")
    session_id: str | None = Field(None, alias="sessionId")
    timestamp: str | None = None
    cwd: str | None = None
    is_sidechain: bool = Field(False, alias="isSidechain")


class UserTranscriptLine(_TranscriptLineBase):
    """A ``type: 'user'`` JSONL line.

    Regular user prompts have ``message.content`` as a string. Tool-result
    lines carry the structured result at top-level as ``toolUseResult``
    *and* inside ``message.content`` as ``tool_result`` blocks; both are
    preserved so the indexer can pick whichever shape it prefers.
    """

    type: Literal["user"]
    message: UserMessagePayload
    tool_use_result: dict[str, Any] | str | None = Field(
        None, alias="toolUseResult"
    )


class AssistantTranscriptLine(_TranscriptLineBase):
    """A ``type: 'assistant'`` JSONL line."""

    type: Literal["assistant"]
    message: AssistantMessagePayload


TranscriptLine = Annotated[
    Union[UserTranscriptLine, AssistantTranscriptLine],
    Field(discriminator="type"),
]
"""Validated conversation line (user or assistant). Non-conversation line
types (``summary``, ``ai-title``, ``custom-title``, ...) return ``None``
from :func:`parse_transcript_line` and never reach the indexer."""


# --- Parser entry point --------------------------------------------------

# Line types we deliberately ignore: they carry UI/metadata affordances
# that are not part of the indexed conversation. Kept as a frozenset so
# lookups stay O(1) and the set is immutable.
_NON_MESSAGE_TYPES = frozenset(
    {"summary", "ai-title", "custom-title", "system", "meta"}
)


def parse_transcript_line(
    raw: str | bytes,
    *,
    session_path: str | None = None,
    line_number: int | None = None,
) -> UserTranscriptLine | AssistantTranscriptLine | None:
    """Validate one JSONL line and return a typed model, or ``None`` to skip.

    Returns ``None`` (with a logged warning) for:

    - lines that are not valid JSON
    - lines whose top-level is not a JSON object
    - lines whose ``type`` is not ``user``/``assistant``
      (summary/title/system/meta/unknown)
    - lines that fail Pydantic validation (schema drift, malformed content)

    The indexer folds over the non-``None`` results — it never touches raw
    dicts, and it never crashes on a single bad line. ``session_path`` and
    ``line_number`` are threaded through only so the log line points at
    the offending file position.
    """
    try:
        data = json.loads(raw)
    except (json.JSONDecodeError, TypeError) as e:
        log.warning(
            "skipping malformed JSONL line %s:%s — %s",
            session_path or "<unknown>",
            line_number if line_number is not None else "?",
            e,
        )
        return None

    if not isinstance(data, dict):
        log.warning(
            "skipping non-object JSONL line %s:%s (got %s)",
            session_path or "<unknown>",
            line_number if line_number is not None else "?",
            type(data).__name__,
        )
        return None

    msg_type = data.get("type")
    if msg_type in _NON_MESSAGE_TYPES:
        return None
    if msg_type not in ("user", "assistant"):
        # Unknown types: log at debug so schema drift surfaces in -v output
        # without drowning the default log at warning level.
        log.debug(
            "ignoring unknown JSONL type %r at %s:%s",
            msg_type,
            session_path or "<unknown>",
            line_number if line_number is not None else "?",
        )
        return None

    try:
        if msg_type == "user":
            return UserTranscriptLine.model_validate(data)
        return AssistantTranscriptLine.model_validate(data)
    except ValidationError as e:
        # Fail loudly but don't crash the indexer — one bad line must not
        # wedge the whole background refresh cycle.
        log.warning(
            "skipping JSONL line that failed validation at %s:%s — %s",
            session_path or "<unknown>",
            line_number if line_number is not None else "?",
            e,
        )
        return None


__all__ = [
    # Literal aliases
    "SessionStatus",
    "MessageRole",
    "MemoryType",
    "MemorySource",
    "BookmarkKind",
    # DB row models
    "Session",
    "Message",
    "Bookmark",
    "BookmarkCategory",
    "MemoryEntry",
    # JSONL block models
    "TextBlock",
    "ToolUseBlock",
    "ToolResultBlock",
    "ContentBlock",
    # JSONL payload / line models
    "UserMessagePayload",
    "AssistantMessagePayload",
    "UserTranscriptLine",
    "AssistantTranscriptLine",
    "TranscriptLine",
    # Parser
    "parse_transcript_line",
]
