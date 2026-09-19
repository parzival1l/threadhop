//! `threadhop peek` — print cleaned exchanges from another session.
//!
//! Zero LLM. The unit is the *exchange* (ADR-030): one real user turn plus
//! everything until the next real user turn. Three mutually-exclusive
//! windows: `--last N` (default 5), `--range A:B` (1-based inclusive),
//! `--grep PATTERN` (case-insensitive regex; matching exchanges print in
//! full — the window is exchange-bounded, never a bare line).
//!
//! Port of `threadhop_core/cli/commands/peek.py`; error text and exit codes
//! match byte for byte where practical.

use threadhop_core::exchanges::{load_exchanges, Exchange};
use threadhop_core::paths;

use super::{display_name_and_project, find_session_path, open_cli_db, resolve_session_prefix, session_row};

/// Parse `A:B` into 1-based inclusive ints, or `None` if malformed.
fn parse_range(raw: &str) -> Option<(usize, usize)> {
    let trimmed = raw.trim();
    let (a, b) = trimmed.split_once(':')?;
    if a.is_empty() || b.is_empty() {
        return None;
    }
    if !a.chars().all(|c| c.is_ascii_digit()) || !b.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let a: usize = a.parse().ok()?;
    let b: usize = b.parse().ok()?;
    if a < 1 || b < a {
        return None;
    }
    Some((a, b))
}

/// Human label for the displayed window: `3-7` or `2,5,9`.
fn format_indices(indices: &[usize]) -> String {
    if indices.is_empty() {
        return "-".to_string();
    }
    let contiguous = indices
        .iter()
        .enumerate()
        .all(|(i, &v)| v == indices[0] + i);
    if contiguous {
        if indices[0] == indices[indices.len() - 1] {
            indices[0].to_string()
        } else {
            format!("{}-{}", indices[0], indices[indices.len() - 1])
        }
    } else {
        indices
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Print the source header + the selected exchanges to stdout.
fn print_window(
    exchanges: &[Exchange],
    indices: &[usize],
    display_name: &str,
    project: Option<&str>,
) {
    let window: Vec<&Exchange> = indices.iter().map(|&i| &exchanges[i - 1]).collect();
    let first_ts = window
        .iter()
        .find_map(|ex| ex.first_timestamp.as_deref())
        .unwrap_or("?");
    let last_ts = window
        .iter()
        .rev()
        .find_map(|ex| ex.last_timestamp.as_deref())
        .unwrap_or("?");
    let label = if indices.len() == 1 {
        "exchange"
    } else {
        "exchanges"
    };
    println!(
        "[From \"{display_name}\" — {} — {first_ts}..{last_ts} — {label} {} of {}]",
        project.unwrap_or("unknown project"),
        format_indices(indices),
        exchanges.len(),
    );
    for ex in window {
        println!();
        println!("{}", ex.render());
    }
}

/// Resolve the session ref, pick the window, print it. No LLM.
pub fn cmd_peek(
    session_ref: &str,
    last: Option<i64>,
    range: Option<&str>,
    grep: Option<&str>,
) -> i32 {
    let projects = paths::claude_projects_dir();
    let conn = open_cli_db();

    let matches = resolve_session_prefix(conn.as_ref(), &projects, session_ref);
    if matches.is_empty() {
        eprintln!(
            "threadhop peek: no session matches '{session_ref}' (searched {} and the ThreadHop DB).",
            projects.display()
        );
        return 1;
    }
    if matches.len() > 1 {
        eprintln!(
            "threadhop peek: '{session_ref}' is ambiguous — {} sessions match:",
            matches.len()
        );
        for sid in &matches {
            eprintln!("  {sid}");
        }
        return 2;
    }
    let session_id = &matches[0];
    let short: String = session_id.chars().take(8).collect();

    // Locate the transcript — on-disk first, then the DB row's stored path
    // (mirrors the Python fallback for sessions the scanner recorded whose
    // files moved).
    let mut session_path = find_session_path(&projects, session_id);
    if session_path.is_none() {
        if let Some(row) = session_row(conn.as_ref(), session_id) {
            let candidate = std::path::PathBuf::from(row.session_path);
            if candidate.is_file() {
                session_path = Some(candidate);
            }
        }
    }
    let Some(session_path) = session_path else {
        eprintln!(
            "threadhop peek: no transcript found for session {session_id} under {}.",
            projects.display()
        );
        return 1;
    };

    let (display_name, project) =
        display_name_and_project(conn.as_ref(), session_id, Some(&session_path));

    let exchanges = load_exchanges(&session_path);
    let total = exchanges.len();
    if total == 0 {
        eprintln!(
            "threadhop peek: session {short} has no user/assistant exchanges \
             (may be empty or contain only tool output)."
        );
        return 1;
    }

    // --- Window selection (clap made the modes exclusive) ---
    let indices: Vec<usize> = if let Some(pattern) = grep {
        let compiled = match regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
        {
            Ok(re) => re,
            Err(e) => {
                eprintln!("threadhop peek: invalid --grep regex: {e}");
                return 2;
            }
        };
        let hits: Vec<usize> = exchanges
            .iter()
            .enumerate()
            .filter(|(_, ex)| compiled.is_match(&ex.text()))
            .map(|(i, _)| i + 1)
            .collect();
        if hits.is_empty() {
            eprintln!(
                "threadhop peek: no exchanges match '{pattern}' in session {short} \
                 ({total} exchanges scanned)."
            );
            return 1;
        }
        hits
    } else if let Some(raw) = range {
        let Some((a, b)) = parse_range(raw) else {
            eprintln!(
                "threadhop peek: invalid --range '{raw}' \
                 (expected A:B with 1 <= A <= B, 1-based inclusive)."
            );
            return 2;
        };
        if a > total {
            eprintln!(
                "threadhop peek: --range {raw} is out of bounds — \
                 session has {total} exchange(s)."
            );
            return 1;
        }
        (a..=b.min(total)).collect()
    } else {
        let n = last.unwrap_or(5);
        if n < 1 {
            eprintln!("threadhop peek: --last must be >= 1.");
            return 2;
        }
        let n = n as usize;
        (total.saturating_sub(n).max(0) + 1..=total)
            .collect::<Vec<usize>>()
    };

    print_window(&exchanges, &indices, &display_name, project.as_deref());
    0
}

/// Route through the core exchange text for the module-private helpers'
/// visibility in tests.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_range_accepts_one_based_inclusive() {
        assert_eq!(parse_range("2:3"), Some((2, 3)));
        assert_eq!(parse_range(" 1:1 "), Some((1, 1)));
    }

    #[test]
    fn parse_range_rejects_malformed_specs() {
        assert_eq!(parse_range("3-5"), None);
        assert_eq!(parse_range("0:3"), None);
        assert_eq!(parse_range("5:2"), None);
        assert_eq!(parse_range("a:b"), None);
        assert_eq!(parse_range(":3"), None);
        assert_eq!(parse_range("3:"), None);
    }

    #[test]
    fn format_indices_contiguous_and_sparse() {
        assert_eq!(format_indices(&[]), "-");
        assert_eq!(format_indices(&[4]), "4");
        assert_eq!(format_indices(&[2, 3, 4]), "2-4");
        assert_eq!(format_indices(&[2, 5, 9]), "2,5,9");
    }
}
