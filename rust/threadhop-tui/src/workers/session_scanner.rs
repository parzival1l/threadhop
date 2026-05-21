//! session_scanner: Scans ~/.claude/projects/**/*.jsonl every 5s and emits SessionsRefreshed.
//!
//! Placeholder — implementation lands in Wave D.

use tokio::sync::mpsc::Sender;
use super::WorkerEvent;

pub async fn run(_tx: Sender<WorkerEvent>) -> anyhow::Result<()> {
    Ok(())
}
