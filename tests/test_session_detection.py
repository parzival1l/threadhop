"""Unit tests for Claude process-arg parsing and session auto-detection."""

from __future__ import annotations

from threadhop_core.session.detection import _parse_claude_process_args

SID = "11111111-1111-1111-1111-111111111111"


def test_parse_bare_claude_is_interactive():
    assert _parse_claude_process_args("claude") == (True, None)


def test_parse_pathed_claude_is_interactive():
    assert _parse_claude_process_args("/usr/local/bin/claude") == (True, None)


def test_parse_resume_uuid_is_explicit():
    assert _parse_claude_process_args(f"claude --resume {SID}") == (True, SID)
    assert _parse_claude_process_args(f"claude -r {SID}") == (True, SID)


def test_parse_print_mode_is_not_interactive():
    assert _parse_claude_process_args("claude -p --resume " + SID) == (False, None)
    assert _parse_claude_process_args("claude --print do something")[0] is False


def test_parse_worktree_without_resume_is_interactive():
    """`claude --worktree <name>` is a live interactive session (issue #70).

    The binary name is the bare token `claude`, which does not end with
    `/claude`, and the last arg is the worktree name — so the old fallback
    treated this as a non-session process and `threadhop tag` failed to
    auto-detect.
    """
    is_interactive, explicit = _parse_claude_process_args(
        "claude --worktree mutable-crunching-snowflake"
    )
    assert is_interactive is True
    assert explicit is None


def test_parse_worktree_with_resume_uuid():
    is_interactive, explicit = _parse_claude_process_args(
        f"claude --worktree mutable-crunching-snowflake --resume {SID}"
    )
    assert is_interactive is True
    assert explicit == SID


def test_parse_equals_form_worktree_is_interactive():
    is_interactive, explicit = _parse_claude_process_args(
        "claude --worktree=mutable-crunching-snowflake"
    )
    assert is_interactive is True
    assert explicit is None


def test_parse_pathed_worktree_is_interactive():
    is_interactive, _ = _parse_claude_process_args(
        "/opt/homebrew/bin/claude --worktree mutable-crunching-snowflake"
    )
    assert is_interactive is True
