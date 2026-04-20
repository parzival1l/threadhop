"""Tests for the shared bookmark helpers in ``db.py``.

Focuses on the contracts both the TUI and chat-ingest path rely on:
  - toggle remains idempotent per-uuid for the existing TUI keybind
  - upsert is deterministic and stores the built-in ``kind`` + optional note
  - latest-message target resolution is session-scoped
  - list_bookmarks returns newest-first with the joined metadata the browser
    renders and filters across note/kind/text/session fields
  - migration upgrades legacy ``label`` rows into ``note`` + ``kind``
"""

from __future__ import annotations

import sqlite3

import pytest

import db


def _seed_session(conn: sqlite3.Connection, session_id: str, **over):
    row = {
        "session_id": session_id,
        "session_path": f"/tmp/{session_id}.jsonl",
        "project": "-Users-alice-work",
        "custom_name": None,
        "created_at": 1000.0,
        "modified_at": 1000.0,
    }
    row.update(over)
    conn.execute(
        "INSERT INTO sessions(session_id, session_path, project, "
        "custom_name, created_at, modified_at) VALUES (?,?,?,?,?,?)",
        (row["session_id"], row["session_path"], row["project"],
         row["custom_name"], row["created_at"], row["modified_at"]),
    )


def _seed_message(conn, uuid, session_id, role="user", text="hello",
                  timestamp="2026-04-19T00:00:00Z"):
    conn.execute(
        "INSERT INTO messages(uuid, session_id, role, text, timestamp) "
        "VALUES (?,?,?,?,?)",
        (uuid, session_id, role, text, timestamp),
    )


def test_toggle_creates_then_removes(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")

    first = db.toggle_bookmark(conn, "u1", created_at=500.0)
    assert first is not None
    assert first["message_uuid"] == "u1"
    assert first["note"] is None
    assert first["kind"] == "bookmark"
    assert first["tags"] == []

    second = db.toggle_bookmark(conn, "u1")
    assert second is None
    assert db.get_bookmark(conn, "u1") is None


def test_note_round_trip(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    row = db.toggle_bookmark(conn, "u1")

    db.set_bookmark_note(conn, row["id"], "  important  ")
    assert db.get_bookmark(conn, "u1")["note"] == "important"

    db.set_bookmark_note(conn, row["id"], "   ")
    assert db.get_bookmark(conn, "u1")["note"] is None

    db.set_bookmark_note(conn, row["id"], None)
    assert db.get_bookmark(conn, "u1")["note"] is None


def test_upsert_bookmark_creates_and_updates_kind_and_note(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1", role="assistant", text="compare queue backends")

    created = db.upsert_bookmark(
        conn,
        "u1",
        kind="research",
        note="later",
        created_at=1000.0,
    )
    assert created["kind"] == "research"
    assert created["note"] == "later"
    assert created["created_at"] == 1000.0

    updated = db.upsert_bookmark(
        conn,
        "u1",
        kind="bookmark",
        created_at=2000.0,
    )
    assert updated["id"] == created["id"]
    assert updated["kind"] == "bookmark"
    assert updated["note"] == "later"  # omitted note preserves existing text
    assert updated["created_at"] == 2000.0


def test_resolve_bookmark_target_defaults_to_latest_message_in_session(conn):
    _seed_session(conn, "s1")
    _seed_session(conn, "s2")
    _seed_message(conn, "u1", "s1", text="old")
    _seed_message(conn, "u2", "s1", role="assistant", text="newest")
    _seed_message(conn, "u3", "s2", text="other session")

    latest = db.resolve_bookmark_target(conn, session_id="s1")
    explicit = db.resolve_bookmark_target(
        conn, session_id="s1", message_uuid="u1"
    )

    assert latest is not None
    assert latest["uuid"] == "u2"
    assert explicit is not None
    assert explicit["uuid"] == "u1"
    assert db.resolve_bookmark_target(
        conn, session_id="s1", message_uuid="u3"
    ) is None


def test_list_newest_first_with_joined_metadata(conn):
    _seed_session(conn, "s1", custom_name="project-one", project="proj1")
    _seed_session(conn, "s2", custom_name="project-two", project="proj2")
    _seed_message(conn, "u1", "s1", role="user", text="first question")
    _seed_message(conn, "u2", "s2", role="assistant", text="second answer")

    db.upsert_bookmark(conn, "u1", note="keep", created_at=1000.0)
    db.upsert_bookmark(conn, "u2", kind="research", created_at=2000.0)

    rows = db.list_bookmarks(conn)
    assert [r["message_uuid"] for r in rows] == ["u2", "u1"]
    assert rows[0]["kind"] == "research"
    assert rows[0]["role"] == "assistant"
    assert rows[0]["custom_name"] == "project-two"
    assert rows[0]["project"] == "proj2"
    assert rows[0]["text"] == "second answer"
    assert rows[1]["session_id"] == "s1"


@pytest.mark.parametrize(
    "query,expected",
    [
        ("important", {"u1"}),        # matches note
        ("research", {"u2"}),         # matches kind
        ("SECOND", {"u2"}),           # case-insensitive, matches text
        ("project-two", {"u2"}),      # matches custom_name
        ("proj1", {"u1"}),            # matches project
        ("nope", set()),
    ],
)
def test_filter_matches_note_kind_text_and_session(conn, query, expected):
    _seed_session(conn, "s1", custom_name="project-one", project="proj1")
    _seed_session(conn, "s2", custom_name="project-two", project="proj2")
    _seed_message(conn, "u1", "s1", text="first question")
    _seed_message(conn, "u2", "s2", text="second answer")

    db.upsert_bookmark(conn, "u1", note="important note")
    db.upsert_bookmark(conn, "u2", kind="research")

    got = {r["message_uuid"] for r in db.list_bookmarks(conn, query=query)}
    assert got == expected


def test_cascade_delete_on_message_removal(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    _seed_message(conn, "u2", "s1")
    db.toggle_bookmark(conn, "u1")
    db.toggle_bookmark(conn, "u2")

    assert db.get_bookmarked_uuids(conn) == {"u1", "u2"}

    conn.execute("DELETE FROM messages WHERE uuid = ?", ("u1",))
    assert db.get_bookmarked_uuids(conn) == {"u2"}


def test_get_bookmarked_uuids_scoped(conn):
    _seed_session(conn, "s1")
    _seed_session(conn, "s2")
    _seed_message(conn, "u1", "s1")
    _seed_message(conn, "u2", "s2")
    db.toggle_bookmark(conn, "u1")
    db.toggle_bookmark(conn, "u2")

    assert db.get_bookmarked_uuids(conn, "s1") == {"u1"}
    assert db.get_bookmarked_uuids(conn, "s2") == {"u2"}
    assert db.get_bookmarked_uuids(conn) == {"u1", "u2"}


def test_delete_bookmark_removes_row(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    row = db.toggle_bookmark(conn, "u1")

    db.delete_bookmark(conn, row["id"])
    assert db.get_bookmark(conn, "u1") is None


def test_unique_constraint_rejects_duplicate_inserts(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    db.toggle_bookmark(conn, "u1")
    with pytest.raises(sqlite3.IntegrityError):
        conn.execute(
            "INSERT INTO bookmarks (message_uuid, note, kind, tags, created_at) "
            "VALUES (?, NULL, 'bookmark', '[]', ?)",
            ("u1", 1.0),
        )


def test_tags_column_round_trips_json(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    row = db.toggle_bookmark(conn, "u1")

    conn.execute(
        "UPDATE bookmarks SET tags = ? WHERE id = ?",
        ('["bug", "decision"]', row["id"]),
    )
    assert db.get_bookmark(conn, "u1")["tags"] == ["bug", "decision"]

    conn.execute(
        "UPDATE bookmarks SET tags = ? WHERE id = ?",
        ("not json", row["id"]),
    )
    assert db.get_bookmark(conn, "u1")["tags"] == []


def test_migration_upgrades_legacy_label_rows(tmp_path):
    db_path = tmp_path / "legacy.db"
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    try:
        conn.execute("PRAGMA foreign_keys = ON")
        conn.execute("PRAGMA user_version = 7")
        conn.execute(
            """
            CREATE TABLE sessions (
                session_id   TEXT PRIMARY KEY,
                session_path TEXT NOT NULL,
                project      TEXT,
                cwd          TEXT,
                custom_name  TEXT,
                status       TEXT NOT NULL DEFAULT 'active',
                sort_order   INTEGER,
                last_viewed  REAL,
                created_at   REAL,
                modified_at  REAL
            )
            """
        )
        conn.execute(
            """
            CREATE TABLE messages (
                uuid         TEXT PRIMARY KEY,
                session_id   TEXT NOT NULL,
                role         TEXT NOT NULL,
                text         TEXT NOT NULL,
                timestamp    TEXT,
                cwd          TEXT,
                parent_uuid  TEXT,
                is_sidechain INTEGER NOT NULL DEFAULT 0,
                message_id   TEXT
            )
            """
        )
        conn.execute(
            """
            CREATE TABLE bookmarks (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                message_uuid TEXT NOT NULL UNIQUE,
                label        TEXT,
                tags         TEXT NOT NULL DEFAULT '[]',
                created_at   REAL NOT NULL,
                FOREIGN KEY (message_uuid) REFERENCES messages(uuid)
                    ON DELETE CASCADE
            )
            """
        )
        conn.execute(
            "INSERT INTO sessions(session_id, session_path) VALUES (?, ?)",
            ("s1", "/tmp/s1.jsonl"),
        )
        conn.execute(
            "INSERT INTO messages(uuid, session_id, role, text) VALUES (?, ?, ?, ?)",
            ("u1", "s1", "assistant", "legacy"),
        )
        conn.execute(
            "INSERT INTO bookmarks(message_uuid, label, tags, created_at) "
            "VALUES (?, ?, '[]', ?)",
            ("u1", "old note", 1.0),
        )
        conn.commit()
        conn.close()

        upgraded = db.init_db(db_path)
        try:
            row = db.get_bookmark(upgraded, "u1")
            cols = {
                r[1] for r in upgraded.execute("PRAGMA table_info(bookmarks)").fetchall()
            }
            assert row is not None
            assert row["note"] == "old note"
            assert row["kind"] == "bookmark"
            assert "label" not in cols
            assert {"note", "kind"}.issubset(cols)
        finally:
            upgraded.close()
    finally:
        if conn:
            try:
                conn.close()
            except Exception:
                pass
