"""Tests for the observer core function (task #18, ADR-018/019/020).

The real observer shells out to ``claude -p --model haiku``. These tests
inject a fake ``claude`` binary that appends a canned observation JSONL
line to the output file, then verify the orchestrator's behaviour:

  * Threshold gating (< 3 turns → skip, no subprocess).
  * State seeding from ``sessions`` on first run.
  * Byte-offset advancement on success, not on skip, not on failure.
  * Line-count diffing for the ``by_type`` summary.
  * Truncation / rotation handling.

Run from the repo root::

    python -m pytest tests/test_observer.py -v
"""

from __future__ import annotations

import json
import os
import stat
from pathlib import Path
from typing import Callable

import pytest

import db
import observer


# --- Fixtures --------------------------------------------------------------


@pytest.fixture
def obs_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Redirect ``db.OBS_DIR`` into the test's tmp_path.

    The observer writes observation files under ``db.OBS_DIR``. Without
    this redirect, tests would scribble into ``~/.config/threadhop``.
    """
    d = tmp_path / "observations"
    d.mkdir()
    monkeypatch.setattr(db, "OBS_DIR", d)
    return d


@pytest.fixture
def fake_claude(tmp_path: Path) -> Callable[[list[dict] | str], Path]:
    """Build a shell-script stand-in for the ``claude`` CLI.

    The observer passes ``-p <prompt>`` followed by ``--model`` and
    ``--permission-mode`` flags. Our fake ignores the prompt and appends
    whatever observation lines the test specified to the output file
    extracted from the prompt.

    ``script_body`` may be:
      * a ``list[dict]`` — serialized to one JSON line each and appended
        to the observation file the real prompt names.
      * a ``str`` — used as the raw shell body (for exit-code / error
        paths).
    """
    def _make(script_body):
        path = tmp_path / "fake_claude"
        if isinstance(script_body, list):
            # Extract the obs path from the prompt argument, then append
            # the canned entries. The prompt contains a marker line
            # "Append observations to: <path>" — grep it out.
            entries = "\n".join(json.dumps(e) for e in script_body) + "\n"
            payload_file = tmp_path / "fake_claude_payload.jsonl"
            payload_file.write_text(entries)
            body = f"""#!/usr/bin/env bash
set -euo pipefail
# -p <prompt> --model haiku --permission-mode acceptEdits
prompt="$2"
obs_path=$(printf '%s\\n' "$prompt" | sed -n 's/^Append observations to: //p')
cat "{payload_file}" >> "$obs_path"
"""
        else:
            body = script_body
        path.write_text(body)
        path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
        return path
    return _make


# --- JSONL builders (mirror test_incremental_index.py) --------------------


def _user_line(uuid: str, text: str, sid: str = "sess-1") -> str:
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-17T10:00:00Z",
        "message": {"id": f"umsg_{uuid}", "content": [{"type": "text", "text": text}]},
    })


def _assistant_line(uuid: str, mid: str, text: str, sid: str = "sess-1") -> str:
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-17T10:00:01Z",
        "message": {"id": mid, "content": [{"type": "text", "text": text}]},
    })


def _tool_result_user_line(uuid: str, sid: str = "sess-1") -> str:
    """type:user with toolUseResult — must NOT count as a turn."""
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-17T10:00:02Z",
        "toolUseResult": {"type": "tool_result", "content": "ok"},
    })


def _write_session(tmp_path: Path, name: str, lines: list[str]) -> Path:
    jsonl = tmp_path / f"{name}.jsonl"
    jsonl.write_text("\n".join(lines) + "\n")
    return jsonl


def _seed_session_row(conn, session_id: str, session_path: Path):
    db.upsert_session(
        conn, session_id, str(session_path),
        project="test-project",
    )


# --- Pure-function tests ---------------------------------------------------


class TestCountMessageTurns:
    def test_users_and_assistants_each_count(self):
        raw = (
            _user_line("u1", "hi") + "\n" +
            _assistant_line("a1", "m1", "hello") + "\n" +
            _user_line("u2", "more") + "\n"
        ).encode()
        assert observer._count_message_turns(raw) == 3

    def test_assistant_streaming_chunks_collapse_to_one_turn(self):
        # Three assistant lines, same message.id → one turn.
        raw = (
            _user_line("u1", "q") + "\n" +
            _assistant_line("a1", "m1", "part 1") + "\n" +
            _assistant_line("a2", "m1", "part 2") + "\n" +
            _assistant_line("a3", "m1", "part 3") + "\n"
        ).encode()
        assert observer._count_message_turns(raw) == 2

    def test_tool_result_user_lines_are_not_turns(self):
        raw = (
            _user_line("u1", "do a thing") + "\n" +
            _tool_result_user_line("tr1") + "\n" +
            _assistant_line("a1", "m1", "done") + "\n"
        ).encode()
        assert observer._count_message_turns(raw) == 2

    def test_malformed_lines_skipped(self):
        raw = (
            _user_line("u1", "hi") + "\n" +
            "not json at all\n" +
            _assistant_line("a1", "m1", "hey") + "\n"
        ).encode()
        assert observer._count_message_turns(raw) == 2


class TestReadNewBytes:
    def test_partial_line_at_eof_is_held_back(self, tmp_path: Path):
        p = tmp_path / "s.jsonl"
        p.write_bytes(b"first\nsecond\nthird-unfinished")
        out, new_off = observer._read_new_bytes(p, 0)
        assert out == b"first\nsecond\n"
        assert new_off == len(b"first\nsecond\n")

    def test_no_complete_line_returns_unchanged_offset(self, tmp_path: Path):
        p = tmp_path / "s.jsonl"
        p.write_bytes(b"just-partial")
        out, new_off = observer._read_new_bytes(p, 0)
        assert out == b""
        assert new_off == 0


# --- Orchestrator tests ---------------------------------------------------


class TestObserveSession:
    def test_below_threshold_skips_without_subprocess(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # Only 2 turns — threshold is 3. Fake claude would succeed, but
        # we want to prove it's not even invoked.
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "hi"),
            _assistant_line("a1", "m1", "hello"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)

        # Script that would fail loudly if called — proves we skipped.
        claude = fake_claude("#!/usr/bin/env bash\nexit 99\n")

        result = observer.observe_session(
            conn, "sess-1", claude_bin=str(claude),
        )

        assert result["status"] == "below_threshold"
        assert result["turns"] == 2
        assert result["new_entries"] == 0
        # Cursor did NOT advance — next run will re-read same bytes.
        state = db.get_observation_state(conn, "sess-1")
        # Below-threshold on first run doesn't even create a state row —
        # caller sees the offset in the return value.
        if state is not None:
            assert state["source_byte_offset"] == 0

    def test_up_to_date_when_no_new_bytes(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "a"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)
        # Pre-seed state at EOF — nothing left to read.
        size = jsonl.stat().st_size
        obs_path = obs_dir / "sess-1.jsonl"
        db.upsert_observation_state(
            conn, "sess-1", str(jsonl), str(obs_path),
            source_byte_offset=size,
            entry_count=0,
        )

        claude = fake_claude("#!/usr/bin/env bash\nexit 99\n")
        result = observer.observe_session(
            conn, "sess-1", claude_bin=str(claude),
        )

        assert result["status"] == "up_to_date"
        assert result["turns"] == 0

    def test_no_source_when_session_row_missing(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        result = observer.observe_session(
            conn, "ghost-session", claude_bin=str(fake_claude([])),
        )
        assert result["status"] == "no_source"

    def test_no_source_when_file_gone(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        missing = tmp_path / "gone.jsonl"
        _seed_session_row(conn, "sess-1", missing)
        result = observer.observe_session(
            conn, "sess-1", claude_bin=str(fake_claude([])),
        )
        assert result["status"] == "no_source"

    def test_extraction_appends_and_advances_state(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # 3 turns → meets threshold.
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "Should we use SQLite?"),
            _assistant_line("a1", "m1", "Yes, SQLite fits the use case."),
            _user_line("u2", "Great, decided."),
        ])
        _seed_session_row(conn, "sess-1", jsonl)

        canned = [
            {
                "type": "decision",
                "text": "Use SQLite for local state",
                "context": "single-file deployment",
                "ts": "2026-04-17T10:00:03Z",
            },
            {
                "type": "todo",
                "text": "Write the schema migration",
                "context": "",
                "ts": "2026-04-17T10:00:03Z",
            },
        ]
        claude = fake_claude(canned)

        result = observer.observe_session(
            conn, "sess-1", claude_bin=str(claude),
        )

        assert result["status"] == "extracted"
        assert result["turns"] == 3
        assert result["new_entries"] == 2
        assert result["by_type"] == {"decision": 1, "todo": 1}

        # State row exists with cursor at EOF and matching entry count.
        expected_offset = jsonl.stat().st_size
        state = db.get_observation_state(conn, "sess-1")
        assert state is not None
        assert state["source_byte_offset"] == expected_offset
        assert state["entry_count"] == 2
        assert state["status"] == "idle"  # observer function itself
                                         # doesn't flip to running — that
                                         # belongs to the sidecar (#34)
        assert state["last_observed_at"] is not None

        # Observation file contains exactly our two lines.
        obs_path = Path(result["obs_path"])
        lines = obs_path.read_text().strip().splitlines()
        assert len(lines) == 2
        assert json.loads(lines[0])["type"] == "decision"

    def test_resume_reads_only_appended_bytes(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # First extraction.
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "q"),
            _assistant_line("a1", "m1", "a"),
            _user_line("u2", "q2"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)

        claude = fake_claude([
            {"type": "decision", "text": "x", "context": "", "ts": "2026-04-17T10:00:00Z"},
        ])
        r1 = observer.observe_session(conn, "sess-1", claude_bin=str(claude))
        assert r1["status"] == "extracted"

        # Append more messages — meets threshold again.
        with open(jsonl, "a") as f:
            f.write(_assistant_line("a2", "m2", "b") + "\n")
            f.write(_user_line("u3", "q3") + "\n")
            f.write(_assistant_line("a3", "m3", "c") + "\n")

        # Second run: verify the fake only sees the appended bytes.
        # We do that by checking the turn count the observer computed.
        claude2 = fake_claude([
            {"type": "todo", "text": "y", "context": "", "ts": "2026-04-17T10:01:00Z"},
        ])
        r2 = observer.observe_session(conn, "sess-1", claude_bin=str(claude2))

        assert r2["status"] == "extracted"
        # Only the 3 new turns, not the original 3.
        assert r2["turns"] == 3
        assert r2["new_entries"] == 1

        # Observation file now has 2 lines total.
        obs_path = Path(r2["obs_path"])
        assert len(obs_path.read_text().strip().splitlines()) == 2

    def test_subprocess_failure_does_not_advance_state(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "x"),
            _assistant_line("a1", "m1", "y"),
            _user_line("u2", "z"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)

        claude = fake_claude(
            "#!/usr/bin/env bash\necho oops >&2\nexit 2\n"
        )
        result = observer.observe_session(
            conn, "sess-1", claude_bin=str(claude),
        )

        assert result["status"] == "failed"
        assert "exited 2" in result["error"]
        # State row either doesn't exist or is at offset 0 — we must NOT
        # have advanced past the un-processed bytes.
        state = db.get_observation_state(conn, "sess-1")
        if state is not None:
            assert state["source_byte_offset"] == 0

    def test_claude_bin_missing_is_a_failure_not_a_crash(
        self, tmp_path, conn, obs_dir
    ):
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "a"),
            _assistant_line("a1", "m1", "b"),
            _user_line("u2", "c"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)

        result = observer.observe_session(
            conn, "sess-1",
            claude_bin="/does/not/exist/claude-ghost",
        )
        assert result["status"] == "failed"
        assert "not found" in result["error"]

    def test_truncation_rewinds_cursor(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # Start with a session and pretend we've observed past its EOF
        # (simulates a rotated-and-replaced file with fewer bytes).
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "new-a"),
            _assistant_line("a1", "m1", "new-b"),
            _user_line("u2", "new-c"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)
        obs_path = obs_dir / "sess-1.jsonl"
        # Pretend prior run recorded a huge offset from an earlier,
        # now-discarded version of the file.
        db.upsert_observation_state(
            conn, "sess-1", str(jsonl), str(obs_path),
            source_byte_offset=10**9,  # way past current EOF
            entry_count=0,
        )

        claude = fake_claude([
            {"type": "decision", "text": "x", "context": "", "ts": "2026-04-17T10:00:00Z"},
        ])
        result = observer.observe_session(
            conn, "sess-1", claude_bin=str(claude),
        )

        assert result["status"] == "extracted"
        # All 3 turns should be read (cursor rewound to 0).
        assert result["turns"] == 3
