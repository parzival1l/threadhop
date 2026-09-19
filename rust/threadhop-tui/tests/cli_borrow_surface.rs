//! Integration tests for the ADR-029 borrow-surface CLI
//! (`peek` / `search` / `prepare` / `receive`).
//!
//! Mirrors the Python subprocess suites (`tests/test_cli_peek.py`,
//! `tests/test_cli_prepare_receive.py`): each test runs the real binary
//! with `HOME` pointed at a tempdir so `~/.claude/projects`,
//! `~/.config/threadhop/sessions.db`, and the transfers dir are all
//! isolated. `prepare` tests stub the `claude` binary via
//! `THREADHOP_CLAUDE_BIN` so no LLM ever runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SID_A: &str = "aaaa1111-1111-1111-1111-111111111111";
const SID_B: &str = "aaab2222-2222-2222-2222-222222222222";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_threadhop-tui")
}

fn user_line(uuid: &str, text: &str, sid: &str, ts: &str) -> String {
    serde_json::json!({
        "type": "user",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": ts,
        "message": {"content": [{"type": "text", "text": text}]},
    })
    .to_string()
}

fn assistant_line(uuid: &str, mid: &str, text: &str, sid: &str, ts: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "uuid": uuid,
        "sessionId": sid,
        "timestamp": ts,
        "message": {"id": mid, "content": [{"type": "text", "text": text}]},
    })
    .to_string()
}

/// Write a session with `n` user/assistant exchanges, mirroring the Python
/// `_write_session` fixture (exchange 1 carries the "alpha" marker).
fn write_session(home: &Path, project: &str, sid: &str, n_exchanges: usize) -> PathBuf {
    let project_dir = home.join(".claude").join("projects").join(project);
    fs::create_dir_all(&project_dir).unwrap();
    let mut lines: Vec<String> = Vec::new();
    for i in 1..=n_exchanges {
        let ts = format!("2026-04-20T10:{i:02}:00Z");
        let marker = if i == 1 {
            "alpha".to_string()
        } else {
            format!("topic{i}")
        };
        lines.push(user_line(
            &format!("u{i}"),
            &format!("question {i} about {marker}"),
            sid,
            &ts,
        ));
        lines.push(assistant_line(
            &format!("a{i}"),
            &format!("m{i}"),
            &format!("reply {i}"),
            sid,
            &ts,
        ));
    }
    let path = project_dir.join(format!("{sid}.jsonl"));
    fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

fn run(home: &Path, args: &[&str]) -> Output {
    run_env(home, args, &[])
}

fn run_env(home: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(args).env("HOME", home);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("spawn threadhop-tui")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

// --- peek ---------------------------------------------------------------

#[test]
fn peek_defaults_to_last_five_exchanges() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);

    let out = run(home, &["peek", SID_A]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(
        so.contains(
            "[From \"aaaa1111\" — -Users-alice-alpha — \
             2026-04-20T10:02:00Z..2026-04-20T10:06:00Z — \
             exchanges 2-6 of 6]"
        ),
        "header mismatch:\n{so}"
    );
    assert!(!so.contains("question 1 about alpha"));
    assert!(so.contains("User:\nquestion 2 about topic2"));
    assert!(so.contains("Assistant:\nreply 6"));
}

#[test]
fn peek_last_n() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);

    let out = run(home, &["peek", SID_A, "--last", "1"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("exchange 6 of 6]"), "{so}");
    assert!(so.contains("question 6"));
    assert!(!so.contains("question 5"));
}

#[test]
fn peek_range_is_one_based_inclusive() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);

    let out = run(home, &["peek", SID_A, "--range", "2:3"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("exchanges 2-3 of 6]"), "{so}");
    assert!(so.contains("question 2"));
    assert!(so.contains("question 3"));
    assert!(!so.contains("question 1"));
    assert!(!so.contains("question 4"));
}

#[test]
fn peek_range_rejects_malformed_spec_exit_2() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);

    let out = run(home, &["peek", SID_A, "--range", "3-5"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("invalid --range"));
}

#[test]
fn peek_grep_prints_matching_exchange_in_full() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);

    // Case-insensitive: uppercase pattern must match lowercase text.
    let out = run(home, &["peek", SID_A, "--grep", "ALPHA"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("exchange 1 of 6]"), "{so}");
    // Exchange-bounded: the whole exchange prints, including the assistant
    // reply that doesn't itself match.
    assert!(so.contains("question 1 about alpha"));
    assert!(so.contains("Assistant:\nreply 1"));
    assert!(!so.contains("question 2"));
}

#[test]
fn peek_grep_no_match_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);

    let out = run(home, &["peek", SID_A, "--grep", "zebrasaurus"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no exchanges match"));
}

#[test]
fn peek_resolves_unique_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);
    write_session(home, "-Users-alice-beta", SID_B, 6);

    let out = run(home, &["peek", "aaaa", "--last", "1"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("[From \"aaaa1111\""));
}

#[test]
fn peek_ambiguous_prefix_exits_2_listing_candidates() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 2);
    write_session(home, "-Users-alice-beta", SID_B, 2);

    let out = run(home, &["peek", "aaa"]);
    assert_eq!(out.status.code(), Some(2));
    let se = stderr(&out);
    assert!(se.contains("is ambiguous"), "{se}");
    assert!(se.contains(SID_A));
    assert!(se.contains(SID_B));
}

#[test]
fn peek_unknown_session_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 2);

    let out = run(home, &["peek", "ffff0000"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no session matches 'ffff0000'"));
}

#[test]
fn peek_last_must_be_positive_exit_2() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 2);

    let out = run(home, &["peek", SID_A, "--last", "0"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--last must be >= 1."));
}

// --- search ---------------------------------------------------------------

/// Seed the shared DB with the messages/FTS schema the Python migrations
/// create, plus a couple of rows the search can hit.
fn seed_search_db(home: &Path) {
    let dir = home.join(".config").join("threadhop");
    fs::create_dir_all(&dir).unwrap();
    let conn = rusqlite::Connection::open(dir.join("sessions.db")).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE sessions (
            session_id   TEXT PRIMARY KEY,
            session_path TEXT,
            project      TEXT,
            cwd          TEXT,
            custom_name  TEXT,
            created_at   REAL,
            modified_at  REAL
        );
        CREATE TABLE messages (
            uuid         TEXT PRIMARY KEY,
            session_id   TEXT NOT NULL,
            role         TEXT NOT NULL,
            text         TEXT NOT NULL,
            timestamp    TEXT,
            cwd          TEXT,
            parent_uuid  TEXT,
            is_sidechain INTEGER NOT NULL DEFAULT 0
        );
        CREATE VIRTUAL TABLE messages_fts USING fts5(
            text,
            content='messages',
            content_rowid='rowid',
            tokenize='porter unicode61'
        );
        CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
            INSERT INTO messages_fts(rowid, text) VALUES (new.rowid, new.text);
        END;
        INSERT INTO sessions (session_id, project, custom_name)
            VALUES ('aaaa1111-1111-1111-1111-111111111111', '-Users-alice-alpha', 'my session');
        INSERT INTO messages (uuid, session_id, role, text, timestamp)
            VALUES ('u1', 'aaaa1111-1111-1111-1111-111111111111', 'user',
                    'let us retry with backoff', '2026-04-20T10:00:00Z');
        INSERT INTO messages (uuid, session_id, role, text, timestamp)
            VALUES ('u2', 'aaaa1111-1111-1111-1111-111111111111', 'assistant',
                    'unrelated content here', '2026-04-20T10:00:01Z');
        "#,
    )
    .unwrap();
}

#[test]
fn search_text_output_carries_hit_and_tip_line() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    seed_search_db(home);

    let out = run(home, &["search", "retry"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let so = stdout(&out);
    assert!(so.contains("aaaa1111  my session  [-Users-alice-alpha]  2026-04-20T10:00:00Z"), "{so}");
    assert!(so.contains("**retry**"), "match should be **highlighted**: {so}");
    assert!(
        so.contains("Tip: threadhop peek <session> --grep 'retry' shows full exchanges."),
        "{so}"
    );
}

#[test]
fn search_json_emits_expected_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    seed_search_db(home);

    let out = run(home, &["search", "retry", "--json"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    let hit = &arr[0];
    assert_eq!(hit["session_id"], SID_A);
    assert_eq!(hit["session_name"], "my session");
    assert_eq!(hit["project"], "-Users-alice-alpha");
    assert_eq!(hit["timestamp"], "2026-04-20T10:00:00Z");
    assert_eq!(hit["uuid"], "u1");
    let snippet = hit["snippet"].as_str().unwrap();
    assert!(snippet.contains("retry"), "{snippet}");
    assert!(!snippet.contains("**"), "json snippet must not carry ** markers");
}

#[test]
fn search_no_matches_prints_notice_exit_0() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    seed_search_db(home);

    let out = run(home, &["search", "zebrasaurus"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout(&out).contains("No matches for 'zebrasaurus'."));
}

#[test]
fn search_project_filter_narrows() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    seed_search_db(home);

    let hit = run(home, &["search", "retry", "--project", "alpha"]);
    assert!(stdout(&hit).contains("aaaa1111"));
    let miss = run(home, &["search", "retry", "--project", "nomatch"]);
    assert!(stdout(&miss).contains("No matches for 'retry'."));
}

#[test]
fn search_works_without_a_db() {
    // Fresh HOME with no sessions.db: exit 0 + no-matches notice, never a
    // SQL error.
    let tmp = tempfile::tempdir().unwrap();
    let out = run(tmp.path(), &["search", "anything"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("No matches for 'anything'."));
}

// --- receive ---------------------------------------------------------------

fn write_ticket(home: &Path, ticket_id: &str, content: &str) -> PathBuf {
    let dir = home.join(".config").join("threadhop").join("transfers");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{ticket_id}.md"));
    fs::write(&path, content).unwrap();
    path
}

#[test]
fn receive_prints_ticket_verbatim_by_tk_id_bare_hex_and_path() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let content = "# ThreadHop transfer ticket tk_ab12cd34\nbody line\n";
    let path = write_ticket(home, "tk_ab12cd34", content);

    for arg in ["tk_ab12cd34", "ab12cd34", path.to_str().unwrap()] {
        let out = run(home, &["receive", arg]);
        assert_eq!(out.status.code(), Some(0), "arg={arg} stderr={}", stderr(&out));
        assert_eq!(stdout(&out), content, "verbatim output for {arg}");
    }
}

#[test]
fn receive_missing_ticket_exits_1_with_lookup_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let out = run(home, &["receive", "tk_00000000"]);
    assert_eq!(out.status.code(), Some(1));
    let se = stderr(&out);
    let expected = format!(
        "Ticket not found: tk_00000000 (looked in {})",
        home.join(".config/threadhop/transfers").display()
    );
    assert!(se.contains(&expected), "got: {se}");
}

// --- prepare ---------------------------------------------------------------

/// Write an executable stub that plays `claude -p`: appends one line to a
/// call log and prints `summary` on stdout.
fn write_claude_stub(dir: &Path, summary: &str, exit_code: i32) -> (PathBuf, PathBuf) {
    let log = dir.join("claude-calls.log");
    let script = dir.join("claude-stub.sh");
    let body = format!(
        "#!/bin/sh\necho call >> {log}\necho \"{summary}\"\nexit {exit_code}\n",
        log = log.display(),
    );
    fs::write(&script, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    (script, log)
}

fn call_count(log: &Path) -> usize {
    fs::read_to_string(log)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn prepare_writes_ticket_and_receive_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);
    let (stub, log) = write_claude_stub(home, "STUB HEAD SUMMARY", 0);

    let out = run_env(
        home,
        &["handoff", "--session", SID_A],
        &[("THREADHOP_CLAUDE_BIN", stub.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(call_count(&log), 1, "exactly one LLM call");
    let se = stderr(&out);
    assert!(
        se.contains("threadhop handoff: summarizing 3 exchange(s) via haiku…"),
        "{se}"
    );

    let so = stdout(&out);
    let mut lines = so.lines();
    let ticket_path = lines.next().unwrap();
    assert!(ticket_path.ends_with(".md"), "{ticket_path}");
    let paste = lines.next().unwrap();
    assert!(
        paste.starts_with("Paste in the target chat: !threadhop receive tk_"),
        "{paste}"
    );
    assert!(lines.next().unwrap().trim_start().starts_with("(or: threadhop receive tk_"));

    // Ticket content follows the ADR-029 shape: summary + verbatim tail
    // (last 3 exchanges by default).
    let content = fs::read_to_string(ticket_path).unwrap();
    assert!(content.contains("# ThreadHop transfer ticket tk_"));
    assert!(content.contains(&format!("({SID_A}) — -Users-alice-alpha — prepared ")));
    assert!(content.contains("## Context summary\nSTUB HEAD SUMMARY"));
    assert!(content.contains("## Recent conversation (verbatim)"));
    assert!(content.contains("question 4"));
    assert!(content.contains("question 6"));
    assert!(!content.contains("question 3"), "head exchange leaked into tail");

    // Round trip through receive using the printed ticket id.
    let ticket_id = paste.rsplit(' ').next().unwrap();
    let rec = run(home, &["receive", ticket_id]);
    assert_eq!(rec.status.code(), Some(0));
    assert_eq!(stdout(&rec), content);
}

#[test]
fn prepare_reuses_cache_with_zero_llm_calls_when_head_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);
    let (stub, log) = write_claude_stub(home, "CACHED SUMMARY", 0);
    let env: &[(&str, &str)] = &[("THREADHOP_CLAUDE_BIN", stub.to_str().unwrap())];

    let first = run_env(home, &["handoff", "--session", SID_A], env);
    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    assert_eq!(call_count(&log), 1);

    let second = run_env(home, &["handoff", "--session", SID_A], env);
    assert_eq!(second.status.code(), Some(0), "stderr: {}", stderr(&second));
    assert_eq!(call_count(&log), 1, "unchanged head must reuse the cache");
    assert!(
        stderr(&second).contains(
            "threadhop handoff: head unchanged — reusing cached summary (no LLM call)."
        ),
        "{}",
        stderr(&second)
    );
    assert!(
        fs::read_to_string(stdout(&second).lines().next().unwrap())
            .unwrap()
            .contains("CACHED SUMMARY")
    );
}

#[test]
fn prepare_llm_failure_writes_no_ticket_and_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);
    let (stub, _log) = write_claude_stub(home, "", 1);

    let out = run_env(
        home,
        &["handoff", "--session", SID_A],
        &[("THREADHOP_CLAUDE_BIN", stub.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("threadhop handoff: claude -p failed:"), "{}", stderr(&out));
    // No ticket written.
    let transfers = home.join(".config/threadhop/transfers");
    let count = fs::read_dir(&transfers)
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert_eq!(count, 0, "no ticket may be written on LLM failure");
}

#[test]
fn prepare_short_session_skips_llm_entirely() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 2); // <= tail (3) → no head
    let (stub, log) = write_claude_stub(home, "SHOULD NOT RUN", 0);

    let out = run_env(
        home,
        &["handoff", "--session", SID_A],
        &[("THREADHOP_CLAUDE_BIN", stub.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(call_count(&log), 0, "short session must not call the LLM");
    assert!(
        stderr(&out).contains("session too short to summarize"),
        "{}",
        stderr(&out)
    );
    let ticket = fs::read_to_string(stdout(&out).lines().next().unwrap()).unwrap();
    assert!(ticket.contains("Session too short to summarize — full conversation included verbatim."));
}

#[test]
fn prepare_missing_transcript_exits_1() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::create_dir_all(home.join(".claude/projects")).unwrap();
    let out = run(home, &["handoff", "--session", "ffff0000-0000-0000-0000-000000000000"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("no transcript found for session"));
}

#[test]
fn prepare_requires_session_flag() {
    // Deliberate deviation from Python (which auto-detects): the Rust CLI
    // has no process-ancestry detection yet, so clap enforces --session.
    let tmp = tempfile::tempdir().unwrap();
    let out = run(tmp.path(), &["prepare"]);
    assert_eq!(out.status.code(), Some(2), "clap usage error");
    assert!(stderr(&out).contains("--session"));
}

#[test]
fn prepare_tail_budget_drops_oldest_tail_exchanges() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    write_session(home, "-Users-alice-alpha", SID_A, 6);
    let (stub, _log) = write_claude_stub(home, "S", 0);

    // Budget only fits the final exchange (~40 chars each rendered).
    let out = run_env(
        home,
        &["handoff", "--session", SID_A, "--tail", "3", "--tail-budget", "60"],
        &[("THREADHOP_CLAUDE_BIN", stub.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("dropped 2 oldest tail exchange(s) to fit --tail-budget 60."),
        "{}",
        stderr(&out)
    );
    let ticket = fs::read_to_string(stdout(&out).lines().next().unwrap()).unwrap();
    assert!(ticket.contains("question 6"));
    assert!(!ticket.contains("question 4"), "dropped tail exchange leaked");
}
