"""CLI tests for ``threadhop peek`` (subprocess, isolated HOME)."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
THREADHOP = ROOT / "threadhop"

SID_A = "aaaa1111-1111-1111-1111-111111111111"
SID_B = "aaab2222-2222-2222-2222-222222222222"


def _user(uuid: str, text: str, sid: str, ts: str) -> str:
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": ts,
        "message": {"content": [{"type": "text", "text": text}]},
    })


def _assistant(uuid: str, mid: str, text: str, sid: str, ts: str) -> str:
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": ts,
        "message": {"id": mid, "content": [{"type": "text", "text": text}]},
    })


def _write_session(
    home: Path, project: str, sid: str, n_exchanges: int = 6,
) -> Path:
    project_dir = home / ".claude" / "projects" / project
    project_dir.mkdir(parents=True, exist_ok=True)
    lines: list[str] = []
    for i in range(1, n_exchanges + 1):
        ts = f"2026-04-20T10:{i:02d}:00Z"
        marker = "alpha" if i == 1 else f"topic{i}"
        lines.append(_user(f"u{i}", f"question {i} about {marker}", sid, ts))
        lines.append(_assistant(f"a{i}", f"m{i}", f"reply {i}", sid, ts))
    path = project_dir / f"{sid}.jsonl"
    path.write_text("\n".join(lines) + "\n")
    return path


def _run(home: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["HOME"] = str(home)
    return subprocess.run(
        [str(THREADHOP), "peek", *args],
        cwd=ROOT, env=env, capture_output=True, text=True, check=False,
    )


def test_peek_defaults_to_last_five_exchanges(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)

    result = _run(home, SID_A)

    assert result.returncode == 0, result.stderr
    assert (
        f'[From "{SID_A[:8]}" — -Users-alice-alpha — '
        "2026-04-20T10:02:00Z..2026-04-20T10:06:00Z — "
        "exchanges 2-6 of 6]"
    ) in result.stdout
    assert "question 1 about alpha" not in result.stdout
    assert "User:\nquestion 2 about topic2" in result.stdout
    assert "Assistant:\nreply 6" in result.stdout


def test_peek_last_n(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)

    result = _run(home, SID_A, "--last", "1")

    assert result.returncode == 0, result.stderr
    assert "exchange 6 of 6]" in result.stdout
    assert "question 6" in result.stdout
    assert "question 5" not in result.stdout


def test_peek_range_is_one_based_inclusive(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)

    result = _run(home, SID_A, "--range", "2:3")

    assert result.returncode == 0, result.stderr
    assert "exchanges 2-3 of 6]" in result.stdout
    assert "question 2" in result.stdout
    assert "question 3" in result.stdout
    assert "question 1" not in result.stdout
    assert "question 4" not in result.stdout


def test_peek_range_rejects_malformed_spec(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)

    result = _run(home, SID_A, "--range", "3-5")

    assert result.returncode == 2
    assert "invalid --range" in result.stderr


def test_peek_grep_prints_matching_exchange_in_full(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)

    result = _run(home, SID_A, "--grep", "ALPHA")

    assert result.returncode == 0, result.stderr
    assert "exchange 1 of 6]" in result.stdout
    # Exchange-bounded: the whole exchange prints, including the
    # assistant reply that doesn't itself match.
    assert "question 1 about alpha" in result.stdout
    assert "Assistant:\nreply 1" in result.stdout
    assert "question 2" not in result.stdout


def test_peek_grep_no_match_exits_1(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)

    result = _run(home, SID_A, "--grep", "zebrasaurus")

    assert result.returncode == 1
    assert "no exchanges match" in result.stderr


def test_peek_resolves_unique_prefix(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)
    _write_session(home, "-Users-alice-beta", SID_B)

    result = _run(home, "aaaa", "--last", "1")

    assert result.returncode == 0, result.stderr
    assert f'[From "{SID_A[:8]}"' in result.stdout


def test_peek_ambiguous_prefix_exits_2_listing_candidates(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)
    _write_session(home, "-Users-alice-beta", SID_B)

    result = _run(home, "aaa")

    assert result.returncode == 2
    assert "ambiguous" in result.stderr
    assert SID_A in result.stderr
    assert SID_B in result.stderr


def test_peek_unknown_session_exits_1(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)

    result = _run(home, "zzzz9999")

    assert result.returncode == 1
    assert "no session matches" in result.stderr


def test_peek_modes_are_mutually_exclusive(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A)

    result = _run(home, SID_A, "--last", "2", "--grep", "alpha")

    assert result.returncode == 2
    assert "not allowed with" in result.stderr
