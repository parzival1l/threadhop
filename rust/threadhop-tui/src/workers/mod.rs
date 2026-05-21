//! Background workers feed the App via mpsc.
//!
//! Wave E wires `spawn_all` to actually launch all three workers. Each
//! `tokio::spawn`ed task owns its `Sender` clone; the `JoinHandle`s are
//! dropped intentionally — the event loop signals shutdown by dropping the
//! `Receiver` (and, for the fs_watcher, the `watch::Sender`), which causes
//! every worker to exit cleanly on its next `send`.

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
use tokio::sync::watch;

/// Spawn all 3 background workers. Each takes a sender clone and runs forever
/// until the app exits and the channel closes.
///
/// `fs_watcher_rx` is the receiver half of the active-session watch channel —
/// the `Sender` lives on `App` so key handlers can retarget the watcher when
/// the user changes selection.
pub fn spawn_all(
    tx: Sender<WorkerEvent>,
    fs_watcher_rx: watch::Receiver<Option<String>>,
) {
    let scanner_tx = tx.clone();
    tokio::spawn(async move {
        if let Err(err) = session_scanner::run(scanner_tx.clone()).await {
            let _ = scanner_tx
                .send(WorkerEvent::Error(format!("session_scanner exited: {err}")))
                .await;
        }
    });

    let detector_tx = tx.clone();
    tokio::spawn(async move {
        if let Err(err) = active_detector::run(detector_tx.clone()).await {
            let _ = detector_tx
                .send(WorkerEvent::Error(format!("active_detector exited: {err}")))
                .await;
        }
    });

    let watcher_tx = tx;
    tokio::spawn(async move {
        if let Err(err) = fs_watcher::run(watcher_tx.clone(), fs_watcher_rx).await {
            let _ = watcher_tx
                .send(WorkerEvent::Error(format!("fs_watcher exited: {err}")))
                .await;
        }
    });
}
