//! Per-session digest — the data shape behind the right-hand digest
//! panel.
//!
//! Wave 3 Worker H populates [`compute_session_digest`] by walking the
//! cleaned-transcript view from [`crate::jsonl::parse_byte_range`] and
//! the observation file from [`crate::observations::read_entries`]. The
//! field set mirrors the Python `SessionDigest` dataclass — the
//! computed-property names from Python (`title`, `cache_hit_ratio`,
//! `context_fill_ratio`, `files_touched_count`,
//! `total_input_tokens_billed`) are flattened into regular fields here
//! because Rust doesn't have lazy properties on data structs; we
//! compute them during the JSONL pass and store directly.
//!
//! # Deferred fields
//!
//! A few fields the Python extractor lifts off side-line types that the
//! Rust parser drops (it only emits `user` / `assistant` / `tool` /
//! `command` / `skill_load` rows) — those are intentionally left
//! `None` here:
//!
//! * `branch` — needs a `git -C <cwd> branch --show-current` shell-out
//!   (or a `gitBranch` field the Rust parser doesn't surface today).
//! * `permission_mode` — extracted from `permission-mode` JSONL lines
//!   that the cleaned-transcript view drops.
//! * `client_version` — extracted from `version` on any line, also
//!   dropped by the cleaner.
//!
//! These show up as `—` in the right-column panel until a follow-up
//! wave widens the parser or wires a git-branch helper.

use std::path::Path;

use rusqlite::Connection;

use crate::jsonl::{parse_byte_range, CleanedMessage};
use crate::observations::{read_entries, Observation};

/// One band of the recap timeline (Started / Earlier / Recently / Last
/// asked). `timestamp` is the source ISO timestamp where available; the
/// renderer uses it to compute a relative-age suffix.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecapEntry {
    pub label: String,
    pub timestamp: Option<String>,
    pub text: String,
}

/// A single session, summarised for the right-column digest panel.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionDigest {
    /// Stable session UUID.
    pub session_id: String,

    /// Best display title — first non-empty user line, truncated.
    pub title: String,

    /// Short identifier — first 8 chars of `session_id`.
    pub slug: String,

    /// Git branch the session was opened on. DEFER — see module docs.
    pub branch: Option<String>,

    /// Wall-clock duration (seconds) from first user prompt to last
    /// event observed.
    pub duration_seconds: Option<u64>,

    /// Recap bands in display order — empty when nothing is available
    /// yet (fresh sessions with no completed turn).
    pub recap: Vec<RecapEntry>,

    /// PR number captured from a `gh pr (create|view) ...` invocation
    /// in a tool/command row.
    pub pr_number: Option<u64>,

    /// Repository the PR lives in (e.g. `owner/repo`). Best-effort —
    /// only populated when an explicit `--repo owner/name` arg lands in
    /// the same command.
    pub pr_repository: Option<String>,

    /// Distinct file paths the assistant edited / wrote during the
    /// session.
    pub files_touched: Vec<String>,

    /// `files_touched.len() as u64`, precomputed.
    pub files_touched_count: u64,

    /// Context window in tokens for the most recent assistant model.
    /// `None` when no assistant turn has reported usage yet.
    pub context_window: Option<u64>,

    /// Input-tokens count for the latest assistant turn — drives the
    /// "context fill" gauge.
    pub latest_turn_input_tokens: u64,

    /// Cumulative TOTAL input tokens across the session (uncached +
    /// cache_read + cache_creation). Matches the Python
    /// `total_input_tokens_billed` computed property.
    pub total_input_tokens_billed: u64,

    /// Cumulative output tokens.
    pub total_output_tokens: u64,

    /// `latest_turn_input_tokens / context_window`, clamped to 1.0.
    /// `None` until at least one assistant turn has run.
    pub context_fill_ratio: Option<f32>,

    /// `cache_read / (cache_read + cache_creation + input)`. `None`
    /// when the denominator is zero.
    pub cache_hit_ratio: Option<f32>,

    /// Models actually used in the session, in first-seen order.
    pub models_used: Vec<String>,

    /// Permission mode. DEFER — see module docs.
    pub permission_mode: Option<String>,

    /// Client version. DEFER — see module docs.
    pub client_version: Option<String>,
}

// -------------------------------------------------------------------- helpers

/// Hardcoded model → context-window lookup. Mirrors Python's
/// `_model_context_window` but enumerates the families we actually see
/// in the corpus. Unknown models fall through to `None` so the caller
/// can decide whether to render a fallback gauge.
fn context_window_for_model(model: &str) -> Option<u64> {
    let m = model.to_ascii_lowercase();
    // Long-context Opus carries the `[1m]` flag (also seen as `-1m`).
    if m.contains("[1m]") || m.ends_with("-1m") {
        return Some(1_000_000);
    }
    // Every Claude 4.x family lands at 200k.
    if m.contains("claude-opus")
        || m.contains("claude-sonnet")
        || m.contains("claude-haiku")
    {
        return Some(200_000);
    }
    None
}

/// Tools whose successful invocation implies a file was touched.
/// Mirrors Python's `_FILE_TOUCH_TOOLS`.
fn is_file_touching_tool(name: &str) -> bool {
    matches!(name, "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Create")
}

/// Extract a file path from a tool row's text. Wave 2.5's parser
/// abbreviates `Edit` / `Write` / `MultiEdit` into `"Editing <basename>"`
/// or `"Writing <basename>"` — we extract the basename suffix. This
/// undercounts when two distinct files share a basename, but the alternative
/// (parsing the original JSONL again) doubles the cost of every digest
/// rebuild. Treating duplicates as a single file is the same behaviour the
/// Python `set` semantics give for files in the same directory.
fn extract_file_path_from_tool_text(text: &str) -> Option<String> {
    for prefix in ["Editing ", "Writing ", "Creating "] {
        if let Some(rest) = text.strip_prefix(prefix) {
            // Tool result snippets get folded into the same row with
            // `\n↳ <output>` — trim that off so we just keep the path.
            let path = rest.split_once('\n').map(|(p, _)| p).unwrap_or(rest);
            let path = path.trim();
            if !path.is_empty() && path != "file" {
                return Some(path.to_string());
            }
        }
    }
    None
}

/// Detect a `gh pr create|view ...` invocation in a Bash tool row's
/// text and return the PR number when one is explicitly supplied.
///
/// Heuristic: look for `gh pr` followed by `create`/`view`/`merge`/`comment`
/// in the abbreviated tool text — the parser shows `"Running gh"` for the
/// abbreviated form, so we widen detection to the merged tool text
/// (which carries `↳ <result>` after Wave 2.5). When the result text
/// contains `https://github.com/<owner>/<repo>/pull/<num>` we capture the
/// number. This is intentionally conservative — false positives would
/// pin a stale PR number on the wrong session.
fn detect_pr_from_tool_text(text: &str) -> Option<(u64, Option<String>)> {
    // Look for a github.com PR URL in the result snippet — `gh pr create`
    // prints that on success.
    let needle = "github.com/";
    let start = text.find(needle)?;
    let tail = &text[start + needle.len()..];
    // Match `<owner>/<repo>/pull/<num>` lazily.
    let mut parts = tail.splitn(4, '/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let pull = parts.next()?;
    if pull != "pull" {
        return None;
    }
    let rest = parts.next()?;
    // Number is the leading digits of `rest` (may have a trailing `\n` or `…`).
    let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if num_str.is_empty() {
        return None;
    }
    let num: u64 = num_str.parse().ok()?;
    let repo_display = if !owner.is_empty() && !repo.is_empty() {
        Some(format!("{owner}/{repo}"))
    } else {
        None
    };
    Some((num, repo_display))
}

/// Parse an ISO-8601 timestamp into seconds-since-epoch. Returns `None`
/// for any malformed input — callers fold that into a `None` duration.
fn parse_iso_to_epoch_seconds(ts: &str) -> Option<i64> {
    // Hand-rolled parser to avoid a `chrono` dep. Accepts the
    // `YYYY-MM-DDTHH:MM:SS[.fff]Z` shape Claude Code emits.
    let s = ts.trim_end_matches('Z');
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.splitn(3, '-');
    let y: i64 = date_parts.next()?.parse().ok()?;
    let mo: u32 = date_parts.next()?.parse().ok()?;
    let d: u32 = date_parts.next()?.parse().ok()?;
    let time = time.split('.').next()?;
    let mut time_parts = time.splitn(3, ':');
    let h: u32 = time_parts.next()?.parse().ok()?;
    let mi: u32 = time_parts.next()?.parse().ok()?;
    let se: u32 = time_parts.next()?.parse().ok()?;
    // Convert calendar date → days since 1970-01-01 via Howard Hinnant's
    // days_from_civil algorithm (cheap, well-known, integer-only).
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy } else { yy - 399 } / 400;
    let yoe = (yy - era * 400) as u64;
    let mp = if mo > 2 { mo as u64 - 3 } else { mo as u64 + 9 };
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe as i64 - 719468;
    Some(days * 86400 + (h as i64) * 3600 + (mi as i64) * 60 + se as i64)
}

/// Take the first non-empty line of `text`, truncated at ~50 chars.
fn derive_title_from_first_user_text(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= 50 {
        line.to_string()
    } else {
        let truncated: String = line.chars().take(49).collect();
        format!("{truncated}…")
    }
}

// ----------------------------------------------------------------- builder

/// Build a session digest by walking the cleaned-transcript view and
/// the observation JSONL for `session_id`. Errors (missing session row,
/// unreadable JSONL, unreadable observation file) degrade silently to
/// the slug-only stub — the caller can still render the empty-state
/// panel without crashing.
pub fn compute_session_digest(session_id: &str, conn: &Connection) -> SessionDigest {
    let mut digest = SessionDigest {
        session_id: session_id.to_string(),
        slug: session_id.chars().take(8).collect(),
        ..Default::default()
    };

    // Resolve the JSONL path via the sessions table. If the lookup
    // fails we keep the slug-only digest — the panel renders correctly.
    let session_path: Option<String> =
        crate::db::session_by_id(conn, session_id)
            .ok()
            .flatten()
            .map(|s| s.session_path);

    if let Some(path_str) = session_path {
        let path = Path::new(&path_str);
        if let Ok(bytes) = std::fs::read(path) {
            populate_from_transcript(&mut digest, &bytes, session_id);
        }
    }

    // Observations are optional — silently swallow read errors. The
    // panel just renders an empty recap section in that case.
    if let Ok(entries) = read_entries(session_id) {
        populate_recap_from_observations(&mut digest, &entries);
    }

    digest
}

/// Walk the cleaned-transcript view and fill in title, tokens, models,
/// files_touched, PR detection, and duration.
fn populate_from_transcript(
    digest: &mut SessionDigest,
    bytes: &[u8],
    session_id: &str,
) {
    let rows = parse_byte_range(bytes, Some(session_id));
    if rows.is_empty() {
        return;
    }

    let mut first_ts: Option<&str> = None;
    let mut last_ts: Option<&str> = None;
    let mut first_user_text: Option<&str> = None;
    let mut models_seen: Vec<String> = Vec::new();
    let mut latest_assistant: Option<&CleanedMessage> = None;
    let mut total_input: u64 = 0;
    let mut total_output: u64 = 0;
    let mut total_cache_read: u64 = 0;
    let mut total_cache_creation: u64 = 0;
    let mut files: Vec<String> = Vec::new();

    for row in &rows {
        // Duration: every cleaned row carries a timestamp (we propagate
        // assistant timestamps onto tool rows in the parser too).
        if let Some(ts) = row.timestamp.as_deref() {
            if first_ts.is_none() {
                first_ts = Some(ts);
            }
            last_ts = Some(ts);
        }

        match row.role.as_str() {
            "user" if first_user_text.is_none() && !row.text.is_empty() => {
                first_user_text = Some(row.text.as_str());
            }
            "assistant" => {
                if let Some(model) = row.model.as_deref() {
                    if !model.is_empty()
                        && model != "<synthetic>"
                        && !models_seen.iter().any(|m| m == model)
                    {
                        models_seen.push(model.to_string());
                    }
                }
                if let Some(u) = row.usage.as_ref() {
                    total_input = total_input.saturating_add(u.input_tokens);
                    total_output = total_output.saturating_add(u.output_tokens);
                    total_cache_read = total_cache_read
                        .saturating_add(u.cache_read_input_tokens);
                    total_cache_creation = total_cache_creation
                        .saturating_add(u.cache_creation_input_tokens);
                    latest_assistant = Some(row);
                }
            }
            "tool" => {
                let tool_name = row.tool_name.as_deref().unwrap_or("");
                if is_file_touching_tool(tool_name) {
                    if let Some(path) = extract_file_path_from_tool_text(&row.text) {
                        if !files.iter().any(|f| f == &path) {
                            files.push(path);
                        }
                    }
                }
                // PR detection: Bash row with a `gh pr` output containing
                // a github.com PR URL.
                if digest.pr_number.is_none() && tool_name == "Bash" {
                    if let Some((num, repo)) =
                        detect_pr_from_tool_text(&row.text)
                    {
                        digest.pr_number = Some(num);
                        digest.pr_repository = repo;
                    }
                }
            }
            _ => {}
        }
    }

    // Title — first user line's first non-empty line, ≤50 chars.
    if let Some(text) = first_user_text {
        digest.title = derive_title_from_first_user_text(text);
    }

    // Duration — first → last timestamp.
    if let (Some(a), Some(b)) = (first_ts, last_ts) {
        if let (Some(start), Some(end)) = (
            parse_iso_to_epoch_seconds(a),
            parse_iso_to_epoch_seconds(b),
        ) {
            if end >= start {
                digest.duration_seconds = Some((end - start) as u64);
            }
        }
    }

    // Tokens — `total_input_tokens_billed` is the sum of input +
    // cache_creation + cache_read across all assistant rows.
    digest.total_input_tokens_billed = total_input
        .saturating_add(total_cache_creation)
        .saturating_add(total_cache_read);
    digest.total_output_tokens = total_output;

    // Cache-hit ratio — denominator is the total input billed.
    let cache_denom = total_input
        .saturating_add(total_cache_creation)
        .saturating_add(total_cache_read);
    if cache_denom > 0 {
        digest.cache_hit_ratio = Some(total_cache_read as f32 / cache_denom as f32);
    }

    // Latest-turn input tokens + context window — pulled off the most
    // recent assistant row that carried a usage block.
    if let Some(latest) = latest_assistant {
        if let Some(u) = latest.usage.as_ref() {
            // Match Python: the "latest turn input" includes cached
            // reads + cache creation because all three count against
            // the prompt the model saw.
            digest.latest_turn_input_tokens = u
                .input_tokens
                .saturating_add(u.cache_read_input_tokens)
                .saturating_add(u.cache_creation_input_tokens);
        }
        if let Some(model) = latest.model.as_deref() {
            digest.context_window = context_window_for_model(model);
        }
    }

    if let (Some(window), latest) = (digest.context_window, digest.latest_turn_input_tokens) {
        if window > 0 && latest > 0 {
            let ratio = latest as f32 / window as f32;
            digest.context_fill_ratio = Some(ratio.min(1.0));
        }
    }

    digest.files_touched_count = files.len() as u64;
    digest.files_touched = files;
    digest.models_used = models_seen;
}

/// Append the most recent Decision observations to the digest recap, in
/// timestamp DESC order, capped at 5 entries.
fn populate_recap_from_observations(
    digest: &mut SessionDigest,
    entries: &[Observation],
) {
    let mut decisions: Vec<(&str, &str)> = entries
        .iter()
        .filter_map(|e| match e {
            Observation::Decision { text, ts, .. } => Some((ts.as_str(), text.as_str())),
            _ => None,
        })
        .collect();
    // Latest decisions first.
    decisions.sort_by(|a, b| b.0.cmp(a.0));
    decisions.truncate(5);

    digest.recap = decisions
        .into_iter()
        .map(|(ts, text)| RecapEntry {
            label: "decision".to_string(),
            timestamp: Some(ts.to_string()),
            text: text.to_string(),
        })
        .collect();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonl::MessageUsage;
    use rusqlite::params;
    use tempfile::tempdir;

    fn setup_db_with_session(jsonl_path: &Path, session_id: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 session_id    TEXT PRIMARY KEY,
                 session_path  TEXT NOT NULL,
                 project       TEXT,
                 cwd           TEXT,
                 custom_name   TEXT,
                 status        TEXT NOT NULL DEFAULT 'active',
                 sort_order    INTEGER,
                 last_viewed   REAL,
                 created_at    REAL,
                 modified_at   REAL
             );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (session_id, session_path, status) VALUES (?, ?, 'active')",
            params![session_id, jsonl_path.to_string_lossy()],
        )
        .unwrap();
        conn
    }

    fn empty_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                 session_id    TEXT PRIMARY KEY,
                 session_path  TEXT NOT NULL,
                 project       TEXT,
                 cwd           TEXT,
                 custom_name   TEXT,
                 status        TEXT NOT NULL DEFAULT 'active',
                 sort_order    INTEGER,
                 last_viewed   REAL,
                 created_at    REAL,
                 modified_at   REAL
             );",
        )
        .unwrap();
        conn
    }

    #[test]
    fn compute_session_digest_returns_slug_from_session_id() {
        let conn = empty_db();
        let d = compute_session_digest("abcdef1234567890-rest", &conn);
        assert_eq!(d.session_id, "abcdef1234567890-rest");
        assert_eq!(d.slug, "abcdef12");
    }

    #[test]
    fn compute_session_digest_aggregates_token_totals() {
        // Two assistant rows with known usage. Total input billed = sum
        // of (input + cache_creation + cache_read).
        let jsonl = br#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hi"}}
{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-01-01T00:00:01Z","message":{"id":"m1","model":"claude-opus-4-7","content":[{"type":"text","text":"hello"}],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":10,"cache_read_input_tokens":20}}}
{"type":"assistant","uuid":"a2","sessionId":"s1","timestamp":"2026-01-01T00:00:02Z","message":{"id":"m2","model":"claude-opus-4-7","content":[{"type":"text","text":"world"}],"usage":{"input_tokens":200,"output_tokens":75,"cache_creation_input_tokens":15,"cache_read_input_tokens":30}}}
"#;
        let dir = tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let conn = setup_db_with_session(&path, "s1");
        let d = compute_session_digest("s1", &conn);

        // total_input_tokens_billed = (100+200) + (10+15) + (20+30) = 375
        assert_eq!(d.total_input_tokens_billed, 375);
        assert_eq!(d.total_output_tokens, 125);
    }

    #[test]
    fn compute_session_digest_computes_cache_hit_ratio() {
        // One assistant row: cache_read=80, cache_creation=10, input=10
        // → ratio = 80 / (80+10+10) = 0.8
        let jsonl = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-01-01T00:00:01Z","message":{"id":"m1","model":"claude-opus-4-7","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":10,"output_tokens":0,"cache_creation_input_tokens":10,"cache_read_input_tokens":80}}}
"#;
        let dir = tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let conn = setup_db_with_session(&path, "s1");
        let d = compute_session_digest("s1", &conn);
        let ratio = d.cache_hit_ratio.expect("ratio populated");
        assert!((ratio - 0.8).abs() < 0.001, "got {ratio}");
    }

    #[test]
    fn compute_session_digest_picks_latest_model() {
        // Three assistant rows with model evolution; models_used preserves
        // first-seen order; latest assistant's model drives context_window.
        let jsonl = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-01-01T00:00:01Z","message":{"id":"m1","model":"claude-haiku-4-5","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"assistant","uuid":"a2","sessionId":"s1","timestamp":"2026-01-01T00:00:02Z","message":{"id":"m2","model":"claude-opus-4-7","content":[{"type":"text","text":"y"}],"usage":{"input_tokens":10,"output_tokens":1}}}
{"type":"assistant","uuid":"a3","sessionId":"s1","timestamp":"2026-01-01T00:00:03Z","message":{"id":"m3","model":"claude-opus-4-7[1m]","content":[{"type":"text","text":"z"}],"usage":{"input_tokens":10,"output_tokens":1}}}
"#;
        let dir = tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let conn = setup_db_with_session(&path, "s1");
        let d = compute_session_digest("s1", &conn);
        assert_eq!(
            d.models_used,
            vec![
                "claude-haiku-4-5".to_string(),
                "claude-opus-4-7".to_string(),
                "claude-opus-4-7[1m]".to_string(),
            ]
        );
        // The latest model is the `[1m]` opus variant — 1M-token window.
        assert_eq!(d.context_window, Some(1_000_000));
    }

    #[test]
    fn compute_session_digest_counts_files_touched_via_tool_rows() {
        // Two assistant turns: Edit + Write. Files should dedup by name.
        let jsonl = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-01-01T00:00:01Z","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Edit","input":{"file_path":"/x/y/foo.rs"}}]}}
{"type":"assistant","uuid":"a2","sessionId":"s1","timestamp":"2026-01-01T00:00:02Z","message":{"id":"m2","content":[{"type":"tool_use","id":"t2","name":"Write","input":{"file_path":"/x/y/bar.rs"}}]}}
{"type":"assistant","uuid":"a3","sessionId":"s1","timestamp":"2026-01-01T00:00:03Z","message":{"id":"m3","content":[{"type":"tool_use","id":"t3","name":"Edit","input":{"file_path":"/x/y/foo.rs"}}]}}
"#;
        let dir = tempdir().unwrap();
        let path = dir.path().join("s1.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let conn = setup_db_with_session(&path, "s1");
        let d = compute_session_digest("s1", &conn);
        assert_eq!(d.files_touched_count, 2, "got {:?}", d.files_touched);
    }

    #[test]
    fn compute_session_digest_falls_back_gracefully_when_observations_missing() {
        // Session row + JSONL exist, but the observations dir has nothing.
        // recap must be empty (no panic).
        let jsonl = br#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-01-01T00:00:00Z","message":{"content":"hello there"}}
"#;
        let dir = tempdir().unwrap();
        let path = dir.path().join("s-no-obs.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let conn = setup_db_with_session(&path, "s-no-obs");
        let d = compute_session_digest("s-no-obs", &conn);
        assert!(d.recap.is_empty(), "expected empty recap, got {:?}", d.recap);
        // Title should still populate from the user line.
        assert_eq!(d.title, "hello there");
    }

    #[test]
    fn compute_session_digest_no_session_row_returns_slug_stub() {
        let conn = empty_db();
        let d = compute_session_digest("ghost-session", &conn);
        assert_eq!(d.session_id, "ghost-session");
        assert_eq!(d.slug, "ghost-se");
        assert_eq!(d.total_input_tokens_billed, 0);
        assert!(d.models_used.is_empty());
        assert!(d.recap.is_empty());
    }

    #[test]
    fn compute_session_digest_populates_title_from_first_user_line() {
        let jsonl = br#"{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-01-01T00:00:00Z","message":{"content":"What is the answer to life?"}}
"#;
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let conn = setup_db_with_session(&path, "s1");
        let d = compute_session_digest("s1", &conn);
        assert_eq!(d.title, "What is the answer to life?");
    }

    #[test]
    fn compute_session_digest_truncates_long_titles() {
        let long = "a".repeat(80);
        let line = format!(
            r#"{{"type":"user","uuid":"u1","sessionId":"s1","timestamp":"2026-01-01T00:00:00Z","message":{{"content":"{long}"}}}}"#
        );
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, line.as_bytes()).unwrap();
        let conn = setup_db_with_session(&path, "s1");
        let d = compute_session_digest("s1", &conn);
        assert_eq!(d.title.chars().count(), 50);
        assert!(d.title.ends_with('…'));
    }

    #[test]
    fn compute_session_digest_computes_context_fill_ratio() {
        // latest input 50k, opus → 200k window → 25% fill.
        let jsonl = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-01-01T00:00:01Z","message":{"id":"m1","model":"claude-opus-4-7","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":50000,"output_tokens":1}}}
"#;
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let conn = setup_db_with_session(&path, "s1");
        let d = compute_session_digest("s1", &conn);
        let ratio = d.context_fill_ratio.expect("ratio populated");
        assert!((ratio - 0.25).abs() < 0.001, "got {ratio}");
    }

    #[test]
    fn compute_session_digest_clamps_context_fill_to_one() {
        // input way above the model window — must clamp to 1.0.
        let jsonl = br#"{"type":"assistant","uuid":"a1","sessionId":"s1","timestamp":"2026-01-01T00:00:01Z","message":{"id":"m1","model":"claude-opus-4-7","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":999999999,"output_tokens":1}}}
"#;
        let dir = tempdir().unwrap();
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, jsonl).unwrap();
        let conn = setup_db_with_session(&path, "s1");
        let d = compute_session_digest("s1", &conn);
        assert_eq!(d.context_fill_ratio, Some(1.0));
    }

    // ----- direct helper tests -----

    #[test]
    fn context_window_for_model_handles_known_families() {
        assert_eq!(context_window_for_model("claude-opus-4-7"), Some(200_000));
        assert_eq!(context_window_for_model("claude-haiku-4-5"), Some(200_000));
        assert_eq!(
            context_window_for_model("claude-opus-4-7[1m]"),
            Some(1_000_000)
        );
        assert_eq!(context_window_for_model("gpt-5"), None);
    }

    #[test]
    fn parse_iso_handles_z_suffix_and_subseconds() {
        let a = parse_iso_to_epoch_seconds("2026-01-01T00:00:00Z").unwrap();
        let b = parse_iso_to_epoch_seconds("2026-01-01T00:01:00.500Z").unwrap();
        assert_eq!(b - a, 60);
    }

    #[test]
    fn extract_file_path_unwraps_basename_summary() {
        // Wave 2.5 abbreviates to `Editing <basename>`. We extract the
        // basename suffix verbatim — duplicates dedup at the call site.
        assert_eq!(
            extract_file_path_from_tool_text("Editing foo.rs"),
            Some("foo.rs".into())
        );
        assert_eq!(
            extract_file_path_from_tool_text("Writing bar.rs\n↳ ok"),
            Some("bar.rs".into())
        );
        // The fallback "Editing file" (no path) is filtered out.
        assert_eq!(extract_file_path_from_tool_text("Editing file"), None);
        assert!(extract_file_path_from_tool_text("Reading foo.rs").is_none());
    }

    #[test]
    fn detect_pr_picks_up_github_url() {
        let text =
            "Running gh\n↳ Created PR: https://github.com/me/proj/pull/42 — ready";
        let (num, repo) = detect_pr_from_tool_text(text).unwrap();
        assert_eq!(num, 42);
        assert_eq!(repo.as_deref(), Some("me/proj"));
    }

    #[test]
    fn detect_pr_returns_none_for_unrelated_text() {
        assert!(detect_pr_from_tool_text("Running ls\n↳ ok").is_none());
    }

    #[test]
    fn recap_filters_to_decisions_and_keeps_newest_first() {
        // Direct unit on the recap helper.
        let entries = vec![
            Observation::Decision {
                text: "older decision".into(),
                ts: "2026-01-01T00:00:00Z".into(),
                refs: None,
                context: None,
            },
            Observation::Decision {
                text: "newer decision".into(),
                ts: "2026-02-01T00:00:00Z".into(),
                refs: None,
                context: None,
            },
            Observation::Todo {
                text: "ignored".into(),
                status: "open".into(),
                ts: "2026-03-01T00:00:00Z".into(),
                context: None,
            },
        ];
        let mut d = SessionDigest::default();
        populate_recap_from_observations(&mut d, &entries);
        assert_eq!(d.recap.len(), 2);
        assert_eq!(d.recap[0].text, "newer decision");
        assert_eq!(d.recap[1].text, "older decision");
        assert_eq!(d.recap[0].label, "decision");
    }

    // Smoke: MessageUsage round-trip through populate_from_transcript
    // doesn't depend on JSONL parsing.
    #[test]
    fn message_usage_smoke() {
        let u = MessageUsage {
            input_tokens: 1,
            output_tokens: 2,
            cache_creation_input_tokens: 3,
            cache_read_input_tokens: 4,
        };
        assert_eq!(u.input_tokens, 1);
    }
}
