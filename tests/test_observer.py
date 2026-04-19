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


def _write_obs_file(obs_dir: Path, session_id: str, entries: list[dict]) -> Path:
    path = obs_dir / f"{session_id}.jsonl"
    path.write_text("\n".join(json.dumps(e) for e in entries) + "\n")
    return path


# --- Pure-function tests ---------------------------------------------------


class TestTranscriptRendering:
    """The observer now feeds Haiku the cleaned transcript produced by
    indexer.parse_byte_range (tool outputs stripped, tool_use abbreviated,
    streaming chunks merged). These tests cover the assembly step — both
    turn counting (which is just ``len(message_turns)`` now) and the
    transcript-format helper the prompt splices in.
    """

    def test_users_and_assistants_each_count_as_turns(self):
        import indexer
        raw = (
            _user_line("u1", "hi") + "\n" +
            _assistant_line("a1", "m1", "hello") + "\n" +
            _user_line("u2", "more") + "\n"
        ).encode()
        assert len(indexer.parse_byte_range(raw)) == 3

    def test_assistant_streaming_chunks_collapse_to_one_turn(self):
        import indexer
        raw = (
            _user_line("u1", "q") + "\n" +
            _assistant_line("a1", "m1", "part 1") + "\n" +
            _assistant_line("a2", "m1", "part 2") + "\n" +
            _assistant_line("a3", "m1", "part 3") + "\n"
        ).encode()
        turns = indexer.parse_byte_range(raw)
        assert len(turns) == 2
        # The assistant turn is the *merged* text, not three separate ones.
        assistant_turn = next(t for t in turns if t["role"] == "assistant")
        assert "part 1" in assistant_turn["text"]
        assert "part 2" in assistant_turn["text"]
        assert "part 3" in assistant_turn["text"]

    def test_tool_result_user_lines_are_not_turns(self):
        import indexer
        raw = (
            _user_line("u1", "do a thing") + "\n" +
            _tool_result_user_line("tr1") + "\n" +
            _assistant_line("a1", "m1", "done") + "\n"
        ).encode()
        turns = indexer.parse_byte_range(raw)
        assert len(turns) == 2
        assert [t["role"] for t in turns] == ["user", "assistant"]

    def test_format_transcript_uses_role_and_timestamp_headers(self):
        turns = [
            {"role": "user", "timestamp": "2026-04-17T10:00:00Z",
             "text": "Should we use SQLite?"},
            {"role": "assistant", "timestamp": "2026-04-17T10:00:01Z",
             "text": "Yes — single-file deployment wins."},
        ]
        out = observer._format_transcript(turns)
        assert "### user · 2026-04-17T10:00:00Z" in out
        assert "### assistant · 2026-04-17T10:00:01Z" in out
        assert "Should we use SQLite?" in out
        assert "single-file deployment wins" in out

    def test_format_transcript_skips_empty_text(self):
        turns = [
            {"role": "user", "timestamp": "t1", "text": "hello"},
            {"role": "assistant", "timestamp": "t2", "text": ""},
        ]
        out = observer._format_transcript(turns)
        assert "### user" in out
        assert "### assistant" not in out


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

    def test_prompt_contains_cleaned_transcript_not_raw_jsonl(
        self, tmp_path, conn, obs_dir
    ):
        """The observer must send Haiku the cleaned transcript — not raw
        JSONL with message.content blocks, tool results, or system-reminders.
        We capture the prompt arg via a fake claude that dumps it to a file.
        """
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "Should we cache this?"),
            _assistant_line("a1", "m1", "Yes <system-reminder>internal</system-reminder> definitely"),
            _user_line("u2", "Great"),
            # A tool_result user line that must NOT appear in the prompt.
            _tool_result_user_line("tr1"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)

        # Fake claude that writes its prompt arg to a known file.
        captured = tmp_path / "captured_prompt.txt"
        script = tmp_path / "fake_claude"
        script.write_text(
            "#!/usr/bin/env bash\n"
            f"printf '%s' \"$2\" > {captured}\n"
        )
        script.chmod(
            script.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH
        )

        result = observer.observe_session(
            conn, "sess-1", claude_bin=str(script),
        )
        assert result["status"] == "extracted"

        prompt_body = captured.read_text()

        # Isolate the transcript section — the prompt template itself
        # references `<session_chunk>` and `<system-reminder>` in its
        # instructions, so we can only assert stripping happened inside
        # the actual delimited chunk. The delimiter sits on its own line,
        # which lets us skip the backticked mentions in the guidance.
        start = (
            prompt_body.index("<session_chunk>\n")
            + len("<session_chunk>\n")
        )
        end = prompt_body.index("\n</session_chunk>")
        chunk = prompt_body[start:end]

        # Role-labelled transcript headers present.
        assert "### user" in chunk
        assert "### assistant" in chunk

        # Raw JSONL artifacts MUST NOT appear in the transcript.
        assert "\"sessionId\"" not in chunk
        assert "\"toolUseResult\"" not in chunk
        assert "<system-reminder>" not in chunk

        # Cleaned text still makes it through.
        assert "Should we cache this?" in chunk
        assert "definitely" in chunk

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


class TestReflectSession:
    def test_reflector_runs_when_threshold_is_met(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        current_jsonl = _write_session(tmp_path, "sess-1", [_user_line("u1", "a")])
        other_jsonl = _write_session(tmp_path, "sess-2", [_user_line("u2", "b")])
        _seed_session_row(conn, "sess-1", current_jsonl)
        _seed_session_row(conn, "sess-2", other_jsonl)

        current_entries = [
            {"type": "decision", "text": "Use REST", "context": "", "ts": "t1"},
            {"type": "todo", "text": "x1", "context": "", "ts": "t2"},
            {"type": "todo", "text": "x2", "context": "", "ts": "t3"},
            {"type": "observation", "text": "x3", "context": "", "ts": "t4"},
            {"type": "done", "text": "x4", "context": "", "ts": "t5"},
        ]
        other_entries = [
            {"type": "decision", "text": "Use gRPC", "context": "", "ts": "t0"},
        ]
        current_obs = _write_obs_file(obs_dir, "sess-1", current_entries)
        other_obs = _write_obs_file(obs_dir, "sess-2", other_entries)
        db.upsert_observation_state(
            conn, "sess-1", str(current_jsonl), str(current_obs),
            source_byte_offset=current_jsonl.stat().st_size,
            entry_count=len(current_entries),
            reflector_entry_offset=0,
        )
        db.upsert_observation_state(
            conn, "sess-2", str(other_jsonl), str(other_obs),
            source_byte_offset=other_jsonl.stat().st_size,
            entry_count=len(other_entries),
            reflector_entry_offset=0,
        )

        claude = fake_claude([
            {
                "type": "conflict",
                "text": "REST contradicts gRPC",
                "refs": ["sess-1", "sess-2"],
                "topic": "api-protocol",
                "ts": "2026-04-17T11:00:00Z",
            },
        ])
        result = observer.maybe_reflect_session(
            conn, "sess-1",
            threshold=5,
            claude_bin=str(claude),
        )

        assert result["status"] == "reflected"
        assert result["new_entries"] == 1
        assert result["by_type"] == {"conflict": 1}
        state = db.get_observation_state(conn, "sess-1")
        assert state is not None
        assert state["reflector_entry_offset"] == 6

    def test_reflector_advances_offset_without_llm_when_no_new_decisions(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        jsonl = _write_session(tmp_path, "sess-1", [_user_line("u1", "a")])
        _seed_session_row(conn, "sess-1", jsonl)
        entries = [
            {"type": "todo", "text": "x1", "context": "", "ts": "t1"},
            {"type": "todo", "text": "x2", "context": "", "ts": "t2"},
            {"type": "done", "text": "x3", "context": "", "ts": "t3"},
            {"type": "observation", "text": "x4", "context": "", "ts": "t4"},
            {"type": "adr", "text": "x5", "context": "", "ts": "t5"},
        ]
        obs_path = _write_obs_file(obs_dir, "sess-1", entries)
        db.upsert_observation_state(
            conn, "sess-1", str(jsonl), str(obs_path),
            source_byte_offset=jsonl.stat().st_size,
            entry_count=len(entries),
            reflector_entry_offset=0,
        )

        result = observer.maybe_reflect_session(
            conn, "sess-1",
            threshold=5,
            claude_bin=str(fake_claude("#!/usr/bin/env bash\nexit 99\n")),
        )

        assert result["status"] == "up_to_date"
        state = db.get_observation_state(conn, "sess-1")
        assert state is not None
        assert state["reflector_entry_offset"] == len(entries)


# --- Watch-mode tests ------------------------------------------------------


class _StopAfter:
    """should_stop helper: returns False until called ``after`` times.

    Using a class instead of a closure makes the call count inspectable
    from the tests so we can assert how many polls actually happened.
    """

    def __init__(self, after: int) -> None:
        self.after = after
        self.calls = 0

    def __call__(self) -> bool:
        self.calls += 1
        return self.calls > self.after


class TestWatchSession:
    """The watch loop is the layer on top of observe_session that polls
    for source-file growth and re-invokes the observer. These tests
    exercise the loop's branches with a no-op sleep and either a stop
    callback or ``max_iterations`` to terminate deterministically.
    """

    def test_runs_initial_catch_up_then_stops(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # 3 turns waiting at start — meets threshold for the catch-up run.
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "hi"),
            _assistant_line("a1", "m1", "yo"),
            _user_line("u2", "more"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)
        claude = fake_claude([
            {"type": "decision", "text": "x", "context": "",
             "ts": "2026-04-17T10:00:00Z"},
        ])

        results: list[dict] = []
        result = observer.watch_session(
            conn, "sess-1",
            claude_bin=str(claude),
            should_stop=_StopAfter(0),  # exit before the first poll iteration
            sleep_fn=lambda _: None,
            on_result=results.append,
        )

        assert result["status"] == "stopped"
        assert result["extractions"] == 1
        assert result["iterations"] == 0
        # The catch-up call propagated through on_result.
        assert len(results) == 1
        assert results[0]["status"] == "extracted"

    def test_no_growth_means_observer_not_re_invoked(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # Same shape as above. After the catch-up advances the cursor
        # to EOF, no further growth occurs, so the loop should not call
        # observe_session again no matter how many times it polls.
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "a"),
            _assistant_line("a1", "m1", "b"),
            _user_line("u2", "c"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)
        claude = fake_claude([
            {"type": "decision", "text": "x", "context": "",
             "ts": "2026-04-17T10:00:00Z"},
        ])

        result = observer.watch_session(
            conn, "sess-1",
            claude_bin=str(claude),
            max_iterations=5,
            sleep_fn=lambda _: None,
        )

        assert result["status"] == "max_iterations"
        # Only the catch-up extraction happened — no further calls.
        assert result["extractions"] == 1
        assert result["iterations"] == 5
        # Observation file unchanged after catch-up.
        obs_path = obs_dir / "sess-1.jsonl"
        assert len(obs_path.read_text().strip().splitlines()) == 1

    def test_growth_after_catch_up_triggers_extraction(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # Catch-up consumes 3 turns; then before each poll we append
        # more turns and confirm the loop re-extracts.
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "a"),
            _assistant_line("a1", "m1", "b"),
            _user_line("u2", "c"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)

        # Single fake claude — appends one canned line per invocation.
        # Each call to observe_session triggers exactly one append.
        claude = fake_claude([
            {"type": "decision", "text": "x", "context": "",
             "ts": "2026-04-17T10:00:00Z"},
        ])

        # The "growth driver" runs as our sleep_fn: every time the loop
        # sleeps, we append 3 more turns to the source. That guarantees
        # growth is visible on the very next size check.
        appends_left = [2]

        def grow_then_sleep(_):
            if appends_left[0] <= 0:
                return
            appends_left[0] -= 1
            with open(jsonl, "a") as f:
                f.write(_user_line(f"u-extra-{appends_left[0]}", "q") + "\n")
                f.write(_assistant_line(
                    f"a-extra-{appends_left[0]}",
                    f"m-extra-{appends_left[0]}", "a") + "\n")
                f.write(_user_line(f"u-extra2-{appends_left[0]}", "q2") + "\n")

        result = observer.watch_session(
            conn, "sess-1",
            claude_bin=str(claude),
            max_iterations=3,
            sleep_fn=grow_then_sleep,
        )

        assert result["status"] == "max_iterations"
        # 1 catch-up + 2 growth-triggered runs = 3 total extractions.
        assert result["extractions"] == 3

    def test_below_threshold_growth_does_not_advance_cursor(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        """A 1-turn append after catch-up triggers an observe call but
        observe_session returns 'below_threshold' without advancing the
        cursor — and counts as below_threshold_runs in the summary.
        """
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "a"),
            _assistant_line("a1", "m1", "b"),
            _user_line("u2", "c"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)
        claude = fake_claude([
            {"type": "decision", "text": "x", "context": "",
             "ts": "2026-04-17T10:00:00Z"},
        ])

        # First sleep: append exactly one new turn (below the 3-turn
        # threshold). All later sleeps are no-ops.
        appended = [False]

        def sleep_fn(_):
            if appended[0]:
                return
            appended[0] = True
            with open(jsonl, "a") as f:
                f.write(_user_line("u-extra", "tiny") + "\n")

        cursor_before_loop = jsonl.stat().st_size

        result = observer.watch_session(
            conn, "sess-1",
            claude_bin=str(claude),
            max_iterations=2,
            sleep_fn=sleep_fn,
        )

        assert result["extractions"] == 1  # just the catch-up
        assert result["below_threshold_runs"] == 1
        # The state cursor stayed at the catch-up EOF — observe_session
        # is not allowed to advance past bytes it didn't ship to Haiku.
        state = db.get_observation_state(conn, "sess-1")
        assert state is not None
        assert state["source_byte_offset"] == cursor_before_loop

    def test_source_disappears_returns_source_gone(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "a"),
            _assistant_line("a1", "m1", "b"),
            _user_line("u2", "c"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)
        claude = fake_claude([
            {"type": "decision", "text": "x", "context": "",
             "ts": "2026-04-17T10:00:00Z"},
        ])

        deleted = [False]

        def delete_then_sleep(_):
            if deleted[0]:
                return
            deleted[0] = True
            jsonl.unlink()

        result = observer.watch_session(
            conn, "sess-1",
            claude_bin=str(claude),
            max_iterations=10,
            sleep_fn=delete_then_sleep,
        )

        assert result["status"] == "source_gone"
        # The catch-up still ran before the file went away.
        assert result["extractions"] == 1

    def test_missing_source_skips_catch_up(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # No sessions row, no state row — the watcher must not try to
        # call observe_session at all (it would fail with no_source).
        result = observer.watch_session(
            conn, "ghost-sess",
            claude_bin=str(fake_claude([])),
            max_iterations=3,
            sleep_fn=lambda _: None,
        )
        assert result["status"] == "source_gone"
        assert result["extractions"] == 0
        assert result["iterations"] == 0

    def test_subprocess_failure_does_not_kill_the_loop(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # Catch-up will fail (claude exits non-zero). The loop must
        # keep going — failures are tallied, not raised.
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "a"),
            _assistant_line("a1", "m1", "b"),
            _user_line("u2", "c"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)
        claude = fake_claude("#!/usr/bin/env bash\nexit 7\n")

        result = observer.watch_session(
            conn, "sess-1",
            claude_bin=str(claude),
            max_iterations=2,
            sleep_fn=lambda _: None,
        )

        assert result["status"] == "max_iterations"
        assert result["failures"] >= 1
        assert result["extractions"] == 0

    def test_should_stop_polled_each_iteration(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        # No real source — we just want to prove should_stop is consulted
        # before max_iterations and exits cleanly.
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "a"),
            _assistant_line("a1", "m1", "b"),
            _user_line("u2", "c"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)
        claude = fake_claude([
            {"type": "decision", "text": "x", "context": "",
             "ts": "2026-04-17T10:00:00Z"},
        ])
        stop = _StopAfter(3)

        result = observer.watch_session(
            conn, "sess-1",
            claude_bin=str(claude),
            should_stop=stop,
            max_iterations=100,
            sleep_fn=lambda _: None,
        )

        assert result["status"] == "stopped"
        # Polled at the top of every iteration; exits on the 4th call.
        assert stop.calls == 4
        assert result["iterations"] == 3


class TestObserveSidecar:
    def test_stop_flushes_sub_threshold_tail_and_clears_pid(
        self, tmp_path, conn, obs_dir, fake_claude
    ):
        jsonl = _write_session(tmp_path, "sess-1", [
            _user_line("u1", "hi"),
            _assistant_line("a1", "m1", "hello"),
        ])
        _seed_session_row(conn, "sess-1", jsonl)
        claude = fake_claude([
            {"type": "decision", "text": "x", "context": "",
             "ts": "2026-04-17T10:00:00Z"},
        ])

        result = observer.observe_sidecar(
            conn, "sess-1",
            source_path=jsonl,
            batch_threshold=3,
            watch_backend=observer.WATCH_BACKEND_POLL,
            claude_bin=str(claude),
            should_stop=_StopAfter(0),
            session_active_fn=lambda _sid: True,
            sleep_fn=lambda _: None,
        )

        assert result["status"] == "stopped"
        assert result["extractions"] == 1
        state = db.get_observation_state(conn, "sess-1")
        assert state is not None
        assert state["status"] == "stopped"
        assert state["observer_pid"] is None
        assert state["source_byte_offset"] == jsonl.stat().st_size
        assert (obs_dir / "sess-1.jsonl").exists()
