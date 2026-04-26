"""Tests for incremental JSONL indexing (task #9).

Exercises ``indexer.index_session_incremental`` end-to-end: basic indexing,
incremental appends, chunk merging within and across batches, file
truncation re-index, partial line at EOF, and FTS search.

Run from the repo root::

    python -m pytest tests/test_incremental_index.py -v
"""

from __future__ import annotations

import json
import time

import db
import indexer


# --- JSONL line builders ---------------------------------------------------

def _user_line(uuid: str, text: str, session_id: str = "test-session") -> str:
    """Build a minimal user JSONL line."""
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": session_id,
        "timestamp": "2026-04-16T10:00:00Z",
        "message": {"id": f"umsg_{uuid}", "content": [{"type": "text", "text": text}]},
    })


def _assistant_line(
    uuid: str, message_id: str, text: str, session_id: str = "test-session",
) -> str:
    """Build a minimal assistant JSONL line."""
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": session_id,
        "timestamp": "2026-04-16T10:00:01Z",
        "message": {
            "id": message_id,
            "content": [{"type": "text", "text": text}],
        },
    })


def _tool_use_line(
    uuid: str, message_id: str, tool_name: str, session_id: str = "test-session",
) -> str:
    """Build an assistant line containing only a tool_use block (no text)."""
    return json.dumps({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": session_id,
        "timestamp": "2026-04-16T10:00:02Z",
        "message": {
            "id": message_id,
            "content": [{"type": "tool_use", "name": tool_name, "input": {}}],
        },
    })


def _tool_result_line(uuid: str, session_id: str = "test-session") -> str:
    """Build a user line that is a tool result (should be skipped)."""
    return json.dumps({
        "type": "user",
        "uuid": uuid,
        "sessionId": session_id,
        "timestamp": "2026-04-16T10:00:03Z",
        "toolUseResult": {"type": "tool_result", "content": "ok"},
    })


def _title_line(title: str) -> str:
    return json.dumps({"type": "ai-title", "aiTitle": title})


# --- Fixtures --------------------------------------------------------------

SID = "test-session"


class _SessionHelper:
    """Wraps a temp session file + conn for incremental index tests."""

    def __init__(self, conn, tmp_path):
        self.conn = conn
        self.jsonl = tmp_path / f"{SID}.jsonl"
        # Ensure a sessions row exists so the indexer can INSERT messages.
        db.upsert_session(
            conn, SID, str(self.jsonl),
            project="test", created_at=time.time(), modified_at=time.time(),
        )

    def write(self, *lines: str) -> None:
        self.jsonl.write_text("\n".join(lines) + "\n")

    def append(self, *lines: str) -> None:
        with open(self.jsonl, "a") as f:
            for line in lines:
                f.write(line + "\n")

    def index(self) -> None:
        indexer.index_session_incremental(self.conn, SID, str(self.jsonl))

    def messages(self) -> list[dict]:
        return db.query_all(
            self.conn,
            "SELECT * FROM messages WHERE session_id = ? ORDER BY rowid",
            (SID,),
        )

    def fts_search(self, query: str) -> list[dict]:
        return db.query_all(
            self.conn,
            """
            SELECT m.* FROM messages m
            JOIN messages_fts ON messages_fts.rowid = m.rowid
            WHERE messages_fts MATCH ?
            ORDER BY m.rowid
            """,
            (query,),
        )


# --- Basic indexing --------------------------------------------------------

def test_indexes_user_and_assistant_messages(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(
        _user_line("u1", "Hello world"),
        _assistant_line("a1", "msg_1", "Hi there"),
    )
    h.index()

    msgs = h.messages()
    assert len(msgs) == 2
    assert msgs[0]["role"] == "user"
    assert msgs[0]["text"] == "Hello world"
    assert msgs[1]["role"] == "assistant"
    assert msgs[1]["text"] == "Hi there"


def test_skips_non_message_types(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(
        _title_line("My Session"),
        _user_line("u1", "Question"),
    )
    h.index()

    msgs = h.messages()
    assert len(msgs) == 1
    assert msgs[0]["text"] == "Question"


def test_skips_tool_result_lines(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(
        _user_line("u1", "Do something"),
        _tool_result_line("tr1"),
        _assistant_line("a1", "msg_1", "Done"),
    )
    h.index()

    msgs = h.messages()
    assert len(msgs) == 2
    assert msgs[0]["role"] == "user"
    assert msgs[1]["role"] == "assistant"


def test_strips_system_reminders(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    text_with_reminder = "Hello <system-reminder>secret</system-reminder> world"
    h.write(_user_line("u1", text_with_reminder))
    h.index()

    msgs = h.messages()
    assert len(msgs) == 1
    assert msgs[0]["text"] == "Hello  world"


def test_skips_command_only_user_lines(conn, tmp_path):
    """Slash-command markup is harness plumbing — must not pollute FTS.

    Pre-fix this would index the whole ``<command-name>/foo</command-name>``
    blob, so search for "command-name" returned every command invocation.
    The fix routes user lines through ``clean_user_text`` which drops the
    block entirely; a command-only line collapses to empty and is skipped.
    """
    h = _SessionHelper(conn, tmp_path)
    h.write(_user_line(
        "u1",
        "<command-message>foo</command-message>"
        "<command-name>/foo</command-name>",
    ))
    h.index()

    assert len(h.messages()) == 0


def test_skips_skill_load_banner_user_lines(conn, tmp_path):
    """Skill-load banners inject the entire skill body as a user line.

    Indexing them buries real prose under boilerplate and feeds the
    Haiku observer pages of skill markdown to extract trivia from. The
    SKILL_LOAD_BANNER_RE detection in ``clean_user_text`` collapses the
    whole line to empty.
    """
    h = _SessionHelper(conn, tmp_path)
    skill_body = (
        "Base directory for this skill: "
        "/Users/x/.claude/skills/improve-codebase-architecture\n\n"
        "# Improve Codebase Architecture\n\n"
        "Lorem ipsum body of the skill markdown.\n"
    )
    h.write(_user_line("u1", skill_body))
    h.index()

    assert len(h.messages()) == 0


def test_classify_user_text_round_trip():
    """Pin the kinds returned by classify_user_text — TUI render branches
    in tui.py:load_transcript depend on these exact strings."""
    import indexer

    assert indexer.classify_user_text(
        "<command-name>/foo</command-name>"
    ) == ("command", "/foo")

    kind, name = indexer.classify_user_text(
        "Base directory for this skill: /Users/x/.claude/skills/abc\n\n# Body\n"
    )
    assert kind == "skill_load"
    assert name == "abc"

    assert indexer.classify_user_text("hello") == ("user", "hello")
    assert indexer.classify_user_text("") == ("empty", "")


# --- Chunk merging (ADR-003) -----------------------------------------------

def test_merges_consecutive_assistant_chunks(conn, tmp_path):
    """Multiple assistant lines with the same message.id → one row."""
    h = _SessionHelper(conn, tmp_path)
    h.write(
        _user_line("u1", "Question"),
        _assistant_line("a1", "msg_1", "Part one"),
        _assistant_line("a2", "msg_1", "part two"),
        _assistant_line("a3", "msg_1", "part three"),
    )
    h.index()

    msgs = h.messages()
    assert len(msgs) == 2  # 1 user + 1 merged assistant
    assistant = [m for m in msgs if m["role"] == "assistant"]
    assert len(assistant) == 1
    assert "Part one" in assistant[0]["text"]
    assert "part two" in assistant[0]["text"]
    assert "part three" in assistant[0]["text"]
    assert assistant[0]["uuid"] == "a1"  # first uuid is PK


def test_does_not_merge_different_message_ids(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(
        _assistant_line("a1", "msg_1", "First response"),
        _user_line("u1", "Follow-up"),
        _assistant_line("a2", "msg_2", "Second response"),
    )
    h.index()

    msgs = h.messages()
    assert len(msgs) == 3


def test_tool_use_abbreviation_in_chunk(conn, tmp_path):
    """Assistant chunk with tool_use gets abbreviated inline."""
    h = _SessionHelper(conn, tmp_path)
    h.write(
        _user_line("u1", "Read a file"),
        _tool_use_line("a1", "msg_1", "Read"),
    )
    h.index()

    msgs = h.messages()
    # tool_use alone gets abbreviated by _extract_assistant_blocks
    assistant = [m for m in msgs if m["role"] == "assistant"]
    assert len(assistant) == 1
    assert "Reading" in assistant[0]["text"]


# --- Incremental indexing --------------------------------------------------

def test_incremental_append_indexes_only_new_lines(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(_user_line("u1", "First"))
    h.index()
    assert len(h.messages()) == 1

    h.append(
        _user_line("u2", "Second"),
        _assistant_line("a1", "msg_1", "Reply"),
    )
    h.index()

    msgs = h.messages()
    assert len(msgs) == 3
    assert msgs[0]["text"] == "First"
    assert msgs[1]["text"] == "Second"
    assert msgs[2]["text"] == "Reply"


def test_incremental_updates_byte_offset(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(_user_line("u1", "Hello"))
    h.index()

    state = db.get_index_state(conn, SID)
    assert state is not None
    assert state["last_byte_offset"] > 0
    first_offset = state["last_byte_offset"]

    h.append(_user_line("u2", "World"))
    h.index()

    state = db.get_index_state(conn, SID)
    assert state["last_byte_offset"] > first_offset


def test_no_new_bytes_is_noop(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(_user_line("u1", "Only message"))
    h.index()
    state1 = db.get_index_state(conn, SID)

    h.index()
    state2 = db.get_index_state(conn, SID)

    assert state1["last_byte_offset"] == state2["last_byte_offset"]
    assert len(h.messages()) == 1


# --- Cross-batch chunk merging ---------------------------------------------

def test_chunk_merge_across_batches(conn, tmp_path):
    """Assistant chunk split across two index runs → merged into one row."""
    h = _SessionHelper(conn, tmp_path)
    h.write(
        _user_line("u1", "Question"),
        _assistant_line("a1", "msg_1", "Part one"),
    )
    h.index()
    assert len(h.messages()) == 2

    # Append continuation of the same assistant message
    h.append(_assistant_line("a2", "msg_1", "part two"))
    h.index()

    msgs = h.messages()
    assert len(msgs) == 2  # still 2, not 3
    assistant = [m for m in msgs if m["role"] == "assistant"][0]
    assert "Part one" in assistant["text"]
    assert "part two" in assistant["text"]


# --- Edge cases ------------------------------------------------------------

def test_file_truncation_triggers_reindex(conn, tmp_path):
    """If file shrinks, all messages are deleted and file re-indexed."""
    h = _SessionHelper(conn, tmp_path)
    h.write(
        _user_line("u1", "Original first"),
        _user_line("u2", "Original second"),
    )
    h.index()
    assert len(h.messages()) == 2
    old_offset = db.get_index_state(conn, SID)["last_byte_offset"]

    # Truncate: write a shorter file
    h.write(_user_line("u3", "After truncation"))
    new_size = h.jsonl.stat().st_size
    assert new_size < old_offset

    h.index()
    msgs = h.messages()
    assert len(msgs) == 1
    assert msgs[0]["text"] == "After truncation"


def test_partial_line_at_eof_is_not_indexed(conn, tmp_path):
    """An incomplete trailing line is ignored until the next refresh."""
    h = _SessionHelper(conn, tmp_path)
    h.write(_user_line("u1", "Complete"))
    # Append partial line (no trailing newline)
    with open(h.jsonl, "a") as f:
        f.write('{"type":"user","uuid":"u2","sessionId":"test-session","message":')
    h.index()

    msgs = h.messages()
    assert len(msgs) == 1
    assert msgs[0]["text"] == "Complete"

    # Now complete the line
    with open(h.jsonl, "a") as f:
        f.write('{"id":"m2","content":[{"type":"text","text":"Was partial"}]}}\n')
    h.index()

    msgs = h.messages()
    assert len(msgs) == 2
    assert msgs[1]["text"] == "Was partial"


def test_nonexistent_file_is_noop(conn, tmp_path):
    """Indexing a missing file does nothing and doesn't crash."""
    indexer.index_session_incremental(
        conn, SID, str(tmp_path / "nonexistent.jsonl"),
    )
    assert len(db.query_all(conn, "SELECT * FROM messages WHERE session_id = ?", (SID,))) == 0


def test_empty_file_is_noop(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.jsonl.write_text("")
    h.index()
    assert len(h.messages()) == 0


def test_malformed_json_lines_are_skipped(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(
        "not valid json",
        _user_line("u1", "Valid message"),
        "{malformed}",
    )
    h.index()

    msgs = h.messages()
    assert len(msgs) == 1
    assert msgs[0]["text"] == "Valid message"


# --- FTS search ------------------------------------------------------------

def test_fts_search_finds_indexed_messages(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(
        _user_line("u1", "How do I configure rate limiting?"),
        _assistant_line("a1", "msg_1", "Use a token bucket algorithm"),
    )
    h.index()

    results = h.fts_search("rate limiting")
    assert len(results) == 1
    assert results[0]["uuid"] == "u1"

    results = h.fts_search("token bucket")
    assert len(results) == 1
    assert results[0]["uuid"] == "a1"


def test_fts_prefix_search(conn, tmp_path):
    h = _SessionHelper(conn, tmp_path)
    h.write(_user_line("u1", "authentication middleware refactor"))
    h.index()

    results = h.fts_search("auth*")
    assert len(results) == 1


def test_fts_updates_on_chunk_merge(conn, tmp_path):
    """FTS stays correct when a message is updated via cross-batch merge."""
    h = _SessionHelper(conn, tmp_path)
    h.write(_assistant_line("a1", "msg_1", "Initial content about databases"))
    h.index()
    assert len(h.fts_search("databases")) == 1

    h.append(_assistant_line("a2", "msg_1", "and caching strategies"))
    h.index()

    assert len(h.fts_search("databases")) == 1
    assert len(h.fts_search("caching")) == 1


def test_fts_cleaned_on_truncation_reindex(conn, tmp_path):
    """After truncation, old FTS entries are gone."""
    h = _SessionHelper(conn, tmp_path)
    h.write(_user_line("u1", "zebra unicorn"))
    h.index()
    assert len(h.fts_search("zebra")) == 1

    h.write(_user_line("u2", "apple banana"))
    h.index()

    assert len(h.fts_search("zebra")) == 0
    assert len(h.fts_search("apple")) == 1


# --- Migration test --------------------------------------------------------

def test_migration_003_from_v2(tmp_path):
    """Verify migration 003 correctly adds index_state and message_id column."""
    db_path = tmp_path / "test.db"

    # Start with a v2 database (migrations 001+002 only)
    conn = db.connect(db_path)
    conn.execute("BEGIN")
    db._migration_001_initial(conn)
    db._migration_002_messages(conn)
    db._set_schema_version(conn, 2)
    conn.execute("COMMIT")
    assert db.get_schema_version(conn) == 2

    # Verify message_id column does NOT exist yet
    cols = {r[1] for r in conn.execute("PRAGMA table_info(messages)").fetchall()}
    assert "message_id" not in cols

    # Apply pending migrations (003 + any later ones)
    db.apply_migrations(conn)
    assert db.get_schema_version(conn) == db.SCHEMA_VERSION

    # Verify index_state table exists
    tables = {
        r[0] for r in conn.execute(
            "SELECT name FROM sqlite_master WHERE type='table'"
        ).fetchall()
    }
    assert "index_state" in tables

    # Verify message_id column was added
    cols = {r[1] for r in conn.execute("PRAGMA table_info(messages)").fetchall()}
    assert "message_id" in cols

    conn.close()
