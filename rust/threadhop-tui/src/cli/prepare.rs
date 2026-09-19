//! `threadhop prepare` — freeze this session into a transfer ticket.
//!
//! One Haiku call (at most — see the ADR-033 cache in
//! [`threadhop_core::transfers`]) summarizes the conversation head; the last
//! `--tail N` exchanges ride along verbatim, capped at `--tail-budget`
//! characters. On success the ticket path and a paste-ready
//! `!threadhop receive tk_…` line go to stdout; every diagnostic goes to
//! stderr. On LLM failure no ticket is written and the exit code is 1.
//!
//! Port of `threadhop_core/cli/commands/prepare.py`, with one deliberate
//! gap: `--session` is REQUIRED. The Python CLI auto-detects the current
//! session by walking the process ancestry for a `claude` CLI ancestor;
//! the Rust port's `session_detect` module only scans for active sessions
//! box-wide and cannot attribute one to *this* terminal yet.
//!
//! `THREADHOP_CLAUDE_BIN` overrides the `claude` binary (integration tests
//! point it at a stub script).

use threadhop_core::exchanges::load_exchanges;
use threadhop_core::harness::ClaudeCli;
use threadhop_core::{db, paths, transfers};

use super::{display_name_and_project, find_session_path};

/// Build and write the transfer ticket for the targeted session.
pub fn cmd_prepare(session: &str, tail: i64, tail_budget: i64, model: &str) -> i32 {
    let projects = paths::claude_projects_dir();
    let short: String = session.chars().take(8).collect();

    let Some(session_path) = find_session_path(&projects, session) else {
        eprintln!(
            "threadhop handoff: no transcript found for session {session} under {}.",
            projects.display()
        );
        return 1;
    };

    let exchanges = load_exchanges(&session_path);
    if exchanges.is_empty() {
        eprintln!(
            "threadhop handoff: session {short} has no user/assistant exchanges to transfer."
        );
        return 1;
    }

    let (head, tail_exs) = transfers::split_head_tail(&exchanges, tail.max(0) as usize);
    let budget = tail_budget.max(0) as usize;
    let (tail_text, dropped) = transfers::fit_tail_to_budget(&tail_exs, budget);
    if dropped > 0 {
        eprintln!(
            "threadhop handoff: dropped {dropped} oldest tail exchange(s) \
             to fit --tail-budget {tail_budget}."
        );
    }

    // Open the shared DB for the transfer_state cache + session metadata.
    // Unlike the Python CLI (whose bootstrap runs migrations), the Rust
    // side only ensures the `transfer_state` table exists — the same
    // idempotent DDL Python's migration 011 issues.
    let conn = match db::open(&paths::db_path()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("threadhop handoff: cannot open {}: {e}", paths::db_path().display());
            return 1;
        }
    };
    if let Err(e) = db::ensure_transfer_state_table(&conn) {
        eprintln!("threadhop handoff: transfer_state setup failed: {e}");
        return 1;
    }

    let (display_name, project) =
        display_name_and_project(Some(&conn), session, Some(&session_path));

    let summary = if head.is_empty() {
        // Short session: everything already rides verbatim in the tail
        // (subject to budget) — skip the LLM call entirely.
        eprintln!(
            "threadhop handoff: session too short to summarize — skipping the LLM call."
        );
        transfers::TOO_SHORT_NOTE.to_string()
    } else {
        let runner = ClaudeCli {
            claude_bin: std::env::var("THREADHOP_CLAUDE_BIN")
                .unwrap_or_else(|_| "claude".to_string()),
            ..Default::default()
        };
        match transfers::summarize_head(&conn, session, &head, model, &runner) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("threadhop handoff: {e}");
                return 1;
            }
        }
    };

    let ticket_id = transfers::new_ticket_id();
    let content = transfers::build_ticket(
        &ticket_id,
        &display_name,
        session,
        project.as_deref(),
        &transfers::prepared_at_utc(),
        &summary,
        &tail_text,
    );
    let transfers_dir = paths::transfers_dir();
    let path = match transfers::write_ticket(&transfers_dir, &ticket_id, &content) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("threadhop handoff: ticket write failed: {e}");
            return 1;
        }
    };

    println!("{}", path.display());
    println!("Paste in the target chat: !threadhop receive {ticket_id}");
    println!("  (or: threadhop receive {ticket_id})");
    0
}
