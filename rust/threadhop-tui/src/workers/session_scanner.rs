//! session_scanner: Scans `~/.claude/projects/**/*.jsonl` every 5 seconds and
//! emits [`WorkerEvent::SessionsRefreshed`] with a snapshot of sidebar rows.
//!
//! This is the Phase 2 Wave D worker. It mirrors the head-of-file branch of
//! `_gather_session_data` in `threadhop_core/tui/app.py`: walk the Claude
//! projects directory, read just the head metadata from each session JSONL
//! (via `threadhop_core::jsonl::read_session_metadata`), and assemble a
//! `Vec<SessionListItem>` keyed by `last_active_at` descending.
//!
//! Active / working state and richer metrics (turn count, pending tool use)
//! are intentionally **not** computed here — `active_detector` owns the
//! `is_active` channel, and a richer scanner in Phase 5 will compute the
//! whole-file derived fields. This worker only fills the cheap head-scan
//! columns plus `has_observations` (one `stat` call per session).
//!
//! ## Topology
//!
//! ```text
//!     tokio::time::interval(5s)            tokio::task::JoinSet (≤32)
//!            │                                      │
//!            ▼                                      ▼
//!     scan_once(projects_dir) ──► spawn_blocking head-scans ──► Vec<SessionListItem>
//!            │
//!            ▼
//!     tx.send(WorkerEvent::SessionsRefreshed(items))
//! ```
//!
//! Errors during a tick are surfaced as `WorkerEvent::Error(_)`; the loop never
//! exits on its own — only channel closure ends it.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc::Sender;
use tokio::task::JoinSet;
use tokio::time::interval;

use threadhop_core::jsonl::read_session_metadata;
use threadhop_core::paths;

use super::WorkerEvent;
use crate::widgets::session_list::SessionListItem;

/// How long between scans. Mirrors the Python 5-second refresh worker.
const SCAN_INTERVAL: Duration = Duration::from_secs(5);

/// Cap concurrent head-scans so a directory full of sessions does not hammer
/// the filesystem. 32 is empirically well below typical fd limits while still
/// hiding latency on a cold cache.
const MAX_CONCURRENT_SCANS: usize = 32;

/// Worker entry point. Loops forever (until the channel closes), emitting a
/// fresh `SessionsRefreshed` snapshot every `SCAN_INTERVAL`.
///
/// The signature is intentionally narrow — config (`--project`, `--days`) is
/// applied at the consumer side in Wave E. Keeping the worker stateless makes
/// it cheap to swap projects directories under test.
pub async fn run(tx: Sender<WorkerEvent>) -> anyhow::Result<()> {
    let projects_dir = paths::claude_projects_dir();
    let observations_dir = paths::observations_dir();
    let mut ticker = interval(SCAN_INTERVAL);
    // `interval` fires immediately on first `.tick()` — desirable so the
    // sidebar populates before the user sees the empty list.
    loop {
        ticker.tick().await;

        let send_result = match scan_once(&projects_dir, &observations_dir).await {
            Ok(items) => tx.send(WorkerEvent::SessionsRefreshed(items)).await,
            Err(err) => {
                tx.send(WorkerEvent::Error(format!("session_scanner: {err}")))
                    .await
            }
        };

        // Receiver dropped → app is shutting down. Exit cleanly.
        if send_result.is_err() {
            return Ok(());
        }
    }
}

/// One scan pass. Listed pub(crate) so future workers / tests can drive it
/// without standing up the tokio interval loop.
///
/// Walks `projects_dir` (one level deep — Claude Code's on-disk layout is
/// `<projects_dir>/<encoded-project>/<session>.jsonl`, plus the occasional
/// nested `agent-*.jsonl` we exclude), head-scans each session JSONL on a
/// blocking thread pool, then sorts the resulting rows by `last_active_at`
/// descending.
pub(crate) async fn scan_once(
    projects_dir: &Path,
    observations_dir: &Path,
) -> anyhow::Result<Vec<SessionListItem>> {
    let files = list_session_files(projects_dir).await?;
    if files.is_empty() {
        return Ok(Vec::new());
    }

    let total = files.len();
    let mut join_set: JoinSet<Option<SessionListItem>> = JoinSet::new();
    let observations_dir = observations_dir.to_path_buf();
    let mut iter = files.into_iter();

    // Helper: pull one entry off the iterator and spawn its head-scan.
    // Returns `true` when a task was spawned, `false` when the queue
    // drained. Inlined as a macro to sidestep simultaneously holding
    // `&mut JoinSet` and `&mut Iter` across a closure boundary.
    macro_rules! spawn_next {
        () => {{
            if let Some(entry) = iter.next() {
                let obs = observations_dir.clone();
                join_set.spawn_blocking(move || head_scan_file(&entry, &obs));
                true
            } else {
                false
            }
        }};
    }

    // Prime the pool up to MAX_CONCURRENT_SCANS, then top up as tasks
    // finish. `JoinSet::len()` tracks the in-flight count for us so we don't
    // need a parallel counter.
    while join_set.len() < MAX_CONCURRENT_SCANS && spawn_next!() {}

    let mut items: Vec<SessionListItem> = Vec::with_capacity(total);
    while let Some(joined) = join_set.join_next().await {
        match joined {
            Ok(Some(item)) => items.push(item),
            // None = the head-scan failed for this file; skipping mirrors
            // Python's per-file try/except. We deliberately swallow rather
            // than emitting an Error event per file — a missing/empty JSONL
            // is normal mid-write.
            Ok(None) => {}
            Err(join_err) => {
                // Task panic — surface to the caller. This is a real
                // programming error, not a per-file mishap.
                return Err(anyhow::anyhow!("head-scan task panicked: {join_err}"));
            }
        }
        // Top the pool back up. Stops naturally when the iterator drains.
        let _ = spawn_next!();
    }

    sort_by_recency(&mut items);
    Ok(items)
}

/// `<entry, mtime>` pair carried between the directory walk and the
/// head-scan. Bundling them avoids a second `stat()` inside the worker pool.
#[derive(Debug, Clone)]
struct FileEntry {
    path: PathBuf,
    mtime: Option<f64>,
}

/// Walk `<projects_dir>/*/*.jsonl` on the blocking thread pool. Errors at
/// the projects-dir level propagate (the caller surfaces them as
/// `WorkerEvent::Error`); errors on individual subdirectories are skipped
/// — a transient `EACCES` on one project should not blank the whole
/// sidebar.
///
/// The walk runs under `spawn_blocking` rather than `tokio::fs` so the
/// crate doesn't need tokio's `fs` feature flag (kept in lockstep with the
/// existing `Cargo.toml` feature surface).
async fn list_session_files(projects_dir: &Path) -> anyhow::Result<Vec<FileEntry>> {
    let dir = projects_dir.to_path_buf();
    tokio::task::spawn_blocking(move || list_session_files_blocking(&dir))
        .await
        .map_err(|e| anyhow::anyhow!("list_session_files task panicked: {e}"))?
}

/// Synchronous body of [`list_session_files`]. Exposed as a separate fn so
/// the directory walk is also unit-testable without a tokio runtime.
fn list_session_files_blocking(projects_dir: &Path) -> anyhow::Result<Vec<FileEntry>> {
    let mut out: Vec<FileEntry> = Vec::new();

    let top = match std::fs::read_dir(projects_dir) {
        Ok(rd) => rd,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // No `~/.claude/projects` yet → empty sidebar, not an error.
            return Ok(out);
        }
        Err(err) => return Err(err.into()),
    };

    for project_entry in top.flatten() {
        let project_path = project_entry.path();
        let is_dir = project_entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or(false);
        if !is_dir {
            continue;
        }

        let inner = match std::fs::read_dir(&project_path) {
            Ok(rd) => rd,
            Err(_) => continue,
        };

        for session_entry in inner.flatten() {
            let path = session_entry.path();
            if !is_session_jsonl(&path) {
                continue;
            }

            let mtime = session_entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(system_time_to_unix);

            out.push(FileEntry { path, mtime });
        }
    }

    Ok(out)
}

/// Filter for session JSONL files. Mirrors the Python predicate:
/// `*.jsonl` under a project dir, excluding `agent-*.jsonl` which are
/// sub-agent transcripts (not browsable sessions).
fn is_session_jsonl(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
        return false;
    }
    match path.file_name().and_then(|n| n.to_str()) {
        Some(name) if name.starts_with("agent-") => false,
        Some(_) => true,
        None => false,
    }
}

/// Convert a `SystemTime` to fractional unix epoch seconds. Returns `None`
/// for pre-1970 timestamps, which shouldn't occur in practice but would
/// otherwise underflow.
fn system_time_to_unix(t: SystemTime) -> Option<f64> {
    t.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs_f64())
}

/// Sync head-scan invoked on a blocking pool. Returns `None` when the file
/// is unreadable / not a valid JSONL — caller skips silently.
fn head_scan_file(entry: &FileEntry, observations_dir: &Path) -> Option<SessionListItem> {
    let meta = read_session_metadata(&entry.path).ok()?;

    let display_name = meta
        .first_user_text
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            entry
                .path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("session")
                .to_string()
        });

    let observation_path = observations_dir.join(format!("{}.jsonl", meta.session_id));
    let has_observations = observation_path.is_file();

    Some(SessionListItem {
        session_id: meta.session_id,
        display_name,
        has_observations,
        last_active_at: entry.mtime,
        ..Default::default()
    })
}

/// Sort sidebar rows by `last_active_at` descending. Rows missing an mtime
/// sink to the bottom (parity with Python's `key=lambda: ts or 0` ordering).
fn sort_by_recency(items: &mut [SessionListItem]) {
    items.sort_by(|a, b| {
        let av = a.last_active_at.unwrap_or(f64::NEG_INFINITY);
        let bv = b.last_active_at.unwrap_or(f64::NEG_INFINITY);
        // `partial_cmp` is total here — neither value is NaN by construction.
        bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
    });
}

// --- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration as StdDuration;
    use tempfile::tempdir;

    fn write_session_jsonl(dir: &Path, name: &str, session_id: &str, first_user: &str) -> PathBuf {
        let path = dir.join(format!("{name}.jsonl"));
        let line = format!(
            r#"{{"type":"user","uuid":"u1","sessionId":"{session_id}","cwd":"/tmp","timestamp":"2026-05-21T00:00:00Z","message":{{"role":"user","content":"{first_user}"}}}}"#
        );
        fs::write(&path, line.as_bytes()).unwrap();
        path
    }

    #[test]
    fn is_session_jsonl_accepts_normal_session_files() {
        assert!(is_session_jsonl(Path::new("/x/abc.jsonl")));
    }

    #[test]
    fn is_session_jsonl_rejects_agent_subtranscripts() {
        assert!(!is_session_jsonl(Path::new("/x/agent-deadbeef.jsonl")));
    }

    #[test]
    fn is_session_jsonl_rejects_non_jsonl_files() {
        assert!(!is_session_jsonl(Path::new("/x/notes.txt")));
        assert!(!is_session_jsonl(Path::new("/x/sessions")));
    }

    #[test]
    fn sort_by_recency_descending_with_none_sinking() {
        let mut items = vec![
            SessionListItem {
                session_id: "older".into(),
                display_name: "o".into(),
                last_active_at: Some(100.0),
                ..Default::default()
            },
            SessionListItem {
                session_id: "missing-mtime".into(),
                display_name: "m".into(),
                last_active_at: None,
                ..Default::default()
            },
            SessionListItem {
                session_id: "newest".into(),
                display_name: "n".into(),
                last_active_at: Some(500.0),
                ..Default::default()
            },
        ];
        sort_by_recency(&mut items);
        let ids: Vec<&str> = items.iter().map(|i| i.session_id.as_str()).collect();
        assert_eq!(ids, vec!["newest", "older", "missing-mtime"]);
    }

    #[tokio::test]
    async fn scan_once_returns_empty_when_projects_dir_missing() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let obs = tmp.path().join("observations");
        let items = scan_once(&missing, &obs).await.unwrap();
        assert!(items.is_empty());
    }

    #[tokio::test]
    async fn scan_once_walks_projects_and_builds_items() {
        let tmp = tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let obs = tmp.path().join("observations");
        fs::create_dir_all(&projects).unwrap();
        fs::create_dir_all(&obs).unwrap();

        // Two projects, one session each. Write s2 first then sleep so
        // its mtime is strictly older than s1 — establishes the sort order
        // without leaning on the `filetime` crate.
        let p1 = projects.join("project-one");
        let p2 = projects.join("project-two");
        fs::create_dir_all(&p1).unwrap();
        fs::create_dir_all(&p2).unwrap();

        let _s2 = write_session_jsonl(&p2, "session-bbb", "sid-bbb", "second session");
        // 1.1 s gap is comfortably above 1 s mtime granularity on macOS HFS+
        // and the older ext4 defaults. The tradeoff is test latency — we
        // pay this once, on a single test, which is fine.
        std::thread::sleep(StdDuration::from_millis(1100));
        let _s1 = write_session_jsonl(&p1, "session-aaa", "sid-aaa", "first session");

        // Plant an observation file for sid-aaa so has_observations flips.
        fs::write(obs.join("sid-aaa.jsonl"), b"{}\n").unwrap();

        // Plant an `agent-*.jsonl` that must be excluded.
        write_session_jsonl(&p1, "agent-skipme", "sid-skip", "do not surface");

        let items = scan_once(&projects, &obs).await.unwrap();
        assert_eq!(items.len(), 2, "agent-*.jsonl must be filtered out");

        // Sorted newest-first: s1 has the later mtime.
        assert_eq!(items[0].session_id, "sid-aaa");
        assert!(items[0].has_observations, "sid-aaa has an observation file");
        assert_eq!(items[0].display_name, "first session");
        assert!(!items[0].is_active);
        assert!(!items[0].is_working);

        assert_eq!(items[1].session_id, "sid-bbb");
        assert!(!items[1].has_observations);
    }

    #[tokio::test]
    async fn scan_once_falls_back_to_file_stem_when_no_first_user_text() {
        let tmp = tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let obs = tmp.path().join("observations");
        let p = projects.join("proj");
        fs::create_dir_all(&p).unwrap();
        fs::create_dir_all(&obs).unwrap();

        // No user lines → first_user_text stays None → display_name falls
        // back to the file stem.
        let path = p.join("stub-stem.jsonl");
        fs::write(
            &path,
            br#"{"type":"meta","note":"no user line here"}
"#,
        )
        .unwrap();

        let items = scan_once(&projects, &obs).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].session_id, "stub-stem");
        assert_eq!(items[0].display_name, "stub-stem");
    }

    #[tokio::test]
    async fn scan_once_skips_non_directory_entries_in_projects() {
        // A stray file directly under projects/ shouldn't crash the walk.
        let tmp = tempdir().unwrap();
        let projects = tmp.path().join("projects");
        let obs = tmp.path().join("observations");
        fs::create_dir_all(&projects).unwrap();
        fs::create_dir_all(&obs).unwrap();
        fs::write(projects.join("stray.txt"), b"not a project dir").unwrap();

        let items = scan_once(&projects, &obs).await.unwrap();
        assert!(items.is_empty());
    }
}
