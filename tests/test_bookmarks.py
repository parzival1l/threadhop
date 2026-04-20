"""Tests for the bookmarks helpers in db.py (task #18).

Focuses on the contract the TUI relies on:
  - toggle is idempotent per-uuid (space keybind flips a single state)
  - label round-trips, blanks collapse to NULL
  - list_bookmarks returns newest-first with the joined metadata the
    browser modal renders (session / project / timestamp / role / text)
  - filter matches against label, message text, project, custom_name
  - cascade delete removes orphan bookmarks when messages disappear
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
    assert first["label"] is None
    # Tags column lives in the schema to match models.Bookmark even
    # though the editor UI is deferred — new rows default to [].
    assert first["tags"] == []

    second = db.toggle_bookmark(conn, "u1")
    assert second is None  # removed
    assert db.get_bookmark(conn, "u1") is None


def test_label_round_trip(conn):
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    row = db.toggle_bookmark(conn, "u1")

    db.set_bookmark_label(conn, row["id"], "  important  ")
    assert db.get_bookmark(conn, "u1")["label"] == "important"

    # Empty / whitespace collapses to NULL — the browser treats empty
    # submissions as "clear" without a separate code path.
    db.set_bookmark_label(conn, row["id"], "   ")
    assert db.get_bookmark(conn, "u1")["label"] is None

    db.set_bookmark_label(conn, row["id"], None)
    assert db.get_bookmark(conn, "u1")["label"] is None


def test_list_newest_first_with_joined_metadata(conn):
    _seed_session(conn, "s1", custom_name="project-one", project="proj1")
    _seed_session(conn, "s2", custom_name="project-two", project="proj2")
    _seed_message(conn, "u1", "s1", role="user", text="first question")
    _seed_message(conn, "u2", "s2", role="assistant", text="second answer")

    db.toggle_bookmark(conn, "u1", created_at=1000.0)
    db.toggle_bookmark(conn, "u2", created_at=2000.0)

    rows = db.list_bookmarks(conn)
    assert [r["message_uuid"] for r in rows] == ["u2", "u1"]
    assert rows[0]["role"] == "assistant"
    assert rows[0]["custom_name"] == "project-two"
    assert rows[0]["project"] == "proj2"
    assert rows[0]["text"] == "second answer"
    assert rows[1]["session_id"] == "s1"


@pytest.mark.parametrize(
    "query,expected",
    [
        ("important", {"u1"}),         # matches label
        ("SECOND", {"u2"}),            # case-insensitive, matches text
        ("project-two", {"u2"}),       # matches custom_name
        ("proj1", {"u1"}),             # matches project
        ("nope", set()),
    ],
)
def test_filter_matches_label_text_and_session(conn, query, expected):
    _seed_session(conn, "s1", custom_name="project-one", project="proj1")
    _seed_session(conn, "s2", custom_name="project-two", project="proj2")
    _seed_message(conn, "u1", "s1", text="first question")
    _seed_message(conn, "u2", "s2", text="second answer")

    b1 = db.toggle_bookmark(conn, "u1")
    db.toggle_bookmark(conn, "u2")
    db.set_bookmark_label(conn, b1["id"], "important note")

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
    """toggle_bookmark is the only public insert path and handles the
    toggle semantics. Any attempt to insert a second row for the same
    message_uuid directly should still be rejected at the DB layer."""
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    db.toggle_bookmark(conn, "u1")
    with pytest.raises(sqlite3.IntegrityError):
        conn.execute(
            "INSERT INTO bookmarks (message_uuid, label, tags, created_at) "
            "VALUES (?, NULL, '[]', ?)",
            ("u1", 1.0),
        )


def test_tags_column_round_trips_json(conn):
    """The tags column exists to match models.Bookmark; no public setter
    ships in this task but direct writes + the get/list helpers should
    round-trip the JSON payload correctly for the follow-up UI work."""
    _seed_session(conn, "s1")
    _seed_message(conn, "u1", "s1")
    row = db.toggle_bookmark(conn, "u1")

    conn.execute(
        "UPDATE bookmarks SET tags = ? WHERE id = ?",
        ('["bug", "decision"]', row["id"]),
    )
    assert db.get_bookmark(conn, "u1")["tags"] == ["bug", "decision"]

    # Corrupt JSON must not crash the reader — decode failures fall back
    # to an empty list so the browser keeps rendering the row.
    conn.execute(
        "UPDATE bookmarks SET tags = ? WHERE id = ?",
        ("not json", row["id"]),
    )
    assert db.get_bookmark(conn, "u1")["tags"] == []
