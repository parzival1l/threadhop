"""Tests for the exchange model (ADR-030) in ``threadhop_core.exchanges``."""

from __future__ import annotations

import json
from pathlib import Path

from threadhop_core.exchanges import load_exchanges

SID = "eeee1111-1111-1111-1111-111111111111"


def _user(uuid: str, text: str, ts: str = "2026-04-20T10:00:00Z") -> str:
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": SID,
        "timestamp": ts,
        "message": {"content": [{"type": "text", "text": text}]},
    })


def _assistant(
    uuid: str,
    mid: str,
    blocks: list[dict],
    ts: str = "2026-04-20T10:00:01Z",
    sidechain: bool = False,
) -> str:
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": SID,
        "timestamp": ts,
        "isSidechain": sidechain,
        "message": {"id": mid, "content": blocks},
    })


def _tool_result_user(uuid: str) -> str:
    """A ``type=user`` JSONL row that is tool output, not a human prompt."""
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": SID,
        "timestamp": "2026-04-20T10:00:02Z",
        "toolUseResult": {"stdout": "ran fine"},
        "message": {"content": [
            {"type": "tool_result", "content": "ran fine"},
        ]},
    })


def _write(tmp_path: Path, lines: list[str]) -> Path:
    p = tmp_path / f"{SID}.jsonl"
    p.write_text("\n".join(lines) + "\n")
    return p


def test_tool_result_rows_do_not_split_exchanges(tmp_path: Path):
    path = _write(tmp_path, [
        _user("u1", "do the thing"),
        _assistant("a1", "m1", [
            {"type": "text", "text": "working on it"},
            {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}},
        ]),
        _tool_result_user("tr1"),
        _assistant("a2", "m2", [{"type": "text", "text": "all done"}]),
        _user("u2", "next task", ts="2026-04-20T10:05:00Z"),
        _assistant("a3", "m3", [{"type": "text", "text": "on it"}]),
    ])

    exchanges = load_exchanges(path)

    assert len(exchanges) == 2
    first = exchanges[0]
    assert first.user_text == "do the thing"
    # tool_use block dropped, tool-result row skipped, both assistant
    # turns stay inside exchange 1.
    assert first.assistant_texts == ["working on it", "all done"]
    assert exchanges[1].user_text == "next task"
    assert exchanges[1].assistant_texts == ["on it"]


def test_system_reminders_and_sidechains_are_stripped(tmp_path: Path):
    path = _write(tmp_path, [
        _user("u1", "<system-reminder>secret plumbing</system-reminder>hello"),
        _assistant("a1", "m1", [{
            "type": "text",
            "text": "hi <system-reminder>internal</system-reminder>there",
        }]),
        _assistant("side1", "ms", [
            {"type": "text", "text": "sidechain exploration"},
        ], sidechain=True),
    ])

    exchanges = load_exchanges(path)

    assert len(exchanges) == 1
    assert exchanges[0].user_text == "hello"
    assert exchanges[0].assistant_texts == ["hi there"]
    assert "secret plumbing" not in exchanges[0].text
    assert "sidechain exploration" not in exchanges[0].text


def test_streaming_assistant_chunks_merge_into_one_turn(tmp_path: Path):
    path = _write(tmp_path, [
        _user("u1", "explain"),
        _assistant("a1", "m1", [{"type": "text", "text": "part one"}]),
        _assistant("a2", "m1", [{"type": "text", "text": "part two"}]),
    ])

    exchanges = load_exchanges(path)

    assert len(exchanges) == 1
    assert exchanges[0].assistant_texts == ["part one\n\npart two"]


def test_leading_assistant_rows_group_into_userless_exchange(tmp_path: Path):
    path = _write(tmp_path, [
        _assistant("a1", "m1", [{"type": "text", "text": "resuming work"}]),
        _user("u1", "carry on"),
        _assistant("a2", "m2", [{"type": "text", "text": "sure"}]),
    ])

    exchanges = load_exchanges(path)

    assert len(exchanges) == 2
    assert exchanges[0].user_text is None
    assert exchanges[0].assistant_texts == ["resuming work"]
    assert exchanges[1].user_text == "carry on"


def test_offsets_are_monotonic_and_cover_the_file(tmp_path: Path):
    path = _write(tmp_path, [
        _user("u1", "first"),
        _assistant("a1", "m1", [{"type": "text", "text": "reply one"}]),
        _user("u2", "second"),
        _assistant("a2", "m2", [{"type": "text", "text": "reply two"}]),
    ])

    exchanges = load_exchanges(path)

    assert len(exchanges) == 2
    assert exchanges[0].start_offset == 0
    assert exchanges[0].end_offset <= exchanges[1].start_offset
    assert exchanges[1].end_offset == path.stat().st_size


def test_render_labels_user_and_assistant_blocks(tmp_path: Path):
    path = _write(tmp_path, [
        _user("u1", "question"),
        _assistant("a1", "m1", [{"type": "text", "text": "answer"}]),
    ])

    [exchange] = load_exchanges(path)

    assert exchange.render() == "User:\nquestion\n\nAssistant:\nanswer"
