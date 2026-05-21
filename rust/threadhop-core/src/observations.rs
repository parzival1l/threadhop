//! Read-only access to per-session observation JSONL + `latest_summary`.
//!
//! Observation files live at `~/.config/threadhop/observations/<session_id>.jsonl`
//! and are append-only, Python-owned (ADR-019/020). This module never writes to
//! them. The `conflict_reviews` table is also Python-owned; we only read.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::ObservationError;

/// Typed union of observation rows the observer + reflector append.
///
/// Unknown `type` values are captured by [`Observation::Other`] so that
/// forward-compatible reads never abort on a row this binary doesn't know.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Observation {
    Decision {
        text: String,
        ts: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refs: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    Todo {
        text: String,
        #[serde(default)]
        status: String,
        ts: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    Done {
        text: String,
        ts: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    Conflict {
        #[serde(default)]
        refs: Vec<String>,
        #[serde(default)]
        topic: String,
        ts: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    Note {
        text: String,
        ts: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    Adr {
        text: String,
        ts: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<String>,
    },
    #[serde(other)]
    Other,
}

impl Observation {
    /// Return the row's timestamp string, if any.
    ///
    /// ISO-8601 strings sort lexicographically when fully zoned (the observer
    /// emits `...Z`), so callers can use this for "newest" comparisons without
    /// parsing.
    pub fn timestamp(&self) -> Option<&str> {
        match self {
            Observation::Decision { ts, .. }
            | Observation::Todo { ts, .. }
            | Observation::Done { ts, .. }
            | Observation::Conflict { ts, .. }
            | Observation::Note { ts, .. }
            | Observation::Adr { ts, .. } => Some(ts.as_str()),
            Observation::Other => None,
        }
    }
}

/// Summary used by the TUI digest bar (`§5 latest_summary`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObservationSummary {
    pub newest_decision: Option<String>,
    pub open_todo_count: usize,
    pub unresolved_conflict_count: usize,
    pub last_observed_at: Option<String>,
}

/// Read every observation row for `session_id` from the canonical observation
/// directory. Returns an empty `Vec` if the file does not exist.
pub fn read_entries(session_id: &str) -> Result<Vec<Observation>, ObservationError> {
    let path = crate::paths::observation_file(session_id);
    read_entries_from(&path)
}

/// Read every observation row from `path`. Lines that fail to parse are logged
/// via `tracing::warn!` and skipped — the file is append-only and the observer
/// may flush a partial line during a read.
pub fn read_entries_from(path: &Path) -> Result<Vec<Observation>, ObservationError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Observation>(&line) {
            Ok(obs) => out.push(obs),
            Err(e) => tracing::warn!(
                path = %path.display(),
                error = %e,
                "observation parse error; skipping line"
            ),
        }
    }
    Ok(out)
}

/// Compute the per-session digest summary, joining against `conflict_reviews`
/// in the open SQLite connection to count *unresolved* conflicts only.
pub fn latest_summary(
    conn: &Connection,
    session_id: &str,
) -> Result<ObservationSummary, ObservationError> {
    let entries = read_entries(session_id)?;
    summary_from_entries(conn, session_id, &entries)
}

/// Same as [`latest_summary`] but reads from an explicit path. Useful for tests
/// that don't want to relocate `$HOME`.
pub fn latest_summary_from(
    conn: &Connection,
    session_id: &str,
    path: &Path,
) -> Result<ObservationSummary, ObservationError> {
    let entries = read_entries_from(path)?;
    summary_from_entries(conn, session_id, &entries)
}

fn summary_from_entries(
    conn: &Connection,
    session_id: &str,
    entries: &[Observation],
) -> Result<ObservationSummary, ObservationError> {
    let mut newest_decision: Option<&str> = None;
    let mut newest_decision_ts: Option<&str> = None;
    let mut open_todos = 0usize;
    let mut last_ts: Option<&str> = None;
    let mut conflicts: Vec<(&[String], &str)> = Vec::new();

    for entry in entries {
        if let Some(ts) = entry.timestamp() {
            if last_ts.is_none_or(|cur| ts > cur) {
                last_ts = Some(ts);
            }
        }
        match entry {
            Observation::Decision { text, ts, .. }
                if newest_decision_ts.is_none_or(|cur| ts.as_str() > cur) =>
            {
                newest_decision_ts = Some(ts.as_str());
                newest_decision = Some(text.as_str());
            }
            Observation::Todo { status, .. } if status == "open" => {
                open_todos += 1;
            }
            Observation::Conflict { refs, topic, .. } => {
                conflicts.push((refs.as_slice(), topic.as_str()));
            }
            _ => {}
        }
    }

    let mut unresolved = 0usize;
    for (refs, topic) in &conflicts {
        if !is_conflict_reviewed(conn, session_id, refs, topic)? {
            unresolved += 1;
        }
    }

    Ok(ObservationSummary {
        newest_decision: newest_decision.map(str::to_string),
        open_todo_count: open_todos,
        unresolved_conflict_count: unresolved,
        last_observed_at: last_ts.map(str::to_string),
    })
}

/// Mirrors Python's `_normalize_conflict_refs` (storage/db.py): trim, drop
/// empties, dedup, sort, join on `\x1f`. The result is the `refs_key` column.
fn normalize_conflict_refs(refs: &[String]) -> String {
    let mut canon: Vec<String> = refs
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    canon.sort();
    canon.dedup();
    canon.join("\u{1f}")
}

fn is_conflict_reviewed(
    conn: &Connection,
    session_id: &str,
    refs: &[String],
    topic: &str,
) -> Result<bool, ObservationError> {
    let refs_key = normalize_conflict_refs(refs);
    let row: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM conflict_reviews \
             WHERE session_id = ? AND refs_key = ? AND topic = ?",
            params![session_id, refs_key, topic],
            |r| r.get(0),
        )
        .optional()
        .map_err(crate::error::DbError::from)?;
    Ok(row.is_some())
}

#[cfg(test)]
fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("observations_sample.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_mem_db_with_reviews() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE conflict_reviews (
                session_id  TEXT NOT NULL,
                refs_key    TEXT NOT NULL,
                topic       TEXT NOT NULL DEFAULT '',
                reviewed_at REAL NOT NULL,
                PRIMARY KEY (session_id, refs_key, topic)
            );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn read_entries_yields_all_observation_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"decision","text":"use SQLite","ts":"2026-01-01T00:00:01Z"}"#,
                "\n",
                r#"{"type":"todo","text":"port observer","status":"open","ts":"2026-01-01T00:00:02Z"}"#,
                "\n",
                r#"{"type":"conflict","refs":["s1","s2"],"topic":"x","ts":"2026-01-01T00:00:03Z"}"#,
                "\n",
            ),
        )
        .unwrap();
        let entries = read_entries_from(&path).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(matches!(entries[0], Observation::Decision { .. }));
        assert!(matches!(entries[1], Observation::Todo { .. }));
        assert!(matches!(entries[2], Observation::Conflict { .. }));
    }

    #[test]
    fn read_entries_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jsonl");
        let entries = read_entries_from(&path).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn read_entries_skips_blank_and_bad_lines() {
        let entries = read_entries_from(&fixture_path()).unwrap();
        // Fixture has 8 well-formed observation rows plus a blank line and a
        // structurally-invalid row. The invalid row becomes `Other` because
        // serde's `#[serde(other)]` swallows missing/unknown tags too — but
        // serde_json rejects rows with no `type` field at all, so the trailing
        // `{"this":...}` row is logged & skipped, leaving 8.
        assert_eq!(entries.len(), 8);
    }

    #[test]
    fn observation_other_swallows_unknown_type() {
        let json = r#"{"type":"observation","text":"x","ts":"2026-01-01T00:00:00Z"}"#;
        let parsed: Observation = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, Observation::Other);
    }

    #[test]
    fn timestamp_returns_inner_ts_for_known_variants() {
        let d = Observation::Decision {
            text: "x".into(),
            ts: "2026-01-01T00:00:00Z".into(),
            refs: None,
            context: None,
        };
        assert_eq!(d.timestamp(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(Observation::Other.timestamp(), None);
    }

    #[test]
    fn normalize_conflict_refs_matches_python_semantics() {
        let refs = vec!["  s2  ".to_string(), "s1".to_string(), "s1".to_string(), "  ".to_string()];
        let key = normalize_conflict_refs(&refs);
        assert_eq!(key, "s1\u{1f}s2");
        assert_eq!(normalize_conflict_refs(&[]), "");
    }

    #[test]
    fn latest_summary_counts_open_todos_and_unresolved_conflicts() {
        let conn = open_mem_db_with_reviews();
        let summary = latest_summary_from(&conn, "sess-a", &fixture_path()).unwrap();
        // Fixture: 1 open todo, 1 closed todo, 1 conflict (unresolved).
        assert_eq!(summary.open_todo_count, 1);
        assert_eq!(summary.unresolved_conflict_count, 1);
        assert_eq!(summary.newest_decision.as_deref(), Some("use SQLite"));
        // Newest ts across all known rows in the fixture.
        assert_eq!(
            summary.last_observed_at.as_deref(),
            Some("2026-04-17T18:55:00.000Z")
        );
    }

    #[test]
    fn latest_summary_skips_reviewed_conflicts() {
        let conn = open_mem_db_with_reviews();
        // Mark the fixture's only conflict (refs=["s1","s2"], topic="storage-backend") as reviewed.
        let refs_key = normalize_conflict_refs(&["s1".to_string(), "s2".to_string()]);
        conn.execute(
            "INSERT INTO conflict_reviews (session_id, refs_key, topic, reviewed_at)
             VALUES (?, ?, ?, 0.0)",
            params!["sess-a", refs_key, "storage-backend"],
        )
        .unwrap();

        let summary = latest_summary_from(&conn, "sess-a", &fixture_path()).unwrap();
        assert_eq!(summary.unresolved_conflict_count, 0);
        assert_eq!(summary.open_todo_count, 1);
    }

    #[test]
    fn latest_summary_picks_newest_decision_by_ts_not_position() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(
            &path,
            concat!(
                r#"{"type":"decision","text":"older","ts":"2026-01-01T00:00:00Z"}"#,
                "\n",
                r#"{"type":"decision","text":"newer","ts":"2026-02-01T00:00:00Z"}"#,
                "\n",
                r#"{"type":"decision","text":"middle","ts":"2026-01-15T00:00:00Z"}"#,
                "\n",
            ),
        )
        .unwrap();
        let conn = open_mem_db_with_reviews();
        let summary = latest_summary_from(&conn, "sx", &path).unwrap();
        assert_eq!(summary.newest_decision.as_deref(), Some("newer"));
        assert_eq!(
            summary.last_observed_at.as_deref(),
            Some("2026-02-01T00:00:00Z")
        );
    }

    #[test]
    fn latest_summary_empty_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.jsonl");
        std::fs::write(&path, "").unwrap();
        let conn = open_mem_db_with_reviews();
        let summary = latest_summary_from(&conn, "sx", &path).unwrap();
        assert_eq!(summary, ObservationSummary::default());
    }
}
