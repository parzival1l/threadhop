//! Background workers feed the App via mpsc.

// Wave C/D scaffolding — variants and worker `run` fns aren't wired into the
// event loop yet (only `Error` is matched today). Same pattern as
// `widgets/session_list.rs`.
#![allow(dead_code)]

pub mod session_scanner;
pub mod active_detector;
pub mod fs_watcher;

use threadhop_core::jsonl::CleanedMessage;
use threadhop_core::session_detect::ActiveSession;
use crate::widgets::session_list::SessionListItem;

#[derive(Debug, Clone)]
pub enum WorkerEvent {
    /// Sidebar rows refreshed (full snapshot). Emitted by `session_scanner`.
    SessionsRefreshed(Vec<SessionListItem>),
    /// Active-session detection finished. Emitted by `active_detector`.
    ActiveDetectorRefreshed(Vec<ActiveSession>),
    /// Active transcript file changed — reload. Emitted by `fs_watcher`.
    TranscriptRefreshed { session_id: String, messages: Vec<CleanedMessage> },
    /// Non-fatal worker error to surface in the digest bar.
    Error(String),
}

use tokio::sync::mpsc::Sender;

/// Spawn all 3 background workers. Each takes a sender clone and runs forever
/// until the app exits and the channel closes.
pub fn spawn_all(_tx: Sender<WorkerEvent>) {
    // Wave D parallel agents will fill these in. Today this is a no-op so the
    // event loop compiles.
    // tokio::spawn(session_scanner::run(_tx.clone()));
    // tokio::spawn(active_detector::run(_tx.clone()));
    // tokio::spawn(fs_watcher::run(_tx));
}
