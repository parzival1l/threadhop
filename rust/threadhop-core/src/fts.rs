//! FTS5 query builder — prefix search with project/role filters.
//!
//! This module is the Phase 7 search seam. The public contract is:
//!
//! * [`Hit`] — the canonical result row carrying `message_uuid`, `session_id`,
//!   `snippet`, and `score`. Future `search_semantic` and `search_hybrid`
//!   composers will return the same shape.
//! * [`search`] — the Phase 7 entrypoint: takes the raw query string, parses
//!   `project:foo` / `user:` / `assistant:` modifiers via [`parse_query`], and
//!   runs a prefix FTS5 search against `messages_fts` (migration 002).
//! * [`Filters`] — the structured form of the parsed modifiers, accepted by
//!   [`prefix_search`] when callers already split the query themselves.
//!
//! Query parsing: every whitespace-separated token of the remainder is wrapped
//! as `"tok"*` so FTS5 treats it as a prefix term. Double quotes inside tokens
//! are escaped per FTS5 syntax. Empty queries short-circuit to `Ok(vec![])`.
//!
//! Filters compose with `AND` against the joined `messages` / `sessions` rows.
//! Results are ordered by FTS5 `bm25` (ascending — lower is a better match)
//! then by `messages.timestamp` descending, capped at 200 rows.
//!
//! `Hit::score` is the raw `bm25(messages_fts)` value. Snippets use
//! `snippet(messages_fts, 0, '[', ']', '...', 16)` — 16 tokens of context per
//! match (~60 chars on either side at typical token length).

use rusqlite::{Connection, ToSql};

use crate::error::FtsError;
use crate::models::MessageRole;

/// Map a [`MessageRole`] to the on-disk string value `messages.role` stores.
///
/// Mirrors the wire form produced by `#[serde(rename_all = "snake_case")]` on
/// `MessageRole` — kept as a tiny helper so the SQL binding stays a `&'static
/// str` rather than allocating via `serde_json::to_string`.
fn role_as_sql_str(role: MessageRole) -> &'static str {
    match role {
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
    }
}

/// Structured filter form, produced by [`parse_query`] from a raw search
/// string and consumed by [`prefix_search`].
#[derive(Default, Debug, Clone)]
pub struct Filters {
    pub project: Option<String>,
    pub role: Option<MessageRole>,
}

/// Canonical Phase 7 search hit.
///
/// Future composers (`search_semantic`, `search_hybrid`) return the same
/// type, so callers code against this shape rather than against FTS-internal
/// types like rowid. `message_uuid` is the join key everywhere in the app.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub message_uuid: String,
    pub session_id: String,
    pub snippet: String,
    pub score: f64,
}

/// Parse a raw query string into structured filters and a remainder.
///
/// Recognised modifiers (case-sensitive, per ThreadHop convention):
/// * `project:<name>` — narrow to one project.
/// * `user:` — only user messages.
/// * `assistant:` — only assistant messages.
///
/// The last `project:` / role modifier wins if a user types more than one.
/// All other whitespace-separated tokens are returned, joined by single
/// spaces, as the query remainder.
pub fn parse_query(raw: &str) -> (Filters, String) {
    let mut filters = Filters::default();
    let mut remainder: Vec<&str> = Vec::new();
    for tok in raw.split_whitespace() {
        if let Some(p) = tok.strip_prefix("project:") {
            if !p.is_empty() {
                filters.project = Some(p.to_string());
            }
        } else if tok == "user:" {
            filters.role = Some(MessageRole::User);
        } else if tok == "assistant:" {
            filters.role = Some(MessageRole::Assistant);
        } else {
            remainder.push(tok);
        }
    }
    (filters, remainder.join(" "))
}

/// Phase 7 search entrypoint.
///
/// Parses `raw` for `project:` / `user:` / `assistant:` modifiers, then runs
/// a prefix FTS5 search against `messages_fts`. Returns at most 200 hits,
/// ordered by relevance (bm25 ascending) then recency (timestamp descending).
///
/// An empty or all-modifier query returns `Ok(vec![])` without touching the DB.
pub fn search(conn: &Connection, raw: &str) -> Result<Vec<Hit>, FtsError> {
    let (filters, remainder) = parse_query(raw);
    prefix_search(conn, &remainder, &filters)
}

/// Run a prefix FTS5 search with already-parsed filters.
///
/// Used by [`search`] and exposed for callers (tests, future composers) that
/// already hold a structured [`Filters`]. The empty-query short-circuit
/// matches [`search`].
pub fn prefix_search(
    conn: &Connection,
    query: &str,
    filters: &Filters,
) -> Result<Vec<Hit>, FtsError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let fts_q = build_fts_prefix_query(trimmed);

    let mut sql = String::from(
        "SELECT m.uuid, m.session_id, \
                snippet(messages_fts, 0, '[', ']', '...', 16) AS snip, \
                bm25(messages_fts) AS score \
         FROM messages_fts \
         JOIN messages m ON m.rowid = messages_fts.rowid \
         LEFT JOIN sessions s ON s.session_id = m.session_id \
         WHERE messages_fts MATCH ?",
    );
    let mut params: Vec<Box<dyn ToSql>> = vec![Box::new(fts_q)];
    if let Some(p) = &filters.project {
        sql.push_str(" AND s.project = ?");
        params.push(Box::new(p.clone()));
    }
    if let Some(r) = filters.role {
        sql.push_str(" AND m.role = ?");
        params.push(Box::new(role_as_sql_str(r)));
    }
    sql.push_str(" ORDER BY score ASC, m.timestamp DESC LIMIT 200");

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(param_refs), |r| {
        Ok(Hit {
            message_uuid: r.get(0)?,
            session_id: r.get(1)?,
            snippet: r.get(2)?,
            score: r.get(3)?,
        })
    })?;
    let hits: Result<Vec<Hit>, _> = rows.collect();
    Ok(hits?)
}

/// Build an FTS5 prefix MATCH expression from free text.
///
/// Each whitespace-separated token becomes a quoted prefix term (`"tok"*`).
/// Embedded double quotes are escaped per FTS5 syntax (doubled).
fn build_fts_prefix_query(q: &str) -> String {
    q.split_whitespace()
        .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Minimal in-memory schema that matches migrations 001 + 002 of
    /// `threadhop_core/storage/db.py` for the columns this module reads.
    /// Mirrors the production triggers so INSERTs into `messages` populate
    /// `messages_fts` automatically.
    fn open_in_memory_with_fts() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            r#"
            CREATE TABLE sessions (
                session_id   TEXT PRIMARY KEY,
                session_path TEXT,
                project      TEXT,
                cwd          TEXT,
                created_at   REAL,
                modified_at  REAL
            );
            CREATE TABLE messages (
                uuid         TEXT PRIMARY KEY,
                session_id   TEXT NOT NULL,
                role         TEXT NOT NULL,
                text         TEXT NOT NULL,
                timestamp    TEXT,
                cwd          TEXT,
                parent_uuid  TEXT,
                is_sidechain INTEGER NOT NULL DEFAULT 0
            );
            CREATE VIRTUAL TABLE messages_fts USING fts5(
                text,
                content='messages',
                content_rowid='rowid',
                tokenize='porter unicode61'
            );
            CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text);
            END;
            CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, text)
                VALUES ('delete', old.rowid, old.text);
            END;
            CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, text)
                VALUES ('delete', old.rowid, old.text);
                INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text);
            END;
            "#,
        )
        .unwrap();
        c
    }

    fn seed_session(conn: &Connection, session_id: &str, project: Option<&str>) {
        conn.execute(
            "INSERT INTO sessions (session_id, project) VALUES (?, ?)",
            rusqlite::params![session_id, project],
        )
        .unwrap();
    }

    fn seed_message(
        conn: &Connection,
        uuid: &str,
        session_id: &str,
        role: &str,
        text: &str,
        timestamp: &str,
    ) {
        conn.execute(
            "INSERT INTO messages (uuid, session_id, role, text, timestamp) \
             VALUES (?, ?, ?, ?, ?)",
            rusqlite::params![uuid, session_id, role, text, timestamp],
        )
        .unwrap();
    }

    // ---------- parse_query ----------

    #[test]
    fn parse_query_extracts_project_modifier() {
        let (filters, rest) = parse_query("project:threadhop hello world");
        assert_eq!(filters.project.as_deref(), Some("threadhop"));
        assert!(filters.role.is_none());
        assert_eq!(rest, "hello world");
    }

    #[test]
    fn parse_query_extracts_user_role() {
        let (filters, rest) = parse_query("user: questions about migrations");
        assert_eq!(filters.role, Some(MessageRole::User));
        assert_eq!(rest, "questions about migrations");
    }

    #[test]
    fn parse_query_extracts_assistant_role() {
        let (filters, rest) = parse_query("assistant: explanation");
        assert_eq!(filters.role, Some(MessageRole::Assistant));
        assert_eq!(rest, "explanation");
    }

    #[test]
    fn parse_query_combines_filters() {
        let (filters, rest) = parse_query("project:foo user: hello");
        assert_eq!(filters.project.as_deref(), Some("foo"));
        assert_eq!(filters.role, Some(MessageRole::User));
        assert_eq!(rest, "hello");
    }

    #[test]
    fn parse_query_empty_returns_empty_remainder() {
        let (filters, rest) = parse_query("   ");
        assert!(filters.project.is_none());
        assert!(filters.role.is_none());
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_query_bare_project_prefix_is_ignored() {
        // `project:` with no value should not set a filter, and should not
        // pollute the remainder either.
        let (filters, rest) = parse_query("project: hello");
        assert!(filters.project.is_none());
        assert_eq!(rest, "hello");
    }

    // ---------- build_fts_prefix_query ----------

    #[test]
    fn build_fts_prefix_query_appends_star_per_token() {
        assert_eq!(build_fts_prefix_query("hello world"), r#""hello"* "world"*"#);
    }

    #[test]
    fn build_fts_prefix_query_escapes_double_quotes() {
        // Input tokenizes to `say` and `"hi"` (a 4-char token starting and
        // ending with `"`). Each embedded `"` is doubled per FTS5 syntax,
        // yielding `""hi""`, then wrapped with outer quotes + `*`.
        assert_eq!(
            build_fts_prefix_query(r#"say "hi""#),
            r#""say"* """hi"""*"#
        );
    }

    // ---------- search / prefix_search ----------

    #[test]
    fn prefix_search_returns_hits_in_seeded_db() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", Some("proj-a"));
        seed_message(&c, "u1", "s1", "user", "hello world", "2026-05-20T10:00:00Z");
        seed_message(
            &c,
            "u2",
            "s1",
            "assistant",
            "world peace",
            "2026-05-20T10:00:01Z",
        );
        let hits = prefix_search(&c, "world", &Filters::default()).unwrap();
        assert_eq!(hits.len(), 2);
        let uuids: Vec<&str> = hits.iter().map(|h| h.message_uuid.as_str()).collect();
        assert!(uuids.contains(&"u1"));
        assert!(uuids.contains(&"u2"));
    }

    #[test]
    fn prefix_search_empty_query_returns_no_hits() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", None);
        seed_message(&c, "u1", "s1", "user", "hello world", "2026-05-20T10:00:00Z");
        let hits = prefix_search(&c, "   ", &Filters::default()).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn prefix_search_matches_word_prefix() {
        // `migrat` should hit `migrations`.
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", None);
        seed_message(
            &c,
            "u1",
            "s1",
            "user",
            "running migrations now",
            "2026-05-20T10:00:00Z",
        );
        let hits = prefix_search(&c, "migrat", &Filters::default()).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_uuid, "u1");
    }

    #[test]
    fn project_filter_narrows_results() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", Some("proj-a"));
        seed_session(&c, "s2", Some("proj-b"));
        seed_message(
            &c,
            "u1",
            "s1",
            "user",
            "hello world",
            "2026-05-20T10:00:00Z",
        );
        seed_message(
            &c,
            "u2",
            "s2",
            "user",
            "hello world",
            "2026-05-20T10:00:01Z",
        );
        let filters = Filters {
            project: Some("proj-a".into()),
            role: None,
        };
        let hits = prefix_search(&c, "hello", &filters).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
        assert_eq!(hits[0].message_uuid, "u1");
    }

    #[test]
    fn role_filter_user_only() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", None);
        seed_message(&c, "u1", "s1", "user", "shared topic", "2026-05-20T10:00:00Z");
        seed_message(
            &c,
            "u2",
            "s1",
            "assistant",
            "shared topic",
            "2026-05-20T10:00:01Z",
        );
        let filters = Filters {
            project: None,
            role: Some(MessageRole::User),
        };
        let hits = prefix_search(&c, "shared", &filters).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_uuid, "u1");
    }

    #[test]
    fn role_filter_assistant_only() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", None);
        seed_message(&c, "u1", "s1", "user", "shared topic", "2026-05-20T10:00:00Z");
        seed_message(
            &c,
            "u2",
            "s1",
            "assistant",
            "shared topic",
            "2026-05-20T10:00:01Z",
        );
        let filters = Filters {
            project: None,
            role: Some(MessageRole::Assistant),
        };
        let hits = prefix_search(&c, "shared", &filters).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_uuid, "u2");
    }

    #[test]
    fn search_parses_modifiers_and_filters_db() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", Some("threadhop"));
        seed_session(&c, "s2", Some("other"));
        seed_message(
            &c,
            "u1",
            "s1",
            "user",
            "decisions about FTS",
            "2026-05-20T10:00:00Z",
        );
        seed_message(
            &c,
            "u2",
            "s1",
            "assistant",
            "decisions about FTS",
            "2026-05-20T10:00:01Z",
        );
        seed_message(
            &c,
            "u3",
            "s2",
            "user",
            "decisions about FTS",
            "2026-05-20T10:00:02Z",
        );

        let hits = search(&c, "project:threadhop user: decisions").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message_uuid, "u1");
        assert_eq!(hits[0].session_id, "s1");
    }

    #[test]
    fn search_returns_snippet_with_match_markers() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", None);
        seed_message(
            &c,
            "u1",
            "s1",
            "user",
            "the quick brown fox jumps over the lazy dog",
            "2026-05-20T10:00:00Z",
        );
        let hits = search(&c, "fox").unwrap();
        assert_eq!(hits.len(), 1);
        // The snippet wraps the match in `[...]` per our snippet() call.
        assert!(
            hits[0].snippet.contains("[fox]"),
            "snippet missing markers: {}",
            hits[0].snippet
        );
    }

    #[test]
    fn search_score_is_populated() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", None);
        seed_message(&c, "u1", "s1", "user", "alpha beta", "2026-05-20T10:00:00Z");
        let hits = search(&c, "alpha").unwrap();
        assert_eq!(hits.len(), 1);
        // bm25 values are non-zero finite floats; we don't pin the exact value.
        assert!(hits[0].score.is_finite());
    }

    #[test]
    fn search_empty_returns_empty_vec_without_db_access() {
        // We pass a connection without the schema — if `search` short-circuits
        // on empty input it never prepares a statement, so this must succeed.
        let c = Connection::open_in_memory().unwrap();
        let hits = search(&c, "").unwrap();
        assert!(hits.is_empty());
        let hits = search(&c, "   ").unwrap();
        assert!(hits.is_empty());
        // Modifier-only input also has an empty remainder.
        let hits = search(&c, "project:foo user:").unwrap();
        assert!(hits.is_empty());
    }
}
