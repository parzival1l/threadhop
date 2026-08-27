"""Per-session digest extracted from a Claude Code JSONL transcript.

The digest is the data shape behind the right-hand sidebar in the TUI
(``SessionDigestBar``). It is a single read-only snapshot of everything
the bar wants to show, computed in one pass over the JSONL.

Why a separate module rather than extending the indexer:

- The indexer's job is the FTS view of conversation lines. It deliberately
  drops every non-message line type (``_NON_MESSAGE_TYPES`` in ``models.py``).
  The digest reads exactly those dropped lines (``ai-title``, ``custom-title``,
  ``pr-link``, ``last-prompt``, ``permission-mode``, ``system`` subtypes),
  plus a few ambient fields on user/assistant lines (``slug``, ``gitBranch``,
  ``version``, ``message.model``, ``message.usage``).
- Keeping the extractor outside ``tui/`` lets the HTTP sidecar reuse it
  later without pulling Textual into ``server/app.py``.

The four-band recap timeline (Started → Earlier → Recently → Last asked)
is built here too, because it depends on the same single-pass read.
"""

from __future__ import annotations

import json
import logging
import re
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

log = logging.getLogger("threadhop.session.digest")


# Which tool_use names imply a file was touched. Edit/Write/MultiEdit cover
# the common cases; NotebookEdit is rare but cheap to include.
_FILE_TOUCH_TOOLS = frozenset({"Edit", "Write", "MultiEdit", "NotebookEdit"})

# Default context window when we can't infer one from the model id.
# Every Claude 4.x model is ≥200k; the [1m] suffix on opus-4-7 lifts it
# to 1M. We don't try to enumerate every model — just spot the one
# distinguishing flag and let the rest fall through to the safe default.
_DEFAULT_CONTEXT_WINDOW = 200_000


def _model_context_window(model: str | None) -> int:
    """Return the context window size in tokens for a given model id."""
    if not model:
        return _DEFAULT_CONTEXT_WINDOW
    if "[1m]" in model.lower() or model.lower().endswith("-1m"):
        return 1_000_000
    return _DEFAULT_CONTEXT_WINDOW


# Regexes for cleaning the first user prompt before it becomes the
# "Started" recap band. The patterns target the agent-injected
# envelopes that show up in real transcripts (system reminders,
# task-notification XML, "[From …]" previews) — they stay anchored
# at start-of-string so they only fire on prefix noise, never on
# legitimate body text.
_NOISE_PREFIX_PATTERNS = [
    re.compile(r"^\s*<task-notification>.*?</task-notification>\s*", re.DOTALL),
    re.compile(r"^\s*<system-reminder>.*?</system-reminder>\s*", re.DOTALL),
    re.compile(r"^\s*\[From\s+\".*?\"\s*[—-]\s*[^\]]*\]\s*"),
]

# Maximum length of the cleaned Started band. Long enough for a real
# question, short enough that the body never overflows two visual rows
# at the bar's 36-cell width.
_STARTED_MAX_CHARS = 140


def _clean_first_prompt(text: str) -> str:
    """Strip agent-injected envelope noise and take a single-sentence headline.

    Real transcripts often start with ``<task-notification>`` XML, a
    ``[From "..." — path]`` preview, or other agent scaffolding. Those
    prefixes don't read as a useful "what did the user originally ask"
    headline; they're just framing. We strip the known patterns, then
    take the first non-empty line, then truncate at the first sentence
    boundary or 140 chars — whichever comes first.
    """
    cleaned = text.strip()
    for pattern in _NOISE_PREFIX_PATTERNS:
        cleaned = pattern.sub("", cleaned, count=1).strip()

    # First non-empty line.
    for line in cleaned.splitlines():
        if line.strip():
            cleaned = line.strip()
            break

    # First sentence — split on the earliest of '.', '?', '!' if it ends
    # with whitespace (so URLs and version numbers don't trigger).
    match = re.search(r"[.!?](?:\s|$)", cleaned)
    if match and match.start() < _STARTED_MAX_CHARS:
        cleaned = cleaned[: match.end()].rstrip()
    if len(cleaned) > _STARTED_MAX_CHARS:
        # Cut at the last word boundary before the cap so we don't
        # truncate mid-word.
        snippet = cleaned[:_STARTED_MAX_CHARS]
        space = snippet.rfind(" ")
        if space > _STARTED_MAX_CHARS - 30:
            snippet = snippet[:space]
        cleaned = snippet.rstrip(" ,;:.") + "…"
    return cleaned


@dataclass
class RecapEntry:
    """One band of the recap timeline.

    ``label`` is the band name shown in the bar (``"Started"``,
    ``"Earlier"``, ``"Recently"``, ``"Last asked"``). ``timestamp`` is the
    line's ISO timestamp where available — used for the small relative
    age suffix. ``text`` is the recap body, untruncated; the widget
    decides how many lines to show.
    """

    label: str
    timestamp: str | None
    text: str


@dataclass
class SessionDigest:
    """One session, summarized for the sidebar in a single pass."""

    session_id: str

    # Identity / context line
    custom_title: str | None = None
    ai_title: str | None = None
    slug: str | None = None
    branch: str | None = None
    cwd: str | None = None

    # Wall-clock duration in seconds, computed from first to last
    # message timestamps observed in the JSONL.
    duration_seconds: int | None = None

    # Recap bands, in display order. Empty when nothing is available.
    recap: list[RecapEntry] = field(default_factory=list)

    # Outputs
    pr_number: int | None = None
    pr_url: str | None = None
    pr_repository: str | None = None
    files_touched: set[str] = field(default_factory=set)

    # Token usage (sums across the session). Cache hit ratio is computed
    # by the consumer from cache_read / (cache_read + cache_creation).
    total_input_tokens: int = 0
    total_output_tokens: int = 0
    total_cache_read_tokens: int = 0
    total_cache_creation_tokens: int = 0

    # The size of the prompt at the *latest* assistant turn — the
    # number that drives the "context fill" gauge in the bar.
    # Computed as latest_usage.input_tokens + cache_read + cache_creation
    # because all three are part of the prompt the model saw on that
    # turn. Cumulative session totals live in the ``total_*`` fields
    # above and are independent of context-window pressure.
    latest_turn_input_tokens: int = 0

    # Context window in tokens for the *latest* assistant model.
    # Defaults to 200k; bumped to 1M when the model id carries the
    # ``[1m]`` suffix (opus-4-7 long-context). The bar uses this as
    # the denominator of the context-fill ratio.
    context_window: int = _DEFAULT_CONTEXT_WINDOW

    # Models actually used in this session, in first-seen order. Useful
    # when a session has both Opus turns (chat) and Haiku turns
    # (skill subcalls).
    models_used: list[str] = field(default_factory=list)

    # Footer
    permission_mode: str | None = None
    client_version: str | None = None

    @property
    def files_touched_count(self) -> int:
        return len(self.files_touched)

    @property
    def cache_hit_ratio(self) -> float | None:
        denom = self.total_cache_read_tokens + self.total_cache_creation_tokens
        if denom == 0:
            return None
        return self.total_cache_read_tokens / denom

    @property
    def total_tokens(self) -> int:
        return (
            self.total_input_tokens
            + self.total_output_tokens
            + self.total_cache_read_tokens
            + self.total_cache_creation_tokens
        )

    @property
    def total_input_tokens_billed(self) -> int:
        """Cumulative TOTAL input billed across the session.

        ``total_input_tokens`` alone is just the new (uncached) input;
        the cache-read and cache-creation tokens are also part of every
        turn's prompt. The headline "input" number on the bar should
        be all three combined so the user sees the real input weight,
        not just the uncached slice.
        """
        return (
            self.total_input_tokens
            + self.total_cache_read_tokens
            + self.total_cache_creation_tokens
        )

    @property
    def context_fill_ratio(self) -> float | None:
        """Latest turn's prompt size as a fraction of the context window.

        ``None`` when no assistant turn has reported usage yet. Capped
        at 1.0 — sessions that touch compaction can briefly exceed the
        window before the compactor lands, and a "103%" reading is
        more confusing than informative on a passive bar.
        """
        if self.latest_turn_input_tokens <= 0 or self.context_window <= 0:
            return None
        ratio = self.latest_turn_input_tokens / self.context_window
        return min(ratio, 1.0)

    @property
    def title(self) -> str:
        """Best display title: custom > ai > slug > truncated session id."""
        if self.custom_title:
            return self.custom_title
        if self.ai_title:
            return self.ai_title
        if self.slug:
            return self.slug
        return self.session_id[:8]


def _parse_iso(ts: str | None) -> datetime | None:
    if not ts:
        return None
    try:
        return datetime.fromisoformat(str(ts).replace("Z", "+00:00"))
    except Exception:
        return None


def _truncate_lines(text: str, max_lines: int) -> str:
    lines = text.splitlines()
    if len(lines) <= max_lines:
        return text
    return "\n".join(lines[:max_lines]).rstrip() + "\n…"


def extract_digest(jsonl_path: Path, *, session_id: str | None = None) -> SessionDigest:
    """Single-pass extractor. Returns a partial digest if the file errors mid-read.

    ``session_id`` defaults to the file stem when not provided. Lines that
    fail to decode are skipped silently (matches indexer behavior); lines
    with shapes we don't recognize are simply ignored.
    """
    sid = session_id or jsonl_path.stem
    digest = SessionDigest(session_id=sid)

    first_user_text: str | None = None
    first_user_ts: str | None = None
    last_event_ts: str | None = None

    away_summary_text: str | None = None
    away_summary_ts: str | None = None
    compact_summary_text: str | None = None
    compact_summary_ts: str | None = None

    # Dedupe assistant usage by message.id (chunks share an id; usage
    # lives on the final chunk and last-write-wins gives us the totals).
    usage_by_msg_id: dict[str, dict] = {}
    seen_models: list[str] = []
    seen_models_set: set[str] = set()

    # Track the latest assistant turn to compute context fill against
    # that model's window. ``latest_usage`` mirrors the final usage
    # block we saw; ``latest_model`` is whichever model id rode that
    # final assistant message.
    latest_usage: dict | None = None
    latest_model: str | None = None

    try:
        with jsonl_path.open() as fh:
            for line in fh:
                line = line.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not isinstance(obj, dict):
                    continue

                t = obj.get("type")

                # Ambient fields present on most line types.
                if obj.get("slug") and not digest.slug:
                    digest.slug = str(obj["slug"])
                if obj.get("gitBranch"):
                    digest.branch = str(obj["gitBranch"])
                if obj.get("version"):
                    digest.client_version = str(obj["version"])
                if obj.get("cwd") and not digest.cwd:
                    digest.cwd = str(obj["cwd"])
                ts = obj.get("timestamp")
                if isinstance(ts, str):
                    last_event_ts = ts

                if t == "custom-title":
                    val = obj.get("customTitle")
                    if val:
                        digest.custom_title = str(val)
                elif t == "ai-title":
                    val = obj.get("aiTitle")
                    if val:
                        digest.ai_title = str(val)
                elif t == "pr-link":
                    pr_num = obj.get("prNumber")
                    if pr_num is not None:
                        try:
                            digest.pr_number = int(pr_num)
                        except (TypeError, ValueError):
                            pass
                    if obj.get("prUrl"):
                        digest.pr_url = str(obj["prUrl"])
                    if obj.get("prRepository"):
                        digest.pr_repository = str(obj["prRepository"])
                elif t == "permission-mode":
                    val = obj.get("permissionMode")
                    if val:
                        digest.permission_mode = str(val)
                elif t == "system":
                    subtype = obj.get("subtype")
                    content = obj.get("content")
                    if subtype == "away_summary" and isinstance(content, str) and content:
                        away_summary_text = content
                        away_summary_ts = ts if isinstance(ts, str) else away_summary_ts
                    elif subtype == "compact_boundary":
                        cm = obj.get("compactMetadata") or {}
                        # The compact_boundary line itself only carries
                        # "Conversation compacted" — the actual digest is
                        # written as the next user line with
                        # isCompactSummary=true. We capture the timestamp
                        # here; the text is filled in below.
                        if isinstance(ts, str):
                            compact_summary_ts = ts
                        # Fallback: if a future schema ever inlines the
                        # summary on the boundary line, take it.
                        if isinstance(content, str) and len(content) > 30:
                            compact_summary_text = content
                elif t == "user":
                    msg = obj.get("message") or {}
                    content = msg.get("content")
                    is_compact = bool(obj.get("isCompactSummary"))
                    is_tool_result = bool(obj.get("toolUseResult"))
                    if is_compact and isinstance(content, str):
                        compact_summary_text = content
                        if isinstance(ts, str):
                            compact_summary_ts = ts
                    elif not is_tool_result and isinstance(content, str) and content:
                        if first_user_text is None:
                            first_user_text = content
                            if isinstance(ts, str):
                                first_user_ts = ts
                elif t == "assistant":
                    msg = obj.get("message") or {}
                    model = msg.get("model")
                    if isinstance(model, str) and model:
                        if model not in seen_models_set:
                            seen_models.append(model)
                            seen_models_set.add(model)
                        latest_model = model
                    msg_id = msg.get("id")
                    usage = msg.get("usage")
                    if isinstance(msg_id, str) and isinstance(usage, dict):
                        usage_by_msg_id[msg_id] = usage
                        latest_usage = usage
                    content = msg.get("content")
                    if isinstance(content, list):
                        for block in content:
                            if not isinstance(block, dict):
                                continue
                            if block.get("type") != "tool_use":
                                continue
                            if block.get("name") not in _FILE_TOUCH_TOOLS:
                                continue
                            inp = block.get("input") or {}
                            if not isinstance(inp, dict):
                                continue
                            fp = inp.get("file_path") or inp.get("notebook_path")
                            if isinstance(fp, str) and fp:
                                digest.files_touched.add(fp)
    except FileNotFoundError:
        log.debug("digest extractor: %s not found", jsonl_path)
        return digest
    except Exception as e:
        log.warning("digest extractor: partial read of %s — %s", jsonl_path, e)

    # Sum token usage across deduped messages.
    for u in usage_by_msg_id.values():
        digest.total_input_tokens += int(u.get("input_tokens") or 0)
        digest.total_output_tokens += int(u.get("output_tokens") or 0)
        digest.total_cache_read_tokens += int(u.get("cache_read_input_tokens") or 0)
        digest.total_cache_creation_tokens += int(
            u.get("cache_creation_input_tokens") or 0
        )

    digest.models_used = seen_models

    # Latest assistant turn → context fill. Sum input + cache_read +
    # cache_creation because all three count against the prompt size
    # the model saw. The model id picks the context window denominator.
    if isinstance(latest_usage, dict):
        digest.latest_turn_input_tokens = (
            int(latest_usage.get("input_tokens") or 0)
            + int(latest_usage.get("cache_read_input_tokens") or 0)
            + int(latest_usage.get("cache_creation_input_tokens") or 0)
        )
    digest.context_window = _model_context_window(latest_model)

    # Wall-clock duration: first user prompt → last event seen.
    start = _parse_iso(first_user_ts)
    end = _parse_iso(last_event_ts)
    if start and end and end >= start:
        digest.duration_seconds = int((end - start).total_seconds())

    # Build the recap timeline. Each band is included only if its
    # source exists; "Last asked" is *not* a band — the user can read
    # the most recent turn directly in the transcript pane next door,
    # so the bar focuses on what's not visible there: the session's
    # earlier history.
    recap: list[RecapEntry] = []
    if first_user_text:
        recap.append(
            RecapEntry(
                label="Started",
                timestamp=first_user_ts,
                text=_clean_first_prompt(first_user_text),
            )
        )
    if compact_summary_text:
        recap.append(
            RecapEntry(
                label="Earlier",
                timestamp=compact_summary_ts,
                text=_truncate_lines(compact_summary_text.strip(), 8),
            )
        )
    if away_summary_text:
        recap.append(
            RecapEntry(
                label="Recently",
                timestamp=away_summary_ts,
                text=away_summary_text.strip(),
            )
        )
    digest.recap = recap

    return digest


__all__ = [
    "RecapEntry",
    "SessionDigest",
    "extract_digest",
]
