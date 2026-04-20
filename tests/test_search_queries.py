"""Tests for prefix search plus trigram fallback."""

from __future__ import annotations

from pathlib import Path

import db
import search_queries


def _seed_message(
    conn,
    *,
    session_id: str,
    project: str,
    uuid: str,
    role: str,
    text: str,
    timestamp: str = "2026-04-20T10:00:00Z",
) -> None:
    session_path = str(Path("/tmp") / project / f"{session_id}.jsonl")
    db.upsert_session(
        conn,
        session_id,
        session_path,
        project=project,
        created_at=1.0,
        modified_at=1.0,
    )
    conn.execute(
        """
        INSERT INTO messages (uuid, session_id, role, text, timestamp)
        VALUES (?, ?, ?, ?, ?)
        """,
        (uuid, session_id, role, text, timestamp),
    )


def test_prefix_hit_does_not_invoke_fuzzy_fallback(conn, monkeypatch):
    _seed_message(
        conn,
        session_id="s1",
        project="atlas",
        uuid="m1",
        role="assistant",
        text="Retry the request after reconnecting.",
    )

    def _boom(*args, **kwargs):  # pragma: no cover - should never run
        raise AssertionError("fuzzy fallback should not run on exact prefix hits")

    monkeypatch.setattr(search_queries, "_search_trigram_fallback", _boom)

    rows, used_fallback = search_queries.search_messages(conn, "retry", limit=10)

    assert used_fallback is False
    assert [row["uuid"] for row in rows] == ["m1"]


def test_trigram_fallback_matches_inserted_typo(conn):
    _seed_message(
        conn,
        session_id="s1",
        project="atlas",
        uuid="m1",
        role="assistant",
        text="Use connect() first, then retry with backoff.",
    )

    rows, used_fallback = search_queries.search_messages(conn, "connnect", limit=10)

    assert used_fallback is True
    assert [row["uuid"] for row in rows] == ["m1"]
    assert "connect" in rows[0]["snippet"].lower()
    assert search_queries.FTS_MATCH_START in rows[0]["snippet"]
    assert search_queries.FTS_MATCH_END in rows[0]["snippet"]


def test_trigram_fallback_still_respects_role_and_project_filters(conn):
    _seed_message(
        conn,
        session_id="s1",
        project="atlas",
        uuid="m1",
        role="assistant",
        text="Use connect() first, then retry with backoff.",
    )
    _seed_message(
        conn,
        session_id="s2",
        project="apollo",
        uuid="m2",
        role="user",
        text="Maybe connect again once the socket is reset.",
    )

    rows, used_fallback = search_queries.search_messages(
        conn,
        "assistant: project:atlas connnect",
        limit=10,
    )

    assert used_fallback is True
    assert [row["uuid"] for row in rows] == ["m1"]


def test_migration_rebuilds_trigram_index_for_existing_messages(tmp_path):
    db_path = tmp_path / "legacy.db"
    conn = db.connect(db_path)
    try:
        for i, migration in enumerate(db.MIGRATIONS[:7]):
            conn.execute("BEGIN")
            try:
                migration(conn)
                db._set_schema_version(conn, i + 1)
                conn.execute("COMMIT")
            except Exception:
                conn.execute("ROLLBACK")
                raise

        _seed_message(
            conn,
            session_id="s1",
            project="atlas",
            uuid="m1",
            role="assistant",
            text="Use connect() first, then retry with backoff.",
        )

        assert db.get_schema_version(conn) == 7

        db.apply_migrations(conn)

        rows, used_fallback = search_queries.search_messages(
            conn, "connnect", limit=10
        )

        assert db.get_schema_version(conn) == 8
        assert used_fallback is True
        assert [row["uuid"] for row in rows] == ["m1"]
    finally:
        conn.close()
