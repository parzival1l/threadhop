//! macOS process scan — ps/lsof port of `threadhop_core/session/detection.py`.
//!
//! Two layers:
//! - **Pure parsers** ([`parse_claude_process_args`], [`parse_lsof_cwd`],
//!   [`parse_ps_pid_args_table`]) — unit-testable with fixture strings, always
//!   available regardless of the `async` feature.
//! - **Async detector** ([`scan_active`]) — spawns `ps` and `lsof` via
//!   `tokio::process::Command`. Gated behind the `async` feature so the core
//!   crate compiles without tokio for any future sync caller.

#[cfg(feature = "async")]
use crate::error::SessionDetectError;
use crate::paths::claude_projects_dir;
use std::path::PathBuf;

/// What we extracted from a single `ps` args line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedArgs {
    /// Not an interactive `claude` CLI process.
    NotClaude,
    /// Interactive `claude` with no explicit session id (resolve via CWD).
    Interactive,
    /// Interactive `claude --resume <uuid>` with explicit session id.
    Resume(String),
}

/// A live `claude` CLI process and the session id we resolved (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveSession {
    pub pid: u32,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
}

/// Classify a `ps` args line as an interactive `claude` CLI process.
///
/// Port of `_parse_claude_process_args` in `detection.py`. Excludes Claude.app
/// and IDE-embedded `native-binary/` shims, and non-interactive `-p`/`--print`
/// invocations. Returns the explicit `--resume <uuid>` argument when present.
pub fn parse_claude_process_args(args: &str) -> ParsedArgs {
    if args.contains("/Claude.app/") || args.contains("/native-binary/") {
        return ParsedArgs::NotClaude;
    }
    if !args.contains("claude") {
        return ParsedArgs::NotClaude;
    }
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.is_empty() {
        return ParsedArgs::NotClaude;
    }

    let has_p_short = parts.contains(&"-p");
    let has_print = parts.contains(&"--print");

    let mut is_interactive = false;
    let mut explicit_id: Option<String> = None;

    for (i, a) in parts.iter().enumerate() {
        if (*a == "-r" || *a == "--resume") && !has_p_short {
            is_interactive = true;
            if let Some(candidate) = parts.get(i + 1) {
                if candidate.len() == 36 && candidate.matches('-').count() == 4 {
                    explicit_id = Some((*candidate).to_string());
                }
            }
        } else if (*a == "-c" || *a == "--continue") && !has_p_short {
            is_interactive = true;
        }
    }

    if !is_interactive {
        let last = parts[parts.len() - 1];
        let first = parts[0];
        if last == "claude" || (first.ends_with("/claude") && !has_p_short && !has_print) {
            is_interactive = true;
        }
    }

    if !is_interactive {
        return ParsedArgs::NotClaude;
    }
    match explicit_id {
        Some(id) => ParsedArgs::Resume(id),
        None => ParsedArgs::Interactive,
    }
}

/// Extract the CWD from `lsof -Fn` output for a single pid.
///
/// `lsof -Fn` emits a sequence of one-letter-prefixed records; the `n` line
/// after the `cwd` fd holds the working directory path. Mirrors the
/// single-pid scan in `_get_process_cwd`.
pub fn parse_lsof_cwd(output: &str) -> Option<String> {
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix('n') {
            if !rest.is_empty() {
                return Some(rest.to_string());
            }
        }
    }
    None
}

/// Parse `lsof -Fn` output for *multiple* pids into a `(pid, cwd)` list.
///
/// `lsof` emits a `p<pid>` record followed by one or more `n<path>` records;
/// we pair them up. Mirrors the multi-pid scan in
/// `get_active_claude_session_ids`.
pub fn parse_lsof_multi(output: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut current_pid: Option<u32> = None;
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            current_pid = rest.parse().ok();
        } else if let Some(rest) = line.strip_prefix('n') {
            if let Some(pid) = current_pid {
                if !rest.is_empty() {
                    out.push((pid, rest.to_string()));
                }
            }
        }
    }
    out
}

/// Parse `ps -eo pid,args` output into `(pid, args)` rows. Skips the header.
pub fn parse_ps_pid_args_table(output: &str) -> Vec<(u32, String)> {
    let mut rows = Vec::new();
    for (idx, line) in output.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Heuristic: skip header line if first token is non-numeric.
        let mut split = line.splitn(2, char::is_whitespace);
        let pid_str = match split.next() {
            Some(s) => s,
            None => continue,
        };
        let pid: u32 = match pid_str.parse() {
            Ok(p) => p,
            Err(_) => {
                if idx == 0 {
                    continue; // header
                }
                continue;
            }
        };
        let args = split.next().unwrap_or("").trim_start().to_string();
        if args.is_empty() {
            continue;
        }
        rows.push((pid, args));
    }
    rows
}

/// Given a `claude` process CWD, return the most-recently-modified session id
/// in the matching `~/.claude/projects/<encoded>` directory.
///
/// Port of `_resolve_session_id_by_cwd`. The encoded directory name is the
/// CWD with `/` replaced by `-`. `agent-*.jsonl` sidecars are ignored.
pub fn resolve_session_id_by_cwd(cwd: &str) -> Option<String> {
    let encoded = cwd.replace('/', "-");
    let project_path: PathBuf = claude_projects_dir().join(encoded);
    resolve_session_id_in_dir(&project_path)
}

fn resolve_session_id_in_dir(project_path: &std::path::Path) -> Option<String> {
    if !project_path.is_dir() {
        return None;
    }
    let mut best: Option<String> = None;
    let mut best_mtime = std::time::SystemTime::UNIX_EPOCH;
    let entries = std::fs::read_dir(project_path).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };
        if stem.starts_with("agent-") {
            continue;
        }
        let mtime = match entry.metadata().and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if mtime > best_mtime {
            best_mtime = mtime;
            best = Some(stem.to_string());
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Async surface — only compiled with the `async` feature.
// ---------------------------------------------------------------------------

#[cfg(feature = "async")]
mod async_impl {
    use super::*;
    use tokio::process::Command;

    /// Scan every `claude` CLI process on the box and resolve each to an
    /// [`ActiveSession`]. Returns one entry per *process*, even if the same
    /// session id ends up represented twice — callers (the TUI) dedupe.
    ///
    /// Port of `get_active_claude_session_ids`, but richer: we keep pid + cwd
    /// so the TUI can show per-process status, while still surfacing the same
    /// resolved session id set.
    pub async fn scan_active() -> Result<Vec<ActiveSession>, SessionDetectError> {
        // Step 1: ps -eo pid,args
        let ps_out = Command::new("ps")
            .args(["-eo", "pid,args"])
            .output()
            .await?;
        let ps_stdout = String::from_utf8_lossy(&ps_out.stdout);
        let rows = parse_ps_pid_args_table(&ps_stdout);

        let mut interactive: Vec<(u32, Option<String>)> = Vec::new();
        for (pid, args) in rows {
            match parse_claude_process_args(&args) {
                ParsedArgs::NotClaude => continue,
                ParsedArgs::Interactive => interactive.push((pid, None)),
                ParsedArgs::Resume(id) => interactive.push((pid, Some(id))),
            }
        }

        if interactive.is_empty() {
            return Ok(Vec::new());
        }

        // Step 2: resolve CWD for processes without explicit session id.
        let pids_needing_cwd: Vec<u32> = interactive
            .iter()
            .filter_map(|(pid, sid)| if sid.is_none() { Some(*pid) } else { None })
            .collect();

        let mut cwd_map: std::collections::HashMap<u32, String> =
            std::collections::HashMap::new();
        if !pids_needing_cwd.is_empty() {
            let pid_arg = pids_needing_cwd
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let lsof_out = Command::new("lsof")
                .args(["-a", "-d", "cwd", "-p", &pid_arg, "-Fn"])
                .output()
                .await?;
            let lsof_stdout = String::from_utf8_lossy(&lsof_out.stdout);
            for (pid, cwd) in parse_lsof_multi(&lsof_stdout) {
                cwd_map.entry(pid).or_insert(cwd);
            }
        }

        // Step 3: assemble results.
        let mut out = Vec::with_capacity(interactive.len());
        for (pid, sid) in interactive {
            let (session_id, cwd) = match sid {
                Some(id) => (Some(id), None),
                None => {
                    let cwd = cwd_map.get(&pid).cloned();
                    let session_id = cwd.as_deref().and_then(resolve_session_id_by_cwd);
                    (session_id, cwd)
                }
            };
            out.push(ActiveSession {
                pid,
                session_id,
                cwd,
            });
        }

        Ok(out)
    }
}

#[cfg(feature = "async")]
pub use async_impl::scan_active;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_args_rejects_claude_app() {
        assert_eq!(
            parse_claude_process_args("/Applications/Claude.app/Contents/MacOS/Claude"),
            ParsedArgs::NotClaude
        );
    }

    #[test]
    fn parse_args_rejects_native_binary_shim() {
        assert_eq!(
            parse_claude_process_args("/some/path/native-binary/claude"),
            ParsedArgs::NotClaude
        );
    }

    #[test]
    fn parse_args_rejects_non_claude() {
        assert_eq!(parse_claude_process_args("zsh -l"), ParsedArgs::NotClaude);
    }

    #[test]
    fn parse_args_rejects_print_mode() {
        // `claude -p` is non-interactive headless invocation.
        assert_eq!(
            parse_claude_process_args("claude -p --resume abc"),
            ParsedArgs::NotClaude
        );
    }

    #[test]
    fn parse_args_finds_resume_with_uuid() {
        let line = "claude --resume 12345678-1234-1234-1234-123456789012";
        match parse_claude_process_args(line) {
            ParsedArgs::Resume(id) => {
                assert_eq!(id, "12345678-1234-1234-1234-123456789012");
            }
            other => panic!("expected Resume, got {other:?}"),
        }
    }

    #[test]
    fn parse_args_short_resume_flag() {
        let line = "/usr/local/bin/claude -r 12345678-1234-1234-1234-123456789012";
        assert_eq!(
            parse_claude_process_args(line),
            ParsedArgs::Resume("12345678-1234-1234-1234-123456789012".to_string())
        );
    }

    #[test]
    fn parse_args_resume_without_valid_uuid_is_interactive() {
        // `--resume` with a non-UUID arg still marks interactive, but no explicit id.
        let line = "claude --resume not-a-uuid";
        assert_eq!(
            parse_claude_process_args(line),
            ParsedArgs::Interactive
        );
    }

    #[test]
    fn parse_args_continue_is_interactive() {
        assert_eq!(
            parse_claude_process_args("claude --continue"),
            ParsedArgs::Interactive
        );
        assert_eq!(
            parse_claude_process_args("claude -c"),
            ParsedArgs::Interactive
        );
    }

    #[test]
    fn parse_args_bare_claude_is_interactive() {
        assert_eq!(parse_claude_process_args("claude"), ParsedArgs::Interactive);
        assert_eq!(
            parse_claude_process_args("/usr/local/bin/claude"),
            ParsedArgs::Interactive
        );
    }

    #[test]
    fn parse_args_full_path_print_not_interactive() {
        assert_eq!(
            parse_claude_process_args("/usr/local/bin/claude --print foo"),
            ParsedArgs::NotClaude
        );
    }

    #[test]
    fn parse_lsof_extracts_single_cwd() {
        let lsof = "p12345\nfcwd\nn/Users/me/proj\n";
        assert_eq!(parse_lsof_cwd(lsof), Some("/Users/me/proj".to_string()));
    }

    #[test]
    fn parse_lsof_returns_none_when_no_n_line() {
        assert_eq!(parse_lsof_cwd("p123\nfcwd\n"), None);
        assert_eq!(parse_lsof_cwd(""), None);
    }

    #[test]
    fn parse_lsof_multi_pairs_pid_and_cwd() {
        let lsof = "p100\nfcwd\nn/Users/a/one\np200\nfcwd\nn/Users/b/two\n";
        let rows = parse_lsof_multi(lsof);
        assert_eq!(
            rows,
            vec![
                (100, "/Users/a/one".to_string()),
                (200, "/Users/b/two".to_string()),
            ]
        );
    }

    #[test]
    fn parse_lsof_multi_skips_n_without_pid() {
        // A stray `n` line before any `p` line is ignored.
        let lsof = "n/orphan\np55\nn/Users/c/three\n";
        let rows = parse_lsof_multi(lsof);
        assert_eq!(rows, vec![(55, "/Users/c/three".to_string())]);
    }

    #[test]
    fn parse_ps_table_skips_header_and_pulls_pid_args() {
        let sample = "  PID ARGS\n  100 claude --resume abc\n  200 zsh -l\n";
        let rows = parse_ps_pid_args_table(sample);
        assert_eq!(
            rows,
            vec![
                (100, "claude --resume abc".to_string()),
                (200, "zsh -l".to_string()),
            ]
        );
    }

    #[test]
    fn parse_ps_table_handles_empty_and_truncated_lines() {
        let sample = "\n  300 \n  400 claude\n";
        let rows = parse_ps_pid_args_table(sample);
        // The blank-args row is dropped because args is empty after trim.
        assert_eq!(rows, vec![(400, "claude".to_string())]);
    }

    #[test]
    fn resolve_session_id_returns_newest_jsonl_stem() {
        use std::time::{Duration, SystemTime};

        let tmp = tempfile::tempdir().unwrap();
        let old = tmp.path().join("old.jsonl");
        let newer = tmp.path().join("newer.jsonl");
        let agent = tmp.path().join("agent-skip.jsonl");
        std::fs::write(&old, "{}").unwrap();
        std::fs::write(&newer, "{}").unwrap();
        std::fs::write(&agent, "{}").unwrap();

        // Force `old` to be older than `newer`.
        let two_hours_ago = SystemTime::now() - Duration::from_secs(7200);
        let file = std::fs::File::open(&old).unwrap();
        file.set_modified(two_hours_ago).unwrap();

        let sid = resolve_session_id_in_dir(tmp.path()).unwrap();
        assert_eq!(sid, "newer");
    }

    #[test]
    fn resolve_session_id_returns_none_for_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        assert!(resolve_session_id_in_dir(&missing).is_none());
    }

    #[test]
    fn resolve_session_id_ignores_agent_only_dir() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("agent-foo.jsonl"), "{}").unwrap();
        assert!(resolve_session_id_in_dir(tmp.path()).is_none());
    }

    #[cfg(all(feature = "async", target_os = "macos"))]
    #[tokio::test]
    async fn scan_active_runs_without_panicking() {
        // This is a smoke test: it just verifies that the async surface
        // shells out and returns a result without panicking. We do not
        // assert on content because the developer machine may or may not
        // have a claude process running.
        let _ = scan_active().await;
    }
}
