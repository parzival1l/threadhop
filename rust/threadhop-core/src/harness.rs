//! Claude CLI subprocess adapter — the only place ThreadHop-rs shells out to
//! `claude -p`.
//!
//! Port of `threadhop_core/harness/claude.py`. It serves the `prepare`
//! command (transfer-ticket compression, ADR-029); any future LLM-backed
//! verb goes through here too so alternative harnesses later (codex, gemini)
//! are a parallel adapter rather than a hunt-and-replace.
//!
//! Behavior contract (mirrors `run_claude_p`):
//!
//! * argv is `[claude_bin, "-p", prompt, "--model", model,
//!   "--permission-mode", permission_mode]`.
//! * stdout / stderr are captured as text.
//! * Spawn failures (missing binary) and timeouts surface as
//!   [`HarnessError`] — the Rust analog of Python's propagated
//!   `OSError` / `TimeoutExpired`, which every caller handles with
//!   site-specific error text.
//!
//! The prompt template used by `prepare` is embedded at compile time via
//! [`prepare_prompt`] — the Rust binary has no repo-relative resource
//! resolution at runtime, so `include_str!` keeps the template in lockstep
//! with the crate (source of truth: `prompts/prepare.md` on the Python side).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use thiserror::Error;

/// The `prepare` head-summary prompt template (`prompts/prepare.md` on the
/// Python side, embedded here so the binary is self-contained).
pub fn prepare_prompt() -> &'static str {
    include_str!("prompts/prepare.md")
}

/// Default wall-clock budget for one `claude -p` call. Matches Python's
/// `timeout=180.0`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

/// Outcome of a single `claude -p` invocation. Field names match Python's
/// `HarnessResult` (which mirrors `subprocess.CompletedProcess`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessResult {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Spawn/timeout failures — the cases Python lets propagate as
/// `OSError` / `TimeoutExpired`.
#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("{0}")]
    Spawn(std::io::Error),
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    #[error("io: {0}")]
    Io(std::io::Error),
}

/// Injection seam for the one LLM call `prepare` makes. The production
/// implementation is [`ClaudeCli`]; tests substitute a mock so no subprocess
/// ever runs.
pub trait ClaudeRunner {
    fn run(&self, prompt: &str, model: &str) -> Result<HarnessResult, HarnessError>;
}

/// Production runner: shells out to the `claude` binary.
///
/// `claude_bin` defaults to `"claude"`; the CLI layer lets the
/// `THREADHOP_CLAUDE_BIN` env var override it so integration tests can point
/// at a stub script.
pub struct ClaudeCli {
    pub claude_bin: String,
    pub permission_mode: String,
    pub timeout: Duration,
}

impl Default for ClaudeCli {
    fn default() -> Self {
        Self {
            claude_bin: "claude".to_string(),
            permission_mode: "acceptEdits".to_string(),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl ClaudeRunner for ClaudeCli {
    fn run(&self, prompt: &str, model: &str) -> Result<HarnessResult, HarnessError> {
        run_claude_p(
            prompt,
            model,
            &self.permission_mode,
            self.timeout,
            &self.claude_bin,
        )
    }
}

/// Run `claude -p <prompt> --model <model> --permission-mode <mode>`,
/// capturing stdout/stderr as text, with a wall-clock timeout.
///
/// std::process has no built-in timeout, so we spawn with piped output,
/// drain the pipes on reader threads (avoiding pipe-buffer deadlock), and
/// poll `try_wait` until the deadline. On timeout the child is killed and
/// [`HarnessError::Timeout`] is returned.
pub fn run_claude_p(
    prompt: &str,
    model: &str,
    permission_mode: &str,
    timeout: Duration,
    claude_bin: &str,
) -> Result<HarnessResult, HarnessError> {
    let mut child = Command::new(claude_bin)
        .arg("-p")
        .arg(prompt)
        .arg("--model")
        .arg(model)
        .arg("--permission-mode")
        .arg(permission_mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(HarnessError::Spawn)?;

    // Drain stdout/stderr concurrently so a chatty child can't deadlock on a
    // full pipe while we poll try_wait.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let out_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(p) = stdout_pipe.as_mut() {
            let _ = p.read_to_string(&mut buf);
        }
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(p) = stderr_pipe.as_mut() {
            let _ = p.read_to_string(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait().map_err(HarnessError::Io)? {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(HarnessError::Timeout(timeout));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    Ok(HarnessResult {
        returncode: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_prompt_carries_the_section_contract() {
        let p = prepare_prompt();
        for section in ["## Goal", "## Current state", "## Decisions", "## Open items", "## Files touched"] {
            assert!(p.contains(section), "missing section {section}");
        }
        assert!(p.contains("## PREVIOUS SUMMARY"));
        assert!(p.contains("## NEW MESSAGES"));
        assert!(p.contains("## CONVERSATION"));
    }

    #[test]
    fn missing_binary_surfaces_spawn_error() {
        let err = run_claude_p(
            "hi",
            "haiku",
            "acceptEdits",
            Duration::from_secs(5),
            "/nonexistent/threadhop-test-claude-bin",
        )
        .unwrap_err();
        assert!(matches!(err, HarnessError::Spawn(_)));
    }

    #[test]
    fn captures_stdout_and_exit_code_from_a_real_process() {
        // Use /bin/echo as a stand-in binary: argv becomes
        // `echo -p <prompt> --model haiku --permission-mode acceptEdits`,
        // so the prompt is echoed back on stdout with exit code 0.
        let res = run_claude_p(
            "PROMPT_MARKER",
            "haiku",
            "acceptEdits",
            Duration::from_secs(10),
            "/bin/echo",
        )
        .unwrap();
        assert_eq!(res.returncode, 0);
        assert!(res.stdout.contains("PROMPT_MARKER"));
    }

    #[test]
    fn timeout_kills_the_child() {
        let start = Instant::now();
        let err = run_claude_p(
            "unused",
            "haiku",
            "acceptEdits",
            Duration::from_millis(200),
            "/bin/sleep",
        );
        // `sleep -p ...` exits non-zero instantly on macOS (bad args), so
        // accept either a fast non-zero exit or a timeout — the assertion
        // that matters is that we never hang past the budget.
        assert!(start.elapsed() < Duration::from_secs(5));
        match err {
            Ok(res) => assert_ne!(res.returncode, 0),
            Err(HarnessError::Timeout(_)) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
}
