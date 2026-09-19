//! CLI subcommands — the ADR-029 lazy borrow surface (`peek` / `search` /
//! `prepare` / `receive`).
//!
//! Contracts mirror the Python CLI (`threadhop_core/cli/commands/*.py`)
//! exactly — same flags, defaults, exit codes, and output shapes — so users
//! can't tell which binary they hit. Each handler returns an exit code; the
//! `main` dispatcher exits with it before any terminal/TUI setup runs.
//!
//! Shared helpers here mirror `threadhop_core/cli/helpers.py`:
//! * [`find_session_path`] — locate `<projects>/*/{sid}.jsonl`.
//! * [`resolve_session_prefix`] — session id or unique prefix against both
//!   the on-disk transcripts and the ThreadHop DB.
//! * [`open_cli_db`] / [`session_row`] — best-effort DB access. Unlike the
//!   Python CLI (whose `cli_bootstrap` runs migrations), the Rust side never
//!   migrates; a fresh/unmigrated DB degrades to "no rows" rather than
//!   erroring, and `peek` does NOT seed missing session rows (read-only —
//!   the display name / project are derived from the transcript path
//!   instead, which yields identical output).

pub mod peek;
pub mod prepare;
pub mod receive;
pub mod search;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use rusqlite::Connection;
use threadhop_core::{db, paths};

/// The `threadhop` borrow-surface verbs (ADR-029).
#[derive(Subcommand, Debug, Clone)]
pub enum CliCommand {
    /// Print cleaned exchanges from another session (zero LLM)
    #[command(
        after_help = "Examples:\n  \
            threadhop-tui peek 41f3                         # last 5 exchanges\n  \
            threadhop-tui peek 41f3 --last 10\n  \
            threadhop-tui peek 41f3 --range 3:7             # 1-based inclusive\n  \
            threadhop-tui peek 41f3 --grep 'retry.*backoff' # matching exchanges, in full"
    )]
    Peek {
        /// Session id or unique prefix to read from
        #[arg(value_name = "session")]
        session_ref: String,
        /// Show the last N exchanges (default: 5)
        #[arg(long, value_name = "N", conflicts_with_all = ["range", "grep"])]
        last: Option<i64>,
        /// Show exchanges A through B (1-based, inclusive)
        #[arg(long, value_name = "A:B", conflicts_with = "grep")]
        range: Option<String>,
        /// Case-insensitive regex; prints every matching exchange in full
        #[arg(long, value_name = "PATTERN")]
        grep: Option<String>,
    },
    /// Full-text search across every indexed session
    #[command(
        after_help = "Examples:\n  \
            threadhop-tui search 'retry backoff'\n  \
            threadhop-tui search migration --project threadhop --limit 5\n  \
            threadhop-tui search sqlite --json"
    )]
    Search {
        /// Search terms (stemmed prefix match, AND-combined)
        #[arg(value_name = "query")]
        query: String,
        /// Filter by project (substring match on directory name)
        #[arg(long)]
        project: Option<String>,
        /// Maximum hits to print (default: 20)
        #[arg(long, value_name = "N", default_value_t = 20)]
        limit: usize,
        /// Emit results as a JSON list instead of text blocks
        #[arg(long)]
        json: bool,
    },
    /// Create a handoff ticket with a summary and recent conversation
    #[command(
        name = "handoff",
        visible_alias = "prepare",
        after_help = "Examples:\n  \
            threadhop-tui handoff --session 41f3a2b8-...\n  \
            threadhop-tui handoff --session 41f3a2b8-... --tail 5 --tail-budget 12000\n\n\
            NOTE: --session is required — the Rust port\n\
            has no current-session auto-detection from process ancestry yet."
    )]
    Handoff {
        /// Target session id. Required: the Rust port cannot auto-detect
        /// the current session from process ancestry yet.
        #[arg(long, value_name = "id")]
        session: String,
        /// Exchanges to carry verbatim (default: 3)
        #[arg(long, value_name = "N", default_value_t = 3)]
        tail: i64,
        /// Character cap for the verbatim tail; oldest tail exchanges are
        /// dropped first (default: 8000)
        #[arg(long, value_name = "CHARS", default_value_t = 8000)]
        tail_budget: i64,
        /// Model passed to `claude -p` for the head summary (default: haiku)
        #[arg(long, value_name = "M", default_value = "haiku")]
        model: String,
    },
    /// Print a transfer ticket verbatim (zero LLM)
    #[command(
        after_help = "Examples:\n  \
            threadhop-tui receive tk_ab12cd34\n  \
            threadhop-tui receive ab12cd34"
    )]
    Receive {
        /// Ticket id (tk_xxxxxxxx / xxxxxxxx) or path to a ticket file
        #[arg(value_name = "ticket-id")]
        ticket: String,
    },
}

/// Dispatch a subcommand; returns the process exit code.
pub fn run(cmd: CliCommand) -> i32 {
    match cmd {
        CliCommand::Peek {
            session_ref,
            last,
            range,
            grep,
        } => peek::cmd_peek(&session_ref, last, range.as_deref(), grep.as_deref()),
        CliCommand::Search {
            query,
            project,
            limit,
            json,
        } => search::cmd_search(&query, project.as_deref(), limit, json),
        CliCommand::Handoff {
            session,
            tail,
            tail_budget,
            model,
        } => prepare::cmd_prepare(&session, tail, tail_budget, &model),
        CliCommand::Receive { ticket } => receive::cmd_receive(&ticket),
    }
}

// --- Shared helpers ----------------------------------------------------------

/// Locate the JSONL transcript for a session id under the Claude projects
/// dir. Mirrors Python's `find_session_path`.
pub fn find_session_path(projects_root: &Path, session_id: &str) -> Option<PathBuf> {
    let target = format!("{session_id}.jsonl");
    let entries = std::fs::read_dir(projects_root).ok()?;
    for project in entries.flatten() {
        let candidate = project.path().join(&target);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Resolve a session id or unique prefix against known sessions.
///
/// Candidates come from both the on-disk transcripts (skipping `agent-*`
/// sub-agent files) and the `sessions` table, so `peek` works on sessions
/// the TUI has never scanned. An exact id match wins outright; otherwise
/// every id starting with `token` is returned sorted — the caller decides
/// what 0 or >1 candidates mean (not-found vs ambiguous).
pub fn resolve_session_prefix(
    conn: Option<&Connection>,
    projects_root: &Path,
    token: &str,
) -> Vec<String> {
    let mut known: BTreeSet<String> = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(projects_root) {
        for project in entries.flatten() {
            let Ok(inner) = std::fs::read_dir(project.path()) else {
                continue;
            };
            for f in inner.flatten() {
                let path = f.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if stem.starts_with("agent-") {
                    continue;
                }
                known.insert(stem.to_string());
            }
        }
    }
    if let Some(conn) = conn {
        // Guarded: a fresh (unmigrated) DB has no `sessions` table.
        if let Ok(mut stmt) = conn.prepare("SELECT session_id FROM sessions") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                for row in rows.flatten() {
                    known.insert(row);
                }
            }
        }
    }

    if known.contains(token) {
        return vec![token.to_string()];
    }
    known
        .into_iter()
        .filter(|sid| sid.starts_with(token))
        .collect()
}

/// Best-effort open of the shared ThreadHop DB. `None` when the open itself
/// fails; queries against missing tables are guarded at each call site.
pub fn open_cli_db() -> Option<Connection> {
    db::open(&paths::db_path()).ok()
}

/// Guarded `sessions` row lookup — `None` on any error (including a missing
/// table on an unmigrated DB).
pub fn session_row(
    conn: Option<&Connection>,
    session_id: &str,
) -> Option<threadhop_core::models::Session> {
    let conn = conn?;
    db::session_by_id(conn, session_id).ok().flatten()
}

/// Display name + project for a session, mirroring what the Python CLI
/// prints after `_ensure_cli_session_row`: `custom_name` (else `sid[:8]`)
/// and the DB `project` column. When the DB has no row, Python would seed
/// one with `project = session_path.parent.name` — the Rust CLI derives the
/// same values without writing.
pub fn display_name_and_project(
    conn: Option<&Connection>,
    session_id: &str,
    session_path: Option<&Path>,
) -> (String, Option<String>) {
    let short: String = session_id.chars().take(8).collect();
    match session_row(conn, session_id) {
        Some(row) => (row.custom_name.unwrap_or(short), row.project),
        None => {
            let project = session_path
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .map(|s| s.to_string());
            (short, project)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_projects(tmp: &Path) -> PathBuf {
        let projects = tmp.join("projects");
        let p1 = projects.join("-Users-alice-alpha");
        let p2 = projects.join("-Users-alice-beta");
        fs::create_dir_all(&p1).unwrap();
        fs::create_dir_all(&p2).unwrap();
        fs::write(p1.join("aaaa1111.jsonl"), "{}\n").unwrap();
        fs::write(p2.join("aaab2222.jsonl"), "{}\n").unwrap();
        fs::write(p1.join("agent-zzz.jsonl"), "{}\n").unwrap();
        projects
    }

    #[test]
    fn find_session_path_locates_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = make_projects(tmp.path());
        let found = find_session_path(&projects, "aaaa1111").unwrap();
        assert!(found.ends_with("-Users-alice-alpha/aaaa1111.jsonl"));
        assert!(find_session_path(&projects, "missing").is_none());
    }

    #[test]
    fn resolve_prefix_exact_match_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = make_projects(tmp.path());
        assert_eq!(
            resolve_session_prefix(None, &projects, "aaaa1111"),
            vec!["aaaa1111"]
        );
    }

    #[test]
    fn resolve_prefix_unique_and_ambiguous() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = make_projects(tmp.path());
        assert_eq!(
            resolve_session_prefix(None, &projects, "aaab"),
            vec!["aaab2222"]
        );
        let ambiguous = resolve_session_prefix(None, &projects, "aaa");
        assert_eq!(ambiguous, vec!["aaaa1111", "aaab2222"]);
        assert!(resolve_session_prefix(None, &projects, "zzz").is_empty());
    }

    #[test]
    fn resolve_prefix_skips_agent_files_and_merges_db_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = make_projects(tmp.path());
        // agent-zzz must never resolve.
        assert!(resolve_session_prefix(None, &projects, "agent").is_empty());
        // A DB-only session id participates in resolution.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (session_id TEXT PRIMARY KEY, session_path TEXT, \
             project TEXT, cwd TEXT, custom_name TEXT, status TEXT DEFAULT 'active', \
             sort_order INTEGER, last_viewed REAL, created_at REAL, modified_at REAL);
             INSERT INTO sessions (session_id, session_path) VALUES ('dbonly99', '/x');",
        )
        .unwrap();
        assert_eq!(
            resolve_session_prefix(Some(&conn), &projects, "dbonly"),
            vec!["dbonly99"]
        );
    }

    #[test]
    fn resolve_prefix_tolerates_unmigrated_db() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = make_projects(tmp.path());
        let conn = Connection::open_in_memory().unwrap(); // no tables at all
        assert_eq!(
            resolve_session_prefix(Some(&conn), &projects, "aaab"),
            vec!["aaab2222"]
        );
    }

    #[test]
    fn display_name_falls_back_to_short_id_and_path_project() {
        let tmp = tempfile::tempdir().unwrap();
        let projects = make_projects(tmp.path());
        let path = find_session_path(&projects, "aaaa1111").unwrap();
        let (name, project) =
            display_name_and_project(None, "aaaa1111-rest-of-uuid", Some(&path));
        assert_eq!(name, "aaaa1111");
        assert_eq!(project.as_deref(), Some("-Users-alice-alpha"));
    }
}
