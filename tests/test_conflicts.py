from __future__ import annotations

import json
from pathlib import Path

import pytest

import cli_queries
import db


@pytest.fixture
def obs_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    d = tmp_path / "observations"
    d.mkdir()
    monkeypatch.setattr(db, "OBS_DIR", d)
    return d


def _write_obs(obs_dir: Path, session_id: str, entries: list[dict]) -> Path:
    path = obs_dir / f"{session_id}.jsonl"
    path.write_text("".join(json.dumps(entry) + "\n" for entry in entries))
    return path


def _seed_session(
    conn,
    tmp_path: Path,
    session_id: str,
    project: str,
) -> Path:
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


def test_migration_005_from_v4(tmp_path: Path):
    db_path = tmp_path / "test.db"
    conn = db.connect(db_path)
    try:
        conn.execute("BEGIN")
        db._migration_001_initial(conn)
        db._migration_002_messages(conn)
        db._migration_003_index_state(conn)
        db._migration_004_observation_state(conn)
        db._set_schema_version(conn, 4)
        conn.execute("COMMIT")

        db.apply_migrations(conn)
        tables = {
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table'"
            ).fetchall()
        }
        assert "conflict_reviews" in tables
        assert db.get_schema_version(conn) == db.SCHEMA_VERSION
    finally:
        conn.close()


def test_mark_conflict_reviewed_round_trips(conn, tmp_path: Path):
    _seed_session(conn, tmp_path, "sess-a", "atlas")

    assert not db.is_conflict_reviewed(conn, "sess-a", ["sess-a", "sess-b"], "api")
    db.mark_conflict_reviewed(conn, "sess-a", ["sess-b", "sess-a"], "api")
    assert db.is_conflict_reviewed(conn, "sess-a", ["sess-a", "sess-b"], "api")


def test_query_conflicts_filters_project_and_enriches_decisions(
    conn,
    tmp_path: Path,
    obs_dir: Path,
):
    _seed_session(conn, tmp_path, "sess-a", "atlas")
    _seed_session(conn, tmp_path, "sess-b", "atlas")
    _seed_session(conn, tmp_path, "sess-c", "other")

    db.upsert_observation_state(
        conn,
        "sess-a",
        str(tmp_path / "sess-a.jsonl"),
        str(obs_dir / "sess-a.jsonl"),
        entry_count=2,
        reflector_entry_offset=2,
    )
    db.upsert_observation_state(
        conn,
        "sess-b",
        str(tmp_path / "sess-b.jsonl"),
        str(obs_dir / "sess-b.jsonl"),
        entry_count=1,
        reflector_entry_offset=1,
    )

    _write_obs(
        obs_dir,
        "sess-a",
        [
            {
                "type": "decision",
                "text": "Use REST for the public API",
                "ts": "2026-04-17T10:00:00Z",
            },
            {
                "type": "conflict",
                "text": "Session sess-a chose REST while sess-b chose gRPC.",
                "refs": ["sess-a", "sess-b"],
                "topic": "api-protocol",
                "ts": "2026-04-17T12:00:00Z",
            },
        ],
    )
    _write_obs(
        obs_dir,
        "sess-b",
        [
            {
                "type": "decision",
                "text": "Use gRPC for all service APIs",
                "ts": "2026-04-17T11:00:00Z",
            }
        ],
    )
    _write_obs(
        obs_dir,
        "sess-c",
        [
            {
                "type": "conflict",
                "text": "Should not leak across projects",
                "refs": ["sess-c", "sess-x"],
                "topic": "ignored",
                "ts": "2026-04-17T13:00:00Z",
            }
        ],
    )

    results = cli_queries.query_conflicts(
        conn,
        claude_projects_dir=tmp_path / "projects-does-not-matter",
        project="atlas",
        reflect_fn=lambda *_args, **_kw: {"status": "up_to_date"},
    )

    assert len(results) == 1
    row = results[0]
    assert row["session_id"] == "sess-a"
    assert row["project"] == "atlas"
    assert row["topic"] == "api-protocol"
    assert row["resolved"] is False
    assert [d["session_id"] for d in row["decisions"]] == ["sess-a", "sess-b"]
    assert row["decisions"][0]["text"] == "Use REST for the public API"
    assert row["decisions"][1]["text"] == "Use gRPC for all service APIs"


def test_query_conflicts_resolved_marks_and_hides_on_next_run(
    conn,
    tmp_path: Path,
    obs_dir: Path,
):
    _seed_session(conn, tmp_path, "sess-a", "atlas")
    _seed_session(conn, tmp_path, "sess-b", "atlas")
    db.upsert_observation_state(
        conn,
        "sess-a",
        str(tmp_path / "sess-a.jsonl"),
        str(obs_dir / "sess-a.jsonl"),
        entry_count=2,
        reflector_entry_offset=2,
    )

    _write_obs(
        obs_dir,
        "sess-a",
        [
            {
                "type": "decision",
                "text": "Use REST for the public API",
                "ts": "2026-04-17T10:00:00Z",
            },
            {
                "type": "conflict",
                "text": "Session sess-a chose REST while sess-b chose gRPC.",
                "refs": ["sess-a", "sess-b"],
                "topic": "api-protocol",
                "ts": "2026-04-17T12:00:00Z",
            },
        ],
    )
    _write_obs(
        obs_dir,
        "sess-b",
        [
            {
                "type": "decision",
                "text": "Use gRPC for all service APIs",
                "ts": "2026-04-17T11:00:00Z",
            }
        ],
    )

    resolved = cli_queries.query_conflicts(
        conn,
        claude_projects_dir=tmp_path / "projects-does-not-matter",
        project="atlas",
        mark_resolved=True,
        reflect_fn=lambda *_args, **_kw: {"status": "up_to_date"},
    )
    assert len(resolved) == 1
    assert resolved[0]["resolved"] is True
    assert db.is_conflict_reviewed(conn, "sess-a", ["sess-a", "sess-b"], "api-protocol")

    unresolved = cli_queries.query_conflicts(
        conn,
        claude_projects_dir=tmp_path / "projects-does-not-matter",
        project="atlas",
        reflect_fn=lambda *_args, **_kw: {"status": "up_to_date"},
    )
    assert unresolved == []
