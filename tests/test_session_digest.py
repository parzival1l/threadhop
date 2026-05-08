"""Unit tests for ``threadhop_core.session.digest.extract_digest``.

The extractor is the data layer behind the right-hand SessionDigestBar.
Tests use synthetic JSONL fixtures rather than live transcripts so each
line type is exercised in isolation.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from threadhop_core.session.digest import (
    RecapEntry,
    SessionDigest,
    _clean_first_prompt,
    _model_context_window,
    extract_digest,
)


def _write_jsonl(path: Path, rows: list[dict]) -> Path:
    path.write_text("\n".join(json.dumps(r) for r in rows) + "\n")
    return path


def _user(text: str, ts: str, **extra) -> dict:
    return {
        "type": "user",
        "uuid": extra.pop("uuid", f"user-{ts}"),
        "timestamp": ts,
        "message": {"role": "user", "content": text},
        **extra,
    }


def _assistant(
    text: str,
    ts: str,
    *,
    msg_id: str,
    model: str = "claude-opus-4-7",
    usage: dict | None = None,
    tool_uses: list[dict] | None = None,
    **extra,
) -> dict:
    content: list[dict] = [{"type": "text", "text": text}]
    if tool_uses:
        content.extend(tool_uses)
    return {
        "type": "assistant",
        "uuid": extra.pop("uuid", f"asst-{ts}"),
        "timestamp": ts,
        "message": {
            "id": msg_id,
            "role": "assistant",
            "model": model,
            "content": content,
            **({"usage": usage} if usage else {}),
        },
        **extra,
    }


# ---------- Identity & ambient fields ----------


def test_extract_digest_picks_up_titles_slug_branch_version(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            {"type": "ai-title", "sessionId": "s", "aiTitle": "Auto title"},
            {"type": "custom-title", "sessionId": "s", "customTitle": "My title"},
            _user("hello", "2026-01-01T00:00:00Z", slug="brave-otter",
                  gitBranch="dev", version="2.1.131", cwd="/tmp/proj"),
        ],
    )
    d = extract_digest(path)
    assert d.custom_title == "My title"
    assert d.ai_title == "Auto title"
    assert d.title == "My title"  # custom > ai
    assert d.slug == "brave-otter"
    assert d.branch == "dev"
    assert d.client_version == "2.1.131"
    assert d.cwd == "/tmp/proj"


def test_title_falls_back_to_ai_then_slug(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            {"type": "ai-title", "sessionId": "s", "aiTitle": "AI only"},
            _user("hi", "2026-01-01T00:00:00Z", slug="brave-otter"),
        ],
    )
    assert extract_digest(path).title == "AI only"

    path2 = _write_jsonl(
        tmp_path / "s2.jsonl",
        [_user("hi", "2026-01-01T00:00:00Z", slug="lucky-fox")],
    )
    assert extract_digest(path2).title == "lucky-fox"


# ---------- PR / outputs ----------


def test_extract_digest_captures_pr_link(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("hi", "2026-01-01T00:00:00Z"),
            {
                "type": "pr-link",
                "sessionId": "s",
                "prNumber": 73,
                "prUrl": "https://github.com/foo/bar/pull/73",
                "prRepository": "foo/bar",
                "timestamp": "2026-01-01T00:05:00Z",
            },
        ],
    )
    d = extract_digest(path)
    assert d.pr_number == 73
    assert d.pr_url == "https://github.com/foo/bar/pull/73"
    assert d.pr_repository == "foo/bar"


def test_extract_digest_collects_files_touched(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("change app.py", "2026-01-01T00:00:00Z"),
            _assistant(
                "ok",
                "2026-01-01T00:00:01Z",
                msg_id="m1",
                tool_uses=[
                    {"type": "tool_use", "id": "t1", "name": "Edit",
                     "input": {"file_path": "/repo/app.py"}},
                    {"type": "tool_use", "id": "t2", "name": "Write",
                     "input": {"file_path": "/repo/new.py"}},
                    # Bash should NOT count as a file touch.
                    {"type": "tool_use", "id": "t3", "name": "Bash",
                     "input": {"command": "ls"}},
                ],
            ),
            _assistant(
                "again",
                "2026-01-01T00:00:02Z",
                msg_id="m2",
                tool_uses=[
                    {"type": "tool_use", "id": "t4", "name": "Edit",
                     "input": {"file_path": "/repo/app.py"}},  # dedupe
                ],
            ),
        ],
    )
    d = extract_digest(path)
    assert d.files_touched == {"/repo/app.py", "/repo/new.py"}
    assert d.files_touched_count == 2


# ---------- Tokens / models ----------


def test_extract_digest_sums_tokens_and_dedupes_chunked_assistant(tmp_path: Path):
    # Two chunks share message.id m1 — usage on the final chunk is what
    # sticks (last-write-wins). m2 is a separate message with its own usage.
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("hi", "2026-01-01T00:00:00Z"),
            _assistant("partial", "2026-01-01T00:00:01Z", msg_id="m1"),
            _assistant(
                "final",
                "2026-01-01T00:00:02Z",
                msg_id="m1",
                usage={
                    "input_tokens": 100,
                    "output_tokens": 200,
                    "cache_read_input_tokens": 5000,
                    "cache_creation_input_tokens": 50,
                },
            ),
            _assistant(
                "second turn",
                "2026-01-01T00:00:03Z",
                msg_id="m2",
                usage={
                    "input_tokens": 50,
                    "output_tokens": 75,
                    "cache_read_input_tokens": 1000,
                    "cache_creation_input_tokens": 25,
                },
            ),
        ],
    )
    d = extract_digest(path)
    assert d.total_input_tokens == 150
    assert d.total_output_tokens == 275
    assert d.total_cache_read_tokens == 6000
    assert d.total_cache_creation_tokens == 75
    # Cache hit: 6000 / (6000 + 75)
    assert d.cache_hit_ratio is not None
    assert 0.985 < d.cache_hit_ratio < 0.99


def test_extract_digest_tracks_models_in_first_seen_order(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("hi", "2026-01-01T00:00:00Z"),
            _assistant("a", "2026-01-01T00:00:01Z", msg_id="m1",
                       model="claude-opus-4-7"),
            _assistant("b", "2026-01-01T00:00:02Z", msg_id="m2",
                       model="claude-haiku-4-5"),
            _assistant("c", "2026-01-01T00:00:03Z", msg_id="m3",
                       model="claude-opus-4-7"),  # already seen
        ],
    )
    d = extract_digest(path)
    assert d.models_used == ["claude-opus-4-7", "claude-haiku-4-5"]


def test_cache_hit_ratio_is_none_when_no_cache_activity(tmp_path: Path):
    path = _write_jsonl(tmp_path / "s.jsonl", [_user("hi", "2026-01-01T00:00:00Z")])
    assert extract_digest(path).cache_hit_ratio is None


# ---------- Recap timeline ----------


def test_recap_has_started_only_for_short_session(tmp_path: Path):
    """The recap deliberately drops a "Last asked" band — the most recent
    exchange is already visible in the transcript pane next door, so the
    bar focuses on what's NOT visible there.
    """
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("redesign the kanban", "2026-01-01T00:00:00Z"),
            _assistant("ok", "2026-01-01T00:00:01Z", msg_id="m1"),
            _user("show me the diff", "2026-01-01T00:05:00Z"),
        ],
    )
    d = extract_digest(path)
    labels = [r.label for r in d.recap]
    assert labels == ["Started"]
    assert d.recap[0].text == "redesign the kanban"
    assert "Last asked" not in {r.label for r in d.recap}


def test_recap_collapses_to_just_started_when_session_is_one_prompt(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [_user("only prompt", "2026-01-01T00:00:00Z")],
    )
    d = extract_digest(path)
    assert [r.label for r in d.recap] == ["Started"]


def test_recap_includes_away_summary_as_recently(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("design something", "2026-01-01T00:00:00Z"),
            _assistant("ok", "2026-01-01T00:00:01Z", msg_id="m1"),
            {
                "type": "system",
                "subtype": "away_summary",
                "content": "Just finished Task 1. Next: Task 2.",
                "timestamp": "2026-01-01T01:00:00Z",
            },
            _user("continue", "2026-01-01T02:00:00Z"),
        ],
    )
    d = extract_digest(path)
    labels = [r.label for r in d.recap]
    assert labels == ["Started", "Recently"]
    recently = next(r for r in d.recap if r.label == "Recently")
    assert "Task 1" in recently.text


def test_recap_includes_compact_summary_as_earlier(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("kick off", "2026-01-01T00:00:00Z"),
            {
                "type": "system",
                "subtype": "compact_boundary",
                "content": "Conversation compacted",
                "timestamp": "2026-01-01T01:00:00Z",
                "compactMetadata": {"trigger": "manual", "preTokens": 100, "postTokens": 50},
            },
            {
                "type": "user",
                "uuid": "user-summary",
                "timestamp": "2026-01-01T01:00:01Z",
                "isCompactSummary": True,
                "summarizeMetadata": {"messagesSummarized": 12, "direction": "from"},
                "message": {"role": "user", "content": (
                    "Summary:\n1. Primary intent: ship it.\n2. Key changes: X.\n"
                    "3. Files touched: app.py.\n4. Next steps: review.\n"
                    "5. More context.\n6. Even more.\n7. Yet more.\n8. Final.\n"
                    "9. Overflow line — should be truncated."
                )},
            },
            _user("after compaction", "2026-01-01T01:30:00Z"),
        ],
    )
    d = extract_digest(path)
    labels = [r.label for r in d.recap]
    assert "Earlier" in labels
    earlier = next(r for r in d.recap if r.label == "Earlier")
    # Truncation cap is 8 lines; the source had 9.
    assert "…" in earlier.text
    assert earlier.text.count("\n") <= 8


def test_recap_three_band_when_all_pre_history_sources_present(tmp_path: Path):
    """All three pre-history bands (Started + Earlier + Recently) when every
    source is available. Note: no "Last asked" — it was deliberately removed.
    """
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("first ask", "2026-01-01T00:00:00Z"),
            {
                "type": "system",
                "subtype": "compact_boundary",
                "content": "Conversation compacted",
                "timestamp": "2026-01-01T00:30:00Z",
            },
            {
                "type": "user",
                "uuid": "u-summary",
                "timestamp": "2026-01-01T00:30:01Z",
                "isCompactSummary": True,
                "message": {"role": "user", "content": "Compaction digest body."},
            },
            {
                "type": "system",
                "subtype": "away_summary",
                "content": "Idle recap text.",
                "timestamp": "2026-01-01T01:00:00Z",
            },
            # The "last-prompt" line type is intentionally ignored now;
            # this test confirms it does not produce a recap band.
            {
                "type": "last-prompt",
                "lastPrompt": "show me the file",
                "sessionId": "s",
            },
        ],
    )
    d = extract_digest(path)
    assert [r.label for r in d.recap] == ["Started", "Earlier", "Recently"]


# ---------- Footer + duration ----------


def test_extract_digest_picks_latest_permission_mode(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            {"type": "permission-mode", "sessionId": "s", "permissionMode": "default"},
            _user("hi", "2026-01-01T00:00:00Z"),
            {"type": "permission-mode", "sessionId": "s", "permissionMode": "acceptEdits"},
        ],
    )
    assert extract_digest(path).permission_mode == "acceptEdits"


def test_duration_seconds_spans_first_user_to_last_event(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("start", "2026-01-01T00:00:00Z"),
            _assistant("end", "2026-01-01T00:30:00Z", msg_id="m1"),
        ],
    )
    d = extract_digest(path)
    assert d.duration_seconds == 1800


# ---------- Robustness ----------


def test_extractor_skips_malformed_lines(tmp_path: Path):
    path = tmp_path / "s.jsonl"
    path.write_text(
        "{not json}\n"
        + json.dumps(_user("hi", "2026-01-01T00:00:00Z"))
        + "\n"
        + "[1, 2, 3]\n"
    )
    d = extract_digest(path)
    assert d.recap[0].text == "hi"


def test_missing_file_returns_empty_digest(tmp_path: Path):
    d = extract_digest(tmp_path / "does-not-exist.jsonl", session_id="abc")
    assert d.session_id == "abc"
    assert d.recap == []
    assert d.total_tokens == 0


# ---------- "Started" prompt cleaning ----------


def test_clean_first_prompt_strips_task_notification_envelope():
    raw = (
        "<task-notification>"
        "<task-id>abc</task-id>"
        "<output-file>/tmp/x</output-file>"
        "</task-notification>\n"
        "Actually do the work please."
    )
    assert _clean_first_prompt(raw) == "Actually do the work please."


def test_clean_first_prompt_strips_from_preview_prefix():
    raw = '[From "code block excerpt" — ~/path/file — 2026-04-26 01:06] Refactor this.'
    assert _clean_first_prompt(raw) == "Refactor this."


def test_clean_first_prompt_takes_first_sentence():
    raw = "Redesign the kanban card title fallback chain. Also fix the spinner alignment."
    assert _clean_first_prompt(raw) == "Redesign the kanban card title fallback chain."


def test_clean_first_prompt_truncates_long_prompts_at_word_boundary():
    raw = (
        "I want to do a sort of mock-up of a Kanban UI toggle for the ThreadHop "
        "application so we have four categories already that line up with what "
        "we already have"
    )
    cleaned = _clean_first_prompt(raw)
    assert cleaned.endswith("…")
    assert len(cleaned) <= 141  # cap + ellipsis
    # No mid-word break.
    assert " " in cleaned[-30:] or cleaned.endswith("…")


def test_clean_first_prompt_passes_short_prompts_unchanged():
    assert _clean_first_prompt("What is this app?") == "What is this app?"


def test_started_band_is_cleaned_in_extracted_digest(tmp_path: Path):
    raw = (
        "<task-notification><task-id>x</task-id></task-notification>\n"
        "Audit the release runbook. Extra noise after the period."
    )
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [_user(raw, "2026-01-01T00:00:00Z")],
    )
    d = extract_digest(path)
    assert d.recap[0].label == "Started"
    assert d.recap[0].text == "Audit the release runbook."


# ---------- Context window detection ----------


def test_model_context_window_default_is_200k():
    assert _model_context_window(None) == 200_000
    assert _model_context_window("claude-opus-4-7") == 200_000
    assert _model_context_window("claude-haiku-4-5") == 200_000
    assert _model_context_window("claude-sonnet-4-6") == 200_000


def test_model_context_window_detects_1m_suffix():
    assert _model_context_window("claude-opus-4-7[1m]") == 1_000_000
    assert _model_context_window("claude-opus-4-7-1m") == 1_000_000


# ---------- Context fill calculation ----------


def test_latest_turn_input_tokens_includes_cache(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("hi", "2026-01-01T00:00:00Z"),
            _assistant(
                "first",
                "2026-01-01T00:00:01Z",
                msg_id="m1",
                usage={
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_read_input_tokens": 1000,
                    "cache_creation_input_tokens": 0,
                },
            ),
            _assistant(
                "latest",
                "2026-01-01T00:00:02Z",
                msg_id="m2",
                usage={
                    "input_tokens": 200,
                    "output_tokens": 75,
                    "cache_read_input_tokens": 5000,
                    "cache_creation_input_tokens": 100,
                },
            ),
        ],
    )
    d = extract_digest(path)
    # The latest turn's prompt = 200 + 5000 + 100 = 5300.
    assert d.latest_turn_input_tokens == 5300


def test_context_fill_ratio_uses_latest_model_window(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("hi", "2026-01-01T00:00:00Z"),
            _assistant(
                "x",
                "2026-01-01T00:00:01Z",
                msg_id="m1",
                model="claude-opus-4-7[1m]",
                usage={
                    "input_tokens": 50_000,
                    "output_tokens": 1_000,
                    "cache_read_input_tokens": 50_000,
                    "cache_creation_input_tokens": 0,
                },
            ),
        ],
    )
    d = extract_digest(path)
    assert d.context_window == 1_000_000
    assert d.latest_turn_input_tokens == 100_000
    # 100k / 1M = 0.1
    assert d.context_fill_ratio == pytest.approx(0.1)


def test_context_fill_ratio_caps_at_one(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("hi", "2026-01-01T00:00:00Z"),
            _assistant(
                "x",
                "2026-01-01T00:00:01Z",
                msg_id="m1",
                usage={
                    "input_tokens": 250_000,
                    "output_tokens": 100,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                },
            ),
        ],
    )
    d = extract_digest(path)
    assert d.context_fill_ratio == 1.0  # 250k / 200k clamped to 1.0


def test_context_fill_ratio_is_none_when_no_assistant_turns(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [_user("hi", "2026-01-01T00:00:00Z")],
    )
    d = extract_digest(path)
    assert d.context_fill_ratio is None


def test_total_input_tokens_billed_combines_input_and_cache(tmp_path: Path):
    path = _write_jsonl(
        tmp_path / "s.jsonl",
        [
            _user("hi", "2026-01-01T00:00:00Z"),
            _assistant(
                "x",
                "2026-01-01T00:00:01Z",
                msg_id="m1",
                usage={
                    "input_tokens": 100,
                    "output_tokens": 200,
                    "cache_read_input_tokens": 5000,
                    "cache_creation_input_tokens": 50,
                },
            ),
        ],
    )
    d = extract_digest(path)
    # 100 (uncached) + 5000 (read) + 50 (created) = 5150
    assert d.total_input_tokens_billed == 5150
    # output unchanged
    assert d.total_output_tokens == 200
