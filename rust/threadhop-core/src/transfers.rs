//! Transfer tickets — business logic for `prepare` / `receive` (ADR-029/033).
//!
//! `threadhop prepare` freezes a session into a markdown *transfer ticket*:
//! one Haiku call compresses the conversation head into a continuation
//! brief; the last N exchanges ride along verbatim (capped by a character
//! budget). `threadhop receive` prints a ticket back out in the target chat
//! — zero LLM, zero DB.
//!
//! Direct port of `threadhop_core/transfers.py`. Kept free of argument
//! parsing so every piece is unit-testable; the thin CLI wrappers live in
//! `threadhop-tui/src/cli/`.
//!
//! Summary caching (ADR-033) lives in the `transfer_state` table — see
//! [`crate::db::upsert_transfer_state`] for the byte-offset definition.
//! Three paths on `prepare`:
//!
//! * no cache row            → summarize the full head (one LLM call)
//! * row + new head content  → merge call: previous summary + only the NEW
//!                             head exchanges (still one LLM call)
//! * row, head unchanged     → reuse `cached_summary`, ZERO LLM calls

use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

use crate::db;
use crate::exchanges::{render_exchanges, Exchange};
use crate::harness::{prepare_prompt, ClaudeRunner, HarnessResult};

/// Note used in place of the LLM summary when the session has no head
/// (everything fits in the verbatim tail).
pub const TOO_SHORT_NOTE: &str =
    "Session too short to summarize — full conversation included verbatim.";

/// Marker inserted when the final tail exchange alone exceeds the budget and
/// its middle is cut out.
pub const TRUNCATION_MARKER: &str = "\n\n[... middle truncated to fit --tail-budget ...]\n\n";

static TICKET_ID_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(tk_)?[0-9a-f]{8}$").unwrap());

// --- Ticket ids / paths ------------------------------------------------------

/// Return a fresh `tk_<8 lowercase hex>` ticket id.
///
/// Randomness comes from `/dev/urandom` (macOS-only project); a time+pid
/// hash is the fallback if the device is unreadable. Python uses
/// `secrets.token_hex(4)` — same 4 random bytes.
pub fn new_ticket_id() -> String {
    let bytes = read_urandom_4().unwrap_or_else(fallback_entropy_4);
    format!(
        "tk_{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

fn read_urandom_4() -> Option<[u8; 4]> {
    use std::io::Read;
    let mut f = std::fs::File::open("/dev/urandom").ok()?;
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn fallback_entropy_4() -> [u8; 4] {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.write_u32(std::process::id());
    let v = h.finish();
    (v as u32).to_le_bytes()
}

/// On-disk path for a ticket id under `transfers_dir`.
pub fn ticket_path(transfers_dir: &Path, ticket_id: &str) -> PathBuf {
    transfers_dir.join(format!("{ticket_id}.md"))
}

/// Resolve a `receive` argument to a ticket file path.
///
/// Accepts `tk_xxxxxxxx`, bare `xxxxxxxx`, or a filesystem path (absolute,
/// relative, or `~`-prefixed). Returns `None` when nothing exists at the
/// resolved location.
pub fn resolve_ticket_path(raw: &str, transfers_dir: &Path) -> Option<PathBuf> {
    let raw = raw.trim();
    if TICKET_ID_RE.is_match(raw) {
        let tid = if raw.starts_with("tk_") {
            raw.to_string()
        } else {
            format!("tk_{raw}")
        };
        let candidate = ticket_path(transfers_dir, &tid);
        return if candidate.is_file() {
            Some(candidate)
        } else {
            None
        };
    }
    // Treat anything else as a path.
    let expanded: PathBuf = if let Some(rest) = raw.strip_prefix("~/") {
        match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => PathBuf::from(raw),
        }
    } else {
        PathBuf::from(raw)
    };
    if expanded.is_file() {
        Some(expanded)
    } else {
        None
    }
}

// --- Head/tail split ----------------------------------------------------------

/// Split into `(head, tail)`: tail = last `tail_n` exchanges (≥ 1).
pub fn split_head_tail(exchanges: &[Exchange], tail_n: usize) -> (Vec<Exchange>, Vec<Exchange>) {
    let tail_n = tail_n.max(1);
    if exchanges.len() <= tail_n {
        return (Vec::new(), exchanges.to_vec());
    }
    let split = exchanges.len() - tail_n;
    (exchanges[..split].to_vec(), exchanges[split..].to_vec())
}

/// Render the tail within `budget` characters.
///
/// Drops oldest tail exchanges first; always keeps at least the final
/// exchange. If that lone final exchange still exceeds the budget, its
/// middle is cut out with [`TRUNCATION_MARKER`]. Returns
/// `(rendered_text, dropped_count)`. Character counts are Unicode scalar
/// values (Python `len(str)` parity), not bytes.
pub fn fit_tail_to_budget(tail: &[Exchange], budget: usize) -> (String, usize) {
    let mut rendered: Vec<String> = tail.iter().map(Exchange::render).collect();
    let mut dropped = 0usize;

    let total = |r: &[String]| -> usize {
        // Two newlines join each block when rendered together.
        r.iter().map(|s| s.chars().count()).sum::<usize>() + 2 * r.len().saturating_sub(1)
    };

    while rendered.len() > 1 && total(&rendered) > budget {
        rendered.remove(0);
        dropped += 1;
    }

    if rendered.len() == 1 && rendered[0].chars().count() > budget {
        let text = &rendered[0];
        let marker_len = TRUNCATION_MARKER.chars().count();
        let keep = budget.saturating_sub(marker_len).max(2) / 2;
        let keep = keep.max(1);
        let head: String = text.chars().take(keep).collect();
        let tail_chars: Vec<char> = text.chars().collect();
        let tail_part: String = tail_chars[tail_chars.len() - keep..].iter().collect();
        rendered[0] = format!("{head}{TRUNCATION_MARKER}{tail_part}");
    }

    (rendered.join("\n\n"), dropped)
}

// --- Head summary (one LLM call max, ADR-033 cache) ---------------------------

/// LLM summarization failed — the message carries the stderr passthrough.
/// The caller must not write a ticket then (exit 1).
#[derive(Debug)]
pub struct SummaryError(pub String);

impl std::fmt::Display for SummaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for SummaryError {}

/// Return the head summary, spending at most one `claude -p` call.
///
/// Consults the `transfer_state` cache: exchanges whose `start_offset` is
/// `>= source_byte_offset` are new since the cached summary. Upserts the
/// cache after any successful LLM call. Diagnostics mirror the Python CLI's
/// stderr lines byte for byte.
pub fn summarize_head(
    conn: &Connection,
    session_id: &str,
    head: &[Exchange],
    model: &str,
    runner: &dyn ClaudeRunner,
) -> Result<String, SummaryError> {
    let template = prepare_prompt();
    let state = db::get_transfer_state(conn, session_id).ok().flatten();

    let prompt = match &state {
        Some(s) if s.cached_summary.as_deref().map(|c| !c.is_empty()).unwrap_or(false) => {
            let cached = s.cached_summary.as_deref().unwrap_or_default();
            let cutoff = s.source_byte_offset;
            let new_head: Vec<Exchange> = head
                .iter()
                .filter(|ex| ex.start_offset >= cutoff)
                .cloned()
                .collect();
            if new_head.is_empty() {
                // Head unchanged since the last prepare — zero LLM calls.
                eprintln!(
                    "threadhop handoff: head unchanged — reusing cached summary (no LLM call)."
                );
                return Ok(cached.to_string());
            }
            eprintln!(
                "threadhop handoff: merging {} new exchange(s) into the cached summary via {model}…",
                new_head.len()
            );
            format!(
                "{template}\n\n## PREVIOUS SUMMARY\n{cached}\n\n## NEW MESSAGES\n{}",
                render_exchanges(&new_head)
            )
        }
        _ => {
            eprintln!(
                "threadhop handoff: summarizing {} exchange(s) via {model}…",
                head.len()
            );
            format!("{template}\n\n## CONVERSATION\n{}", render_exchanges(head))
        }
    };

    let result: HarnessResult = runner
        .run(&prompt, model)
        .map_err(|e| SummaryError(format!("claude -p failed: {e}")))?;
    if result.returncode != 0 || result.stdout.trim().is_empty() {
        let stderr = result.stderr.trim();
        let detail = if stderr.is_empty() {
            format!("exit code {}", result.returncode)
        } else {
            stderr.to_string()
        };
        return Err(SummaryError(format!("claude -p failed: {detail}")));
    }

    let summary = result.stdout.trim().to_string();
    let end_offset = head.last().map(|ex| ex.end_offset).unwrap_or(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    db::upsert_transfer_state(conn, session_id, end_offset, &summary, now)
        .map_err(|e| SummaryError(format!("transfer_state write failed: {e}")))?;
    Ok(summary)
}

// --- Ticket rendering ----------------------------------------------------------

/// Assemble the ticket markdown per the ADR-029 contract. Byte-identical to
/// Python's `build_ticket`.
#[allow(clippy::too_many_arguments)]
pub fn build_ticket(
    ticket_id: &str,
    display_name: &str,
    session_id: &str,
    project: Option<&str>,
    prepared_at: &str,
    summary: &str,
    tail_text: &str,
) -> String {
    format!(
        "# ThreadHop transfer ticket {ticket_id}\n\
         Source: {display_name} ({session_id}) — {project} — prepared {prepared_at}\n\
         \n\
         ## Context summary\n\
         {summary}\n\
         \n\
         ## Recent conversation (verbatim)\n\
         {tail_text}\n",
        project = project.unwrap_or("unknown project"),
    )
}

/// Write a ticket under `transfers_dir` (created on demand).
pub fn write_ticket(
    transfers_dir: &Path,
    ticket_id: &str,
    content: &str,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(transfers_dir)?;
    let path = ticket_path(transfers_dir, ticket_id);
    std::fs::write(&path, content)?;
    Ok(path)
}

/// UTC timestamp in the `%Y-%m-%dT%H:%M:%SZ` shape `prepare` stamps tickets
/// with (mirrors `datetime.now(timezone.utc).strftime(...)`).
pub fn prepared_at_utc() -> String {
    let format = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second]Z"
    );
    time::OffsetDateTime::now_utc()
        .format(&format)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

// --- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::HarnessError;
    use std::cell::RefCell;

    fn ex(user: &str, assistant: &str, start: u64, end: u64) -> Exchange {
        Exchange {
            user_text: Some(user.to_string()),
            assistant_texts: vec![assistant.to_string()],
            first_timestamp: None,
            last_timestamp: None,
            start_offset: start,
            end_offset: end,
        }
    }

    // ---------- ticket ids / paths ----------

    #[test]
    fn new_ticket_id_shape() {
        let id = new_ticket_id();
        assert!(TICKET_ID_RE.is_match(&id), "bad ticket id: {id}");
        assert!(id.starts_with("tk_"));
        assert_ne!(new_ticket_id(), id, "ids should not repeat trivially");
    }

    #[test]
    fn resolve_ticket_accepts_tk_id_bare_hex_and_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_ticket(dir.path(), "tk_ab12cd34", "content").unwrap();

        assert_eq!(
            resolve_ticket_path("tk_ab12cd34", dir.path()).as_deref(),
            Some(path.as_path())
        );
        assert_eq!(
            resolve_ticket_path("ab12cd34", dir.path()).as_deref(),
            Some(path.as_path())
        );
        assert_eq!(
            resolve_ticket_path(path.to_str().unwrap(), dir.path()).as_deref(),
            Some(path.as_path())
        );
        assert!(resolve_ticket_path("tk_00000000", dir.path()).is_none());
        assert!(resolve_ticket_path("/no/such/file.md", dir.path()).is_none());
    }

    // ---------- head/tail split ----------

    #[test]
    fn split_head_tail_keeps_last_n_in_tail() {
        let exs: Vec<Exchange> = (0..5).map(|i| ex(&format!("q{i}"), "a", 0, 0)).collect();
        let (head, tail) = split_head_tail(&exs, 3);
        assert_eq!(head.len(), 2);
        assert_eq!(tail.len(), 3);
        assert_eq!(tail[0].user_text.as_deref(), Some("q2"));
    }

    #[test]
    fn split_head_tail_short_session_all_tail() {
        let exs: Vec<Exchange> = (0..2).map(|i| ex(&format!("q{i}"), "a", 0, 0)).collect();
        let (head, tail) = split_head_tail(&exs, 3);
        assert!(head.is_empty());
        assert_eq!(tail.len(), 2);
    }

    #[test]
    fn split_head_tail_clamps_tail_n_to_one() {
        let exs: Vec<Exchange> = (0..3).map(|i| ex(&format!("q{i}"), "a", 0, 0)).collect();
        let (head, tail) = split_head_tail(&exs, 0);
        assert_eq!(head.len(), 2);
        assert_eq!(tail.len(), 1);
    }

    // ---------- tail budget fitting ----------

    #[test]
    fn fit_tail_within_budget_drops_nothing() {
        let tail = vec![ex("short q", "short a", 0, 0)];
        let (text, dropped) = fit_tail_to_budget(&tail, 8000);
        assert_eq!(dropped, 0);
        assert!(text.contains("short q"));
    }

    #[test]
    fn fit_tail_drops_oldest_first() {
        let tail = vec![
            ex(&"x".repeat(100), "old", 0, 0),
            ex(&"y".repeat(100), "mid", 0, 0),
            ex("newest question", "newest answer", 0, 0),
        ];
        // Budget fits only the final exchange.
        let (text, dropped) = fit_tail_to_budget(&tail, 60);
        assert_eq!(dropped, 2);
        assert!(text.contains("newest question"));
        assert!(!text.contains("old"));
    }

    #[test]
    fn fit_tail_truncates_middle_of_lone_oversized_exchange() {
        let big = format!("START{}{}END", "m".repeat(500), "n".repeat(500));
        let tail = vec![ex(&big, "a", 0, 0)];
        let (text, dropped) = fit_tail_to_budget(&tail, 200);
        assert_eq!(dropped, 0, "the final exchange is never dropped");
        assert!(text.contains(TRUNCATION_MARKER.trim()));
        assert!(text.starts_with("User:\nSTART"));
        // The rendered exchange ends with the assistant block, so the kept
        // tail slice does too.
        assert!(text.ends_with("Assistant:\na"));
        // Result respects the budget (keep*2 + marker <= budget + slack of 1).
        assert!(text.chars().count() <= 200 + TRUNCATION_MARKER.chars().count());
    }

    // ---------- ticket rendering ----------

    #[test]
    fn build_ticket_matches_python_shape() {
        let t = build_ticket(
            "tk_ab12cd34",
            "my-session",
            "sid-1",
            Some("projA"),
            "2026-05-01T00:00:00Z",
            "SUMMARY",
            "TAIL",
        );
        assert_eq!(
            t,
            "# ThreadHop transfer ticket tk_ab12cd34\n\
             Source: my-session (sid-1) — projA — prepared 2026-05-01T00:00:00Z\n\
             \n\
             ## Context summary\n\
             SUMMARY\n\
             \n\
             ## Recent conversation (verbatim)\n\
             TAIL\n"
        );
        let t2 = build_ticket("tk_1", "n", "s", None, "ts", "S", "T");
        assert!(t2.contains("— unknown project —"));
    }

    // ---------- summarize_head cache behavior ----------

    struct MockRunner {
        calls: RefCell<Vec<String>>,
        response: Result<HarnessResult, String>,
    }

    impl MockRunner {
        fn ok(stdout: &str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                response: Ok(HarnessResult {
                    returncode: 0,
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                }),
            }
        }
        fn failing(stderr: &str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                response: Ok(HarnessResult {
                    returncode: 1,
                    stdout: String::new(),
                    stderr: stderr.to_string(),
                }),
            }
        }
    }

    impl ClaudeRunner for MockRunner {
        fn run(&self, prompt: &str, _model: &str) -> Result<HarnessResult, HarnessError> {
            self.calls.borrow_mut().push(prompt.to_string());
            match &self.response {
                Ok(r) => Ok(r.clone()),
                Err(msg) => Err(HarnessError::Spawn(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    msg.clone(),
                ))),
            }
        }
    }

    fn mem_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        db::ensure_transfer_state_table(&c).unwrap();
        c
    }

    #[test]
    fn fresh_head_makes_one_call_and_caches() {
        let conn = mem_conn();
        let head = vec![ex("q1", "a1", 0, 100), ex("q2", "a2", 100, 200)];
        let runner = MockRunner::ok("THE SUMMARY");
        let summary = summarize_head(&conn, "s1", &head, "haiku", &runner).unwrap();
        assert_eq!(summary, "THE SUMMARY");
        assert_eq!(runner.calls.borrow().len(), 1);
        assert!(runner.calls.borrow()[0].contains("## CONVERSATION"));

        let state = db::get_transfer_state(&conn, "s1").unwrap().unwrap();
        assert_eq!(state.source_byte_offset, 200);
        assert_eq!(state.cached_summary.as_deref(), Some("THE SUMMARY"));
    }

    #[test]
    fn unchanged_head_reuses_cache_with_zero_calls() {
        let conn = mem_conn();
        db::upsert_transfer_state(&conn, "s1", 200, "CACHED", 1.0).unwrap();
        let head = vec![ex("q1", "a1", 0, 100), ex("q2", "a2", 100, 200)];
        let runner = MockRunner::ok("SHOULD NOT BE CALLED");
        let summary = summarize_head(&conn, "s1", &head, "haiku", &runner).unwrap();
        assert_eq!(summary, "CACHED");
        assert_eq!(runner.calls.borrow().len(), 0, "cache hit must spend zero LLM calls");
    }

    #[test]
    fn grown_head_makes_merge_call_with_previous_summary() {
        let conn = mem_conn();
        db::upsert_transfer_state(&conn, "s1", 200, "OLD SUMMARY", 1.0).unwrap();
        let head = vec![
            ex("q1", "a1", 0, 100),
            ex("q2", "a2", 100, 200),
            ex("q3 new", "a3 new", 200, 300),
        ];
        let runner = MockRunner::ok("MERGED");
        let summary = summarize_head(&conn, "s1", &head, "haiku", &runner).unwrap();
        assert_eq!(summary, "MERGED");
        let calls = runner.calls.borrow();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains("## PREVIOUS SUMMARY\nOLD SUMMARY"));
        assert!(calls[0].contains("## NEW MESSAGES"));
        assert!(calls[0].contains("q3 new"));
        assert!(
            !calls[0].contains("q1"),
            "already-summarized exchanges must not be re-sent"
        );
        drop(calls);
        let state = db::get_transfer_state(&conn, "s1").unwrap().unwrap();
        assert_eq!(state.source_byte_offset, 300);
        assert_eq!(state.cached_summary.as_deref(), Some("MERGED"));
    }

    #[test]
    fn llm_failure_surfaces_error_and_leaves_cache_untouched() {
        let conn = mem_conn();
        let head = vec![ex("q1", "a1", 0, 100)];
        let runner = MockRunner::failing("model exploded");
        let err = summarize_head(&conn, "s1", &head, "haiku", &runner).unwrap_err();
        assert!(err.to_string().contains("claude -p failed: model exploded"));
        assert!(db::get_transfer_state(&conn, "s1").unwrap().is_none());
    }

    #[test]
    fn empty_stdout_counts_as_failure() {
        let conn = mem_conn();
        let head = vec![ex("q1", "a1", 0, 100)];
        let runner = MockRunner::ok("   ");
        let err = summarize_head(&conn, "s1", &head, "haiku", &runner).unwrap_err();
        assert!(err.to_string().contains("exit code 0"));
    }

    #[test]
    fn prepared_at_shape() {
        let ts = prepared_at_utc();
        let re = Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$").unwrap();
        assert!(re.is_match(&ts), "bad timestamp: {ts}");
    }
}
