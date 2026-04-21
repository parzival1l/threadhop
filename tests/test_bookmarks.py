"""Tests for category-backed bookmarks and the task #58 compatibility seam."""

from __future__ import annotations

import sqlite3

import pytest

import db


def _seed_session(conn: sqlite3.Connection, session_id: str, **over) -> None:
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
        (
            row["session_id"],
            row["session_path"],
            row["project"],
            row["custom_name"],
            row["created_at"],
            row["modified_at"],
        ),
    )


def _seed_message(
    conn: sqlite3.Connection,
    uuid: str,
    session_id: str,
    *,
    role: str = "user",
    text: str = "hello",
    timestamp: str = "2026-04-19T00:00:00Z",
) -> None:
    conn.execute(
        "INSERT INTO messages(uuid, session_id, role, text, timestamp) "
        "VALUES (?,?,?,?,?)",
        (uuid, session_id, role, text, timestamp),
    )


def test_builtin_categories_exist(conn):
    rows = {row["name"]: row for row in db.list_bookmark_categories(conn)}
    assert db.DEFAULT_BOOKMARK_CATEGORY in rows
    assert db.DEFAULT_RESEARCH_CATEGORY in rows
    assert rows[db.DEFAULT_BOOKMARK_CATEGORY]["has_prompt"] is False
    assert rows[db.DEFAULT_RESEARCH_CATEGORY]["has_prompt"] is True


def test_toggle_bookmark_defaults_to_builtin_bookmark_category(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")

    first = db.toggle_bookmark(conn, "u1", created_at=500.0)
    assert first is not None
    assert first["message_uuid"] == "u1"
    assert first["category_name"] == db.DEFAULT_BOOKMARK_CATEGORY
    assert first["kind"] == db.DEFAULT_BOOKMARK_CATEGORY
    assert first["note"] is None

    second = db.toggle_bookmark(conn, "u1")
    assert second is None
    assert db.get_bookmark(conn, "u1") is None


def test_add_bookmark_auto_creates_category_without_prompt(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")

    row = db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name="unknown-term",
        note="look this up",
    )

    assert row["category_name"] == "unknown-term"
    assert row["note"] == "look this up"
    category = db.get_bookmark_category(conn, "unknown-term")
    assert category is not None
    assert category["research_prompt"] is None


def test_upsert_bookmark_preserves_task_58_kind_contract(conn):
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
    assert created["category_name"] == "research"
    assert created["note"] == "later"
    assert created["created_at"] == 1000.0

    updated = db.upsert_bookmark(
        conn,
        "u1",
        kind="research",
        created_at=2000.0,
    )
    assert updated["id"] == created["id"]
    assert updated["kind"] == "research"
    assert updated["note"] == "later"


def test_resolve_bookmark_target_defaults_to_latest_message_in_session(conn):
    _seed_session(conn, "s1")
    _seed_session(conn, "s2")
    _seed_message(conn, "u1", "s1", text="old")
    _seed_message(conn, "u2", "s1", role="assistant", text="newest")
    _seed_message(conn, "u3", "s2", text="other session")

    latest = db.resolve_bookmark_target(conn, session_id="s1")
    explicit = db.resolve_bookmark_target(conn, session_id="s1", message_uuid="u1")

    assert latest is not None
    assert latest["uuid"] == "u2"
    assert explicit is not None
    assert explicit["uuid"] == "u1"
    assert db.resolve_bookmark_target(
        conn,
        session_id="s1",
        message_uuid="u3",
    ) is None


def test_same_message_can_exist_in_multiple_categories(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")

    db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name=db.DEFAULT_BOOKMARK_CATEGORY,
    )
    research = db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name=db.DEFAULT_RESEARCH_CATEGORY,
    )

    assert research["category_name"] == db.DEFAULT_RESEARCH_CATEGORY
    assert db.get_bookmarked_uuids(conn) == {"u1"}
    assert db.get_bookmarked_uuids(conn, category_name="research") == {"u1"}
    rows = db.list_bookmarks(conn)
    assert {row["category_name"] for row in rows} == {
        db.DEFAULT_BOOKMARK_CATEGORY,
        db.DEFAULT_RESEARCH_CATEGORY,
    }


def test_note_round_trip_via_note_and_legacy_label_alias(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    row = db.toggle_bookmark(conn, "u1")

    db.set_bookmark_note(conn, row["id"], "  important  ")
    stored = db.get_bookmark(conn, "u1")
    assert stored["note"] == "important"
    assert stored["label"] == "important"

    db.set_bookmark_label(conn, row["id"], "  still important  ")
    stored = db.get_bookmark(conn, "u1")
    assert stored["note"] == "still important"
    assert stored["label"] == "still important"

    db.set_bookmark_note(conn, row["id"], "   ")
    assert db.get_bookmark(conn, "u1")["note"] is None


def test_list_newest_first_with_joined_category_metadata(conn):
    _seed_session(conn, "s1", custom_name="project-one", project="proj1")
    _seed_session(conn, "s2", custom_name="project-two", project="proj2")
    _seed_message(conn, "u1", "s1", role="user", text="first question")
    _seed_message(conn, "u2", "s2", role="assistant", text="second answer")

    db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name="background-topic",
        note="needs context",
        created_at=1000.0,
    )
    db.upsert_bookmark(conn, "u2", kind="research", created_at=2000.0)

    rows = db.list_bookmarks(conn)
    assert [r["message_uuid"] for r in rows] == ["u2", "u1"]
    assert rows[0]["kind"] == "research"
    assert rows[0]["category_name"] == "research"
    assert rows[0]["role"] == "assistant"
    assert rows[0]["custom_name"] == "project-two"
    assert rows[0]["project"] == "proj2"
    assert rows[0]["text"] == "second answer"
    assert rows[1]["category_name"] == "background-topic"
    assert rows[1]["session_id"] == "s1"


@pytest.mark.parametrize(
    "query,expected",
    [
        ("important", {"u1"}),
        ("research", {"u2"}),
        ("SECOND", {"u2"}),
        ("project-two", {"u2"}),
        ("proj1", {"u1"}),
        ("unknown-term", {"u1"}),
        ("nope", set()),
    ],
)
def test_filter_matches_note_kind_text_session_and_category(conn, query, expected):
    _seed_session(conn, "s1", custom_name="project-one", project="proj1")
    _seed_session(conn, "s2", custom_name="project-two", project="proj2")
    _seed_message(conn, "u1", "s1", text="first question")
    _seed_message(conn, "u2", "s2", text="second answer")

    db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name="unknown-term",
        note="important note",
    )
    db.upsert_bookmark(conn, "u2", kind="research")

    got = {r["message_uuid"] for r in db.list_bookmarks(conn, query=query)}
    assert got == expected


def test_list_bookmark_categories_counts_and_prompt_state(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    _seed_message(conn, "u2", "s1")
    db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name="background-topic",
    )
    db.add_bookmark(
        conn,
        "u2",
        session_id="s1",
        category_name="background-topic",
    )
    db.set_bookmark_category_prompt(conn, "background-topic", "research this")

    rows = {row["name"]: row for row in db.list_bookmark_categories(conn)}
    assert rows["background-topic"]["bookmark_count"] == 2
    assert rows["background-topic"]["has_prompt"] is True


def test_list_bookmarks_for_research_respects_researched_at(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1", text="first")
    _seed_message(conn, "u2", "s1", text="second")
    first = db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name="background-topic",
    )
    second = db.add_bookmark(
        conn,
        "u2",
        session_id="s1",
        category_name="background-topic",
    )

    db.mark_bookmarks_researched(conn, [first["id"]], researched_at=1234.0)

    pending = db.list_bookmarks_for_research(conn, "background-topic")
    assert [row["id"] for row in pending] == [second["id"]]

    forced = db.list_bookmarks_for_research(conn, "background-topic", force=True)
    assert [row["id"] for row in forced] == [first["id"], second["id"]]


def test_cascade_delete_on_message_removal(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name=db.DEFAULT_BOOKMARK_CATEGORY,
    )
    db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name=db.DEFAULT_RESEARCH_CATEGORY,
    )

    conn.execute("DELETE FROM messages WHERE uuid = ?", ("u1",))
    assert db.get_bookmarked_uuids(conn) == set()


def test_unique_constraint_rejects_duplicate_message_category_pairs(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    row = db.add_bookmark(
        conn,
        "u1",
        session_id="s1",
        category_name="background-topic",
    )

    with pytest.raises(sqlite3.IntegrityError):
        conn.execute(
            "INSERT INTO bookmarks "
            "(session_id, message_uuid, category_id, note, created_at) "
            "VALUES (?, ?, ?, NULL, ?)",
            ("s1", "u1", row["category_id"], 1.0),
        )


def test_migration_upgrades_precursor_bookmark_kinds_to_categories(tmp_path):
    db_path = tmp_path / "precursor.db"
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    try:
        conn.execute("PRAGMA foreign_keys = ON")
        conn.execute("PRAGMA user_version = 9")
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
                note         TEXT,
                kind         TEXT NOT NULL DEFAULT 'bookmark'
                    CHECK (kind IN ('bookmark', 'research')),
                tags         TEXT NOT NULL DEFAULT '[]',
                created_at   REAL NOT NULL,
                FOREIGN KEY (message_uuid) REFERENCES messages(uuid)
                    ON DELETE CASCADE
            )
            """
        )
        conn.execute(
            "CREATE INDEX idx_bookmarks_created_at ON bookmarks(created_at)"
        )
        conn.execute(
            "CREATE INDEX idx_bookmarks_kind_created_at "
            "ON bookmarks(kind, created_at)"
        )
        conn.execute(
            "INSERT INTO sessions(session_id, session_path) VALUES (?, ?)",
            ("s1", "/tmp/s1.jsonl"),
        )
        conn.execute(
            "INSERT INTO messages(uuid, session_id, role, text) VALUES (?, ?, ?, ?)",
            ("u1", "s1", "assistant", "legacy bookmark"),
        )
        conn.execute(
            "INSERT INTO messages(uuid, session_id, role, text) VALUES (?, ?, ?, ?)",
            ("u2", "s1", "assistant", "legacy research"),
        )
        conn.execute(
            "INSERT INTO bookmarks(message_uuid, note, kind, tags, created_at) "
            "VALUES (?, ?, 'bookmark', '[]', ?)",
            ("u1", "old note", 1.0),
        )
        conn.execute(
            "INSERT INTO bookmarks(message_uuid, note, kind, tags, created_at) "
            "VALUES (?, ?, 'research', '[]', ?)",
            ("u2", "look deeper", 2.0),
        )
        conn.commit()
        conn.close()

        upgraded = db.init_db(db_path)
        try:
            bookmark = db.get_bookmark(upgraded, "u1", session_id="s1")
            research = db.get_bookmark(
                upgraded,
                "u2",
                category_name="research",
                session_id="s1",
            )
            bookmark_cols = {
                r[1] for r in upgraded.execute("PRAGMA table_info(bookmarks)").fetchall()
            }
            categories = {r["name"] for r in db.list_bookmark_categories(upgraded)}

            assert bookmark is not None
            assert bookmark["note"] == "old note"
            assert bookmark["kind"] == "bookmark"
            assert research is not None
            assert research["note"] == "look deeper"
            assert research["kind"] == "research"
            assert "kind" not in bookmark_cols
            assert {"session_id", "category_id", "researched_at"}.issubset(bookmark_cols)
            assert {"bookmark", "research"}.issubset(categories)
        finally:
            upgraded.close()
    finally:
        if conn:
            try:
                conn.close()
            except Exception:
                pass
