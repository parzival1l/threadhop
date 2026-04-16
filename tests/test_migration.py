"""Tests for the config.json -> SQLite migration (task #7 in docs/TASKS.md).

Exercises `db.migrate_config_json_to_sqlite` end-to-end against a
temporary config file, JSONL project tree, and SQLite database.

Covers:
  - session_names / session_order / last_viewed land in the sessions table
  - theme / sidebar_width (and unknown top-level keys) are preserved in
    config.json; legacy session-level keys are stripped
  - idempotency: second call is a no-op, no duplicate rows, no clobbering
    of post-migration user changes
  - rollback: DB transaction failure leaves config.json and sessions table
    in their pre-migration state; retry succeeds after recovery
  - edge cases: missing config, empty config, malformed JSON, sessions
    missing from disk, defensive coercion of bad types
  - temp-dir isolation: nothing touches ~/.config/threadhop

Run from the project root:

    pytest tests/
    pytest tests/test_migration.py -v
    pytest tests/test_migration.py::TestIdempotency -v
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

import db


# =========================================================================
# Helpers
# =========================================================================

# Realistic UUID-style session ids. Named for readability in assertions.
SID_A = "11111111-1111-1111-1111-111111111111"
SID_B = "22222222-2222-2222-2222-222222222222"
SID_C = "33333333-3333-3333-3333-333333333333"

PROJECT = "-Users-alice-work"
PROJECT_B = "-Users-alice-home"


def _migrate(conn, config_path, projects_dir, **kw):
    """Short alias for the migration function under test."""
    return db.migrate_config_json_to_sqlite(conn, config_path, projects_dir, **kw)


def _make_session_file(projects_dir: Path, project: str, session_id: str) -> Path:
    """Create a minimal JSONL file so the migration's path-resolution scan
    can discover `session_id`."""
    proj = projects_dir / project
    proj.mkdir(parents=True, exist_ok=True)
    f = proj / f"{session_id}.jsonl"
    f.write_text('{"sessionId":"' + session_id + '"}\n')
    return f


# =========================================================================
# Basic migration: values land in the right columns
# =========================================================================

class TestBasicMigration:
    def test_session_names_become_custom_name(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        _make_session_file(projects_dir, PROJECT_B, SID_B)

        write_config({
            "theme": "textual-light",
            "session_names": {SID_A: "Pivot talk", SID_B: "Groceries"},
            "session_order": [SID_B, SID_A],
            "last_viewed": {SID_A: 100.0, SID_B: 200.0},
        })

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "migrated"
        assert set(result["migrated"]) == {SID_A, SID_B}
        assert result["skipped"] == []

        names = {
            r["session_id"]: r["custom_name"]
            for r in db.query_all(conn, "SELECT session_id, custom_name FROM sessions")
        }
        assert names[SID_A] == "Pivot talk"
        assert names[SID_B] == "Groceries"

    def test_session_order_becomes_sort_order(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        _make_session_file(projects_dir, PROJECT_B, SID_B)

        write_config({
            "session_names": {SID_A: "A", SID_B: "B"},
            "session_order": [SID_B, SID_A],
        })

        _migrate(conn, config_path, projects_dir)

        rows = db.query_all(
            conn,
            "SELECT session_id, sort_order FROM sessions "
            "WHERE sort_order IS NOT NULL ORDER BY sort_order",
        )
        assert [r["session_id"] for r in rows] == [SID_B, SID_A]

    def test_last_viewed_is_migrated(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        _make_session_file(projects_dir, PROJECT_B, SID_B)

        write_config({
            "session_names": {SID_A: "A", SID_B: "B"},
            "last_viewed": {SID_A: 100.0, SID_B: 200.0},
        })

        _migrate(conn, config_path, projects_dir)

        lv = {
            r["session_id"]: r["last_viewed"]
            for r in db.query_all(conn, "SELECT session_id, last_viewed FROM sessions")
        }
        assert lv[SID_A] == 100.0
        assert lv[SID_B] == 200.0

    def test_session_without_name_or_last_viewed_gets_null(
        self, conn, config_path, projects_dir, write_config
    ):
        """A session that only appears in session_order (no name, no
        last_viewed) is still created, with those fields NULL."""
        _make_session_file(projects_dir, PROJECT, SID_A)
        _make_session_file(projects_dir, PROJECT_B, SID_B)

        write_config({
            "session_names": {SID_A: "Named"},
            "session_order": [SID_A, SID_B],
            "last_viewed": {SID_A: 100.0},
        })

        _migrate(conn, config_path, projects_dir)

        row_b = db.query_one(
            conn, "SELECT custom_name, last_viewed FROM sessions WHERE session_id = ?",
            (SID_B,),
        )
        assert row_b["custom_name"] is None
        assert row_b["last_viewed"] is None

    def test_session_path_and_project_are_set_from_disk(
        self, conn, config_path, projects_dir, write_config
    ):
        """Migration resolves session_path from the projects directory scan."""
        f = _make_session_file(projects_dir, PROJECT, SID_A)

        write_config({"session_names": {SID_A: "Foo"}})
        _migrate(conn, config_path, projects_dir)

        row = db.query_one(
            conn, "SELECT session_path, project FROM sessions WHERE session_id = ?",
            (SID_A,),
        )
        assert row["session_path"] == str(f)
        assert row["project"] == PROJECT


# =========================================================================
# config.json preservation & slimming
# =========================================================================

class TestConfigPreservation:
    def test_theme_and_sidebar_width_preserved(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        write_config({
            "theme": "textual-light",
            "sidebar_width": 42,
            "session_names": {SID_A: "foo"},
            "session_order": [SID_A],
            "last_viewed": {SID_A: 1.0},
        })

        _migrate(conn, config_path, projects_dir)

        remaining = json.loads(config_path.read_text())
        assert remaining == {"theme": "textual-light", "sidebar_width": 42}

    def test_session_level_keys_removed_from_config(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        write_config({
            "theme": "textual-dark",
            "session_names": {SID_A: "x"},
            "session_order": [SID_A],
            "last_viewed": {SID_A: 1.0},
        })

        _migrate(conn, config_path, projects_dir)

        remaining = json.loads(config_path.read_text())
        for key in db._MIGRATED_CONFIG_KEYS:
            assert key not in remaining, f"{key} should have been stripped"

    def test_unknown_app_level_keys_are_preserved(
        self, conn, config_path, projects_dir, write_config
    ):
        """Forward-compat: unknown top-level keys must survive migration so
        newer config files aren't silently clobbered by an older migrator."""
        _make_session_file(projects_dir, PROJECT, SID_A)
        write_config({
            "theme": "textual-light",
            "future_setting": {"nested": "value"},
            "experimental_flag": True,
            "session_names": {SID_A: "foo"},
        })

        _migrate(conn, config_path, projects_dir)

        remaining = json.loads(config_path.read_text())
        assert remaining["theme"] == "textual-light"
        assert remaining["future_setting"] == {"nested": "value"}
        assert remaining["experimental_flag"] is True
        assert "session_names" not in remaining

    def test_missing_config_file_sets_flag(self, conn, config_path, projects_dir):
        assert not config_path.exists()

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "skipped"
        assert result["reason"] == "no config.json"
        assert db.get_setting(conn, db.MIGRATION_FLAG) is True
        assert db.query_all(conn, "SELECT * FROM sessions") == []

    def test_empty_session_keys_clean_up_config(
        self, conn, config_path, projects_dir, write_config
    ):
        write_config({
            "theme": "textual-dark",
            "session_names": {},
            "session_order": [],
            "last_viewed": {},
        })

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "migrated"
        assert result["migrated"] == []
        remaining = json.loads(config_path.read_text())
        assert remaining == {"theme": "textual-dark"}

    def test_config_with_only_app_settings(
        self, conn, config_path, projects_dir, write_config
    ):
        """Config that has no session-level keys at all is cleanly handled."""
        write_config({"theme": "textual-dark", "sidebar_width": 36})

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "migrated"
        assert result["migrated"] == []
        remaining = json.loads(config_path.read_text())
        assert remaining == {"theme": "textual-dark", "sidebar_width": 36}
        assert db.query_all(conn, "SELECT * FROM sessions") == []

    def test_rewrite_config_false_leaves_file_untouched(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        original = {"theme": "textual-dark", "session_names": {SID_A: "kept"}}
        write_config(original)

        result = _migrate(conn, config_path, projects_dir, rewrite_config=False)

        assert result["action"] == "migrated"
        row = db.query_one(
            conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (SID_A,),
        )
        assert row["custom_name"] == "kept"
        # config.json unchanged because rewrite_config=False.
        assert json.loads(config_path.read_text()) == original


# =========================================================================
# Idempotency
# =========================================================================

class TestIdempotency:
    def test_second_call_is_skipped(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        write_config({"theme": "textual-dark", "session_names": {SID_A: "first"}})

        r1 = _migrate(conn, config_path, projects_dir)
        assert r1["action"] == "migrated"

        r2 = _migrate(conn, config_path, projects_dir)
        assert r2["action"] == "skipped"
        assert r2["reason"] == "already migrated"

        rows = db.query_all(conn, "SELECT * FROM sessions")
        assert len(rows) == 1
        assert rows[0]["custom_name"] == "first"

    def test_no_duplicate_rows_on_second_call(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        _make_session_file(projects_dir, PROJECT_B, SID_B)
        payload = {
            "session_names": {SID_A: "A", SID_B: "B"},
            "session_order": [SID_A, SID_B],
        }

        write_config(payload)
        _migrate(conn, config_path, projects_dir)

        # Even if config.json is somehow restored, the flag prevents re-insert.
        write_config(payload)
        _migrate(conn, config_path, projects_dir)

        count = db.query_one(conn, "SELECT COUNT(*) AS n FROM sessions")["n"]
        assert count == 2

    def test_second_call_does_not_clobber_user_changes(
        self, conn, config_path, projects_dir, write_config
    ):
        """After migration, the TUI may rename a session. A second migration
        call must NOT overwrite that rename."""
        _make_session_file(projects_dir, PROJECT, SID_A)
        write_config({"session_names": {SID_A: "original"}})

        _migrate(conn, config_path, projects_dir)
        db.set_custom_name(conn, SID_A, "renamed by user")

        # Second call — must be a no-op.
        _migrate(conn, config_path, projects_dir)

        row = db.query_one(
            conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (SID_A,),
        )
        assert row["custom_name"] == "renamed by user"

    def test_migration_flag_is_set(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        write_config({"session_names": {SID_A: "x"}})

        _migrate(conn, config_path, projects_dir)

        assert db.get_setting(conn, db.MIGRATION_FLAG) is True

    def test_pre_existing_row_is_updated_not_duplicated(
        self, conn, config_path, projects_dir, write_config
    ):
        """If a session row already exists (e.g. from a prior scan),
        migration updates it via ON CONFLICT rather than creating a dupe."""
        f = _make_session_file(projects_dir, PROJECT, SID_A)
        db.upsert_session(conn, SID_A, str(f), project=PROJECT, modified_at=999.0)

        write_config({
            "session_names": {SID_A: "Named"},
            "last_viewed": {SID_A: 100.0},
            "session_order": [SID_A],
        })

        _migrate(conn, config_path, projects_dir)

        rows = db.query_all(conn, "SELECT * FROM sessions")
        assert len(rows) == 1
        row = rows[0]
        assert row["custom_name"] == "Named"
        assert row["last_viewed"] == 100.0
        assert row["sort_order"] == 0

    def test_partial_previous_run_cleanup(
        self, conn, config_path, projects_dir, write_config
    ):
        """Simulate a prior partial-success state (DB migrated + flag set,
        but config.json still has legacy keys). Second call cleans up the
        lingering keys without touching DB rows."""
        _make_session_file(projects_dir, PROJECT, SID_A)
        db.set_setting(conn, db.MIGRATION_FLAG, True)
        db.upsert_session(conn, SID_A, str(projects_dir / PROJECT / f"{SID_A}.jsonl"))
        db.set_custom_name(conn, SID_A, "established name")

        # Legacy keys still in config.json.
        write_config({
            "theme": "textual-dark",
            "session_names": {SID_A: "would clobber"},
            "session_order": [SID_A],
        })

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "skipped"
        # Legacy keys cleaned up.
        assert json.loads(config_path.read_text()) == {"theme": "textual-dark"}
        # DB row not clobbered.
        row = db.query_one(
            conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (SID_A,),
        )
        assert row["custom_name"] == "established name"


# =========================================================================
# Rollback on failure
# =========================================================================

class TestRollbackOnFailure:
    def test_db_failure_preserves_config_and_unsets_flag(
        self, conn, config_path, projects_dir, write_config
    ):
        """If the DB transaction fails, config.json is untouched and the
        migration flag is NOT set — so the next run can retry."""
        _make_session_file(projects_dir, PROJECT, SID_A)
        original = {
            "theme": "textual-dark",
            "session_names": {SID_A: "preserved"},
            "session_order": [SID_A],
            "last_viewed": {SID_A: 12.0},
        }
        write_config(original)

        # Sabotage the INSERT target so the transaction aborts.
        conn.execute("ALTER TABLE sessions RENAME TO sessions_bak")
        try:
            result = _migrate(conn, config_path, projects_dir)
        finally:
            conn.execute("ALTER TABLE sessions_bak RENAME TO sessions")

        assert result["action"] == "failed"
        assert json.loads(config_path.read_text()) == original
        assert db.get_setting(conn, db.MIGRATION_FLAG) is None

    def test_failed_migration_can_be_retried(
        self, conn, config_path, projects_dir, write_config
    ):
        _make_session_file(projects_dir, PROJECT, SID_A)
        write_config({"session_names": {SID_A: "retryable"}})

        # Fail.
        conn.execute("ALTER TABLE sessions RENAME TO sessions_bak")
        fail_result = _migrate(conn, config_path, projects_dir)
        conn.execute("ALTER TABLE sessions_bak RENAME TO sessions")
        assert fail_result["action"] == "failed"

        # Retry.
        retry_result = _migrate(conn, config_path, projects_dir)
        assert retry_result["action"] == "migrated"
        row = db.query_one(
            conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (SID_A,),
        )
        assert row["custom_name"] == "retryable"

    def test_malformed_json_fails_safely(self, conn, config_path, projects_dir):
        config_path.write_text("{not json")

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "failed"
        assert db.query_all(conn, "SELECT * FROM sessions") == []
        assert db.get_setting(conn, db.MIGRATION_FLAG) is None

    def test_non_object_config_fails_safely(self, conn, config_path, projects_dir):
        config_path.write_text("[1, 2, 3]")

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "failed"
        assert "not a JSON object" in result["reason"]
        assert db.query_all(conn, "SELECT * FROM sessions") == []


# =========================================================================
# Edge cases: defensive coercion, missing files, bad types
# =========================================================================

class TestEdgeCases:
    def test_sessions_missing_from_disk_are_skipped(
        self, conn, config_path, projects_dir, write_config
    ):
        """A session_id in config.json without a JSONL file on disk is
        reported as skipped (session_path is NOT NULL)."""
        live_id = SID_A
        ghost_id = SID_B
        _make_session_file(projects_dir, PROJECT, live_id)

        write_config({
            "session_names": {live_id: "visible", ghost_id: "orphan"},
            "session_order": [ghost_id, live_id],
            "last_viewed": {ghost_id: 99.0},
        })

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "migrated"
        assert live_id in result["migrated"]
        assert ghost_id in result["skipped"]
        rows = db.query_all(conn, "SELECT session_id FROM sessions")
        assert [r["session_id"] for r in rows] == [live_id]

    def test_malformed_session_names_coerced_to_empty(
        self, conn, config_path, projects_dir, write_config
    ):
        """If session_names is an array instead of a dict, migration should
        coerce it to empty rather than crash."""
        write_config({
            "theme": "textual-dark",
            "session_names": ["not", "a", "dict"],
        })

        result = _migrate(conn, config_path, projects_dir)

        # No sessions to migrate, but the legacy key is still stripped.
        assert result["action"] == "migrated"
        assert result["migrated"] == []
        remaining = json.loads(config_path.read_text())
        assert "session_names" not in remaining

    def test_non_numeric_last_viewed_is_dropped(
        self, conn, config_path, projects_dir, write_config
    ):
        """A non-numeric last_viewed value should be treated as NULL rather
        than crashing the migration."""
        _make_session_file(projects_dir, PROJECT, SID_A)
        write_config({
            "session_names": {SID_A: "foo"},
            "last_viewed": {SID_A: "not-a-timestamp"},
        })

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "migrated"
        row = db.query_one(
            conn, "SELECT last_viewed FROM sessions WHERE session_id = ?",
            (SID_A,),
        )
        assert row["last_viewed"] is None

    def test_empty_projects_dir_skips_all(
        self, conn, config_path, projects_dir, write_config
    ):
        """If the projects directory is empty, all sessions are skipped."""
        write_config({
            "session_names": {SID_A: "foo", SID_B: "bar"},
        })

        result = _migrate(conn, config_path, projects_dir)

        assert result["action"] == "migrated"
        assert result["migrated"] == []
        assert set(result["skipped"]) == {SID_A, SID_B}

    def test_missing_projects_dir_skips_all(
        self, conn, config_path, tmp_path, write_config
    ):
        """If the projects directory doesn't exist at all, migration still
        completes gracefully (all sessions are skipped)."""
        nonexistent = tmp_path / "no-such-dir"
        write_config({"session_names": {SID_A: "foo"}})

        result = _migrate(conn, config_path, nonexistent)

        assert result["action"] == "migrated"
        assert result["skipped"] == [SID_A]


# =========================================================================
# Session-row helper smoke tests (post-migration TUI write path)
# =========================================================================

class TestSessionHelpers:
    """Sanity tests for the session-row helpers the TUI calls post-migration."""

    @pytest.fixture(autouse=True)
    def _seed_session(self, conn, tmp_path):
        self.sid = SID_A
        db.upsert_session(
            conn,
            session_id=self.sid,
            session_path=str(tmp_path / "fake.jsonl"),
            project=PROJECT,
            cwd="/Users/alice/work",
            modified_at=1234.5,
        )

    def test_upsert_preserves_user_owned_fields(self, conn, tmp_path):
        db.set_custom_name(conn, self.sid, "mine")
        db.set_last_viewed(conn, self.sid, 555.0)

        # Second upsert — simulates a background refresh.
        db.upsert_session(
            conn,
            session_id=self.sid,
            session_path=str(tmp_path / "fake.jsonl"),
            project=PROJECT,
            modified_at=9999.0,
        )

        row = db.query_one(
            conn, "SELECT * FROM sessions WHERE session_id = ?", (self.sid,)
        )
        assert row["custom_name"] == "mine"
        assert row["last_viewed"] == 555.0
        assert row["modified_at"] == 9999.0  # filesystem-owned DID update

    def test_set_custom_name_empty_clears(self, conn):
        db.set_custom_name(conn, self.sid, "temp")
        db.set_custom_name(conn, self.sid, "")

        row = db.query_one(
            conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (self.sid,),
        )
        assert row["custom_name"] is None

    def test_set_session_order_assigns_positions(self, conn, tmp_path):
        sid2 = SID_B
        db.upsert_session(
            conn, sid2, str(tmp_path / "two.jsonl"), project=PROJECT_B,
        )

        db.set_session_order(conn, [sid2, self.sid])
        assert db.get_session_order(conn) == [sid2, self.sid]

        db.set_session_order(conn, [self.sid, sid2])
        assert db.get_session_order(conn) == [self.sid, sid2]

    def test_get_helpers_return_only_populated_rows(self, conn):
        assert db.get_custom_names(conn) == {}
        assert db.get_last_viewed(conn) == {}

        db.set_custom_name(conn, self.sid, "hello")
        db.set_last_viewed(conn, self.sid, 7.0)
        assert db.get_custom_names(conn) == {self.sid: "hello"}
        assert db.get_last_viewed(conn) == {self.sid: 7.0}


# =========================================================================
# Temp-dir isolation sanity check
# =========================================================================

class TestIsolation:
    def test_real_config_dir_is_untouched(
        self, conn, config_path, projects_dir, write_config
    ):
        """Regression check: if the user has a real config.json, the test
        suite must not modify it."""
        real_config = Path.home() / ".config" / "threadhop" / "config.json"
        before = real_config.stat().st_mtime if real_config.exists() else None
        before_bytes = real_config.read_bytes() if real_config.exists() else None

        _make_session_file(projects_dir, PROJECT, SID_A)
        write_config({
            "theme": "textual-dark",
            "session_names": {SID_A: "test"},
        })
        _migrate(conn, config_path, projects_dir)

        after = real_config.stat().st_mtime if real_config.exists() else None
        after_bytes = real_config.read_bytes() if real_config.exists() else None

        assert before == after
        assert before_bytes == after_bytes
