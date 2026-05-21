//! active_detector: Runs `ps`/`lsof` every 5s and emits ActiveDetectorRefreshed.
//!
//! Thin wrapper around [`threadhop_core::session_detect::scan_active`] — the
//! heavy lifting (parsing `ps -eo pid,args`, resolving CWDs via `lsof`) lives
//! in core. This worker just paces the polling, forwards results, and surfaces
//! errors as non-fatal `WorkerEvent::Error` entries so the loop never dies.

use std::time::Duration;

use tokio::sync::mpsc::Sender;
use tokio::time::interval;

use super::WorkerEvent;

/// Poll interval — matches the Python TUI's 5s active-session refresh cadence.
const TICK: Duration = Duration::from_secs(5);

/// Run the active-detector loop forever.
///
/// On every tick: scan, emit either `ActiveDetectorRefreshed` (success) or
/// `Error` (failure), then continue. Exits cleanly only when the receiver is
/// dropped (channel closed) — `send` will return `Err` and we propagate.
pub async fn run(tx: Sender<WorkerEvent>) -> anyhow::Result<()> {
    let mut ticker = interval(TICK);
    loop {
        ticker.tick().await;
        // Bail out only when the receiver is gone (app shutting down).
        if tx.is_closed() {
            return Ok(());
        }
        if scan_once(&tx).await.is_err() {
            // `scan_once` only returns Err when the channel is closed — same
            // shutdown signal as above.
            return Ok(());
        }
    }
}

/// One tick of the loop, factored out for unit testing.
///
/// Returns `Err` only when the receiver is gone — scan failures are reported
/// in-band via `WorkerEvent::Error` so the caller keeps polling.
pub async fn scan_once(tx: &Sender<WorkerEvent>) -> anyhow::Result<()> {
    match threadhop_core::session_detect::scan_active().await {
        Ok(sessions) => {
            tx.send(WorkerEvent::ActiveDetectorRefreshed(sessions)).await?;
        }
        Err(err) => {
            tx.send(WorkerEvent::Error(format!("active_detector: {err}")))
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// `scan_once` should always send exactly one event per tick — either an
    /// `ActiveDetectorRefreshed` (when `scan_active` succeeds) or an `Error`
    /// (when it fails). On macOS CI the call usually succeeds with an empty
    /// vec; on Linux it also succeeds (returns empty). Either way, *some*
    /// event must land on the channel.
    #[tokio::test]
    async fn scan_once_emits_one_event() {
        let (tx, mut rx) = mpsc::channel::<WorkerEvent>(8);
        scan_once(&tx).await.expect("channel open");
        let evt = rx.try_recv().expect("one event emitted");
        match evt {
            WorkerEvent::ActiveDetectorRefreshed(_) | WorkerEvent::Error(_) => {}
            other => panic!("unexpected event: {other:?}"),
        }
        // And only one.
        assert!(rx.try_recv().is_err(), "exactly one event per tick");
    }

    /// When the receiver is dropped, `scan_once` should surface the send
    /// failure so the outer loop can shut down cleanly.
    #[tokio::test]
    async fn scan_once_errors_when_receiver_dropped() {
        let (tx, rx) = mpsc::channel::<WorkerEvent>(1);
        drop(rx);
        let result = scan_once(&tx).await;
        assert!(result.is_err(), "send must fail with no receiver");
    }
}
