//! SQLite connection wrapper, schema-version handshake, and typed read/write
//! helpers.
//!
//! This module is the data access layer for the Rust TUI port. The Python
//! side owns the schema and runs migrations — Rust only opens an
//! already-migrated DB, verifies the `PRAGMA user_version`, and uses a narrow
//! set of read / write helpers.
//!
//! Implements Tasks 1.6, 1.7, and 1.8 of the Rust TUI port plan.
//!
//! ## Write surface
//!
//! Rust writes to exactly two tables:
//!   - `bookmarks` — INSERT / DELETE via [`toggle_bookmark`], [`upsert_bookmark`],
//!     [`delete_bookmark`].
//!   - `sessions` — UPDATE the `status`, `custom_name`, and `last_viewed`
//!     columns via [`set_session_status`], [`set_custom_name`],
//!     [`set_last_viewed`].
//!
//! Everything else (`observations`, `conflict_reviews`, schema migrations) is
//! Python-owned. Helpers in this module never touch those tables.
//!
//! ## Busy retry
//!
//! Individual helpers do NOT wrap their writes in [`with_busy_retry`]. Callers
//! that orchestrate writes (TUI worker dispatch, etc.) pick the retry policy
//! and wrap as needed — see the rustdoc on [`with_busy_retry`].

use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::time::Duration;

use crate::error::DbError;
use crate::models::{Bookmark, BookmarkKind, Message, MessageRole, Session, SessionStatus};
use crate::EXPECTED_SCHEMA_VERSION;

// --- SQL <-> model helpers -------------------------------------------------
// `models.rs` is the source of truth for the enum variants; the SQL CHECK
// values are spelled in lockstep here. Keeping the (de)serialization local
// to `db.rs` means `models.rs` doesn't need to grow rusqlite-aware impls.

fn session_status_from_sql(raw: &str) -> SessionStatus {
    match raw {
        "in_progress" => SessionStatus::InProgress,
        "in_review" => SessionStatus::InReview,
        "done" => SessionStatus::Done,
        "archived" => SessionStatus::Archived,
        // Mirrors migration 006's legacy normalization (unknown -> 'active').
        _ => SessionStatus::Active,
    }
}

fn session_status_to_sql(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Active => "active",
        SessionStatus::InProgress => "in_progress",
        SessionStatus::InReview => "in_review",
        SessionStatus::Done => "done",
        SessionStatus::Archived => "archived",
    }
}

fn message_role_from_sql(raw: &str) -> MessageRole {
    match raw {
        "assistant" => MessageRole::Assistant,
        _ => MessageRole::User,
    }
}

fn bookmark_kind_from_sql(raw: &str) -> BookmarkKind {
    match raw {
        "research" => BookmarkKind::Research,
        _ => BookmarkKind::Bookmark,
    }
}

fn bookmark_kind_to_sql(kind: BookmarkKind) -> &'static str {
    match kind {
        BookmarkKind::Bookmark => "bookmark",
        BookmarkKind::Research => "research",
    }
}

/// Bulk sidebar metadata for one session — what the TUI refresh loop needs in
/// one row. Mirrors Python's `get_session_sidebar_metadata`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarMeta {
    pub session_id: String,
    pub status: SessionStatus,
    pub has_observations: bool,
}

// --- Task 1.6: connection + schema check + busy retry ----------------------

/// Result of the schema-version handshake.
///
/// `Match` is the only happy path for writes; on `Older` or `Newer` the TUI
/// degrades to read-only and shows a banner. The variants carry the on-disk
/// version so the banner can render specifics. The `fts` and `observations`
/// lanes propagate [`DbError::SchemaMismatch`] through their own error enums
/// when they use [`open_strict`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaCheck {
    Match,
    Older(u32),
    Newer(u32),
}

/// Open the SQLite DB at `path`. Creates the parent directory if missing.
///
/// PRAGMAs applied (matches Python's `connect()` exactly):
///   - `journal_mode = WAL`        — concurrent readers + one writer (ADR-001).
///   - `synchronous = NORMAL`      — durability level recommended for WAL.
///   - `foreign_keys = ON`         — FK enforcement (Python toggles too).
///   - `recursive_triggers = ON`   — required for FTS shadow tables.
///
/// `busy_timeout` is set to 5000 ms so casual contention with the Python
/// writer doesn't bubble straight up as `SQLITE_BUSY`.
pub fn open(path: &Path) -> Result<Connection, DbError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;\
         PRAGMA synchronous = NORMAL;\
         PRAGMA foreign_keys = ON;\
         PRAGMA recursive_triggers = ON;",
    )?;
    Ok(conn)
}

/// Compare the DB's `PRAGMA user_version` against
/// [`EXPECTED_SCHEMA_VERSION`].
///
/// Returns the typed [`SchemaCheck`] enum. The banner UX (and the
/// "run ./threadhop to migrate" exit path) live in the TUI; this helper only
/// reports the diff.
pub fn check_schema(conn: &Connection) -> Result<SchemaCheck, DbError> {
    let v: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    Ok(match v.cmp(&EXPECTED_SCHEMA_VERSION) {
        std::cmp::Ordering::Equal => SchemaCheck::Match,
        std::cmp::Ordering::Less => SchemaCheck::Older(v),
        std::cmp::Ordering::Greater => SchemaCheck::Newer(v),
    })
}

/// Convenience: open + check in one call. Returns `(conn, SchemaCheck)` so
/// callers can decide whether to enter read-only mode or exit non-zero.
pub fn open_and_check(path: &Path) -> Result<(Connection, SchemaCheck), DbError> {
    let conn = open(path)?;
    let check = check_schema(&conn)?;
    Ok((conn, check))
}

/// Open the DB and require the schema version match. Returns
/// [`DbError::SchemaMismatch`] on any drift.
///
/// The `fts` and `observations` lanes can propagate this typed error through
/// `#[from]` conversions when a hard fail is preferable to the read-only
/// banner path.
pub fn open_strict(path: &Path) -> Result<Connection, DbError> {
    let conn = open(path)?;
    match check_schema(&conn)? {
        SchemaCheck::Match => Ok(conn),
        SchemaCheck::Older(v) | SchemaCheck::Newer(v) => Err(DbError::SchemaMismatch {
            db: v,
            expected: EXPECTED_SCHEMA_VERSION,
        }),
    }
}

/// Retry a SQLite operation once on `SQLITE_BUSY`. Callers that want a more
/// aggressive policy (exponential backoff, etc.) should build it on top.
///
/// Helpers in this module deliberately do NOT call this themselves — they
/// stay single-statement and let the caller pick the policy.
pub fn with_busy_retry<T, F>(mut f: F) -> Result<T, DbError>
where
    F: FnMut() -> rusqlite::Result<T>,
{
    match f() {
        Ok(v) => Ok(v),
        Err(rusqlite::Error::SqliteFailure(e, _))
            if e.code == rusqlite::ErrorCode::DatabaseBusy =>
        {
            std::thread::sleep(Duration::from_millis(50));
            Ok(f()?)
        }
        Err(e) => Err(DbError::Sqlite(e)),
    }
}

// --- Task 1.7: read helpers ------------------------------------------------

const SESSION_COLUMNS: &str = "session_id, session_path, project, cwd, custom_name, \
                                status, sort_order, last_viewed, created_at, modified_at";

fn map_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let status_raw: String = row.get(5)?;
    Ok(Session {
        session_id: row.get(0)?,
        session_path: row.get(1)?,
        project: row.get(2)?,
        cwd: row.get(3)?,
        custom_name: row.get(4)?,
        status: session_status_from_sql(&status_raw),
        sort_order: row.get(6)?,
        last_viewed: row.get(7)?,
        created_at: row.get(8)?,
        modified_at: row.get(9)?,
    })
}

/// All sessions, ordered by `modified_at` DESC (most recently touched first).
/// Sessions with NULL `modified_at` sort to the end.
pub fn list_sessions(conn: &Connection) -> Result<Vec<Session>, DbError> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM sessions \
         ORDER BY COALESCE(modified_at, 0) DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], map_session)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Look up one session row by id. Returns `None` if no row matches.
pub fn session_by_id(conn: &Connection, session_id: &str) -> Result<Option<Session>, DbError> {
    let sql = format!("SELECT {SESSION_COLUMNS} FROM sessions WHERE session_id = ?");
    let row = conn
        .query_row(&sql, params![session_id], map_session)
        .optional()?;
    Ok(row)
}

/// Bulk sidebar state — `(session_id, status, has_observations)` for every
/// known session. One query for the TUI refresh loop instead of N lookups.
pub fn session_sidebar_metadata(conn: &Connection) -> Result<Vec<SidebarMeta>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT s.session_id, s.status,
                CASE WHEN COALESCE(os.entry_count, 0) > 0 THEN 1 ELSE 0 END
         FROM sessions s
         LEFT JOIN observation_state os ON os.session_id = s.session_id",
    )?;
    let rows = stmt.query_map([], |r| {
        let status_raw: String = r.get(1)?;
        let has_obs: i64 = r.get(2)?;
        Ok(SidebarMeta {
            session_id: r.get(0)?,
            status: session_status_from_sql(&status_raw),
            has_observations: has_obs != 0,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

const MESSAGE_COLUMNS: &str = "uuid, session_id, role, text, timestamp, cwd, parent_uuid, \
                                is_sidechain, message_id";

fn map_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let role_raw: String = row.get(2)?;
    let is_sidechain: i64 = row.get(7)?;
    Ok(Message {
        uuid: row.get(0)?,
        session_id: row.get(1)?,
        role: message_role_from_sql(&role_raw),
        text: row.get(3)?,
        timestamp: row.get(4)?,
        cwd: row.get(5)?,
        parent_uuid: row.get(6)?,
        is_sidechain: is_sidechain != 0,
        message_id: row.get(8)?,
    })
}

/// Messages for one session ordered by rowid (append order — matches the
/// transcript view). FTS-style queries live in the `fts` module.
pub fn messages_for_session(conn: &Connection, session_id: &str) -> Result<Vec<Message>, DbError> {
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS} FROM messages \
         WHERE session_id = ? ORDER BY rowid"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![session_id], map_message)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn map_bookmark(row: &rusqlite::Row<'_>) -> rusqlite::Result<Bookmark> {
    let tags_raw: String = row.get(4)?;
    let tags: Vec<String> = serde_json::from_str(&tags_raw).unwrap_or_default();
    let kind_raw: String = row.get(3)?;
    Ok(Bookmark {
        id: Some(row.get(0)?),
        message_uuid: row.get(1)?,
        note: row.get(2)?,
        kind: bookmark_kind_from_sql(&kind_raw),
        tags,
        created_at: row.get(5)?,
    })
}

/// All bookmarks tied to messages in `session_id`, newest first.
pub fn bookmarks_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<Bookmark>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT b.id, b.message_uuid, b.note, b.kind, b.tags, b.created_at
         FROM bookmarks b
         JOIN messages m ON m.uuid = b.message_uuid
         WHERE m.session_id = ?
         ORDER BY b.created_at DESC",
    )?;
    let rows = stmt.query_map(params![session_id], map_bookmark)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Just the bookmarked message UUIDs for one session — used by the transcript
/// view to render the pin gutter. Cheaper than [`bookmarks_for_session`] when
/// the caller doesn't need notes or tags.
pub fn bookmark_uuids_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<String>, DbError> {
    let mut stmt = conn.prepare(
        "SELECT b.message_uuid FROM bookmarks b
         JOIN messages m ON m.uuid = b.message_uuid
         WHERE m.session_id = ?",
    )?;
    let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Look up an app-level setting from the `settings` table. Returns the raw
/// JSON-decoded value as a `serde_json::Value` — callers cast to the shape
/// they expect. Mirrors Python's `get_setting`.
pub fn get_setting(
    conn: &Connection,
    key: &str,
) -> Result<Option<serde_json::Value>, DbError> {
    let row: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?",
            params![key],
            |r| r.get(0),
        )
        .optional()?;
    match row {
        None => Ok(None),
        Some(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) => Ok(Some(v)),
            // Tolerate legacy raw-string values (parity with Python).
            Err(_) => Ok(Some(serde_json::Value::String(raw))),
        },
    }
}

// --- Task 1.8: write helpers ----------------------------------------------

/// Upsert a bookmark on `message_uuid` with an explicit kind / note. Returns
/// the row after the write. Mirrors Python's `upsert_bookmark` minus the
/// `_BOOKMARK_NOTE_UNSET` sentinel — Rust callers pass `Option<&str>`
/// explicitly.
pub fn upsert_bookmark(
    conn: &Connection,
    message_uuid: &str,
    kind: BookmarkKind,
    note: Option<&str>,
    created_at: f64,
) -> Result<Bookmark, DbError> {
    let clean_note = clean_text(note);

    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM bookmarks WHERE message_uuid = ?",
            params![message_uuid],
            |r| r.get(0),
        )
        .optional()?;

    if let Some(id) = existing {
        conn.execute(
            "UPDATE bookmarks SET note = ?, kind = ?, created_at = ? WHERE id = ?",
            params![clean_note, bookmark_kind_to_sql(kind), created_at, id],
        )?;
        Ok(Bookmark {
            id: Some(id),
            message_uuid: message_uuid.to_string(),
            note: clean_note,
            kind,
            tags: vec![],
            created_at,
        })
    } else {
        conn.execute(
            "INSERT INTO bookmarks (message_uuid, note, kind, tags, created_at) \
             VALUES (?, ?, ?, '[]', ?)",
            params![message_uuid, clean_note, bookmark_kind_to_sql(kind), created_at],
        )?;
        Ok(Bookmark {
            id: Some(conn.last_insert_rowid()),
            message_uuid: message_uuid.to_string(),
            note: clean_note,
            kind,
            tags: vec![],
            created_at,
        })
    }
}

/// Delete a bookmark by its rowid. No-op if the row does not exist.
pub fn delete_bookmark(conn: &Connection, bookmark_id: i64) -> Result<(), DbError> {
    conn.execute("DELETE FROM bookmarks WHERE id = ?", params![bookmark_id])?;
    Ok(())
}

/// Toggle the bookmark on `message_uuid`. Returns `Some(bookmark)` on create
/// and `None` on delete, matching Python's `toggle_bookmark` ergonomics so
/// the TUI can show the right toast.
pub fn toggle_bookmark(
    conn: &Connection,
    message_uuid: &str,
    now: f64,
) -> Result<Option<Bookmark>, DbError> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM bookmarks WHERE message_uuid = ?",
            params![message_uuid],
            |r| r.get(0),
        )
        .optional()?;

    if let Some(id) = existing {
        conn.execute("DELETE FROM bookmarks WHERE id = ?", params![id])?;
        return Ok(None);
    }

    conn.execute(
        "INSERT INTO bookmarks (message_uuid, note, kind, tags, created_at) \
         VALUES (?, NULL, 'bookmark', '[]', ?)",
        params![message_uuid, now],
    )?;
    Ok(Some(Bookmark {
        id: Some(conn.last_insert_rowid()),
        message_uuid: message_uuid.to_string(),
        note: None,
        kind: BookmarkKind::Bookmark,
        tags: vec![],
        created_at: now,
    }))
}

/// Validate `status` against the enum and UPDATE the session row. Validation
/// runs in Rust so we surface a typed error before the SQL CHECK fires.
///
/// Unknown values produce `DbError::Sqlite(InvalidParameterName(...))` —
/// `DbError` doesn't currently have a dedicated "validation" variant, and
/// the plan calls out this mapping (Task 1.8). Callers that need to
/// distinguish can match the inner `rusqlite::Error`.
pub fn set_session_status(
    conn: &Connection,
    session_id: &str,
    status: &str,
) -> Result<(), DbError> {
    if !matches!(
        status,
        "active" | "in_progress" | "in_review" | "done" | "archived"
    ) {
        return Err(DbError::Sqlite(rusqlite::Error::InvalidParameterName(
            format!("invalid session status: {status}"),
        )));
    }
    conn.execute(
        "UPDATE sessions SET status = ? WHERE session_id = ?",
        params![status, session_id],
    )?;
    Ok(())
}

/// Type-safe variant of [`set_session_status`].
pub fn set_session_status_typed(
    conn: &Connection,
    session_id: &str,
    status: SessionStatus,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE sessions SET status = ? WHERE session_id = ?",
        params![session_status_to_sql(status), session_id],
    )?;
    Ok(())
}

/// Set the user-chosen display name. `None` or whitespace-only strings clear
/// the column (parity with Python's `set_custom_name`).
pub fn set_custom_name(
    conn: &Connection,
    session_id: &str,
    name: Option<&str>,
) -> Result<(), DbError> {
    let cleaned = clean_text(name);
    conn.execute(
        "UPDATE sessions SET custom_name = ? WHERE session_id = ?",
        params![cleaned, session_id],
    )?;
    Ok(())
}

/// Record the last time the user opened this session (unread marking).
pub fn set_last_viewed(
    conn: &Connection,
    session_id: &str,
    timestamp: f64,
) -> Result<(), DbError> {
    conn.execute(
        "UPDATE sessions SET last_viewed = ? WHERE session_id = ?",
        params![timestamp, session_id],
    )?;
    Ok(())
}

/// Trim a free-text input; whitespace-only or empty inputs collapse to `None`.
fn clean_text(raw: Option<&str>) -> Option<String> {
    raw.and_then(|n| {
        let t = n.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

// --- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Build the schema-9 shape needed by the read/write tests. This covers
    /// just the subset Rust touches — `sessions`, `messages`, `bookmarks`,
    /// `observation_state`, `settings`. FTS shadow tables and migration
    /// bookkeeping aren't needed here; this exercises the helpers above.
    fn build_schema_9(conn: &Connection) {
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE settings (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
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
             );
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
             );
             CREATE TABLE bookmarks (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 message_uuid TEXT NOT NULL UNIQUE,
                 note         TEXT,
                 kind         TEXT NOT NULL DEFAULT 'bookmark'
                     CHECK (kind IN ('bookmark', 'research')),
                 tags         TEXT NOT NULL DEFAULT '[]',
                 created_at   REAL NOT NULL,
                 FOREIGN KEY (message_uuid) REFERENCES messages(uuid) ON DELETE CASCADE
             );
             CREATE TABLE observation_state (
                 session_id              TEXT PRIMARY KEY,
                 source_path             TEXT NOT NULL,
                 obs_path                TEXT NOT NULL,
                 source_byte_offset      INTEGER NOT NULL DEFAULT 0,
                 entry_count             INTEGER NOT NULL DEFAULT 0,
                 reflector_entry_offset  INTEGER NOT NULL DEFAULT 0,
                 observer_pid            INTEGER,
                 status                  TEXT NOT NULL DEFAULT 'idle',
                 started_at              REAL,
                 last_observed_at        REAL,
                 FOREIGN KEY (session_id) REFERENCES sessions(session_id)
             );
             PRAGMA user_version = 9;",
        )
        .unwrap();
    }

    fn seed_session(conn: &Connection, sid: &str) {
        conn.execute(
            "INSERT INTO sessions (session_id, session_path, status, modified_at) \
             VALUES (?, ?, 'active', ?)",
            params![sid, format!("/tmp/{sid}.jsonl"), 1000.0_f64],
        )
        .unwrap();
    }

    fn seed_message(conn: &Connection, uuid: &str, sid: &str) {
        conn.execute(
            "INSERT INTO messages (uuid, session_id, role, text) VALUES (?, ?, 'user', 'hi')",
            params![uuid, sid],
        )
        .unwrap();
    }

    // --- Task 1.6 tests ----------------------------------------------------

    fn fresh_in_memory(version: u32) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {version}"))
            .unwrap();
        conn
    }

    #[test]
    fn schema_check_match() {
        let conn = fresh_in_memory(EXPECTED_SCHEMA_VERSION);
        assert!(matches!(check_schema(&conn).unwrap(), SchemaCheck::Match));
    }

    #[test]
    fn schema_check_older() {
        let conn = fresh_in_memory(EXPECTED_SCHEMA_VERSION - 1);
        let want = EXPECTED_SCHEMA_VERSION - 1;
        assert!(matches!(
            check_schema(&conn).unwrap(),
            SchemaCheck::Older(v) if v == want
        ));
    }

    #[test]
    fn schema_check_newer() {
        let conn = fresh_in_memory(EXPECTED_SCHEMA_VERSION + 1);
        let want = EXPECTED_SCHEMA_VERSION + 1;
        assert!(matches!(
            check_schema(&conn).unwrap(),
            SchemaCheck::Newer(v) if v == want
        ));
    }

    #[test]
    fn open_sets_wal_mode() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.db");
        let conn = open(&path).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn open_sets_foreign_keys_and_recursive_triggers() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.db");
        let conn = open(&path).unwrap();
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
        let rt: i64 = conn
            .query_row("PRAGMA recursive_triggers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rt, 1);
    }

    #[test]
    fn open_strict_errors_on_mismatch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("t.db");
        let conn = open(&path).unwrap();
        // Force an older version on the freshly-opened DB.
        conn.execute_batch("PRAGMA user_version = 1").unwrap();
        drop(conn);
        let err = open_strict(&path).unwrap_err();
        assert!(matches!(err, DbError::SchemaMismatch { db: 1, expected: 9 }));
    }

    #[test]
    fn busy_retry_passes_through_ok() {
        let result: Result<i32, _> = with_busy_retry(|| Ok(42));
        assert_eq!(result.unwrap(), 42);
    }

    // --- Task 1.7 tests ----------------------------------------------------

    #[test]
    fn list_sessions_returns_seeded_row() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        let rows = list_sessions(&c).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, "s1");
        assert!(matches!(rows[0].status, SessionStatus::Active));
    }

    #[test]
    fn list_sessions_orders_by_modified_at_desc() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        c.execute(
            "INSERT INTO sessions (session_id, session_path, modified_at) \
             VALUES ('old','/o',100.0)",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO sessions (session_id, session_path, modified_at) \
             VALUES ('new','/n',200.0)",
            [],
        )
        .unwrap();
        let rows = list_sessions(&c).unwrap();
        assert_eq!(rows[0].session_id, "new");
        assert_eq!(rows[1].session_id, "old");
    }

    #[test]
    fn session_by_id_returns_some_then_none() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        assert!(session_by_id(&c, "s1").unwrap().is_some());
        assert!(session_by_id(&c, "missing").unwrap().is_none());
    }

    #[test]
    fn session_sidebar_metadata_marks_observed_sessions() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        seed_session(&c, "s2");
        c.execute(
            "INSERT INTO observation_state (session_id, source_path, obs_path, entry_count) \
             VALUES ('s1','/src','/obs', 3)",
            [],
        )
        .unwrap();
        let mut meta = session_sidebar_metadata(&c).unwrap();
        meta.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        assert_eq!(meta.len(), 2);
        assert_eq!(meta[0].session_id, "s1");
        assert!(meta[0].has_observations);
        assert_eq!(meta[1].session_id, "s2");
        assert!(!meta[1].has_observations);
    }

    #[test]
    fn messages_for_session_orders_by_rowid() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        seed_message(&c, "u1", "s1");
        seed_message(&c, "u2", "s1");
        let rows = messages_for_session(&c, "s1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].uuid, "u1");
        assert_eq!(rows[1].uuid, "u2");
    }

    #[test]
    fn bookmarks_and_uuids_for_session_scope_correctly() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        seed_session(&c, "s2");
        seed_message(&c, "u1", "s1");
        seed_message(&c, "u2", "s2");
        toggle_bookmark(&c, "u1", 100.0).unwrap();
        toggle_bookmark(&c, "u2", 200.0).unwrap();

        let bms = bookmarks_for_session(&c, "s1").unwrap();
        assert_eq!(bms.len(), 1);
        assert_eq!(bms[0].message_uuid, "u1");

        let uuids = bookmark_uuids_for_session(&c, "s2").unwrap();
        assert_eq!(uuids, vec!["u2".to_string()]);
    }

    #[test]
    fn get_setting_returns_json_value() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('theme', '\"dark\"')",
            [],
        )
        .unwrap();
        let v = get_setting(&c, "theme").unwrap().unwrap();
        assert_eq!(v.as_str(), Some("dark"));
        assert!(get_setting(&c, "missing").unwrap().is_none());
    }

    #[test]
    fn get_setting_tolerates_legacy_raw_string() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        // Legacy: not JSON-encoded.
        c.execute(
            "INSERT INTO settings (key, value) VALUES ('theme', 'dark')",
            [],
        )
        .unwrap();
        let v = get_setting(&c, "theme").unwrap().unwrap();
        assert_eq!(v.as_str(), Some("dark"));
    }

    // --- Task 1.8 tests ----------------------------------------------------

    #[test]
    fn toggle_bookmark_creates_then_deletes() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        seed_message(&c, "u1", "s1");

        let created = toggle_bookmark(&c, "u1", 1234.0).unwrap();
        assert!(created.is_some());
        let bm = created.unwrap();
        assert!(matches!(bm.kind, BookmarkKind::Bookmark));
        assert_eq!(bm.created_at, 1234.0);

        let removed = toggle_bookmark(&c, "u1", 5678.0).unwrap();
        assert!(removed.is_none());

        // Row count is now zero.
        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM bookmarks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn upsert_bookmark_creates_and_updates_in_place() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        seed_message(&c, "u1", "s1");

        let first =
            upsert_bookmark(&c, "u1", BookmarkKind::Bookmark, Some("first note"), 1000.0)
                .unwrap();
        assert_eq!(first.note.as_deref(), Some("first note"));

        let second =
            upsert_bookmark(&c, "u1", BookmarkKind::Research, Some("second"), 2000.0).unwrap();
        assert_eq!(second.id, first.id);
        assert!(matches!(second.kind, BookmarkKind::Research));
        assert_eq!(second.note.as_deref(), Some("second"));
        assert_eq!(second.created_at, 2000.0);

        let count: i64 = c
            .query_row("SELECT COUNT(*) FROM bookmarks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn upsert_bookmark_trims_and_nulls_blank_notes() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        seed_message(&c, "u1", "s1");

        let bm =
            upsert_bookmark(&c, "u1", BookmarkKind::Bookmark, Some("   "), 100.0).unwrap();
        assert!(bm.note.is_none());

        let bm =
            upsert_bookmark(&c, "u1", BookmarkKind::Bookmark, Some("  ok  "), 200.0).unwrap();
        assert_eq!(bm.note.as_deref(), Some("ok"));
    }

    #[test]
    fn delete_bookmark_is_noop_for_missing_id() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        delete_bookmark(&c, 9999).unwrap();
    }

    #[test]
    fn set_session_status_rejects_unknown() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        let err = set_session_status(&c, "s1", "backlog").unwrap_err();
        // Validation surfaces as DbError::Sqlite(InvalidParameterName(_)) —
        // see the rustdoc on set_session_status for rationale.
        match err {
            DbError::Sqlite(rusqlite::Error::InvalidParameterName(msg)) => {
                assert!(msg.contains("backlog"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn set_session_status_updates_row() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        set_session_status(&c, "s1", "in_progress").unwrap();
        let session = session_by_id(&c, "s1").unwrap().unwrap();
        assert!(matches!(session.status, SessionStatus::InProgress));
    }

    #[test]
    fn set_session_status_typed_round_trips() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        set_session_status_typed(&c, "s1", SessionStatus::Done).unwrap();
        let s = session_by_id(&c, "s1").unwrap().unwrap();
        assert!(matches!(s.status, SessionStatus::Done));
    }

    #[test]
    fn set_custom_name_clears_on_empty_string() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        set_custom_name(&c, "s1", Some("My Session")).unwrap();
        let s = session_by_id(&c, "s1").unwrap().unwrap();
        assert_eq!(s.custom_name.as_deref(), Some("My Session"));

        set_custom_name(&c, "s1", Some("   ")).unwrap();
        let s = session_by_id(&c, "s1").unwrap().unwrap();
        assert!(s.custom_name.is_none());

        set_custom_name(&c, "s1", Some("Renamed")).unwrap();
        set_custom_name(&c, "s1", None).unwrap();
        let s = session_by_id(&c, "s1").unwrap().unwrap();
        assert!(s.custom_name.is_none());
    }

    #[test]
    fn set_last_viewed_writes_timestamp() {
        let c = Connection::open_in_memory().unwrap();
        build_schema_9(&c);
        seed_session(&c, "s1");
        set_last_viewed(&c, "s1", 1700000000.0).unwrap();
        let s = session_by_id(&c, "s1").unwrap().unwrap();
        assert_eq!(s.last_viewed, Some(1700000000.0));
    }
}
