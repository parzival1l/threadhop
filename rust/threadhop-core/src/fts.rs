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
//! Results are ordered most-recent-first by `messages.timestamp` descending,
//! then by FTS5 `bm25` (ascending — lower is a better match) as a tiebreaker,
//! capped at 200 rows.
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
/// ordered most-recent-first (timestamp descending) then by relevance
/// (bm25 ascending) as a tiebreaker.
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
    sql.push_str(" ORDER BY m.timestamp DESC, score ASC LIMIT 200");

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

// --- CLI search lane (ADR-029 `threadhop search`) ---------------------------
//
// The CLI needs more columns per hit than the TUI's [`Hit`] carries (session
// display name, project, timestamp) and mirrors the *Python* CLI's query
// parser (`storage.search_queries._parse_search_query`) rather than the
// modifier syntax above. Layered here, next to [`prefix_search`], so both
// lanes share the recency-first ordering contract
// (`ORDER BY m.timestamp DESC, rank`) established for this module.

/// Sentinel characters bracketing matched spans in CLI snippets. Mirrors
/// Python's `FTS_MATCH_START` / `FTS_MATCH_END` — the CLI converts them to
/// `**` (text mode) or strips them (`--json`).
pub const FTS_MATCH_START: char = '\u{1}';
pub const FTS_MATCH_END: char = '\u{2}';

/// One `threadhop search` hit — the row shape Python's `search_messages`
/// returns (minus `session_path`, which the CLI output never surfaces).
#[derive(Debug, Clone, PartialEq)]
pub struct CliHit {
    pub uuid: String,
    pub session_id: String,
    pub timestamp: Option<String>,
    /// Snippet with [`FTS_MATCH_START`] / [`FTS_MATCH_END`] sentinels.
    pub snippet: String,
    pub custom_name: Option<String>,
    pub project: Option<String>,
}

/// Parsed form of a raw CLI query — mirrors Python's
/// `_parse_search_query(raw) -> (terms, role, project)`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CliQuery {
    pub terms: Vec<String>,
    pub role: Option<String>,
    pub project: Option<String>,
}

/// Parse raw input into search terms + role/project filters.
///
/// * `user:` / `assistant:` (case-insensitive) set the role filter.
/// * `project:<val>` sets the project filter (substring LIKE match).
/// * every other token is stripped of non-word characters (`[^\w]` → gone,
///   Python parity) and, if non-empty, becomes an AND-combined prefix term.
pub fn parse_cli_query(raw: &str) -> CliQuery {
    let mut out = CliQuery::default();
    for tok in raw.split_whitespace() {
        let low = tok.to_lowercase();
        if low == "user:" {
            out.role = Some("user".to_string());
        } else if low == "assistant:" {
            out.role = Some("assistant".to_string());
        } else if let Some(val) = tok.strip_prefix("project:") {
            if !val.is_empty() {
                out.project = Some(val.to_string());
            }
        } else {
            let clean: String = tok
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !clean.is_empty() {
                out.terms.push(clean);
            }
        }
    }
    out
}

/// Run the CLI search — Python's `search_messages` prefix path.
///
/// Term-less queries with a role/project filter fall back to a filter-only
/// scan (newest first, 160-char raw snippets); term-less queries with no
/// filters return no rows. The trigram fuzzy fallback the Python side layers
/// on zero prefix hits is NOT ported yet — callers get the exact-match rows
/// only.
pub fn cli_search(
    conn: &Connection,
    query: &CliQuery,
    limit: usize,
) -> Result<Vec<CliHit>, FtsError> {
    if query.terms.is_empty() {
        if query.role.is_none() && query.project.is_none() {
            return Ok(Vec::new());
        }
        return cli_filter_only(conn, query, limit);
    }

    let fts_expr = query
        .terms
        .iter()
        .map(|t| format!("{t}*"))
        .collect::<Vec<_>>()
        .join(" ");

    let mut sql = String::from(
        "SELECT m.uuid, m.session_id, m.timestamp, \
                snippet(messages_fts, 0, ?, ?, '…', 16) AS snip, \
                s.custom_name, s.project \
         FROM messages_fts \
         JOIN messages m ON m.rowid = messages_fts.rowid \
         LEFT JOIN sessions s ON s.session_id = m.session_id \
         WHERE messages_fts MATCH ?",
    );
    let mut params: Vec<Box<dyn ToSql>> = vec![
        Box::new(FTS_MATCH_START.to_string()),
        Box::new(FTS_MATCH_END.to_string()),
        Box::new(fts_expr),
    ];
    if let Some(role) = &query.role {
        sql.push_str(" AND m.role = ?");
        params.push(Box::new(role.clone()));
    }
    if let Some(project) = &query.project {
        sql.push_str(" AND s.project LIKE ?");
        params.push(Box::new(format!("%{project}%")));
    }
    // Recency-first: newest matches lead, bm25 `rank` breaks timestamp ties
    // (same ordering contract as `prefix_search` above).
    sql.push_str(" ORDER BY m.timestamp DESC, rank LIMIT ?");
    params.push(Box::new(limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(param_refs), map_cli_hit)?;
    let hits: Result<Vec<CliHit>, _> = rows.collect();
    Ok(hits?)
}

/// Filter-only scan for term-less queries — Python's `_search_filter_only`.
fn cli_filter_only(
    conn: &Connection,
    query: &CliQuery,
    limit: usize,
) -> Result<Vec<CliHit>, FtsError> {
    let mut sql = String::from(
        "SELECT m.uuid, m.session_id, m.timestamp, \
                substr(m.text, 1, 160) AS snip, \
                s.custom_name, s.project \
         FROM messages m \
         LEFT JOIN sessions s ON s.session_id = m.session_id \
         WHERE 1=1",
    );
    let mut params: Vec<Box<dyn ToSql>> = Vec::new();
    if let Some(role) = &query.role {
        sql.push_str(" AND m.role = ?");
        params.push(Box::new(role.clone()));
    }
    if let Some(project) = &query.project {
        sql.push_str(" AND s.project LIKE ?");
        params.push(Box::new(format!("%{project}%")));
    }
    sql.push_str(" ORDER BY m.timestamp DESC LIMIT ?");
    params.push(Box::new(limit as i64));

    let mut stmt = conn.prepare(&sql)?;
    let param_refs: Vec<&dyn ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let rows = stmt.query_map(rusqlite::params_from_iter(param_refs), map_cli_hit)?;
    let hits: Result<Vec<CliHit>, _> = rows.collect();
    Ok(hits?)
}

fn map_cli_hit(r: &rusqlite::Row<'_>) -> rusqlite::Result<CliHit> {
    Ok(CliHit {
        uuid: r.get(0)?,
        session_id: r.get(1)?,
        timestamp: r.get(2)?,
        snippet: r.get(3)?,
        custom_name: r.get(4)?,
        project: r.get(5)?,
    })
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
                custom_name  TEXT,
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
    fn prefix_search_orders_most_recent_first() {
        // Two matches where recency and relevance disagree: the older message
        // is a near-exact match (better bm25), the newer one buries the term in
        // a long sentence (worse bm25). Recency is primary, so the newer u2
        // must come first — this would fail under score-primary ordering.
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", None);
        seed_message(&c, "u1", "s1", "user", "alpha", "2026-05-20T10:00:00Z");
        seed_message(
            &c,
            "u2",
            "s1",
            "user",
            "lots of unrelated padding words before the alpha term appears here",
            "2026-05-20T10:00:05Z",
        );
        let hits = prefix_search(&c, "alpha", &Filters::default()).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].message_uuid, "u2", "newest hit should sort first");
        assert_eq!(hits[1].message_uuid, "u1");
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

    // ---------- CLI search lane (parse_cli_query / cli_search) ----------

    #[test]
    fn parse_cli_query_splits_terms_role_and_project() {
        let q = parse_cli_query("retry backoff project:threadhop user:");
        assert_eq!(q.terms, vec!["retry", "backoff"]);
        assert_eq!(q.role.as_deref(), Some("user"));
        assert_eq!(q.project.as_deref(), Some("threadhop"));
    }

    #[test]
    fn parse_cli_query_strips_non_word_chars_from_terms() {
        let q = parse_cli_query("retry.backoff() 'quoted'");
        assert_eq!(q.terms, vec!["retrybackoff", "quoted"]);
    }

    #[test]
    fn cli_search_orders_most_recent_first_with_sentinel_snippets() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", Some("proj-a"));
        seed_message(&c, "u1", "s1", "user", "retry the request", "2026-05-20T10:00:00Z");
        seed_message(
            &c,
            "u2",
            "s1",
            "assistant",
            "we should retry with backoff",
            "2026-05-20T10:00:05Z",
        );
        let hits = cli_search(&c, &parse_cli_query("retry"), 20).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].uuid, "u2", "newest hit sorts first");
        assert!(
            hits[0]
                .snippet
                .contains(&format!("{FTS_MATCH_START}retry{FTS_MATCH_END}")),
            "snippet missing sentinels: {:?}",
            hits[0].snippet
        );
        assert_eq!(hits[0].project.as_deref(), Some("proj-a"));
    }

    #[test]
    fn cli_search_project_filter_is_substring_like() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", Some("-Users-alice-threadhop"));
        seed_session(&c, "s2", Some("-Users-alice-other"));
        seed_message(&c, "u1", "s1", "user", "shared term", "2026-05-20T10:00:00Z");
        seed_message(&c, "u2", "s2", "user", "shared term", "2026-05-20T10:00:01Z");
        let hits = cli_search(&c, &parse_cli_query("shared project:threadhop"), 20).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
    }

    #[test]
    fn cli_search_respects_limit() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", None);
        for i in 0..5 {
            seed_message(
                &c,
                &format!("u{i}"),
                "s1",
                "user",
                "alpha term",
                &format!("2026-05-20T10:00:0{i}Z"),
            );
        }
        let hits = cli_search(&c, &parse_cli_query("alpha"), 2).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn cli_search_termless_no_filters_returns_empty() {
        // No schema needed — must short-circuit before touching the DB.
        let c = Connection::open_in_memory().unwrap();
        let hits = cli_search(&c, &parse_cli_query("..."), 20).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn cli_search_termless_with_filter_scans_newest_first() {
        let c = open_in_memory_with_fts();
        seed_session(&c, "s1", Some("proj-a"));
        seed_message(&c, "u1", "s1", "user", "older", "2026-05-20T10:00:00Z");
        seed_message(&c, "u2", "s1", "user", "newer", "2026-05-20T10:00:01Z");
        let hits = cli_search(&c, &parse_cli_query("project:proj-a"), 20).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].uuid, "u2");
        assert_eq!(hits[0].snippet, "newer");
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
