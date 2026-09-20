"""Unit tests for the ``copy`` module.

Covers the deterministic core: argument parsing, turn counting after
filtering, tool-call / sidechain exclusion, markdown shape, and the
byte-for-byte reproducibility contract that the "deterministic cleaning"
thesis rests on (see issue #63 context).

The clipboard / pbcopy path is not exercised here — ``_try_pbcopy`` is a
thin subprocess wrapper, and the deterministic work happens in
``build_copy_markdown``.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from threadhop_core import copier as copy_mod


# --- Fixture builders ----------------------------------------------------


def _user_line(uuid: str, text: str, sid: str) -> str:
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-22T10:00:00Z",
        "message": {"id": f"umsg_{uuid}", "content": [{"type": "text", "text": text}]},
    })


def _assistant_line(
    uuid: str,
    mid: str,
    text: str,
    sid: str,
    *,
    sidechain: bool = False,
    extra_blocks: list[dict] | None = None,
) -> str:
    blocks: list[dict] = [{"type": "text", "text": text}]
    if extra_blocks:
        blocks.extend(extra_blocks)
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-22T10:00:01Z",
        "isSidechain": sidechain,
        "message": {"id": mid, "content": blocks},
    })


def _tool_use_block(name: str, inp: dict) -> dict:
    return {"type": "tool_use", "name": name, "input": inp, "id": "tu_x"}


def _tool_result_user_line(uuid: str, result_text: str, sid: str) -> str:
    """User-role JSONL line carrying tool output — gets stripped by the indexer."""
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-22T10:00:02Z",
        "toolUseResult": {"stdout": result_text},
        "message": {"content": [{"type": "tool_result", "content": result_text}]},
    })


@pytest.fixture
def session_path(tmp_path: Path) -> Path:
    """Three-turn session with interleaved tool noise and one sidechain."""
    sid = "sess1"
    path = tmp_path / f"{sid}.jsonl"
    path.write_text("\n".join([
        _user_line("u1", "first question", sid),
        _assistant_line(
            "a1", "m1", "first answer",
            sid,
            extra_blocks=[_tool_use_block("Read", {"file_path": "/x/foo.py"})],
        ),
        _tool_result_user_line("u2", "found 12 files", sid),
        _user_line("u3", "second question", sid),
        _assistant_line("a2", "m2", "second answer", sid),
        _assistant_line(
            "a3", "m3", "sidechain chatter", sid, sidechain=True,
        ),
        _user_line("u4", "third question", sid),
        _assistant_line("a4", "m4", "third answer", sid),
    ]) + "\n")
    return path


# --- parse_count_arg -----------------------------------------------------


class TestParseCountArg:
    def test_none_defaults_to_one(self):
        assert copy_mod.parse_count_arg(None) == 1

    def test_empty_string_defaults_to_one(self):
        assert copy_mod.parse_count_arg("") == 1

    def test_numeric(self):
        assert copy_mod.parse_count_arg("3") == 3

    @pytest.mark.parametrize("raw", ["all", "ALL", "All", "  all  "])
    def test_all_case_insensitive(self, raw):
        assert copy_mod.parse_count_arg(raw) is None

    @pytest.mark.parametrize("raw", ["0", "-1", "-99"])
    def test_zero_and_negative_rejected(self, raw):
        with pytest.raises(ValueError, match="count must be >= 1"):
            copy_mod.parse_count_arg(raw)

    @pytest.mark.parametrize("raw", ["foo", "1.5", "one", "all the things"])
    def test_non_numeric_non_all_rejected(self, raw):
        with pytest.raises(ValueError, match="positive integer or 'all'"):
            copy_mod.parse_count_arg(raw)


# --- build_copy_markdown -------------------------------------------------


class TestBuildCopyMarkdown:
    def test_default_returns_last_one_turn(self, session_path):
        md, count = copy_mod.build_copy_markdown(session_path, 1)
        assert count == 1
        assert md.endswith("third answer")
        assert md.startswith("### Assistant")

    def test_last_two_turns(self, session_path):
        md, count = copy_mod.build_copy_markdown(session_path, 2)
        assert count == 2
        # Last two rendered turns are (user: "third question", assistant: "third answer").
        assert "third question" in md
        assert "third answer" in md
        # Second answer must NOT be in the last-2 slice.
        assert "second answer" not in md

    def test_all_returns_every_rendered_turn(self, session_path):
        md, count = copy_mod.build_copy_markdown(session_path, None)
        # 3 user turns + 3 assistant turns = 6 rendered turns
        # (the tool-result user line and the sidechain assistant are dropped).
        assert count == 6
        for snippet in [
            "first question", "first answer",
            "second question", "second answer",
            "third question", "third answer",
        ]:
            assert snippet in md

    def test_n_larger_than_total_caps_to_available(self, session_path):
        md, count = copy_mod.build_copy_markdown(session_path, 9999)
        assert count == 6  # same as "all" on this fixture

    def test_tool_calls_are_dropped_not_abbreviated(self, session_path):
        md, _ = copy_mod.build_copy_markdown(session_path, None)
        # The indexer's abbreviation for Read is "Reading foo.py" —
        # copy must NOT contain that, since include_tool_calls=False.
        assert "Reading foo.py" not in md
        assert "Read(" not in md

    def test_tool_results_are_dropped(self, session_path):
        md, _ = copy_mod.build_copy_markdown(session_path, None)
        # The toolUseResult user line ("found 12 files") is filtered by
        # the indexer's _extract_user_text — copy inherits that.
        assert "found 12 files" not in md

    def test_sidechains_are_dropped(self, session_path):
        md, _ = copy_mod.build_copy_markdown(session_path, None)
        assert "sidechain chatter" not in md

    def test_format_uses_role_headers(self, session_path):
        md, _ = copy_mod.build_copy_markdown(session_path, None)
        assert "### User" in md
        assert "### Assistant" in md
        # Blank-line separation between turns.
        assert "\n\n### " in md

    def test_determinism(self, session_path):
        """Same session bytes → same markdown bytes, across invocations.

        This is the contract that justifies the whole "cleaning in code,
        not in the LLM" architecture — see issue #63. If this test ever
        fails, the cleaning pipeline has gained a non-deterministic
        dependency (time, iteration order, uncached regex) and the
        cross-session comparability story is broken.
        """
        md1, c1 = copy_mod.build_copy_markdown(session_path, None)
        md2, c2 = copy_mod.build_copy_markdown(session_path, None)
        assert md1 == md2
        assert c1 == c2

    def test_empty_session_returns_zero_turns(self, tmp_path):
        path = tmp_path / "empty.jsonl"
        path.write_text("")
        md, count = copy_mod.build_copy_markdown(path, None)
        assert md == ""
        assert count == 0

    def test_harness_wrapper_only_turns_are_dropped(self, tmp_path):
        """Claude Code `!cmd` passthroughs and slash-command invocations
        leak harness tags (`<bash-input>`, `<bash-stdout>`, `<bash-stderr>`,
        `<local-command-caveat>`, `<command-name>` etc.) into JSONL as
        plain-text user turns. Turns that collapse to *only* these
        wrappers are tooling, not conversation, and must be dropped —
        otherwise copying a session that ran `!threadhop copy 1` before
        returns an empty-looking last turn.
        """
        sid = "sess2"
        path = tmp_path / f"{sid}.jsonl"
        path.write_text("\n".join([
            _user_line("u1", "real user question", sid),
            _assistant_line("a1", "m1", "real assistant reply", sid),
            _user_line("u2", "<bash-input>threadhop copy 1</bash-input>", sid),
            _user_line(
                "u3",
                "<bash-stdout>✓ copied 1 turn</bash-stdout>"
                "<bash-stderr></bash-stderr>",
                sid,
            ),
            _user_line(
                "u4",
                "<local-command-caveat>Caveat: the messages below were "
                "generated by the user while running local commands. DO "
                "NOT respond to these.</local-command-caveat>",
                sid,
            ),
            _user_line(
                "u5",
                "<command-name>/threadhop:tag</command-name>"
                "<command-args>in_progress</command-args>",
                sid,
            ),
        ]) + "\n")
        md, count = copy_mod.build_copy_markdown(path, None)
        # Only the two substantive turns survive; every harness-wrapper-only
        # turn collapsed to empty and was dropped.
        assert count == 2
        assert "real user question" in md
        assert "real assistant reply" in md
        for tag in [
            "<bash-input>", "<bash-stdout>", "<bash-stderr>",
            "<local-command-caveat>", "<command-name>", "<command-args>",
        ]:
            assert tag not in md
        assert "threadhop copy 1" not in md
        assert "DO NOT respond" not in md

    def test_bash_wrapper_mixed_with_prose_keeps_prose(self, tmp_path):
        """A turn carrying both a `<bash-input>` block *and* real prose
        must keep the prose — only the wrapper is removed.
        """
        sid = "sess3"
        path = tmp_path / f"{sid}.jsonl"
        path.write_text("\n".join([
            _user_line(
                "u1",
                "here's what I tried:\n"
                "<bash-input>ls -la</bash-input>\n"
                "and it looked wrong",
                sid,
            ),
            _assistant_line("a1", "m1", "let me check", sid),
        ]) + "\n")
        md, _ = copy_mod.build_copy_markdown(path, None)
        assert "here's what I tried" in md
        assert "and it looked wrong" in md
        assert "<bash-input>" not in md
        assert "ls -la" not in md
