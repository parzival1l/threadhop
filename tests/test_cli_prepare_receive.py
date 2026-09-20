"""Tests for ``threadhop prepare`` / ``threadhop receive``.

Run in-process so ``run_claude_p`` can be monkeypatched. HOME-adjacent
paths (DB, projects root, transfers dir) are all redirected into
``tmp_path`` — nothing touches the real ``~/.config``.
"""

from __future__ import annotations

import json
import re
from pathlib import Path

import pytest

from threadhop_core import transfers
from threadhop_core.cli.commands.prepare import cmd_prepare
from threadhop_core.cli.commands.receive import cmd_receive
from threadhop_core.cli.parser import build_parser
from threadhop_core.harness import claude
from threadhop_core.harness.claude import HarnessResult
from threadhop_core.session import detection
from threadhop_core.storage import db as db_mod

SID = "ffff5555-5555-5555-5555-555555555555"


# --- Fixtures ---------------------------------------------------------------


@pytest.fixture
def cli_env(tmp_path: Path, monkeypatch) -> Path:
    """Redirect DB / projects root / transfers dir into tmp_path.

    Returns the fake ``~/.claude/projects`` root.
    """
    projects = tmp_path / "projects"
    projects.mkdir()
    monkeypatch.setattr(db_mod, "DB_PATH", tmp_path / "sessions.db")
    monkeypatch.setattr(detection, "CLAUDE_PROJECTS", projects)
    monkeypatch.setattr(transfers, "TRANSFERS_DIR", tmp_path / "transfers")
    return projects


class FakeClaude:
    """Recording stand-in for ``run_claude_p``."""

    def __init__(self, stdout="SUMMARY BRIEF", returncode=0, stderr=""):
        self.stdout = stdout
        self.returncode = returncode
        self.stderr = stderr
        self.calls: list[str] = []

    def __call__(self, prompt: str, **kwargs) -> HarnessResult:
        self.calls.append(prompt)
        return HarnessResult(
            returncode=self.returncode,
            stdout=self.stdout,
            stderr=self.stderr,
        )


def _user(uuid: str, text: str) -> str:
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": SID,
        "timestamp": "2026-04-20T10:00:00Z",
        "message": {"content": [{"type": "text", "text": text}]},
    })


def _assistant(uuid: str, mid: str, text: str) -> str:
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": SID,
        "timestamp": "2026-04-20T10:00:01Z",
        "message": {"id": mid, "content": [{"type": "text", "text": text}]},
    })


def _exchange_lines(i: int) -> list[str]:
    return [
        _user(f"u{i}", f"user prompt {i}"),
        _assistant(f"a{i}", f"m{i}", f"assistant reply {i}"),
    ]


def _write_session(projects: Path, n_exchanges: int) -> Path:
    project_dir = projects / "-Users-alice-alpha"
    project_dir.mkdir(parents=True, exist_ok=True)
    lines: list[str] = []
    for i in range(1, n_exchanges + 1):
        lines.extend(_exchange_lines(i))
    path = project_dir / f"{SID}.jsonl"
    path.write_text("\n".join(lines) + "\n")
    return path


def _append_exchanges(path: Path, start: int, count: int) -> None:
    lines: list[str] = []
    for i in range(start, start + count):
        lines.extend(_exchange_lines(i))
    with open(path, "a") as f:
        f.write("\n".join(lines) + "\n")


def _prepare_args(*extra: str):
    return build_parser().parse_args(["prepare", "--session", SID, *extra])


def _ticket_files() -> list[Path]:
    """Ticket files in creation order (ids are random hex, so sort by mtime)."""
    if not transfers.TRANSFERS_DIR.is_dir():
        return []
    return sorted(
        transfers.TRANSFERS_DIR.glob("tk_*.md"),
        key=lambda p: (p.stat().st_mtime_ns, p.name),
    )


# --- prepare ----------------------------------------------------------------


def test_prepare_writes_ticket_and_paste_line(cli_env, monkeypatch, capsys):
    _write_session(cli_env, 5)
    fake = FakeClaude(stdout="SUMMARY BRIEF")
    monkeypatch.setattr(claude, "run_claude_p", fake)

    rc = cmd_prepare(_prepare_args())

    assert rc == 0
    assert len(fake.calls) == 1
    # Full-head path: instruction template + labelled conversation.
    assert "## CONVERSATION" in fake.calls[0]
    assert "user prompt 1" in fake.calls[0]
    assert "user prompt 2" in fake.calls[0]
    # Tail exchanges (3-5) are NOT in the summarization prompt.
    assert "user prompt 4" not in fake.calls[0]

    [ticket] = _ticket_files()
    ticket_id = ticket.stem
    assert re.fullmatch(r"tk_[0-9a-f]{8}", ticket_id)

    content = ticket.read_text()
    assert content.startswith(f"# ThreadHop transfer ticket {ticket_id}\n")
    assert f"({SID})" in content
    assert "-Users-alice-alpha" in content
    assert "## Context summary\nSUMMARY BRIEF" in content
    assert "## Recent conversation (verbatim)" in content
    # Tail = last 3 exchanges, verbatim with labels.
    assert "User:\nuser prompt 3" in content
    assert "Assistant:\nassistant reply 5" in content
    assert "user prompt 2" not in content

    out = capsys.readouterr().out
    assert str(ticket) in out
    assert f"Paste in the target chat: !threadhop receive {ticket_id}" in out
    assert f"(or: threadhop receive {ticket_id})" in out


def test_prepare_reuses_cached_summary_with_zero_llm_calls(
    cli_env, monkeypatch,
):
    _write_session(cli_env, 5)
    fake = FakeClaude(stdout="SUMMARY BRIEF")
    monkeypatch.setattr(claude, "run_claude_p", fake)

    assert cmd_prepare(_prepare_args()) == 0
    assert len(fake.calls) == 1

    # Source JSONL unchanged → second prepare must not call the LLM.
    assert cmd_prepare(_prepare_args()) == 0
    assert len(fake.calls) == 1

    tickets = _ticket_files()
    assert len(tickets) == 2
    assert "## Context summary\nSUMMARY BRIEF" in tickets[1].read_text()


def test_prepare_merges_incrementally_when_source_grew(cli_env, monkeypatch):
    path = _write_session(cli_env, 5)
    fake = FakeClaude(stdout="SUMMARY V1")
    monkeypatch.setattr(claude, "run_claude_p", fake)
    assert cmd_prepare(_prepare_args()) == 0
    assert len(fake.calls) == 1

    # Session grows by two exchanges → head now covers exchanges 1-4;
    # 1-2 are already summarized, so only 3-4 go into the merge call.
    _append_exchanges(path, 6, 2)
    fake.stdout = "SUMMARY V2"
    assert cmd_prepare(_prepare_args()) == 0

    assert len(fake.calls) == 2
    merge_prompt = fake.calls[1]
    assert "## PREVIOUS SUMMARY" in merge_prompt
    assert "SUMMARY V1" in merge_prompt
    assert "## NEW MESSAGES" in merge_prompt
    assert "user prompt 3" in merge_prompt
    assert "user prompt 4" in merge_prompt
    # Already-summarized head and the verbatim tail stay out.
    assert "user prompt 1" not in merge_prompt
    assert "user prompt 6" not in merge_prompt

    latest = _ticket_files()[-1].read_text()
    assert "SUMMARY V2" in latest


def test_prepare_llm_failure_writes_no_ticket(cli_env, monkeypatch, capsys):
    _write_session(cli_env, 5)
    fake = FakeClaude(returncode=1, stdout="", stderr="model exploded")
    monkeypatch.setattr(claude, "run_claude_p", fake)

    rc = cmd_prepare(_prepare_args())

    assert rc == 1
    assert _ticket_files() == []
    err = capsys.readouterr().err
    assert "model exploded" in err
    # No stale cache row either — the failed call must not poison reuse.
    conn = db_mod.init_db(db_mod.DB_PATH)
    try:
        assert db_mod.get_transfer_state(conn, SID) is None
    finally:
        conn.close()


def test_prepare_short_session_skips_llm_entirely(cli_env, monkeypatch):
    _write_session(cli_env, 2)  # <= --tail 3 → empty head

    def _boom(*a, **kw):  # pragma: no cover — must never run
        raise AssertionError("LLM must not be called for a short session")

    monkeypatch.setattr(claude, "run_claude_p", _boom)

    rc = cmd_prepare(_prepare_args())

    assert rc == 0
    [ticket] = _ticket_files()
    content = ticket.read_text()
    assert transfers.TOO_SHORT_NOTE in content
    # Everything rides verbatim in the tail.
    assert "User:\nuser prompt 1" in content
    assert "Assistant:\nassistant reply 2" in content


def test_prepare_tail_budget_drops_oldest_tail_exchanges(
    cli_env, monkeypatch,
):
    _write_session(cli_env, 5)
    fake = FakeClaude(stdout="SUMMARY BRIEF")
    monkeypatch.setattr(claude, "run_claude_p", fake)

    # Each rendered exchange is ~60 chars; a 120-char budget keeps the
    # final two at most — exchange 3 (oldest tail entry) is dropped.
    rc = cmd_prepare(_prepare_args("--tail-budget", "120"))

    assert rc == 0
    content = _ticket_files()[0].read_text()
    assert "user prompt 5" in content
    assert "user prompt 3" not in content


def test_prepare_auto_detect_failure_exits_2(cli_env, monkeypatch, capsys):
    import threadhop_core.cli.helpers as helpers

    monkeypatch.setattr(helpers, "detect_current_session_id", lambda: None)
    args = build_parser().parse_args(["prepare"])

    rc = cmd_prepare(args)

    assert rc == 2
    assert "could not auto-detect" in capsys.readouterr().err


# --- tail budget unit edge --------------------------------------------------


def test_fit_tail_truncates_lone_oversized_final_exchange():
    from threadhop_core.exchanges import Exchange

    ex = Exchange(user_text="x" * 500, assistant_texts=["y" * 500])
    text, dropped = transfers.fit_tail_to_budget([ex], budget=200)

    assert dropped == 0
    assert transfers.TRUNCATION_MARKER.strip() in text
    assert len(text) <= 200 + len(transfers.TRUNCATION_MARKER)
    assert text.startswith("User:\nxxx")
    assert text.endswith("y" * 5)  # the exchange's ending survives


# --- receive ------------------------------------------------------------------


def test_receive_roundtrip(cli_env, monkeypatch, capsys):
    _write_session(cli_env, 5)
    fake = FakeClaude(stdout="SUMMARY BRIEF")
    monkeypatch.setattr(claude, "run_claude_p", fake)
    assert cmd_prepare(_prepare_args()) == 0
    [ticket] = _ticket_files()
    ticket_id = ticket.stem
    capsys.readouterr()  # clear prepare output

    # Full tk_ id.
    args = build_parser().parse_args(["receive", ticket_id])
    assert cmd_receive(args) == 0
    assert capsys.readouterr().out == ticket.read_text()

    # Bare hex id.
    args = build_parser().parse_args(["receive", ticket_id[3:]])
    assert cmd_receive(args) == 0
    assert capsys.readouterr().out == ticket.read_text()

    # Filesystem path.
    args = build_parser().parse_args(["receive", str(ticket)])
    assert cmd_receive(args) == 0
    assert capsys.readouterr().out == ticket.read_text()


def test_receive_missing_ticket_exits_1(cli_env, capsys):
    args = build_parser().parse_args(["receive", "tk_deadbeef"])

    rc = cmd_receive(args)

    assert rc == 1
    err = capsys.readouterr().err
    assert "Ticket not found: tk_deadbeef" in err
    assert str(transfers.TRANSFERS_DIR) in err
