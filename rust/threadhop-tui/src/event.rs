//! The `tokio::select!` event loop.
//!
//! Wave A wires three of the four eventual arms: a crossterm key stream, a
//! 16ms render tick (~60fps), and a worker channel that's reserved but never
//! sends in this wave. The fourth arm — modal results — joins in Wave D.

use std::io::Stdout;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event as CtEvent, EventStream};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::app::App;
use crate::workers::WorkerEvent;

/// Run the event loop until `app.should_quit` is true (or the crossterm stream
/// closes, which only happens on EOF).
///
/// Each iteration races four sources:
///   1. A crossterm key/resize/mouse event.
///   2. A ~60fps render tick — keeps spinners and clocks ticking even when
///      the user is idle. Wave A doesn't have spinners yet, but the tick is
///      cheap and lets later waves drop in.
///   3. The worker channel (no senders in Wave A — never fires).
///
/// After every source we redraw. ratatui's diffing means redundant draws on a
/// static frame are nearly free.
pub async fn run(
    mut app: App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<()> {
    let mut crossterm_events = EventStream::new();
    let mut tick = interval(Duration::from_millis(16));
    let (worker_tx, mut worker_rx) = mpsc::channel::<WorkerEvent>(64);
    crate::workers::spawn_all(worker_tx.clone());
    drop(worker_tx);

    // Prime draw — without this the user stares at an empty terminal until
    // their first keystroke.
    terminal.draw(|f| app.draw(f))?;

    loop {
        tokio::select! {
            // Crossterm key/resize/mouse event. EventStream yields
            // `Option<Result<Event>>`; we treat any decoding error as a fatal
            // exit (the terminal is in a bad state if crossterm can't parse).
            maybe_event = crossterm_events.next() => {
                match maybe_event {
                    Some(Ok(event)) => handle_terminal_event(&mut app, event),
                    Some(Err(err)) => {
                        tracing::error!("crossterm read error: {err}");
                        return Err(err.into());
                    }
                    // Stream exhausted — treat as a clean shutdown.
                    None => return Ok(()),
                }
            }
            // Render tick. Wave A doesn't have time-based state, so the
            // redraw at the bottom of the loop handles this implicitly. We
            // still consume the tick so the interval doesn't backlog.
            _ = tick.tick() => {}
            // Worker channel. Wave C/D agents fan more variants in here;
            // today only `Error` has a real handler. The catch-all keeps the
            // shape `match event { ... _ => {} }` so later waves drop new
            // arms in without restructuring (clippy::single_match suppressed
            // for the same reason).
            Some(event) = worker_rx.recv() => {
                #[allow(clippy::single_match)]
                match event {
                    WorkerEvent::Error(msg) => {
                        tracing::warn!("worker error: {msg}");
                    }
                    _ => {}
                }
            }
        }

        if app.should_quit {
            return Ok(());
        }

        terminal.draw(|f| app.draw(f))?;
    }
}

fn handle_terminal_event(app: &mut App, event: CtEvent) {
    match event {
        CtEvent::Key(key) => {
            app.handle_key(key);
        }
        CtEvent::Resize(_, _) => {
            // ratatui handles resize on the next draw automatically.
        }
        // Mouse / paste / focus are not bound in Wave A.
        _ => {}
    }
}
