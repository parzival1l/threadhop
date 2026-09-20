"""CLI tests for ``threadhop search`` (subprocess, isolated HOME).

The subprocess writes its DB under the temp HOME, so the incremental
indexer builds a fresh FTS index from the temp transcripts on the fly —
exactly the "CLI results are fresh without the TUI running" contract.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
THREADHOP = ROOT / "threadhop"

SID_A = "cccc1111-1111-1111-1111-111111111111"
SID_B = "dddd2222-2222-2222-2222-222222222222"


def _line(kind: str, uuid: str, text: str, sid: str, mid: str | None) -> str:
    payload: dict = {
        "type": kind,
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": "2026-04-20T10:00:00Z",
        "message": {"content": [{"type": "text", "text": text}]},
    }
    if mid:
        payload["message"]["id"] = mid
    return json.dumps(payload)


def _write_session(home: Path, project: str, sid: str, texts: list[str]) -> None:
    project_dir = home / ".claude" / "projects" / project
    project_dir.mkdir(parents=True, exist_ok=True)
    lines = [
        _line("user" if i % 2 == 0 else "assistant",
              f"{sid[:4]}-{i}", text, sid,
              None if i % 2 == 0 else f"m{i}")
        for i, text in enumerate(texts)
    ]
    (project_dir / f"{sid}.jsonl").write_text("\n".join(lines) + "\n")


def _run(home: Path, *args: str) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["HOME"] = str(home)
    return subprocess.run(
        [str(THREADHOP), "search", *args],
        cwd=ROOT, env=env, capture_output=True, text=True, check=False,
    )


def test_search_indexes_incrementally_and_prints_hits(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A, [
        "let's discuss the flibbertigibbet retry strategy",
        "flibbertigibbet noted, applying backoff",
    ])
    _write_session(home, "-Users-alice-beta", SID_B, [
        "unrelated conversation about gardening",
        "sure, tulips it is",
    ])

    result = _run(home, "flibbertigibbet")

    assert result.returncode == 0, result.stderr
    assert SID_A[:8] in result.stdout
    assert "-Users-alice-alpha" in result.stdout
    assert "flibbertigibbet" in result.stdout
    assert SID_B[:8] not in result.stdout
    assert (
        "Tip: threadhop peek <session> --grep 'flibbertigibbet' "
        "shows full exchanges."
    ) in result.stdout


def test_search_project_filter(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A, ["shared keyword here"])
    _write_session(home, "-Users-alice-beta", SID_B, ["shared keyword there"])

    result = _run(home, "keyword", "--project", "beta")

    assert result.returncode == 0, result.stderr
    assert SID_B[:8] in result.stdout
    assert SID_A[:8] not in result.stdout


def test_search_json_output(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A, [
        "the flibbertigibbet strategy again",
    ])

    result = _run(home, "flibbertigibbet", "--json")

    assert result.returncode == 0, result.stderr
    payload = json.loads(result.stdout)
    assert isinstance(payload, list) and payload
    hit = payload[0]
    assert hit["session_id"] == SID_A
    assert hit["session_name"] == SID_A[:8]
    assert hit["project"] == "-Users-alice-alpha"
    assert hit["uuid"] == f"{SID_A[:4]}-0"
    assert "flibbertigibbet" in hit["snippet"]
    # No FTS sentinel bytes leak into JSON.
    assert "\x01" not in hit["snippet"] and "\x02" not in hit["snippet"]


def test_search_no_matches(tmp_path: Path):
    home = tmp_path / "home"
    _write_session(home, "-Users-alice-alpha", SID_A, ["hello world"])

    result = _run(home, "qqqqzzzz")

    assert result.returncode == 0, result.stderr
    assert "No matches" in result.stdout
