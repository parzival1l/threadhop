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
"""

from __future__ import annotations

import json
import sqlite3
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Iterator, Sequence

# --- Paths ---
DB_DIR = Path.home() / ".config" / "threadhop"
DB_PATH = DB_DIR / "sessions.db"

# --- Schema version ---
# Each migration N in MIGRATIONS moves the DB from version N to N+1.
# The DB's current version lives in PRAGMA user_version.
SCHEMA_VERSION = 1


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


# Ordered list; MIGRATIONS[i] moves schema from version i to i+1.
MIGRATIONS: list = [
    _migration_001_initial,
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
