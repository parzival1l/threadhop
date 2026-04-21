"""Tests for the handoff brief builder (task #26, ADR-012/016/018/020).

Covers the orchestration:

  * Direct-format path for short observation sets.
  * Polish path when the set is large (Haiku sub-agent, faked).
  * --full path with transcript injection (fake claude sees the
    ``<transcript>`` block).
  * no_source when neither a session row nor prior observations exist.
  * no_observations when the observer runs but nothing is extracted and
    there are no prior observations.
  * Catch-up behaviour when the source JSONL grew since last observation.
  * Graceful fall-back to direct formatting when the polish prompt is
    missing or the sub-agent exits non-zero.

The real observer and handoff polish both shell out to ``claude -p``.
Tests inject a fake shell script via the ``claude_bin`` argument so we
exercise the whole pipeline end-to-end without needing Haiku.

Run from the repo root::

    python -m pytest tests/test_handoff.py -v
"""

from __future__ import annotations

import json
import stat
from pathlib import Path
from typing import Callable

import pytest

import db
import handoff


# --- Fixtures -------------------------------------------------------------


@pytest.fixture
def obs_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Redirect ``db.OBS_DIR`` into tmp_path so observations don't escape."""
    d = tmp_path / "observations"
    d.mkdir()
    monkeypatch.setattr(db, "OBS_DIR", d)
    return d


@pytest.fixture(autouse=True)
def _quiet_reflector(monkeypatch: pytest.MonkeyPatch):
    """Default the reflector to a benign stub across every test in this file.

    ``build_handoff`` resolves ``reflect_fn`` late (when the sentinel
    ``"default"`` is passed, which is the signature default), so patching
    ``reflector.reflect_session`` here replaces the call without forcing
    every existing test to thread ``reflect_fn=None`` through.

    Reflector-specific tests pass their own callable via ``reflect_fn=<fake>``
    which takes precedence over this stub.
    """
    import reflector as reflector_mod

    def _noop_reflect(conn, session_id, **_kwargs):
        return {"status": "up_to_date", "new_entries": 0, "entry_count": 0}

    monkeypatch.setattr(reflector_mod, "reflect_session", _noop_reflect)


@pytest.fixture
def fake_claude_factory(tmp_path: Path):
    """Build a shell script that stands in for ``claude -p ...``.

    The observer invokes ``claude -p <prompt> --model haiku
    --permission-mode acceptEdits``. The handoff polish invokes the same
    flags. We distinguish the two by looking at the prompt body:

      * Observer prompt contains ``Append observations to: <path>``.
        → Append the supplied ``observer_entries`` JSONL to that path.
      * Handoff polish prompt contains ``## Mode``.
        → Print ``polish_output`` to stdout.

    Either entry can be set to ``None`` to leave the script inert for
    that role (useful for testing a path that shouldn't trigger it).

    If ``capture_polish_prompt`` is supplied, the polish invocation
    writes its whole prompt payload to that path — lets tests assert
    the ``--full`` transcript actually made it through.
    """
    def _make(
        observer_entries: list[dict] | None = None,
        polish_output: str | None = None,
        capture_polish_prompt: Path | None = None,
        polish_exit_code: int = 0,
        polish_stderr: str = "",
    ) -> Path:
        path = tmp_path / "fake_claude"
        obs_payload = tmp_path / "fake_observer_entries.jsonl"
        if observer_entries is not None:
            obs_payload.write_text(
                "\n".join(json.dumps(e) for e in observer_entries) + "\n"
            )
        else:
            obs_payload.write_text("")

        polish_payload = tmp_path / "fake_polish_output.md"
        if polish_output is not None:
            polish_payload.write_text(polish_output)
        else:
            polish_payload.write_text("")

        capture_line = (
            f'printf "%s" "$prompt" > "{capture_polish_prompt}"'
            if capture_polish_prompt is not None
            else "true"
        )

        body = f"""#!/usr/bin/env bash
set -euo pipefail
# Args: -p <prompt> --model haiku --permission-mode acceptEdits
prompt="$2"

if printf "%s" "$prompt" | grep -q '^Append observations to: '; then
    obs_path=$(printf '%s' "$prompt" | sed -n 's/^Append observations to: //p' | tail -n 1)
    if [ -s "{obs_payload}" ]; then
        cat "{obs_payload}" >> "$obs_path"
    fi
    exit 0
fi

# Handoff polish invocation.
{capture_line}
if [ "{polish_exit_code}" != "0" ]; then
    printf "%s" "{polish_stderr}" 1>&2
    exit {polish_exit_code}
fi
cat "{polish_payload}"
"""
        path.write_text(body)
        path.chmod(
            path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH
        )
        return path

    return _make


# --- JSONL builders (mirror test_observer.py) -----------------------------


def _user_line(uuid: str, text: str, sid: str = "sess-1") -> str:
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-17T10:00:00Z",
        "message": {
            "id": f"umsg_{uuid}",
            "content": [{"type": "text", "text": text}],
        },
    })


def _assistant_line(uuid: str, mid: str, text: str, sid: str = "sess-1") -> str:
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-17T10:00:01Z",
        "message": {
            "id": mid,
            "content": [{"type": "text", "text": text}],
        },
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


def _three_turn_session(tmp_path: Path, sid: str = "sess-1") -> Path:
    """Three-turn session — meets the default observer threshold."""
    return _write_session(tmp_path, sid, [
        _user_line("u1", "Should we use SQLite?", sid=sid),
        _assistant_line(
            "a1", "m1", "Yes, SQLite fits the use case.", sid=sid,
        ),
        _user_line("u2", "Great, decided.", sid=sid),
    ])


# --- Direct-format unit tests --------------------------------------------


class TestFormatBriefDirect:
    def test_groups_entries_by_type_in_display_order(self):
        entries = [
            {"type": "observation", "text": "JSONL has dup ids",
             "context": "", "ts": "t1"},
            {"type": "decision", "text": "REST over gRPC",
             "context": "SDK constraints", "ts": "t2"},
            {"type": "todo", "text": "Write tests", "context": "",
             "ts": "t3"},
            {"type": "adr", "text": "Use SQLite",
             "context": "single-file deployment", "ts": "t4"},
        ]
        out = handoff._format_brief_direct(
            "abc123456789xxx", {"project": "atlas"}, entries,
        )
        # Title carries short session id and project.
        assert out.splitlines()[0] == "# Handoff — session abc123456789 · project `atlas`"
        # Ordering: decision, adr, todo, observation.
        decisions_idx = out.index("## Decisions")
        adrs_idx = out.index("## ADRs")
        todos_idx = out.index("## Open TODOs")
        obs_idx = out.index("## Observations")
        assert decisions_idx < adrs_idx < todos_idx < obs_idx

    def test_empty_sections_are_omitted(self):
        entries = [
            {"type": "decision", "text": "x", "context": "", "ts": "t"},
        ]
        out = handoff._format_brief_direct("abc", None, entries)
        assert "## Decisions" in out
        assert "## Open TODOs" not in out
        assert "## ADRs" not in out

    def test_context_rendered_as_parenthetical(self):
        entries = [
            {"type": "decision", "text": "REST over gRPC",
             "context": "SDK constraints", "ts": "t"},
        ]
        out = handoff._format_brief_direct("abc", None, entries)
        assert "- REST over gRPC  _(SDK constraints)_" in out

    def test_conflict_includes_refs(self):
        entries = [
            {"type": "conflict", "text": "REST vs gRPC",
             "context": "", "ts": "t",
             "refs": ["abcd1234efgh", "ijkl5678mnop"]},
        ]
        out = handoff._format_brief_direct("sid", None, entries)
        assert "## Conflicts" in out
        # First 8 chars of each ref.
        assert "refs: abcd1234, ijkl5678" in out

    def test_unknown_type_still_surfaces(self):
        entries = [
            {"type": "mystery", "text": "unknown", "context": "",
             "ts": "t"},
        ]
        out = handoff._format_brief_direct("sid", None, entries)
        assert "## Mystery" in out
        assert "- unknown" in out


class TestReadObservations:
    def test_missing_file_returns_empty(self, tmp_path: Path):
        assert handoff._read_observations(tmp_path / "nope.jsonl") == []

    def test_skips_malformed_lines(self, tmp_path: Path):
        p = tmp_path / "obs.jsonl"
        p.write_text(
            '{"type":"decision","text":"ok","context":"","ts":"t"}\n'
            "not-json\n"
            "\n"
            '{"type":"todo","text":"x","context":"","ts":"t"}\n'
        )
        entries = handoff._read_observations(p)
        assert [e["type"] for e in entries] == ["decision", "todo"]


# --- Orchestration tests --------------------------------------------------


class TestBuildHandoff:
    def test_no_source_when_unknown_session(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        claude = fake_claude_factory()
        result = handoff.build_handoff(
            conn, "ghost", claude_bin=str(claude),
        )
        assert result["status"] == "no_source"
        assert result["brief"] == ""

    def test_direct_format_when_observer_extracts_few_entries(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)

        canned = [
            {"type": "decision", "text": "Use SQLite",
             "context": "single-file deployment",
             "ts": "2026-04-17T10:00:03Z"},
            {"type": "todo", "text": "Write schema migration",
             "context": "", "ts": "2026-04-17T10:00:03Z"},
        ]
        # No polish output — direct path should never invoke polish.
        claude = fake_claude_factory(observer_entries=canned)

        result = handoff.build_handoff(
            conn, "sess-1", claude_bin=str(claude),
        )

        assert result["status"] == "ok"
        assert result["mode"] == "direct"
        assert result["entry_count"] == 2
        brief = result["brief"]
        assert brief.startswith("# Handoff — session sess-1")
        assert "## Decisions" in brief
        assert "- Use SQLite  _(single-file deployment)_" in brief
        assert "## Open TODOs" in brief
        assert "- Write schema migration" in brief

    def test_no_observations_when_observer_extracts_nothing(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)

        # Observer runs but writes no observation lines.
        claude = fake_claude_factory(observer_entries=[])

        result = handoff.build_handoff(
            conn, "sess-1", claude_bin=str(claude),
        )
        assert result["status"] == "no_observations"
        assert result["brief"] == ""
        # Observer did run — its result is attached.
        assert result["observer_result"]["status"] == "extracted"

    def test_polish_path_when_set_exceeds_large_threshold(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)

        many = [
            {"type": "decision", "text": f"decision {i}",
             "context": "", "ts": "t"}
            for i in range(5)
        ]
        polished = "# Handoff — polished\n\n- compressed bullet\n"
        claude = fake_claude_factory(
            observer_entries=many, polish_output=polished,
        )

        result = handoff.build_handoff(
            conn, "sess-1", claude_bin=str(claude),
            large_set_threshold=3,  # force polish path
        )
        assert result["status"] == "ok"
        assert result["mode"] == "polish"
        assert result["entry_count"] == 5
        assert result["brief"] == polished + "\n" or result["brief"].startswith(polished)
        assert "polished" in result["brief"]

    def test_polish_fallback_to_direct_on_nonzero_exit(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)

        canned = [
            {"type": "decision", "text": "x", "context": "", "ts": "t"},
            {"type": "decision", "text": "y", "context": "", "ts": "t"},
            {"type": "decision", "text": "z", "context": "", "ts": "t"},
            {"type": "decision", "text": "w", "context": "", "ts": "t"},
        ]
        claude = fake_claude_factory(
            observer_entries=canned,
            polish_output="should-not-appear",
            polish_exit_code=7,
            polish_stderr="synthetic failure",
        )

        result = handoff.build_handoff(
            conn, "sess-1", claude_bin=str(claude),
            large_set_threshold=2,  # force polish
        )
        assert result["status"] == "ok"
        assert result["mode"] == "direct"  # fell back
        assert "fallback" in result["message"]
        assert "polish exited 7" in result["message"]
        assert "should-not-appear" not in result["brief"]
        assert "- x" in result["brief"]

    def test_full_flag_always_invokes_polish_with_transcript(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)

        canned = [
            {"type": "decision", "text": "Use SQLite",
             "context": "", "ts": "t"},
        ]  # only 1 entry — wouldn't trigger polish without --full
        capture = tmp_path / "polish_prompt.txt"
        polished = "# Handoff — full\n\n## Summary\n\nstub full handoff\n"
        claude = fake_claude_factory(
            observer_entries=canned,
            polish_output=polished,
            capture_polish_prompt=capture,
        )

        result = handoff.build_handoff(
            conn, "sess-1", full=True, claude_bin=str(claude),
        )
        assert result["status"] == "ok"
        assert result["mode"] == "full"
        assert polished.strip() in result["brief"]

        # Polish prompt saw both the observations and the transcript.
        prompt_text = capture.read_text()
        assert "<observations>" in prompt_text
        assert '"text": "Use SQLite"' in prompt_text or '"text":"Use SQLite"' in prompt_text
        assert "<transcript>" in prompt_text
        # The cleaned transcript renders our three user/assistant turns.
        assert "Should we use SQLite?" in prompt_text
        assert "SQLite fits the use case" in prompt_text

    def test_catch_up_reads_appended_bytes(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)

        # First handoff — extracts 1 decision.
        first_entries = [{
            "type": "decision", "text": "First pass", "context": "",
            "ts": "t1",
        }]
        claude_first = fake_claude_factory(observer_entries=first_entries)
        r1 = handoff.build_handoff(
            conn, "sess-1", claude_bin=str(claude_first),
        )
        assert r1["status"] == "ok"
        assert r1["entry_count"] == 1

        # Append more turns to the source JSONL.
        with open(jsonl, "a") as f:
            f.write(_assistant_line("a2", "m2", "new thought") + "\n")
            f.write(_user_line("u3", "follow-up") + "\n")
            f.write(_assistant_line("a3", "m3", "another decision") + "\n")

        # Second handoff — extracts 1 more decision from the NEW bytes only.
        second_entries = [{
            "type": "decision", "text": "Second pass", "context": "",
            "ts": "t2",
        }]
        claude_second = fake_claude_factory(observer_entries=second_entries)
        r2 = handoff.build_handoff(
            conn, "sess-1", claude_bin=str(claude_second),
        )
        assert r2["status"] == "ok"
        # The observation file accumulates — both entries are in the brief.
        assert r2["entry_count"] == 2
        assert "First pass" in r2["brief"]
        assert "Second pass" in r2["brief"]

    def test_existing_observations_used_when_observer_fails(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        # Seed an observation file directly — no session row, no claude.
        obs_file = obs_dir / "sess-1.jsonl"
        obs_file.write_text(
            '{"type":"decision","text":"Prior decision","context":"","ts":"t"}\n'
        )
        # Seed observation_state so the observer can find the file on resume.
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)
        db.upsert_observation_state(
            conn, "sess-1", str(jsonl), str(obs_file),
            source_byte_offset=jsonl.stat().st_size,  # cursor at EOF
            entry_count=1,
        )

        # Fake claude will fail if invoked — but it shouldn't be: with
        # cursor already at EOF, the observer returns up_to_date without
        # running claude.
        bad_claude = tmp_path / "bad_claude"
        bad_claude.write_text("#!/usr/bin/env bash\nexit 99\n")
        bad_claude.chmod(0o755)

        result = handoff.build_handoff(
            conn, "sess-1", claude_bin=str(bad_claude),
        )
        assert result["status"] == "ok"
        assert result["entry_count"] == 1
        assert "Prior decision" in result["brief"]

    def test_polish_prompt_missing_falls_back_to_direct(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)

        canned = [
            {"type": "decision", "text": f"d{i}", "context": "", "ts": "t"}
            for i in range(5)
        ]
        # observer_entries supplied; polish_output irrelevant since we
        # point the prompt_path at a non-existent file.
        claude = fake_claude_factory(observer_entries=canned)

        result = handoff.build_handoff(
            conn, "sess-1", claude_bin=str(claude),
            large_set_threshold=2,  # force polish path
            prompt_path=tmp_path / "does-not-exist.md",
        )
        assert result["status"] == "ok"
        assert result["mode"] == "direct"
        assert "fallback" in result["message"]
        assert "- d0" in result["brief"]

    def test_reflect_fn_invoked_after_observer(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        """ADR-022: the reflector runs as a follow-up step after the observer.

        Asserts ``reflect_fn`` is called with ``(conn, session_id)`` and that
        a ``reflector_result`` appears in the returned dict so the CLI (and
        eventual consumers) can surface reflector state.
        """
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)
        canned = [
            {"type": "decision", "text": "Use SQLite",
             "context": "", "ts": "t"},
        ]
        claude = fake_claude_factory(observer_entries=canned)

        calls: list[dict] = []

        def fake_reflect(conn_arg, session_id, **kwargs):
            calls.append({"session_id": session_id, **kwargs})
            return {"status": "extracted", "new_entries": 1}

        result = handoff.build_handoff(
            conn, "sess-1",
            claude_bin=str(claude),
            reflect_fn=fake_reflect,
        )

        assert result["status"] == "ok"
        assert len(calls) == 1
        assert calls[0]["session_id"] == "sess-1"
        # claude_bin is forwarded so the reflector uses the same fake binary.
        assert calls[0]["claude_bin"] == str(claude)
        # Reflector outcome surfaces on the returned dict.
        assert result["reflector_result"] == {
            "status": "extracted", "new_entries": 1,
        }
        # No error when the reflector succeeds.
        assert "reflector_error" not in result

    def test_reflect_fn_exception_is_swallowed(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        """A reflector failure MUST NOT break the handoff — the observer's
        output is already on disk and the brief should still render.
        """
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)
        canned = [
            {"type": "decision", "text": "Use SQLite",
             "context": "", "ts": "t"},
        ]
        claude = fake_claude_factory(observer_entries=canned)

        def boom(conn_arg, session_id, **kwargs):
            raise RuntimeError("synthetic reflector failure")

        result = handoff.build_handoff(
            conn, "sess-1",
            claude_bin=str(claude),
            reflect_fn=boom,
        )

        assert result["status"] == "ok"
        assert "Use SQLite" in result["brief"]
        assert result["reflector_result"] is None
        assert "RuntimeError: synthetic reflector failure" in result["reflector_error"]

    def test_reflect_fn_none_skips_reflector(
        self, tmp_path, conn, obs_dir, fake_claude_factory,
    ):
        """Explicit ``reflect_fn=None`` disables the reflector entirely —
        useful for ``--no-reflect`` debugging and for tests that don't want
        to care about the reflector.
        """
        jsonl = _three_turn_session(tmp_path)
        _seed_session_row(conn, "sess-1", jsonl)
        canned = [
            {"type": "decision", "text": "Use SQLite",
             "context": "", "ts": "t"},
        ]
        claude = fake_claude_factory(observer_entries=canned)

        result = handoff.build_handoff(
            conn, "sess-1",
            claude_bin=str(claude),
            reflect_fn=None,
        )

        assert result["status"] == "ok"
        # Nothing attempted — reflector_result stays None.
        assert result["reflector_result"] is None
        assert "reflector_error" not in result
