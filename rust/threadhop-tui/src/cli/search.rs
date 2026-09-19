//! `threadhop search` — FTS5 search across every indexed session.
//!
//! Port of `threadhop_core/cli/commands/search.py`, layered on the core
//! CLI-search lane in [`threadhop_core::fts`] (recency-first prefix FTS).
//!
//! Deviations from the Python CLI (both noted in the port plan):
//! * No incremental index refresh before querying — the Rust port has no
//!   JSONL→SQLite indexer yet; results reflect whatever the Python TUI/CLI
//!   last indexed into the shared DB.
//! * No trigram fuzzy fallback on zero exact hits (migration 008's
//!   `messages_fts_trigram` lane is unported), so the
//!   "(no exact matches — showing fuzzy results)" path never fires.

use serde::Serialize;
use threadhop_core::fts::{cli_search, parse_cli_query, CliHit, FTS_MATCH_END, FTS_MATCH_START};

use super::open_cli_db;

/// JSON row shape — field order matches the Python dict literal so the
/// serialized output is byte-comparable.
#[derive(Serialize)]
struct JsonHit {
    session_id: String,
    session_name: String,
    project: Option<String>,
    timestamp: Option<String>,
    snippet: String,
    uuid: String,
}

/// Convert FTS sentinel-bracketed snippets to CLI text.
///
/// `highlight = true` wraps matches in `**`; `false` (the JSON path) drops
/// the sentinels entirely. Newlines collapse to spaces so one hit stays one
/// visual block.
fn clean_snippet(snippet: &str, highlight: bool) -> String {
    let (start, end) = if highlight { ("**", "**") } else { ("", "") };
    let replaced = snippet
        .replace(FTS_MATCH_START, start)
        .replace(FTS_MATCH_END, end);
    replaced.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn session_name(hit: &CliHit) -> String {
    hit.custom_name
        .clone()
        .unwrap_or_else(|| hit.session_id.chars().take(8).collect())
}

/// Query FTS and print hits. (Python additionally refreshes the incremental
/// index first — see module docs for why the Rust CLI skips that step.)
pub fn cmd_search(query: &str, project: Option<&str>, limit: usize, json: bool) -> i32 {
    // `parse_cli_query` understands `project:` tokens — reuse it instead of
    // duplicating the filter plumbing (same trick as the Python CLI).
    let raw_query = match project {
        Some(p) => format!("{query} project:{p}"),
        None => query.to_string(),
    };
    let parsed = parse_cli_query(&raw_query);

    let rows: Vec<CliHit> = match open_cli_db() {
        Some(conn) => cli_search(&conn, &parsed, limit).unwrap_or_default(),
        None => Vec::new(),
    };

    if json {
        let payload: Vec<JsonHit> = rows
            .iter()
            .map(|hit| JsonHit {
                session_id: hit.session_id.clone(),
                session_name: session_name(hit),
                project: hit.project.clone(),
                timestamp: hit.timestamp.clone(),
                snippet: clean_snippet(&hit.snippet, false),
                uuid: hit.uuid.clone(),
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "[]".to_string())
        );
        return 0;
    }

    if rows.is_empty() {
        println!("No matches for '{query}'.");
        return 0;
    }

    for hit in &rows {
        let sid_short: String = hit.session_id.chars().take(8).collect();
        let name = session_name(hit);
        let project = hit.project.as_deref().unwrap_or("unknown project");
        let ts = hit.timestamp.as_deref().unwrap_or("?");
        let snippet = clean_snippet(&hit.snippet, true);
        println!("{sid_short}  {name}  [{project}]  {ts}");
        println!("  {snippet}");
        println!();
    }

    println!("Tip: threadhop peek <session> --grep '{query}' shows full exchanges.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_snippet_highlights_or_strips_sentinels() {
        let raw = format!("found {FTS_MATCH_START}term{FTS_MATCH_END} here");
        assert_eq!(clean_snippet(&raw, true), "found **term** here");
        assert_eq!(clean_snippet(&raw, false), "found term here");
    }

    #[test]
    fn clean_snippet_collapses_whitespace() {
        assert_eq!(clean_snippet("a\n  b\tc", false), "a b c");
    }

    #[test]
    fn json_hit_serializes_in_python_key_order() {
        let hit = JsonHit {
            session_id: "sid".into(),
            session_name: "name".into(),
            project: None,
            timestamp: None,
            snippet: "s".into(),
            uuid: "u".into(),
        };
        let s = serde_json::to_string(&hit).unwrap();
        let keys: Vec<usize> = ["session_id", "session_name", "project", "timestamp", "snippet", "uuid"]
            .iter()
            .map(|k| s.find(&format!("\"{k}\"")).unwrap())
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "keys must serialize in declaration order");
    }
}
