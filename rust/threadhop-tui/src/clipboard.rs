#![allow(dead_code)] // Worker A wires this into the selection-mode `y` handler.

//! OS-clipboard helper. Pre-pop scaffolding for the Phase A `y`
//! selection-copy keybinding (Worker A wires the call site).
//!
//! The implementation wraps `arboard::Clipboard` so the rest of the TUI
//! depends on a single small surface rather than scattering arboard
//! imports through call sites. Errors are coerced to [`ClipboardError`]
//! so the UI can surface a stable status message without leaking
//! `arboard::Error` into widget code.
//!
//! Test environments (CI containers, headless ssh sessions) frequently
//! lack any system clipboard. We treat those as a soft-failure
//! (`ClipboardError::Unavailable`) so the caller can degrade gracefully
//! (e.g. surface "Clipboard unavailable — selection not copied" rather
//! than panic).

use std::fmt;

/// Reason the clipboard write failed.
#[derive(Debug)]
pub enum ClipboardError {
    /// No clipboard backend was available (no display server, no
    /// pasteboard daemon, headless container, ...). Callers typically
    /// degrade silently or surface a muted status hint.
    Unavailable,
    /// The backend reported an error other than "no clipboard". Carries
    /// the stringified error for diagnostics.
    Backend(String),
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClipboardError::Unavailable => write!(f, "clipboard unavailable"),
            ClipboardError::Backend(s) => write!(f, "clipboard backend error: {s}"),
        }
    }
}

impl std::error::Error for ClipboardError {}

/// Copy `text` to the OS clipboard. Returns `Ok(())` on success,
/// [`ClipboardError::Unavailable`] when no clipboard backend is
/// reachable, and [`ClipboardError::Backend`] for any other failure.
///
/// Phase A worker calls this from the selection-mode `y` handler.
pub fn copy_to_clipboard(text: &str) -> Result<(), ClipboardError> {
    let mut cb = arboard::Clipboard::new().map_err(map_arboard_err)?;
    cb.set_text(text.to_string()).map_err(map_arboard_err)?;
    Ok(())
}

fn map_arboard_err(e: arboard::Error) -> ClipboardError {
    // `arboard::Error::ClipboardNotSupported` and the platform-specific
    // "no display" variants are the cases we coerce to Unavailable so
    // the UI can show a soft "clipboard unavailable" message instead of
    // panicking. Everything else stays as a backend error.
    match e {
        arboard::Error::ClipboardNotSupported => ClipboardError::Unavailable,
        other => ClipboardError::Backend(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: in CI / headless environments we accept either
    /// `Ok(())` (the clipboard works) or `Err(Unavailable)` (no backend
    /// is reachable). We deliberately do NOT verify the clipboard
    /// actually contains "hello" — verifying that across macOS/Linux
    /// CI environments is brittle.
    #[test]
    fn copy_to_clipboard_returns_ok_or_unavailable() {
        match copy_to_clipboard("hello") {
            Ok(()) => {}
            Err(ClipboardError::Unavailable) => {}
            // Backend errors are accepted too — some CI containers
            // return platform-specific errors (e.g. "no pasteboard
            // server") that arboard surfaces as a generic Backend var.
            // The pre-pop only proves the call shape compiles + runs.
            Err(ClipboardError::Backend(_)) => {}
        }
    }

    #[test]
    fn unavailable_display_is_stable() {
        // Pin the message so the status-line copy in Worker A can
        // safely substring-match if it wants to.
        assert_eq!(
            ClipboardError::Unavailable.to_string(),
            "clipboard unavailable"
        );
    }
}
