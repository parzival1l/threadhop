//! ThreadHop Rust TUI — entry point.
//!
//! Phase 2 Wave A: parses CLI flags, sets up file-based tracing (stdout is
//! reserved for the alternate-screen TUI), installs a panic hook that restores
//! the terminal, enters raw mode + alternate screen, and runs the event loop
//! until the user quits.

mod anim;
mod app;
mod event;
mod keys;
mod screens;
mod widgets;
mod workers;

use std::io::{stdout, Stdout};

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::app::App;

/// ThreadHop Rust TUI CLI flags. Mirrors the Python `./threadhop` entrypoint
/// so the eventual `./threadhop-rs` wrapper is a drop-in for testing parity.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "threadhop-tui",
    version,
    about = "ThreadHop — Rust TUI for browsing Claude Code session transcripts"
)]
pub struct Cli {
    /// Filter to a single project (matches the Python --project flag).
    #[arg(long)]
    pub project: Option<String>,

    /// Days of history to include in the sidebar.
    #[arg(long, default_value_t = 7)]
    pub days: u32,

    /// Open a specific session by id on launch.
    #[arg(long)]
    pub session: Option<String>,

    /// Disable terminal mouse capture. Without this flag the TUI listens
    /// for click + scroll events; with it set, the terminal's native
    /// text-selection (e.g. Cmd+drag on macOS) keeps working at the cost
    /// of clickable sidebar rows + scroll-wheel transcript navigation.
    #[arg(long)]
    pub no_mouse: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing()?;

    // Panic hook MUST be installed before we touch raw mode, so a panic from
    // anywhere — even before the terminal guard is constructed — restores the
    // terminal before the message hits the screen.
    install_panic_hook();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    let result = runtime.block_on(async { run(cli).await });

    // Always restore the terminal, even on Err. This is the happy-path twin
    // of the panic hook — when run() returns normally we still need to leave
    // the alt screen and disable raw mode.
    restore_terminal();
    result
}

async fn run(cli: Cli) -> Result<()> {
    let mouse_enabled = !cli.no_mouse;
    enter_terminal(mouse_enabled)?;
    let mut terminal = build_terminal()?;
    let mut app = App::new();
    app.mouse_enabled = mouse_enabled;
    // Phase 6: route CLI flags into the App. `--days` defaults to 7 via
    // clap; 0 means "no filter" (apply_cli short-circuits in that case).
    app.apply_cli(cli.project, Some(cli.days), cli.session);
    event::run(app, &mut terminal).await
}

fn enter_terminal(mouse: bool) -> Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    if mouse {
        // Best-effort: terminals that don't support mouse reporting silently
        // ignore the escape; we don't surface an error because the rest of
        // the TUI is still usable.
        let _ = execute!(stdout(), EnableMouseCapture);
    }
    Ok(())
}

fn build_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    let backend = CrosstermBackend::new(stdout());
    Ok(Terminal::new(backend)?)
}

fn restore_terminal() {
    // Best-effort: if either step fails we've already lost control of the
    // terminal, and the user will get a corrupted shell. Logging is no help
    // because tracing writes to a file. The next shell command (`reset`) is
    // the recovery path.
    //
    // Always emit `DisableMouseCapture` — sending it when capture wasn't
    // enabled is a no-op on every terminal we care about, and the alternative
    // (threading the flag through the panic hook) costs more than it saves.
    let _ = execute!(stdout(), DisableMouseCapture);
    let _ = disable_raw_mode();
    let _ = execute!(stdout(), LeaveAlternateScreen);
}

fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original(info);
    }));
}

fn init_tracing() -> Result<()> {
    // We MUST NOT log to stdout — that's the alt-screen ratatui surface.
    // Logs go to ~/.config/threadhop/logs/threadhop-tui.log so the user can
    // tail them from another terminal while the TUI is running.
    let dir = threadhop_core::paths::logs_dir();
    std::fs::create_dir_all(&dir)?;
    let log_path = dir.join("threadhop-tui.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let filter = EnvFilter::try_from_env("THREADHOP_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_ansi(false).with_writer(file))
        .init();

    tracing::info!("threadhop-tui v{} starting", env!("CARGO_PKG_VERSION"));
    Ok(())
}
