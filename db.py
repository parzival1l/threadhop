"""SQLite database module for ThreadHop.

Handles DB initialization, schema migrations, and query helpers for persistent
session metadata. Per ADR-001, this replaces config.json for all state except
app-level settings (theme, sidebar_width).

DB location: ~/.config/threadhop/sessions.db
Journal mode: WAL (concurrent reads for TUI + skill plugin).

Typical usage:

    import db
    conn = db.init_db()                 # creates file + runs migrations
    db.set_setting(conn, "theme", "textual-dark")
    theme = db.get_setting(conn, "theme")
    rows = db.query_all(conn, "SELECT * FROM sessions WHERE status = ?", ("active",))

Schema / Python-type co-evolution (task #24):
Each SQL table has a matching Pydantic shape in ``models.py`` (``Session``,
``Message``, ``Bookmark``, ``MemoryEntry``). When a migration adds or renames
a column, update the matching model in the same change — the two are the
contract between SQL and Python and must not drift. Enum-like columns use
``CHECK`` constraints here and ``Literal`` types there, so an invalid value
is rejected at the DB layer *and* the parse layer.
"""

from __future__ import annotations

import json
import sqlite3
from contextlib import contextmanager
from datetime import datetime
from pathlib import Path
from typing import Any, Iterator, Sequence

# --- Paths ---
DB_DIR = Path.home() / ".config" / "threadhop"
DB_PATH = DB_DIR / "sessions.db"

# --- Schema version ---
# Each migration N in MIGRATIONS moves the DB from version N to N+1.
# The DB's current version lives in PRAGMA user_version.
SCHEMA_VERSION = 7


# --- Migrations -----------------------------------------------------------

def _migration_001_initial(conn: sqlite3.Connection) -> None:
    """Phase 1 foundation: settings + sessions tables.

    Schema mirrors ADR-001 in docs/DESIGN-DECISIONS.md. Additional tables
    (messages, messages_fts, index_state, bookmarks, memory) arrive in
    later migrations as their features land.

    Statements are issued one at a time (not via executescript) so they
    run inside the caller's BEGIN/COMMIT transaction — executescript
    disregards isolation_level and commits on its own.
    """
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL  -- JSON-encoded
        )
        """
    )
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS sessions (
            session_id    TEXT PRIMARY KEY,
            session_path  TEXT NOT NULL,
            project       TEXT,
            cwd           TEXT,
            custom_name   TEXT,
            status        TEXT NOT NULL DEFAULT 'active',
                -- active | in_progress | in_review | done | archived
            sort_order    INTEGER,
            last_viewed   REAL,
            created_at    REAL,
            modified_at   REAL
        )
        """
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status)"
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_sessions_sort_order ON sessions(sort_order)"
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_sessions_modified_at ON sessions(modified_at)"
    )


def _migration_002_messages(conn: sqlite3.Connection) -> None:
    """Phase 2: messages table + FTS5 index.

    Schema per ADR-003 in docs/DESIGN-DECISIONS.md. The indexer merges
    consecutive assistant JSONL lines that share the same `message.id`
    into one row before insertion — see `indexer.parse_messages`.

    FTS5 uses external content (`content='messages'`) so the `text`
    column isn't stored twice. The three triggers below keep the FTS
    shadow table in sync on any write to `messages`. The `porter`
    stemmer lets "running" match "run" (ADR-002).
    """
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS messages (
            uuid         TEXT PRIMARY KEY,
            session_id   TEXT NOT NULL,
            role         TEXT NOT NULL,       -- 'user' | 'assistant'
            text         TEXT NOT NULL,
            timestamp    TEXT,
            cwd          TEXT,
            parent_uuid  TEXT,
            is_sidechain INTEGER NOT NULL DEFAULT 0
        )
        """
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_messages_session_id ON messages(session_id)"
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_messages_timestamp ON messages(timestamp)"
    )

    # External-content FTS5: the virtual table stores tokens only and
    # uses `messages.rowid` to retrieve the text. The triggers below
    # mirror INSERT / DELETE / UPDATE on messages into the FTS index.
    conn.execute(
        """
        CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
            text,
            content='messages',
            content_rowid='rowid',
            tokenize='porter unicode61'
        )
        """
    )
    conn.execute(
        """
        CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text);
        END
        """
    )
    conn.execute(
        """
        CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, text)
            VALUES ('delete', old.rowid, old.text);
        END
        """
    )
    conn.execute(
        """
        CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
            INSERT INTO messages_fts(messages_fts, rowid, text)
            VALUES ('delete', old.rowid, old.text);
            INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text);
        END
        """
    )


def _migration_003_index_state(conn: sqlite3.Connection) -> None:
    """Task #9: incremental indexing support.

    - ``index_state``: tracks byte offset per session so the refresh
      cycle only parses newly-appended JSONL bytes.
    - ``messages.message_id``: stores the Claude API ``message.id``
      (shared across streaming chunks).  Needed for cross-batch chunk
      merging — when an assistant response spans two refresh cycles,
      the second batch must look up and UPDATE the row created by the
      first batch.
    """
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS index_state (
            session_id       TEXT PRIMARY KEY,
            file_path        TEXT NOT NULL,
            last_byte_offset INTEGER NOT NULL DEFAULT 0,
            last_indexed_at  REAL NOT NULL
        )
        """
    )

    # ALTER TABLE doesn't support IF NOT EXISTS, so guard with a
    # column-presence check to keep the migration idempotent.
    cols = {r[1] for r in conn.execute("PRAGMA table_info(messages)").fetchall()}
    if "message_id" not in cols:
        conn.execute("ALTER TABLE messages ADD COLUMN message_id TEXT")
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_message_id "
            "ON messages(message_id)"
        )


def _migration_004_observation_state(conn: sqlite3.Connection) -> None:
    """Phase 3: observer + reflector state tracking (ADR-019, ADR-022).

    Tracks per-session observation state: where the observer left off in
    the source JSONL (``source_byte_offset``), how many observation entries
    have been written (``entry_count``), what the reflector has already
    compared (``reflector_entry_offset``), and whether the observer is
    currently running (``observer_pid``).

    This table is the single source of truth for "has this session been
    observed?" and "where did we leave off?" — queried by the TUI for the
    observation indicator, by the observer for incremental processing, and
    by the reflector for its own cadence tracking.
    """
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS observation_state (
            session_id              TEXT PRIMARY KEY,
            source_path             TEXT NOT NULL,
            obs_path                TEXT NOT NULL,
            source_byte_offset      INTEGER NOT NULL DEFAULT 0,
            entry_count             INTEGER NOT NULL DEFAULT 0,
            reflector_entry_offset  INTEGER NOT NULL DEFAULT 0,
            observer_pid            INTEGER,
            status                  TEXT NOT NULL DEFAULT 'idle',
                -- idle | running | stopped
            started_at              REAL,
            last_observed_at        REAL,
            FOREIGN KEY (session_id) REFERENCES sessions(session_id)
        )
        """
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_obs_state_status "
        "ON observation_state(status)"
    )


def _migration_005_conflict_reviews(conn: sqlite3.Connection) -> None:
    """Persist review state for conflict entries without mutating JSONL.

    Conflict entries are append-only in the per-session observation files
    (ADR-020). Review state therefore lives in SQLite, keyed by the same
    dedup dimensions the reflector prompt uses: the session whose file
    contains the conflict, the refs pair, and the semantic topic.
    """
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS conflict_reviews (
            session_id    TEXT NOT NULL,
            refs_key      TEXT NOT NULL,
            topic         TEXT NOT NULL DEFAULT '',
            reviewed_at   REAL NOT NULL,
            PRIMARY KEY (session_id, refs_key, topic),
            FOREIGN KEY (session_id) REFERENCES sessions(session_id)
        )
        """
    )
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_conflict_reviews_session "
        "ON conflict_reviews(session_id)"
    )


_SESSIONS_TABLE_DDL_WITH_CHECK = """\
CREATE TABLE sessions (
    session_id    TEXT PRIMARY KEY,
    session_path  TEXT NOT NULL,
    project       TEXT,
    cwd           TEXT,
    custom_name   TEXT,
    status        TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN
            ('active', 'in_progress', 'in_review', 'done', 'archived')),
    sort_order    INTEGER,
    last_viewed   REAL,
    created_at    REAL,
    modified_at   REAL
)"""


def _migration_006_sessions_status_check(conn: sqlite3.Connection) -> None:
    """Enforce the session status enum at the DB layer (ADR-004, task #24).

    Pairs with :data:`models.SessionStatus` — the ``Literal`` there and the
    ``CHECK`` here list the same values. Updating one without the other is
    a bug; keep them in lockstep.

    SQLite has no ``ALTER TABLE ... ADD CONSTRAINT``. The textbook workaround
    (create ``sessions_new``, copy, ``DROP``, ``RENAME``) breaks in our
    context because ``observation_state`` and ``conflict_reviews``
    (migrations 004 and 005) hold ``FOREIGN KEY`` references to
    ``sessions(session_id)``. With ``PRAGMA foreign_keys = ON`` (set in
    :func:`connect`), the rebuild either fails at ``DROP TABLE`` or fails
    at ``COMMIT``, and ``PRAGMA foreign_keys = OFF`` is a no-op inside the
    migration runner's open transaction.

    Instead, patch the stored ``CREATE TABLE`` DDL directly via
    ``PRAGMA writable_schema``. The table keeps its rowids, its indexes,
    and its identity — no rows are moved, child FKs don't re-resolve, and
    the CHECK takes effect for all subsequent writes. The
    ``PRAGMA schema_version`` bump forces this connection to invalidate
    its parsed-schema cache so the CHECK fires on the next INSERT without
    requiring callers to reopen the connection.

    Steps:

    1. Normalize any pre-existing rows whose status somehow slipped past
       the app-level validation — otherwise the next write touching
       ``sessions`` would trip the new CHECK on legacy data.
    2. With ``writable_schema = ON``, replace the stored ``CREATE TABLE``
       text in ``sqlite_master`` with a version that includes the CHECK.
    3. Bump ``schema_version`` so SQLite reloads the schema definition in
       this connection's cache.
    4. Run ``PRAGMA integrity_check`` to verify the patched schema parses
       cleanly before the migration runner commits.
    """
    conn.execute(
        """
        UPDATE sessions
        SET status = 'active'
        WHERE status NOT IN
            ('active', 'in_progress', 'in_review', 'done', 'archived')
        """
    )

    current_sv = conn.execute("PRAGMA schema_version").fetchone()[0]

    conn.execute("PRAGMA writable_schema = ON")
    try:
        conn.execute(
            "UPDATE sqlite_master SET sql = ? "
            "WHERE type = 'table' AND name = 'sessions'",
            (_SESSIONS_TABLE_DDL_WITH_CHECK,),
        )
        # Bumping schema_version forces SQLite to re-parse sqlite_master
        # on the next statement — without this, the same connection keeps
        # using its cached pre-CHECK definition until it's reopened.
        conn.execute(f"PRAGMA schema_version = {current_sv + 1}")
    finally:
        conn.execute("PRAGMA writable_schema = OFF")

    # Guard rail: if the DDL swap produced anything SQLite can't parse,
    # integrity_check reports it here so the migration rolls back rather
    # than shipping a corrupt schema.
    rows = conn.execute("PRAGMA integrity_check").fetchall()
    if rows and rows[0][0] != "ok":
        raise RuntimeError(
            f"integrity_check failed after sessions DDL patch: {rows}"
        )


def _migration_007_bookmarks(conn: sqlite3.Connection) -> None:
    """Task #18: bookmarks table.

    Users pin messages for later recall via `space` in selection mode.
    One bookmark per message (UNIQUE on ``message_uuid``) so the keybind
    is a pure toggle. ``FK … ON DELETE CASCADE`` keeps the table free
    of orphan rows if a message is ever reindexed out from under a pin.

    ``tags`` is a JSON-encoded array of strings, mirroring the
    :class:`models.Bookmark` shape declared for this table in
    ``models.py`` (task #24's model/schema lockstep rule). The tag
    *editor UX* is deferred — tracked separately — so the column exists
    but only ``label`` has a TUI surface in this migration's companion
    commit.
    """
    conn.execute(
        """
        CREATE TABLE IF NOT EXISTS bookmarks (
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
        "CREATE INDEX IF NOT EXISTS idx_bookmarks_created_at "
        "ON bookmarks(created_at)"
    )


# Ordered list; MIGRATIONS[i] moves schema from version i to i+1.
MIGRATIONS: list = [
    _migration_001_initial,
    _migration_002_messages,
    _migration_003_index_state,
    _migration_004_observation_state,
    _migration_005_conflict_reviews,
    _migration_006_sessions_status_check,
    _migration_007_bookmarks,
]

assert len(MIGRATIONS) == SCHEMA_VERSION, (
    "SCHEMA_VERSION must equal len(MIGRATIONS)"
)


# --- Connection management ------------------------------------------------

def connect(db_path: Path | str | None = None) -> sqlite3.Connection:
    """Open a connection with WAL mode, row factory, and FK enforcement.

    Ensures the parent directory exists. Uses isolation_level=None so callers
    control transactions explicitly via BEGIN/COMMIT (see `transaction()`).
    `check_same_thread=False` because the TUI's background worker may access
    the DB from a different thread than the main loop — WAL makes this safe
    for readers, and we funnel writes through the app's async loop.
    """
    path = Path(db_path) if db_path is not None else DB_PATH
    path.parent.mkdir(parents=True, exist_ok=True)

    conn = sqlite3.connect(
        str(path),
        isolation_level=None,
        check_same_thread=False,
    )
    conn.row_factory = sqlite3.Row

    # WAL: concurrent readers alongside a writer. Required by ADR-001.
    conn.execute("PRAGMA journal_mode = WAL")
    # NORMAL is the WAL-recommended durability level — faster than FULL,
    # safe against crashes (only loses uncommitted transactions on power loss).
    conn.execute("PRAGMA synchronous = NORMAL")
    conn.execute("PRAGMA foreign_keys = ON")
    # Required for `INSERT OR REPLACE` on tables with AFTER DELETE triggers
    # (e.g., messages → messages_fts sync). With the default OFF, REPLACE
    # silently skips delete triggers, causing the FTS shadow to accumulate
    # stale entries that still match queries. See:
    # https://www.sqlite.org/lang_conflict.html#the_replace_algorithm
    conn.execute("PRAGMA recursive_triggers = ON")
    return conn


# --- Schema version helpers ----------------------------------------------

def get_schema_version(conn: sqlite3.Connection) -> int:
    """Return the current schema version stored in PRAGMA user_version."""
    row = conn.execute("PRAGMA user_version").fetchone()
    # Row may be tuple-like or sqlite3.Row; index 0 works for both.
    return int(row[0]) if row is not None else 0


def _set_schema_version(conn: sqlite3.Connection, version: int) -> None:
    """Write the schema version. PRAGMA doesn't accept parameter binding,
    so the value is interpolated after an int() cast."""
    conn.execute(f"PRAGMA user_version = {int(version)}")


def apply_migrations(conn: sqlite3.Connection) -> int:
    """Run any pending migrations in order. Returns the final schema version.

    Each migration runs in its own transaction so a failure rolls back
    cleanly without leaving the DB in a half-migrated state.
    """
    current = get_schema_version(conn)
    if current > SCHEMA_VERSION:
        raise RuntimeError(
            f"DB schema version {current} is newer than code's {SCHEMA_VERSION}. "
            "Upgrade ThreadHop or restore an older DB."
        )
    for i in range(current, SCHEMA_VERSION):
        migration = MIGRATIONS[i]
        conn.execute("BEGIN")
        try:
            migration(conn)
            _set_schema_version(conn, i + 1)
            conn.execute("COMMIT")
        except Exception:
            conn.execute("ROLLBACK")
            raise
    return SCHEMA_VERSION


def init_db(db_path: Path | str | None = None) -> sqlite3.Connection:
    """Open (creating if needed) the DB and apply any pending migrations.

    This is the single entry point callers should use — it guarantees the
    returned connection points at an up-to-date schema.
    """
    conn = connect(db_path)
    apply_migrations(conn)
    return conn


# --- Query helpers --------------------------------------------------------

def row_to_dict(row: sqlite3.Row | None) -> dict | None:
    """Convert a sqlite3.Row to a plain dict. Returns None for None input."""
    if row is None:
        return None
    return dict(row)


def rows_to_dicts(rows: Sequence[sqlite3.Row]) -> list[dict]:
    """Convert an iterable of sqlite3.Row objects to a list of dicts."""
    return [dict(r) for r in rows]


def query_one(
    conn: sqlite3.Connection,
    sql: str,
    params: tuple | dict = (),
) -> dict | None:
    """Execute a parameterized SELECT and return the first row as a dict.

    Returns None if no rows match. Always use `?` (or named) placeholders —
    never string formatting — to prevent SQL injection.
    """
    row = conn.execute(sql, params).fetchone()
    return row_to_dict(row)


def query_all(
    conn: sqlite3.Connection,
    sql: str,
    params: tuple | dict = (),
) -> list[dict]:
    """Execute a parameterized SELECT and return all matching rows as dicts."""
    return rows_to_dicts(conn.execute(sql, params).fetchall())


def execute(
    conn: sqlite3.Connection,
    sql: str,
    params: tuple | dict = (),
) -> sqlite3.Cursor:
    """Execute a parameterized write (INSERT / UPDATE / DELETE)."""
    return conn.execute(sql, params)


def executemany(
    conn: sqlite3.Connection,
    sql: str,
    seq_of_params: Sequence[tuple] | Sequence[dict],
) -> sqlite3.Cursor:
    """Bulk-execute a parameterized write for a sequence of parameter sets."""
    return conn.executemany(sql, seq_of_params)


@contextmanager
def transaction(conn: sqlite3.Connection) -> Iterator[sqlite3.Connection]:
    """Run a block inside an explicit transaction.

    Commits on clean exit, rolls back on any exception. Use this when
    grouping multiple writes that must succeed or fail together.
    """
    conn.execute("BEGIN")
    try:
        yield conn
        conn.execute("COMMIT")
    except Exception:
        conn.execute("ROLLBACK")
        raise


# --- Settings helpers -----------------------------------------------------
# The settings table stores small app-level key/value pairs. Values are
# JSON-encoded so callers can round-trip ints, bools, lists, dicts.

def get_setting(
    conn: sqlite3.Connection,
    key: str,
    default: Any = None,
) -> Any:
    """Return the JSON-decoded value for `key`, or `default` if not set."""
    row = conn.execute(
        "SELECT value FROM settings WHERE key = ?", (key,)
    ).fetchone()
    if row is None:
        return default
    try:
        return json.loads(row["value"])
    except (json.JSONDecodeError, TypeError):
        # Tolerate legacy raw-string values written before this helper existed.
        return row["value"]


def set_setting(conn: sqlite3.Connection, key: str, value: Any) -> None:
    """Upsert a setting. Value is JSON-encoded before storage."""
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?, ?) "
        "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, json.dumps(value)),
    )


def delete_setting(conn: sqlite3.Connection, key: str) -> None:
    """Remove a setting. No-op if the key doesn't exist."""
    conn.execute("DELETE FROM settings WHERE key = ?", (key,))


# --- Session-row helpers --------------------------------------------------
# Thin wrappers over the `sessions` table for the fields that get touched
# by both the filesystem scan and the user's UI actions. Splitting these
# keeps the callers honest about which fields they own:
#   - filesystem scan owns: session_path, project, cwd, modified_at, created_at
#   - user actions own:     custom_name, sort_order, last_viewed, status
# upsert_session deliberately does NOT touch the user-owned fields so the
# background refresh loop can refresh rows without clobbering renames.

def upsert_session(
    conn: sqlite3.Connection,
    session_id: str,
    session_path: str,
    *,
    project: str | None = None,
    cwd: str | None = None,
    created_at: float | None = None,
    modified_at: float | None = None,
) -> None:
    """Ensure a row exists for `session_id` with fresh filesystem metadata.

    On conflict, refreshes the filesystem-owned columns (path/project/cwd/
    modified_at) but leaves custom_name, sort_order, last_viewed, and
    status alone — those are owned by user actions and must survive a
    routine rescan.
    """
    conn.execute(
        """
        INSERT INTO sessions (
            session_id, session_path, project, cwd, created_at, modified_at
        ) VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(session_id) DO UPDATE SET
            session_path = excluded.session_path,
            project      = COALESCE(excluded.project, sessions.project),
            cwd          = COALESCE(excluded.cwd, sessions.cwd),
            modified_at  = COALESCE(excluded.modified_at, sessions.modified_at)
        """,
        (session_id, session_path, project, cwd, created_at, modified_at),
    )


def set_custom_name(
    conn: sqlite3.Connection,
    session_id: str,
    name: str | None,
) -> None:
    """Set the user-chosen display name for a session. Empty string clears it."""
    conn.execute(
        "UPDATE sessions SET custom_name = ? WHERE session_id = ?",
        (name or None, session_id),
    )


def set_last_viewed(
    conn: sqlite3.Connection,
    session_id: str,
    timestamp: float,
) -> None:
    """Record the last time the user opened this session (for unread marking)."""
    conn.execute(
        "UPDATE sessions SET last_viewed = ? WHERE session_id = ?",
        (timestamp, session_id),
    )


def set_session_order(conn: sqlite3.Connection, ordered_ids: list[str]) -> None:
    """Rewrite sort_order so ordered_ids[0] is first, [1] second, ….

    Sessions not in the list get sort_order = NULL (they'll be sorted to
    the end by the UI's ordering pass). Runs inside a single transaction
    so the list is never observed in a half-updated state.
    """
    with transaction(conn):
        conn.execute("UPDATE sessions SET sort_order = NULL")
        if ordered_ids:
            conn.executemany(
                "UPDATE sessions SET sort_order = ? WHERE session_id = ?",
                [(i, sid) for i, sid in enumerate(ordered_ids)],
            )


def get_custom_names(conn: sqlite3.Connection) -> dict[str, str]:
    """Return {session_id: custom_name} for every session with a custom name set."""
    rows = conn.execute(
        "SELECT session_id, custom_name FROM sessions "
        "WHERE custom_name IS NOT NULL"
    ).fetchall()
    return {r["session_id"]: r["custom_name"] for r in rows}


def get_last_viewed(conn: sqlite3.Connection) -> dict[str, float]:
    """Return {session_id: last_viewed} for every session that's been opened."""
    rows = conn.execute(
        "SELECT session_id, last_viewed FROM sessions "
        "WHERE last_viewed IS NOT NULL"
    ).fetchall()
    return {r["session_id"]: float(r["last_viewed"]) for r in rows}


def get_session_order(conn: sqlite3.Connection) -> list[str]:
    """Return session_ids sorted by sort_order (NULLs excluded)."""
    rows = conn.execute(
        "SELECT session_id FROM sessions "
        "WHERE sort_order IS NOT NULL ORDER BY sort_order"
    ).fetchall()
    return [r["session_id"] for r in rows]


def set_session_status(
    conn: sqlite3.Connection,
    session_id: str,
    status: str,
) -> None:
    """Set the status for a session (active|in_progress|in_review|done|archived)."""
    conn.execute(
        "UPDATE sessions SET status = ? WHERE session_id = ?",
        (status, session_id),
    )


def get_session_statuses(conn: sqlite3.Connection) -> dict[str, str]:
    """Return {session_id: status} for all sessions."""
    rows = conn.execute(
        "SELECT session_id, status FROM sessions"
    ).fetchall()
    return {r["session_id"]: r["status"] for r in rows}


# --- Index-state helpers ---------------------------------------------------

def get_index_state(
    conn: sqlite3.Connection,
    session_id: str,
) -> dict | None:
    """Return the index state for a session, or None if never indexed."""
    return query_one(
        conn,
        "SELECT * FROM index_state WHERE session_id = ?",
        (session_id,),
    )


def upsert_index_state(
    conn: sqlite3.Connection,
    session_id: str,
    file_path: str,
    last_byte_offset: int,
    last_indexed_at: float,
) -> None:
    """Insert or update the index state for a session."""
    conn.execute(
        """
        INSERT INTO index_state (
            session_id, file_path, last_byte_offset, last_indexed_at
        ) VALUES (?, ?, ?, ?)
        ON CONFLICT(session_id) DO UPDATE SET
            file_path        = excluded.file_path,
            last_byte_offset = excluded.last_byte_offset,
            last_indexed_at  = excluded.last_indexed_at
        """,
        (session_id, file_path, last_byte_offset, last_indexed_at),
    )


def delete_index_state(conn: sqlite3.Connection, session_id: str) -> None:
    """Remove the index state for a session (used on truncation/rotation)."""
    conn.execute("DELETE FROM index_state WHERE session_id = ?", (session_id,))


def delete_session_messages(conn: sqlite3.Connection, session_id: str) -> None:
    """Delete all indexed messages for a session.

    The FTS table is kept in sync automatically via the ``messages_ad``
    trigger created in migration 002.
    """
    conn.execute("DELETE FROM messages WHERE session_id = ?", (session_id,))


# --- One-time config.json → SQLite migration (ADR-001) --------------------
# Runs on first startup. Moves session-level keys out of config.json into
# the `sessions` table. Keeps `theme` (and any future app-level keys like
# `sidebar_width`) in config.json.

# Settings-table flag marking the migration complete. Stored as True after
# a successful run; checked at the top of `migrate_config_json_to_sqlite`
# so subsequent calls are a cheap no-op (= idempotent).
MIGRATION_FLAG = "config_migrated_v1"

# Legacy session-level keys that must NOT remain in config.json after
# migration. Anything else (theme, sidebar_width, unknown keys) is preserved.
_MIGRATED_CONFIG_KEYS = ("session_names", "session_order", "last_viewed")


def _rewrite_config_stripped(config_path: Path, raw: dict) -> None:
    """Rewrite config.json with the legacy session-level keys removed.

    Unknown keys are preserved — users and future features may add their
    own app-level settings, and we don't want to silently drop them.
    """
    remaining = {k: v for k, v in raw.items() if k not in _MIGRATED_CONFIG_KEYS}
    config_path.parent.mkdir(parents=True, exist_ok=True)
    config_path.write_text(json.dumps(remaining, indent=2))


# --- Session status helpers ------------------------------------------------

# Ordered list of valid status values. The TUI renders sidebar groups in
# this order. ADR-004.
SESSION_STATUS_ORDER: list[str] = [
    "active",
    "in_progress",
    "in_review",
    "done",
    "archived",
]


def get_session(
    conn: sqlite3.Connection,
    session_id: str,
) -> dict | None:
    """Return the full session row as a dict, or None if not found."""
    return query_one(
        conn, "SELECT * FROM sessions WHERE session_id = ?", (session_id,)
    )


def get_session_statuses(conn: sqlite3.Connection) -> dict[str, str]:
    """Return a {session_id: status} map for every known session.

    Used by the TUI each refresh to stamp `status` onto in-memory session
    dicts without issuing one SELECT per row.
    """
    rows = conn.execute("SELECT session_id, status FROM sessions").fetchall()
    return {r["session_id"]: r["status"] for r in rows}


def get_session_sidebar_metadata(
    conn: sqlite3.Connection,
) -> dict[str, dict[str, object]]:
    """Return bulk sidebar state for every known session.

    The TUI refresh loop uses this to stamp persisted session status and
    the ADR-021 observation indicator bit in one query, instead of
    issuing row-by-row lookups during the 5-second refresh cycle.
    """
    rows = conn.execute(
        """
        SELECT
            s.session_id,
            s.status,
            CASE WHEN COALESCE(os.entry_count, 0) > 0 THEN 1 ELSE 0 END
                AS has_observations
        FROM sessions s
        LEFT JOIN observation_state os ON os.session_id = s.session_id
        """
    ).fetchall()
    return {
        row["session_id"]: {
            "status": row["status"],
            "has_observations": bool(row["has_observations"]),
        }
        for row in rows
    }


def set_session_status(
    conn: sqlite3.Connection,
    session_id: str,
    status: str,
) -> None:
    """Write a new status value for a session.

    Validates against SESSION_STATUS_ORDER so typos fail loudly before the
    CHECK constraint (task #24) is added in a later migration.
    """
    if status not in SESSION_STATUS_ORDER:
        raise ValueError(
            f"invalid session status {status!r}; "
            f"expected one of {SESSION_STATUS_ORDER}"
        )
    conn.execute(
        "UPDATE sessions SET status = ? WHERE session_id = ?",
        (status, session_id),
    )


# --- One-time config.json → SQLite migration (ADR-001) --------------------

def migrate_config_json_to_sqlite(
    conn: sqlite3.Connection,
    config_path: Path,
    claude_projects_dir: Path,
    *,
    rewrite_config: bool = True,
) -> dict:
    """One-time migration per ADR-001: move session-level state from
    config.json into the `sessions` table.

    Moves:   session_names, session_order, last_viewed.
    Keeps:   theme and any other app-level keys (sidebar_width, …).

    **Idempotent** via the `config_migrated_v1` settings flag — subsequent
    calls return `{"action": "skipped"}` without touching either store.

    **Failure safety:**
    * The DB writes (row inserts + flag-set) all happen inside a single
      transaction. If any INSERT fails, the transaction rolls back and the
      flag is not set, so the next run can retry.
    * The config.json rewrite happens AFTER the DB transaction commits.
      If the rewrite fails, the DB is already authoritative and the flag
      is set; legacy keys may linger in config.json but will be ignored
      by the TUI (which reads session-level state from SQLite now).
    * In either failure mode, the original config.json content is
      preserved — we only overwrite it at the very end of a successful
      migration.

    **Missing sessions:** any session_id referenced in config.json whose
    JSONL file no longer exists on disk is skipped. We can't satisfy the
    `session_path NOT NULL` constraint without a real path, and a
    custom_name for a deleted session is invisible to the user anyway.

    Args:
        conn: Open DB connection with the schema applied.
        config_path: Path to `config.json` (may or may not exist).
        claude_projects_dir: `~/.claude/projects`; scanned to resolve
            session_ids to their JSONL paths.
        rewrite_config: When False, skip the config.json rewrite step.
            Tests use this to inspect the pre-rewrite state; production
            callers leave it True.

    Returns:
        A summary dict:
            {"action": "migrated",  "migrated": [...], "skipped": [...]}
            {"action": "skipped",   "reason": "..."}
            {"action": "partial",   "reason": "...", "migrated": [...], "skipped": [...]}
            {"action": "failed",    "reason": "..."}
    """
    # --- 1. Idempotency guard ---------------------------------------------
    if get_setting(conn, MIGRATION_FLAG):
        # If a prior run set the flag but failed to rewrite config.json
        # (the "partial" outcome), try again here so the file eventually
        # converges. This is purely a cleanup path; the DB is already
        # authoritative.
        if rewrite_config and config_path.exists():
            try:
                raw = json.loads(config_path.read_text())
            except (OSError, json.JSONDecodeError):
                raw = None
            if isinstance(raw, dict) and any(
                k in raw for k in _MIGRATED_CONFIG_KEYS
            ):
                try:
                    _rewrite_config_stripped(config_path, raw)
                except OSError:
                    pass
        return {"action": "skipped", "reason": "already migrated"}

    # --- 2. Nothing to migrate? ------------------------------------------
    # A missing config.json is a normal first-run state. Set the flag so
    # we don't re-read the filesystem on every future launch.
    if not config_path.exists():
        set_setting(conn, MIGRATION_FLAG, True)
        return {"action": "skipped", "reason": "no config.json"}

    # --- 3. Read and validate the existing config ------------------------
    try:
        raw = json.loads(config_path.read_text())
    except (OSError, json.JSONDecodeError) as e:
        # Don't set the flag — a bad file today might be a good file
        # tomorrow (the user may be fixing it manually).
        return {"action": "failed", "reason": f"could not read config.json: {e}"}
    if not isinstance(raw, dict):
        return {"action": "failed", "reason": "config.json is not a JSON object"}

    # Defensive coercion: a malformed legacy config shouldn't crash
    # migration. Missing or wrong-typed keys become empty containers.
    session_names = raw.get("session_names") or {}
    session_order = raw.get("session_order") or []
    last_viewed = raw.get("last_viewed") or {}
    if not isinstance(session_names, dict):
        session_names = {}
    if not isinstance(session_order, list):
        session_order = []
    if not isinstance(last_viewed, dict):
        last_viewed = {}

    all_ids = set(session_names) | set(session_order) | set(last_viewed)

    # Nothing to move into sessions rows, but we still want to drop the
    # (possibly empty) legacy keys from config.json and mark the flag.
    if not all_ids:
        set_setting(conn, MIGRATION_FLAG, True)
        if rewrite_config:
            try:
                _rewrite_config_stripped(config_path, raw)
            except OSError as e:
                return {
                    "action": "partial",
                    "reason": f"DB migrated but could not rewrite config.json: {e}",
                    "migrated": [],
                    "skipped": [],
                }
        return {"action": "migrated", "migrated": [], "skipped": []}

    # --- 4. Resolve session_id → filesystem metadata ---------------------
    # One scan of ~/.claude/projects builds the lookup; sessions whose
    # JSONL is gone stay out of id_meta and get reported as `skipped`.
    id_meta: dict[str, tuple[str, str, float, float]] = {}
    try:
        for proj_dir in claude_projects_dir.iterdir():
            if not proj_dir.is_dir():
                continue
            for jsonl in proj_dir.glob("*.jsonl"):
                sid = jsonl.stem
                if sid in all_ids and sid not in id_meta:
                    try:
                        st = jsonl.stat()
                    except OSError:
                        continue
                    id_meta[sid] = (
                        str(jsonl),
                        proj_dir.name,
                        st.st_ctime,
                        st.st_mtime,
                    )
    except OSError:
        # Projects dir missing entirely — every session will be skipped.
        pass

    # Lower index = higher in the sidebar. Missing IDs get sort_order=NULL.
    order_index = {sid: i for i, sid in enumerate(session_order)}

    migrated: list[str] = []
    skipped: list[str] = []

    # --- 5. DB writes + flag in a single transaction ---------------------
    try:
        with transaction(conn):
            for sid in sorted(all_ids):
                meta = id_meta.get(sid)
                if meta is None:
                    skipped.append(sid)
                    continue
                path, project, ctime, mtime = meta

                name = session_names.get(sid) or None
                sort_order = order_index.get(sid)
                last_viewed_ts = last_viewed.get(sid)
                # Defensive: last_viewed should be numeric; drop garbage.
                if not isinstance(last_viewed_ts, (int, float)):
                    last_viewed_ts = None

                # ON CONFLICT handling covers the (rare) case where a row
                # was pre-populated by a concurrent writer or an earlier
                # failed attempt — we overwrite the user-owned fields to
                # match config.json, since config.json is the source of
                # truth for this one-time import.
                conn.execute(
                    """
                    INSERT INTO sessions (
                        session_id, session_path, project,
                        custom_name, sort_order, last_viewed,
                        created_at, modified_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(session_id) DO UPDATE SET
                        session_path = excluded.session_path,
                        project      = COALESCE(excluded.project, sessions.project),
                        custom_name  = excluded.custom_name,
                        sort_order   = excluded.sort_order,
                        last_viewed  = excluded.last_viewed,
                        created_at   = COALESCE(sessions.created_at, excluded.created_at),
                        modified_at  = COALESCE(excluded.modified_at, sessions.modified_at)
                    """,
                    (
                        sid, path, project,
                        name, sort_order, last_viewed_ts,
                        ctime, mtime,
                    ),
                )
                migrated.append(sid)

            # Flag is part of the transaction so partial rollback can't
            # leave us "migrated according to the flag, but missing rows".
            set_setting(conn, MIGRATION_FLAG, True)
    except Exception as e:
        # Transaction rolled back. config.json is still the pristine
        # original — we haven't touched it yet.
        return {"action": "failed", "reason": str(e)}

    # --- 6. Rewrite config.json (after DB commit) ------------------------
    if rewrite_config:
        try:
            _rewrite_config_stripped(config_path, raw)
        except OSError as e:
            # The DB is already authoritative; leave a breadcrumb so the
            # caller can log it. Next startup will retry the rewrite via
            # the cleanup path at the top of this function.
            return {
                "action": "partial",
                "reason": f"DB migrated but could not rewrite config.json: {e}",
                "migrated": migrated,
                "skipped": skipped,
            }

    return {"action": "migrated", "migrated": migrated, "skipped": skipped}


# --- Observation state helpers (ADR-019, ADR-022) --------------------------

# Directory where per-session observation JSONL files live.
OBS_DIR = DB_DIR / "observations"


def get_observation_state(
    conn: sqlite3.Connection,
    session_id: str,
) -> dict | None:
    """Return the observation state for a session, or None if never observed."""
    return query_one(
        conn,
        "SELECT * FROM observation_state WHERE session_id = ?",
        (session_id,),
    )


def upsert_observation_state(
    conn: sqlite3.Connection,
    session_id: str,
    source_path: str,
    obs_path: str,
    *,
    source_byte_offset: int = 0,
    entry_count: int = 0,
    reflector_entry_offset: int = 0,
    observer_pid: int | None = None,
    status: str = "idle",
    started_at: float | None = None,
    last_observed_at: float | None = None,
) -> None:
    """Insert or update the observation state for a session.

    On conflict, updates all mutable fields. The caller controls which
    fields to advance — typically ``source_byte_offset`` and ``entry_count``
    after an observer run, or ``observer_pid`` and ``status`` on start/stop.
    """
    conn.execute(
        """
        INSERT INTO observation_state (
            session_id, source_path, obs_path,
            source_byte_offset, entry_count, reflector_entry_offset,
            observer_pid, status, started_at, last_observed_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(session_id) DO UPDATE SET
            source_path            = excluded.source_path,
            obs_path               = excluded.obs_path,
            source_byte_offset     = excluded.source_byte_offset,
            entry_count            = excluded.entry_count,
            reflector_entry_offset = excluded.reflector_entry_offset,
            observer_pid           = excluded.observer_pid,
            status                 = excluded.status,
            started_at             = COALESCE(excluded.started_at, observation_state.started_at),
            last_observed_at       = COALESCE(excluded.last_observed_at, observation_state.last_observed_at)
        """,
        (
            session_id, source_path, obs_path,
            source_byte_offset, entry_count, reflector_entry_offset,
            observer_pid, status, started_at, last_observed_at,
        ),
    )


def update_observer_offset(
    conn: sqlite3.Connection,
    session_id: str,
    source_byte_offset: int,
    entry_count: int,
    last_observed_at: float,
) -> None:
    """Advance the observer's position after processing a chunk.

    Called after each observer extraction. Updates the byte offset (where
    to resume reading source JSONL) and entry count (how many observations
    have been written so far).
    """
    conn.execute(
        """
        UPDATE observation_state
        SET source_byte_offset = ?,
            entry_count        = ?,
            last_observed_at   = ?
        WHERE session_id = ?
        """,
        (source_byte_offset, entry_count, last_observed_at, session_id),
    )


def update_reflector_offset(
    conn: sqlite3.Connection,
    session_id: str,
    reflector_entry_offset: int,
    *,
    entry_count: int | None = None,
) -> None:
    """Advance the reflector's position after a comparison pass.

    The reflector processes observation entries (not source bytes), so its
    offset is an entry index, not a byte offset.
    """
    if entry_count is None:
        conn.execute(
            """
            UPDATE observation_state
            SET reflector_entry_offset = ?
            WHERE session_id = ?
            """,
            (reflector_entry_offset, session_id),
        )
        return
    conn.execute(
        """
        UPDATE observation_state
        SET reflector_entry_offset = ?,
            entry_count = ?
        WHERE session_id = ?
        """,
        (reflector_entry_offset, entry_count, session_id),
    )


def set_observer_running(
    conn: sqlite3.Connection,
    session_id: str,
    pid: int,
    started_at: float,
) -> None:
    """Record that the observer is now running for this session."""
    conn.execute(
        """
        UPDATE observation_state
        SET observer_pid = ?, status = 'running', started_at = ?
        WHERE session_id = ?
        """,
        (pid, started_at, session_id),
    )


def set_observer_stopped(
    conn: sqlite3.Connection,
    session_id: str,
) -> None:
    """Record that the observer has stopped (graceful or detected stale)."""
    conn.execute(
        """
        UPDATE observation_state
        SET observer_pid = NULL, status = 'stopped'
        WHERE session_id = ?
        """,
        (session_id,),
    )


def delete_observation_state(
    conn: sqlite3.Connection,
    session_id: str,
) -> int:
    """Remove the observation_state row so the next run starts at offset 0.

    Returns the number of rows deleted (0 if no row existed).
    The on-disk observation JSONL is NOT touched — callers decide
    whether to wipe it.
    """
    cur = conn.execute(
        "DELETE FROM observation_state WHERE session_id = ?",
        (session_id,),
    )
    return cur.rowcount


def get_observed_sessions(
    conn: sqlite3.Connection,
) -> list[dict]:
    """Return all sessions that have observations (entry_count > 0).

    Used by the TUI to show the observation indicator (ADR-021).
    """
    return query_all(
        conn,
        "SELECT session_id, entry_count, status, observer_pid, obs_path "
        "FROM observation_state WHERE entry_count > 0",
    )


def get_running_observers(
    conn: sqlite3.Connection,
) -> list[dict]:
    """Return all sessions with a running observer (for --stop-all)."""
    return query_all(
        conn,
        "SELECT session_id, observer_pid "
        "FROM observation_state WHERE status = 'running' AND observer_pid IS NOT NULL",
    )


def _normalize_conflict_refs(refs: Sequence[str] | None) -> str:
    """Canonicalize a refs pair so review keys match prompt dedup semantics."""
    if not refs:
        return ""
    cleaned = sorted({str(ref).strip() for ref in refs if str(ref).strip()})
    return "\x1f".join(cleaned)


def is_conflict_reviewed(
    conn: sqlite3.Connection,
    session_id: str,
    refs: Sequence[str] | None,
    topic: str | None,
) -> bool:
    """Return True when this conflict has already been marked reviewed."""
    row = query_one(
        conn,
        """
        SELECT 1
        FROM conflict_reviews
        WHERE session_id = ? AND refs_key = ? AND topic = ?
        """,
        (session_id, _normalize_conflict_refs(refs), (topic or "")),
    )
    return row is not None


def mark_conflict_reviewed(
    conn: sqlite3.Connection,
    session_id: str,
    refs: Sequence[str] | None,
    topic: str | None,
    *,
    reviewed_at: float | None = None,
) -> None:
    """Mark a conflict as reviewed.

    Uses INSERT .. ON CONFLICT so repeated reviews simply refresh the
    timestamp without creating duplicates.
    """
    conn.execute(
        """
        INSERT INTO conflict_reviews (
            session_id, refs_key, topic, reviewed_at
        ) VALUES (?, ?, ?, ?)
        ON CONFLICT(session_id, refs_key, topic) DO UPDATE SET
            reviewed_at = excluded.reviewed_at
        """,
        (
            session_id,
            _normalize_conflict_refs(refs),
            (topic or ""),
            reviewed_at if reviewed_at is not None else datetime.now().timestamp(),
        ),
    )


# --- Bookmarks ------------------------------------------------------------
# Users pin messages via `space` in selection mode; the browser modal
# reads back via ``list_bookmarks``. Timestamps are Unix epoch floats to
# match ``sessions.modified_at`` and the rest of the time columns here.
#
# ``tags`` is stored as a JSON-encoded array to match ``models.Bookmark``
# (task #24's lockstep rule). Helpers decode to ``list[str]`` on read so
# callers never see the raw JSON.


def _decode_bookmark_tags(row: dict | None) -> dict | None:
    """Decode ``row['tags']`` from JSON to a list. Mutates ``row`` in place
    and returns it; ``None`` passes through."""
    if row is None:
        return None
    raw = row.get("tags")
    if isinstance(raw, str):
        try:
            decoded = json.loads(raw) if raw else []
        except json.JSONDecodeError:
            decoded = []
        row["tags"] = decoded if isinstance(decoded, list) else []
    elif raw is None:
        row["tags"] = []
    return row


def get_bookmark(
    conn: sqlite3.Connection, message_uuid: str
) -> dict | None:
    """Return the bookmark row for ``message_uuid`` or None."""
    row = query_one(
        conn,
        "SELECT id, message_uuid, label, tags, created_at FROM bookmarks "
        "WHERE message_uuid = ?",
        (message_uuid,),
    )
    return _decode_bookmark_tags(row)


def toggle_bookmark(
    conn: sqlite3.Connection,
    message_uuid: str,
    created_at: float | None = None,
) -> dict | None:
    """Toggle a bookmark on ``message_uuid``.

    Returns the new bookmark dict on create, or ``None`` on delete so
    callers can show the right toast ("Bookmarked" vs "Removed"). New
    rows start with an empty tag list — tag editing is a follow-up to
    this feature, but the column exists so the schema matches
    :class:`models.Bookmark`.
    """
    existing = get_bookmark(conn, message_uuid)
    if existing is not None:
        conn.execute("DELETE FROM bookmarks WHERE id = ?", (existing["id"],))
        return None
    ts = created_at if created_at is not None else datetime.now().timestamp()
    cur = conn.execute(
        "INSERT INTO bookmarks (message_uuid, label, tags, created_at) "
        "VALUES (?, NULL, '[]', ?)",
        (message_uuid, ts),
    )
    return {
        "id": cur.lastrowid,
        "message_uuid": message_uuid,
        "label": None,
        "tags": [],
        "created_at": ts,
    }


def set_bookmark_label(
    conn: sqlite3.Connection,
    bookmark_id: int,
    label: str | None,
) -> None:
    """Update a bookmark's label. Empty strings collapse to NULL."""
    clean = label.strip() if label else ""
    conn.execute(
        "UPDATE bookmarks SET label = ? WHERE id = ?",
        (clean or None, bookmark_id),
    )


def delete_bookmark(conn: sqlite3.Connection, bookmark_id: int) -> None:
    """Remove a bookmark by row id. No-op if the row doesn't exist."""
    conn.execute("DELETE FROM bookmarks WHERE id = ?", (bookmark_id,))


def list_bookmarks(
    conn: sqlite3.Connection,
    query: str | None = None,
    limit: int = 500,
) -> list[dict]:
    """Return bookmarks joined with message + session metadata, newest first.

    ``query`` is an optional case-insensitive substring filter applied
    across the label, the message body, and the session's project /
    custom name — mirrors the single filter input in the browser modal.
    """
    sql = (
        "SELECT b.id, b.message_uuid, b.label, b.tags, b.created_at, "
        "       m.session_id, m.role, m.text, m.timestamp, "
        "       s.custom_name, s.project "
        "FROM bookmarks b "
        "JOIN messages m ON m.uuid = b.message_uuid "
        "LEFT JOIN sessions s ON s.session_id = m.session_id "
    )
    params: tuple = ()
    if query:
        sql += (
            "WHERE b.label LIKE ? OR m.text LIKE ? "
            "OR s.custom_name LIKE ? OR s.project LIKE ? "
        )
        like = f"%{query}%"
        params = (like, like, like, like)
    sql += "ORDER BY b.created_at DESC LIMIT ?"
    params = params + (limit,)
    rows = query_all(conn, sql, params)
    for row in rows:
        _decode_bookmark_tags(row)
    return rows


def get_bookmarked_uuids(
    conn: sqlite3.Connection,
    session_id: str | None = None,
) -> set[str]:
    """Return the set of bookmarked message UUIDs, optionally scoped
    to one session. Used by the transcript view to tag pinned widgets
    when rendering."""
    if session_id is not None:
        rows = conn.execute(
            "SELECT b.message_uuid FROM bookmarks b "
            "JOIN messages m ON m.uuid = b.message_uuid "
            "WHERE m.session_id = ?",
            (session_id,),
        ).fetchall()
    else:
        rows = conn.execute("SELECT message_uuid FROM bookmarks").fetchall()
    return {r[0] for r in rows}
