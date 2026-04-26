"""Tests for the reflector core function."""

from __future__ import annotations

import json
import stat
from pathlib import Path

import pytest

from threadhop_core.storage import db
from threadhop_core.observation import reflector


@pytest.fixture
def obs_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    d = tmp_path / "observations"
    d.mkdir()
    monkeypatch.setattr(db, "OBS_DIR", d)
    return d


@pytest.fixture
def fake_claude(tmp_path: Path):
    def _make(entries: list[dict]) -> Path:
        path = tmp_path / "fake_claude_reflector"
        payload = tmp_path / "reflector_payload.jsonl"
        payload.write_text("\n".join(json.dumps(e) for e in entries) + "\n")
        path.write_text(
            f"""#!/usr/bin/env bash
set -euo pipefail
prompt="$2"
obs_path=$(printf '%s\\n' "$prompt" | sed -n 's/^Append conflicts to: //p')
cat "{payload}" >> "$obs_path"
"""
        )
        path.chmod(path.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
        return path
    return _make


def _seed_session(conn, tmp_path: Path, session_id: str, project: str) -> Path:
    session_path = tmp_path / f"{session_id}.jsonl"
    session_path.write_text("")
    db.upsert_session(
        conn,
        session_id,
        str(session_path),
        project=project,
        created_at=1.0,
        modified_at=1.0,
    )
    return session_path


def test_no_new_decisions_advances_offset(conn, tmp_path: Path, obs_dir: Path):
    source = _seed_session(conn, tmp_path, "sess-a", "atlas")
    obs_path = obs_dir / "sess-a.jsonl"
    obs_path.write_text(
        json.dumps({"type": "decision", "text": "REST", "ts": "2026-04-17T10:00:00Z"}) + "\n" +
        json.dumps({"type": "conflict", "text": "already known", "refs": ["sess-a", "sess-b"], "topic": "api", "ts": "2026-04-17T11:00:00Z"}) + "\n"
    )
    db.upsert_observation_state(
        conn,
        "sess-a",
        str(source),
        str(obs_path),
        entry_count=2,
        reflector_entry_offset=1,
    )

    result = reflector.reflect_session(conn, "sess-a")

    assert result["status"] == "no_decisions"
    state = db.get_observation_state(conn, "sess-a")
    assert state["reflector_entry_offset"] == 2
    assert state["entry_count"] == 2


def test_extraction_appends_conflict_and_updates_state(
    conn,
    tmp_path: Path,
    obs_dir: Path,
    fake_claude,
):
    source_a = _seed_session(conn, tmp_path, "sess-a", "atlas")
    _seed_session(conn, tmp_path, "sess-b", "atlas")

    obs_a = obs_dir / "sess-a.jsonl"
    obs_a.write_text(
        json.dumps(
            {
                "type": "decision",
                "text": "Use REST for the public API",
                "ts": "2026-04-17T10:00:00Z",
            }
        ) + "\n"
    )
    obs_b = obs_dir / "sess-b.jsonl"
    obs_b.write_text(
        json.dumps(
            {
                "type": "decision",
                "text": "Use gRPC for all service APIs",
                "ts": "2026-04-17T09:00:00Z",
            }
        ) + "\n"
    )
    db.upsert_observation_state(
        conn,
        "sess-a",
        str(source_a),
        str(obs_a),
        entry_count=1,
        reflector_entry_offset=0,
    )
    db.upsert_observation_state(
        conn,
        "sess-b",
        str(tmp_path / "sess-b.jsonl"),
        str(obs_b),
        entry_count=1,
        reflector_entry_offset=1,
    )

    claude = fake_claude(
        [
            {
                "type": "conflict",
                "text": "REST conflicts with gRPC.",
                "refs": ["sess-a", "sess-b"],
                "topic": "api-protocol",
                "ts": "2026-04-17T12:00:00Z",
            }
        ]
    )
    result = reflector.reflect_session(conn, "sess-a", claude_bin=str(claude))

    assert result["status"] == "extracted"
    assert result["new_entries"] == 1
    state = db.get_observation_state(conn, "sess-a")
    assert state["entry_count"] == 2
    assert state["reflector_entry_offset"] == 2
    lines = obs_a.read_text().strip().splitlines()
    assert len(lines) == 2
    conflict = json.loads(lines[-1])
    assert conflict["type"] == "conflict"
    assert conflict["topic"] == "api-protocol"
