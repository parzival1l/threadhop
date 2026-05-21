//! fs_watcher: Polls the active session's JSONL file at 1Hz; on change emits TranscriptRefreshed.
//!
//! Placeholder — implementation lands in Wave D.

use tokio::sync::mpsc::Sender;
use super::WorkerEvent;

pub async fn run(_tx: Sender<WorkerEvent>) -> anyhow::Result<()> {
    Ok(())
}
