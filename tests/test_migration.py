"""Tests for the one-time config.json → SQLite migration (ADR-001).

Exercises `db.migrate_config_json_to_sqlite` end-to-end against a
temporary config file, JSONL project tree, and SQLite database.

Run from the repo root:

    python -m unittest tests.test_migration -v
"""

from __future__ import annotations

import json
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

# Make the sibling `db` module importable when running from the repo root.
REPO_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO_ROOT))
import db  # noqa: E402


def _make_session_file(projects_dir: Path, project: str, session_id: str) -> Path:
    """Create a minimal JSONL file so the migration's path-resolution scan
    has something real to discover for `session_id`."""
    proj = projects_dir / project
    proj.mkdir(parents=True, exist_ok=True)
    f = proj / f"{session_id}.jsonl"
    # Content doesn't matter — migration only uses the filename + stat().
    f.write_text('{"sessionId":"' + session_id + '"}\n')
    return f


class MigrationTest(unittest.TestCase):
    """End-to-end tests for migrate_config_json_to_sqlite."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.config_path = self.root / "config.json"
        self.projects = self.root / "projects"
        self.projects.mkdir()
        self.db_path = self.root / "test.db"
        self.conn = db.init_db(self.db_path)

    def tearDown(self) -> None:
        self.conn.close()
        self._tmp.cleanup()

    def _write_config(self, payload: dict) -> None:
        self.config_path.write_text(json.dumps(payload))

    # --- Happy path -------------------------------------------------------

    def test_moves_session_names_order_and_last_viewed_into_sqlite(self) -> None:
        """All three legacy session-level keys are preserved end-to-end."""
        sid_a = "11111111-1111-1111-1111-111111111111"
        sid_b = "22222222-2222-2222-2222-222222222222"
        _make_session_file(self.projects, "-Users-alice-work", sid_a)
        _make_session_file(self.projects, "-Users-alice-home", sid_b)

        self._write_config({
            "theme": "textual-light",
            "session_names": {sid_a: "Pivot talk", sid_b: "Groceries"},
            "session_order": [sid_b, sid_a],
            "last_viewed": {sid_a: 100.0, sid_b: 200.0},
        })

        result = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )

        self.assertEqual(result["action"], "migrated")
        self.assertCountEqual(result["migrated"], [sid_a, sid_b])
        self.assertEqual(result["skipped"], [])

        rows = db.query_all(
            self.conn, "SELECT * FROM sessions ORDER BY sort_order"
        )
        # sid_b came first in session_order → sort_order 0, sid_a → 1.
        self.assertEqual([r["session_id"] for r in rows], [sid_b, sid_a])
        self.assertEqual(
            {r["session_id"]: r["custom_name"] for r in rows},
            {sid_a: "Pivot talk", sid_b: "Groceries"},
        )
        self.assertEqual(
            {r["session_id"]: r["last_viewed"] for r in rows},
            {sid_a: 100.0, sid_b: 200.0},
        )

    def test_keeps_app_level_keys_and_drops_legacy_keys_from_config(self) -> None:
        """theme + unknown keys (sidebar_width) remain in config.json;
        legacy session-level keys are removed."""
        sid = "33333333-3333-3333-3333-333333333333"
        _make_session_file(self.projects, "-Users-alice-work", sid)

        self._write_config({
            "theme": "textual-light",
            "sidebar_width": 42,
            "session_names": {sid: "foo"},
            "session_order": [sid],
            "last_viewed": {sid: 1.0},
        })

        db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )

        remaining = json.loads(self.config_path.read_text())
        self.assertEqual(remaining, {"theme": "textual-light", "sidebar_width": 42})

    # --- Idempotency ------------------------------------------------------

    def test_second_call_is_a_noop(self) -> None:
        """Running migration twice yields identical state to running once."""
        sid = "44444444-4444-4444-4444-444444444444"
        _make_session_file(self.projects, "-Users-alice-work", sid)

        self._write_config({
            "theme": "textual-dark",
            "session_names": {sid: "first"},
        })

        r1 = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        self.assertEqual(r1["action"], "migrated")

        r2 = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        self.assertEqual(r2["action"], "skipped")
        self.assertEqual(r2["reason"], "already migrated")

        rows = db.query_all(self.conn, "SELECT * FROM sessions")
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["custom_name"], "first")

    def test_second_call_does_not_overwrite_post_migration_user_changes(self) -> None:
        """After migration, the TUI may rename a session or update last_viewed.
        A second migration call must NOT clobber those fresh values."""
        sid = "55555555-5555-5555-5555-555555555555"
        _make_session_file(self.projects, "-Users-alice-work", sid)

        self._write_config({"session_names": {sid: "original"}})

        db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        # Simulate a user rename via the TUI's post-migration write path.
        db.set_custom_name(self.conn, sid, "renamed by user")

        # Second migration call — must be a no-op because the flag is set.
        db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )

        row = db.query_one(
            self.conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (sid,),
        )
        self.assertEqual(row["custom_name"], "renamed by user")

    # --- Failure preservation --------------------------------------------

    def test_preserves_config_and_flag_when_db_write_fails(self) -> None:
        """If the DB transaction fails, config.json is untouched and the
        migration flag is NOT set — so the next run can retry."""
        sid = "66666666-6666-6666-6666-666666666666"
        _make_session_file(self.projects, "-Users-alice-work", sid)

        original = {
            "theme": "textual-dark",
            "session_names": {sid: "preserved"},
            "session_order": [sid],
            "last_viewed": {sid: 12.0},
        }
        self._write_config(original)

        # Sabotage the INSERT target so the transaction aborts. RENAME
        # the table out from under the migration; we restore it before
        # the assertions so the rest of the test can query cleanly.
        self.conn.execute("ALTER TABLE sessions RENAME TO sessions_bak")
        try:
            result = db.migrate_config_json_to_sqlite(
                self.conn, self.config_path, self.projects
            )
        finally:
            self.conn.execute("ALTER TABLE sessions_bak RENAME TO sessions")

        self.assertEqual(result["action"], "failed")
        # config.json must be byte-for-byte identical.
        self.assertEqual(
            json.loads(self.config_path.read_text()), original
        )
        # No flag → next run retries migration.
        self.assertIsNone(db.get_setting(self.conn, db.MIGRATION_FLAG))

    def test_failed_migration_can_be_retried_successfully(self) -> None:
        """After a failure, the next call should re-run and succeed."""
        sid = "77777777-7777-7777-7777-777777777777"
        _make_session_file(self.projects, "-Users-alice-work", sid)
        self._write_config({"session_names": {sid: "retryable"}})

        # Force failure.
        self.conn.execute("ALTER TABLE sessions RENAME TO sessions_bak")
        fail_result = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        self.conn.execute("ALTER TABLE sessions_bak RENAME TO sessions")
        self.assertEqual(fail_result["action"], "failed")

        # Retry — should now succeed.
        retry_result = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        self.assertEqual(retry_result["action"], "migrated")
        row = db.query_one(
            self.conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (sid,),
        )
        self.assertEqual(row["custom_name"], "retryable")

    # --- Edge cases -------------------------------------------------------

    def test_skips_sessions_missing_from_disk(self) -> None:
        """A session_id in config.json without a JSONL file on disk is
        reported as skipped (we can't satisfy session_path NOT NULL)."""
        live_id = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
        ghost_id = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"
        _make_session_file(self.projects, "-Users-alice-work", live_id)

        self._write_config({
            "session_names": {live_id: "visible", ghost_id: "orphan"},
            "session_order": [ghost_id, live_id],
            "last_viewed": {ghost_id: 99.0},
        })

        result = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        self.assertEqual(result["action"], "migrated")
        self.assertIn(live_id, result["migrated"])
        self.assertIn(ghost_id, result["skipped"])

        rows = db.query_all(self.conn, "SELECT session_id FROM sessions")
        self.assertEqual([r["session_id"] for r in rows], [live_id])

    def test_missing_config_json_sets_flag_without_error(self) -> None:
        """First-ever launch with no config.json is a valid no-op; the
        flag still gets set so we don't re-scan the filesystem later."""
        # Note: we did NOT call _write_config.
        result = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        self.assertEqual(result["action"], "skipped")
        self.assertEqual(result["reason"], "no config.json")
        self.assertTrue(db.get_setting(self.conn, db.MIGRATION_FLAG))

    def test_empty_session_keys_still_clean_up_config(self) -> None:
        """A config with only empty session-level containers migrates
        cleanly: no rows inserted, and the legacy keys are dropped
        from config.json."""
        self._write_config({
            "theme": "textual-dark",
            "session_names": {},
            "session_order": [],
            "last_viewed": {},
        })

        result = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        self.assertEqual(result["action"], "migrated")
        self.assertEqual(result["migrated"], [])

        remaining = json.loads(self.config_path.read_text())
        self.assertEqual(remaining, {"theme": "textual-dark"})

    def test_malformed_config_fails_safely(self) -> None:
        """An unreadable/invalid config.json returns 'failed' without
        touching the DB or setting the flag."""
        self.config_path.write_text("{not json")

        result = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        self.assertEqual(result["action"], "failed")
        # No rows, no flag.
        rows = db.query_all(self.conn, "SELECT * FROM sessions")
        self.assertEqual(rows, [])
        self.assertIsNone(db.get_setting(self.conn, db.MIGRATION_FLAG))

    def test_rewrite_config_false_leaves_file_untouched(self) -> None:
        """rewrite_config=False is for callers who want to migrate DB state
        but inspect the pre-rewrite config first (tests, dry-runs)."""
        sid = "cccccccc-cccc-cccc-cccc-cccccccccccc"
        _make_session_file(self.projects, "-Users-alice-work", sid)
        original = {"theme": "textual-dark", "session_names": {sid: "kept"}}
        self._write_config(original)

        result = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects, rewrite_config=False
        )
        self.assertEqual(result["action"], "migrated")

        # DB has the row…
        row = db.query_one(
            self.conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (sid,),
        )
        self.assertEqual(row["custom_name"], "kept")
        # …but config.json is still the original.
        self.assertEqual(json.loads(self.config_path.read_text()), original)

    def test_cleanup_of_partial_previous_run(self) -> None:
        """Simulate a prior partial-success state (DB migrated, flag set,
        but config.json still has legacy keys). A subsequent call should
        clean up the lingering legacy keys without touching DB rows."""
        sid = "dddddddd-dddd-dddd-dddd-dddddddddddd"
        _make_session_file(self.projects, "-Users-alice-work", sid)
        # Set the flag manually to simulate "already migrated".
        db.set_setting(self.conn, db.MIGRATION_FLAG, True)
        # Insert a pre-existing row with data (must NOT be overwritten).
        db.upsert_session(
            self.conn, sid, str(self.projects / "stub.jsonl")
        )
        db.set_custom_name(self.conn, sid, "established name")

        # Leave legacy keys in config.json to simulate prior partial rewrite.
        self._write_config({
            "theme": "textual-dark",
            "session_names": {sid: "would clobber"},
            "session_order": [sid],
        })

        result = db.migrate_config_json_to_sqlite(
            self.conn, self.config_path, self.projects
        )
        self.assertEqual(result["action"], "skipped")

        # Legacy keys must have been cleaned up from config.json…
        self.assertEqual(
            json.loads(self.config_path.read_text()),
            {"theme": "textual-dark"},
        )
        # …and the DB row must be unchanged (flag-guard prevents clobber).
        row = db.query_one(
            self.conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (sid,),
        )
        self.assertEqual(row["custom_name"], "established name")


class SessionHelperTest(unittest.TestCase):
    """Sanity tests for the session-row helpers the TUI calls post-migration."""

    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.conn = db.init_db(self.root / "test.db")
        self.sid = "eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee"
        db.upsert_session(
            self.conn,
            session_id=self.sid,
            session_path=str(self.root / "fake.jsonl"),
            project="-Users-alice-work",
            cwd="/Users/alice/work",
            modified_at=1234.5,
        )

    def tearDown(self) -> None:
        self.conn.close()
        self._tmp.cleanup()

    def test_upsert_preserves_user_owned_fields(self) -> None:
        """A rescan shouldn't clobber a user's rename or last_viewed."""
        db.set_custom_name(self.conn, self.sid, "mine")
        db.set_last_viewed(self.conn, self.sid, 555.0)

        # Second upsert — simulates the background refresh loop.
        db.upsert_session(
            self.conn,
            session_id=self.sid,
            session_path=str(self.root / "fake.jsonl"),
            project="-Users-alice-work",
            modified_at=9999.0,
        )
        row = db.query_one(
            self.conn, "SELECT * FROM sessions WHERE session_id = ?", (self.sid,)
        )
        self.assertEqual(row["custom_name"], "mine")
        self.assertEqual(row["last_viewed"], 555.0)
        # But the filesystem-owned modified_at DID update.
        self.assertEqual(row["modified_at"], 9999.0)

    def test_set_custom_name_empty_clears(self) -> None:
        db.set_custom_name(self.conn, self.sid, "temp")
        db.set_custom_name(self.conn, self.sid, "")
        row = db.query_one(
            self.conn, "SELECT custom_name FROM sessions WHERE session_id = ?",
            (self.sid,),
        )
        self.assertIsNone(row["custom_name"])

    def test_set_session_order_assigns_positions(self) -> None:
        sid2 = "ffffffff-ffff-ffff-ffff-ffffffffffff"
        db.upsert_session(
            self.conn, sid2, str(self.root / "two.jsonl"),
            project="-Users-alice-home",
        )

        db.set_session_order(self.conn, [sid2, self.sid])

        self.assertEqual(
            db.get_session_order(self.conn), [sid2, self.sid]
        )

        # Re-order and confirm the first ID now comes second.
        db.set_session_order(self.conn, [self.sid, sid2])
        self.assertEqual(
            db.get_session_order(self.conn), [self.sid, sid2]
        )

    def test_get_helpers_return_only_populated_rows(self) -> None:
        """Sessions without a custom_name / last_viewed shouldn't appear
        in the returned dicts (keeps the TUI reads sparse)."""
        self.assertEqual(db.get_custom_names(self.conn), {})
        self.assertEqual(db.get_last_viewed(self.conn), {})

        db.set_custom_name(self.conn, self.sid, "hello")
        db.set_last_viewed(self.conn, self.sid, 7.0)
        self.assertEqual(db.get_custom_names(self.conn), {self.sid: "hello"})
        self.assertEqual(db.get_last_viewed(self.conn), {self.sid: 7.0})


if __name__ == "__main__":
    unittest.main()
