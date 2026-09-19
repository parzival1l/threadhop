//! `threadhop receive` — print a transfer ticket verbatim.
//!
//! Zero LLM, zero DB: the target chat's Claude reads the printed markdown
//! via the `!threadhop receive` bash passthrough and picks the work up.
//! Accepts `tk_xxxxxxxx`, bare `xxxxxxxx`, or a filesystem path.
//!
//! Port of `threadhop_core/cli/commands/receive.py`.

use std::io::Write;

use threadhop_core::{paths, transfers};

/// Locate the ticket and echo its content to stdout.
pub fn cmd_receive(ticket: &str) -> i32 {
    let transfers_dir = paths::transfers_dir();
    let Some(path) = transfers::resolve_ticket_path(ticket, &transfers_dir) else {
        eprintln!(
            "Ticket not found: {ticket} (looked in {})",
            transfers_dir.display()
        );
        return 1;
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            // Verbatim — no trailing newline appended (Python uses
            // sys.stdout.write, not print).
            let mut out = std::io::stdout();
            let _ = out.write_all(content.as_bytes());
            let _ = out.flush();
            0
        }
        Err(e) => {
            eprintln!("Ticket not readable: {} ({e})", path.display());
            1
        }
    }
}
