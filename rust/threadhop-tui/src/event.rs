//! The `tokio::select!` event loop.
//!
//! Wave E wires real handlers for every `WorkerEvent` variant and pulls the
//! active-session `watch::Receiver` off `App` so `spawn_all` can hand it to
//! the fs_watcher.

use std::collections::HashSet;
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
/// Each iteration races three sources:
///   1. A crossterm key/resize/mouse event.
///   2. A ~60fps render tick — keeps spinners and clocks ticking even when
///      the user is idle.
///   3. The worker mpsc channel.
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
    // The fs_watcher needs the receiver half of the active-session watch
    // channel. Grab it off the App before handing the App into the loop —
    // `App::active_session_tx` stays on the App so key handlers can publish
    // selection changes.
    let fs_watcher_rx = app.active_session_rx();
    crate::workers::spawn_all(worker_tx.clone(), fs_watcher_rx);
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
            // Render tick. The redraw at the bottom of the loop handles
            // anything time-driven; we still consume the tick so the
            // interval doesn't backlog.
            //
            // Phase 3 Wave 2: also drives the search-modal debounce —
            // `should_execute` returns true once the user's typing has gone
            // quiet for `DEBOUNCE`, and we run the FTS query against the
            // App-owned DB connection.
            _ = tick.tick() => {
                if let Some(state) = app.search.as_mut() {
                    if crate::screens::search::should_execute(state) {
                        crate::screens::search::execute_query(state, &app.db);
                    }
                }
            }
            Some(event) = worker_rx.recv() => {
                handle_worker_event(&mut app, event);
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
        // Mouse / paste / focus are not bound today.
        _ => {}
    }
}

/// Apply one worker event to App state. Pulled out of the `select!` so it's
/// unit-testable without standing up an async runtime.
fn handle_worker_event(app: &mut App, event: WorkerEvent) {
    match event {
        WorkerEvent::SessionsRefreshed(items) => {
            app.sidebar = items;
            // Preserve selection if it still exists; otherwise fall back to
            // the first row. The fs_watcher rebinds to the new id via the
            // watch channel.
            let still_present = app
                .selected_session_id
                .as_deref()
                .map(|id| app.sidebar.iter().any(|i| i.session_id == id))
                .unwrap_or(false);
            if !still_present {
                app.selected_session_id =
                    app.sidebar.first().map(|i| i.session_id.clone());
                let _ = app.active_session_tx.send(app.selected_session_id.clone());
                // Selection changed → reset the scroll so we don't carry an
                // out-of-range value into the new transcript.
                app.scroll = 0;
            }
        }
        WorkerEvent::ActiveDetectorRefreshed(active) => {
            let active_ids: HashSet<String> =
                active.iter().filter_map(|a| a.session_id.clone()).collect();
            for item in &mut app.sidebar {
                item.is_active = active_ids.contains(&item.session_id);
            }
        }
        WorkerEvent::TranscriptRefreshed { session_id, messages } => {
            // Stale loads (the user moved on before the worker finished) are
            // dropped. The fs_watcher will emit the right transcript on the
            // next tick anyway.
            if app.selected_session_id.as_deref() == Some(session_id.as_str()) {
                app.transcript = messages;
                // If a find bar is open, its match positions reference the
                // *previous* transcript. Recompute now that the transcript
                // changed so highlights stay in sync.
                if let Some(fs) = app.find_state.as_mut() {
                    fs.recompute_matches(&app.transcript);
                }
                // Phase 3 Wave 2: resolve any pending search-modal jump.
                app.try_resolve_pending_jump();
            }
        }
        WorkerEvent::Error(msg) => {
            tracing::warn!("worker error: {msg}");
            app.status_message = Some(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use threadhop_core::jsonl::CleanedMessage;
    use threadhop_core::session_detect::ActiveSession;
    use crate::widgets::session_list::SessionListItem;

    fn stub_msg() -> CleanedMessage {
        CleanedMessage {
            uuid: "u1".into(),
            session_id: None,
            role: "user".into(),
            text: "hi".into(),
            timestamp: None,
            cwd: None,
            parent_uuid: None,
            is_sidechain: 0,
            message_id: None,
        }
    }

    fn item(id: &str) -> SessionListItem {
        SessionListItem {
            session_id: id.into(),
            display_name: id.into(),
            is_active: false,
            is_working: false,
            has_observations: false,
            last_active_at: None,
        }
    }

    #[test]
    fn sessions_refreshed_seeds_selection_when_empty() {
        let mut app = App::new();
        let mut rx = app.active_session_rx();
        let _ = rx.borrow_and_update();

        handle_worker_event(
            &mut app,
            WorkerEvent::SessionsRefreshed(vec![item("alpha"), item("beta")]),
        );
        assert_eq!(app.selected_session_id.as_deref(), Some("alpha"));
        assert!(rx.has_changed().unwrap());
        assert_eq!(rx.borrow().as_deref(), Some("alpha"));
    }

    #[test]
    fn sessions_refreshed_preserves_existing_selection() {
        let mut app = App::new();
        app.selected_session_id = Some("beta".into());
        app.sidebar = vec![item("alpha"), item("beta")];

        handle_worker_event(
            &mut app,
            WorkerEvent::SessionsRefreshed(vec![item("beta"), item("gamma")]),
        );
        assert_eq!(app.selected_session_id.as_deref(), Some("beta"));
    }

    #[test]
    fn sessions_refreshed_falls_back_when_selection_disappears() {
        let mut app = App::new();
        app.selected_session_id = Some("gone".into());

        handle_worker_event(
            &mut app,
            WorkerEvent::SessionsRefreshed(vec![item("alpha")]),
        );
        assert_eq!(app.selected_session_id.as_deref(), Some("alpha"));
    }

    #[test]
    fn active_detector_marks_only_matching_sidebar_items() {
        let mut app = App::new();
        app.sidebar = vec![item("alpha"), item("beta"), item("gamma")];
        handle_worker_event(
            &mut app,
            WorkerEvent::ActiveDetectorRefreshed(vec![
                ActiveSession {
                    pid: 1,
                    session_id: Some("beta".into()),
                    cwd: None,
                },
                ActiveSession {
                    pid: 2,
                    session_id: None,
                    cwd: None,
                },
            ]),
        );
        let actives: Vec<bool> = app.sidebar.iter().map(|i| i.is_active).collect();
        assert_eq!(actives, vec![false, true, false]);
    }

    #[test]
    fn transcript_refreshed_only_adopts_matching_session() {
        let mut app = App::new();
        app.selected_session_id = Some("alpha".into());
        handle_worker_event(
            &mut app,
            WorkerEvent::TranscriptRefreshed {
                session_id: "beta".into(),
                messages: vec![stub_msg()],
            },
        );
        assert!(app.transcript.is_empty(), "stale transcript must be dropped");

        handle_worker_event(
            &mut app,
            WorkerEvent::TranscriptRefreshed {
                session_id: "alpha".into(),
                messages: vec![stub_msg()],
            },
        );
        assert_eq!(app.transcript.len(), 1);
    }

    #[test]
    fn error_event_lands_on_status_message() {
        let mut app = App::new();
        handle_worker_event(&mut app, WorkerEvent::Error("disk on fire".into()));
        assert_eq!(app.status_message.as_deref(), Some("disk on fire"));
    }
}
