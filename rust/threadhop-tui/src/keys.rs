//! Command + Scope registry. Mirrors `threadhop_core/tui/keybindings.py`.
//!
//! Wave E adds the main-screen navigation bindings (j/k/g/G/PgUp/PgDn/Ctrl-d/
//! Ctrl-u). The shape — scope + global fallback, lookup by (code, modifiers) —
//! is unchanged from Wave A.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// UI surface a key event is dispatched against. The same physical key can
/// mean different things in different scopes (e.g. `q` quits on Main, cancels
/// on a modal). `commands_for_scope` returns the bindings that apply.
///
/// Wave A only constructs `Global` and `MainScreen`; the modal scopes are
/// reserved for later waves so the enum doesn't reshape when they land.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    MainScreen,
    SearchModal,
    /// Find-in-transcript bar overlay. Sits on top of `MainScreen` but takes
    /// over key dispatch while open so `n`/`N`/`Esc`/`Enter` map to find-bar
    /// actions and the footer reflects the right hints.
    FindBar,
    BookmarkModal,
    KanbanModal,
    HelpOverlay,
    LabelPrompt,
    Confirm,
    /// Phase 4: bookmark browser modal — list of pinned messages, Enter
    /// jumps via the App's `pending_jump_message_uuid` channel.
    BookmarkBrowser,
    /// Phase 4: generic confirm modal (yes/no destructive-action gate).
    ConfirmModal,
}

/// High-level action a binding fires. Wave E grows this to cover sidebar
/// navigation and transcript scrolling; later waves add OpenSearch /
/// ToggleBookmark / etc.
///
/// Phase 4 reserves several variants (`ToggleBookmark`, `OpenBookmarkBrowser`,
/// `CycleSessionStatus`, `OpenLabelPrompt`, `MoveCursorDown`, `MoveCursorUp`)
/// that aren't bound to any key yet — Wave 1 / Wave 2 register the bindings.
/// `#[allow(dead_code)]` keeps `-D warnings` happy in the pre-pop commit.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Quit,
    /// Move the sidebar selection one row down.
    SelectNextSession,
    /// Move the sidebar selection one row up.
    SelectPrevSession,
    /// Jump the transcript scroll to the top (`g`).
    ScrollTop,
    /// Jump the transcript scroll to the bottom (`G`). The renderer clamps
    /// `u16::MAX` to the last line.
    ScrollBottom,
    /// Scroll the transcript down half a page (PageDown / Ctrl-d).
    ScrollDownHalf,
    /// Scroll the transcript up half a page (PageUp / Ctrl-u).
    ScrollUpHalf,
    /// Open the help overlay. Wave E registers the binding as a no-op so the
    /// label shows up in the footer; the overlay itself lands in Phase 6.
    OpenHelp,
    /// Confirm/activate current selection. Wave E binds it for label coverage;
    /// selection happens immediately on j/k, so the handler is a no-op.
    Confirm,
    /// Open the FTS search modal (Phase 3 Wave 2). App swaps to
    /// `Scope::SearchModal` and modal-first dispatch takes over.
    OpenSearchModal,
    /// Open the in-transcript find bar (Phase 3 Wave 2). App swaps to
    /// `Scope::FindBar`; the find_bar widget handles its own keys.
    OpenFindBar,
    /// Close the find bar (footer hint while `Scope::FindBar` is active).
    /// Actual dispatch goes through `widgets::find_bar::handle_key`.
    CloseFindBar,
    /// Jump to the currently-highlighted find-bar match (footer hint only).
    JumpToCurrentMatch,
    /// Step to the next find-bar match (footer hint only).
    NextMatch,
    /// Step to the previous find-bar match (footer hint only).
    PrevMatch,
    // ---- Phase 4 additions ------------------------------------------------
    /// Bookmark the message at `App::message_cursor` in the current
    /// transcript. Wave 1/2 wires the binding (`b` on the main screen).
    ToggleBookmark,
    /// Open the bookmark browser modal (Wave 1).
    OpenBookmarkBrowser,
    /// Cycle the focused session's status forward through the legal label
    /// values (Wave 2). Distinct from `OpenLabelPrompt` — Wave 2 picks which
    /// keybinding (if any) maps to which.
    CycleSessionStatus,
    /// Open the explicit label/status picker modal (Wave 1).
    OpenLabelPrompt,
    /// Generic cancel (Esc / `n` in confirm modals). Distinct from
    /// `CloseFindBar` which is find-bar-specific. Wave 1 modal handlers
    /// translate this into their own `Cancelled` result.
    Cancel,
    /// Move the in-transcript message cursor down one message (Wave 2 —
    /// driver for `ToggleBookmark`).
    MoveCursorDown,
    /// Move the in-transcript message cursor up one message (Wave 2).
    MoveCursorUp,
}

/// Single binding row. `label` drives the contextual footer + help overlay.
///
/// `scope` and `label` aren't read in Wave A — `lookup` matches on `key` +
/// `command` and the footer/help overlay land in Wave B. Allowing dead_code
/// here keeps the registry shape stable across waves.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct CommandBinding {
    pub key: KeyEvent,
    pub scope: Scope,
    pub command: Command,
    pub label: &'static str,
}

const fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    // `KeyEvent::new` isn't const, but the public struct fields are. This
    // helper keeps the binding tables readable.
    KeyEvent {
        code,
        modifiers: mods,
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

const GLOBAL_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Char('q'), KeyModifiers::NONE),
        scope: Scope::Global,
        command: Command::Quit,
        label: "quit",
    },
    CommandBinding {
        key: key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        scope: Scope::Global,
        command: Command::Quit,
        label: "quit",
    },
];

/// Bindings active on the main screen. Each row also gets the global
/// fallback (q / Ctrl-c) via `lookup`.
///
/// Both the letter key and an alternate (Down/Up/PageDown/PageUp) are
/// registered as separate rows — the footer only needs to advertise one of
/// them, but the lookup table needs both.
const MAIN_SCREEN_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Char('j'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::SelectNextSession,
        label: "next session",
    },
    CommandBinding {
        key: key(KeyCode::Down, KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::SelectNextSession,
        label: "next session",
    },
    CommandBinding {
        key: key(KeyCode::Char('k'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::SelectPrevSession,
        label: "prev session",
    },
    CommandBinding {
        key: key(KeyCode::Up, KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::SelectPrevSession,
        label: "prev session",
    },
    CommandBinding {
        key: key(KeyCode::Enter, KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::Confirm,
        label: "open",
    },
    CommandBinding {
        key: key(KeyCode::Char('g'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ScrollTop,
        label: "top",
    },
    CommandBinding {
        key: key(KeyCode::Char('G'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::ScrollBottom,
        label: "bottom",
    },
    CommandBinding {
        key: key(KeyCode::PageDown, KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ScrollDownHalf,
        label: "page down",
    },
    CommandBinding {
        key: key(KeyCode::Char('d'), KeyModifiers::CONTROL),
        scope: Scope::MainScreen,
        command: Command::ScrollDownHalf,
        label: "page down",
    },
    CommandBinding {
        key: key(KeyCode::PageUp, KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ScrollUpHalf,
        label: "page up",
    },
    CommandBinding {
        key: key(KeyCode::Char('u'), KeyModifiers::CONTROL),
        scope: Scope::MainScreen,
        command: Command::ScrollUpHalf,
        label: "page up",
    },
    CommandBinding {
        key: key(KeyCode::Char('?'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::OpenHelp,
        label: "help",
    },
    // Phase 3 Wave 2: open the FTS search modal. `/` matches the Python
    // TUI's binding so muscle memory ports over cleanly.
    CommandBinding {
        key: key(KeyCode::Char('/'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::OpenSearchModal,
        label: "search",
    },
    // Open the in-transcript find bar. Plain `f` (not Ctrl-F) — Ctrl-F is
    // commonly intercepted by terminal multiplexers, and plain `f` is free
    // on the main screen.
    CommandBinding {
        key: key(KeyCode::Char('f'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::OpenFindBar,
        label: "find",
    },
    // ---- Phase 4 Wave 2 main-screen bindings ----
    // Lowercase j/k already drive the sidebar; uppercase shift-J/K move the
    // in-transcript message cursor. `b` toggles bookmark on the message at
    // the cursor; `Shift+B` opens the cross-session bookmark browser.
    // `s` opens the label-prompt modal (status picker; Tab inside the modal
    // toggles to custom-name mode).
    CommandBinding {
        key: key(KeyCode::Char('b'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ToggleBookmark,
        label: "bookmark",
    },
    CommandBinding {
        key: key(KeyCode::Char('B'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::OpenBookmarkBrowser,
        label: "browse bookmarks",
    },
    CommandBinding {
        key: key(KeyCode::Char('s'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::OpenLabelPrompt,
        label: "status",
    },
    // `Shift+S` is intentionally bound to the same command — the modal's
    // Tab toggles between status picker and custom-name modes, so we don't
    // need two separate openers today.
    CommandBinding {
        key: key(KeyCode::Char('S'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::OpenLabelPrompt,
        label: "rename",
    },
    CommandBinding {
        key: key(KeyCode::Char('J'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::MoveCursorDown,
        label: "msg ↓",
    },
    CommandBinding {
        key: key(KeyCode::Char('K'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::MoveCursorUp,
        label: "msg ↑",
    },
];

/// Footer hints shown while the search modal is open. The actual key
/// dispatch is handled directly by `screens::search::handle_key` in
/// modal-first routing — these entries exist purely so the footer shows the
/// right labels.
const SEARCH_MODAL_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Esc, KeyModifiers::NONE),
        scope: Scope::SearchModal,
        command: Command::CloseFindBar,
        label: "close",
    },
    CommandBinding {
        key: key(KeyCode::Enter, KeyModifiers::NONE),
        scope: Scope::SearchModal,
        command: Command::Confirm,
        label: "jump",
    },
    CommandBinding {
        key: key(KeyCode::Down, KeyModifiers::NONE),
        scope: Scope::SearchModal,
        command: Command::SelectNextSession,
        label: "next hit",
    },
    CommandBinding {
        key: key(KeyCode::Up, KeyModifiers::NONE),
        scope: Scope::SearchModal,
        command: Command::SelectPrevSession,
        label: "prev hit",
    },
];

/// Footer hints shown while the find bar is focused. As with the search
/// modal, key dispatch is delegated to `widgets::find_bar::handle_key`.
const FIND_BAR_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Esc, KeyModifiers::NONE),
        scope: Scope::FindBar,
        command: Command::CloseFindBar,
        label: "close",
    },
    CommandBinding {
        key: key(KeyCode::Enter, KeyModifiers::NONE),
        scope: Scope::FindBar,
        command: Command::JumpToCurrentMatch,
        label: "jump",
    },
    CommandBinding {
        key: key(KeyCode::Char('n'), KeyModifiers::NONE),
        scope: Scope::FindBar,
        command: Command::NextMatch,
        label: "next",
    },
    CommandBinding {
        key: key(KeyCode::Char('N'), KeyModifiers::SHIFT),
        scope: Scope::FindBar,
        command: Command::PrevMatch,
        label: "prev",
    },
];

/// Footer hints shown while the bookmark browser modal is open. Actual key
/// dispatch is handled by `screens::bookmark_browser::handle_key`.
const BOOKMARK_BROWSER_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Esc, KeyModifiers::NONE),
        scope: Scope::BookmarkBrowser,
        command: Command::Cancel,
        label: "close",
    },
    CommandBinding {
        key: key(KeyCode::Enter, KeyModifiers::NONE),
        scope: Scope::BookmarkBrowser,
        command: Command::Confirm,
        label: "jump",
    },
    CommandBinding {
        key: key(KeyCode::Char('j'), KeyModifiers::NONE),
        scope: Scope::BookmarkBrowser,
        command: Command::SelectNextSession,
        label: "next",
    },
    CommandBinding {
        key: key(KeyCode::Char('k'), KeyModifiers::NONE),
        scope: Scope::BookmarkBrowser,
        command: Command::SelectPrevSession,
        label: "prev",
    },
    CommandBinding {
        key: key(KeyCode::Char('d'), KeyModifiers::NONE),
        scope: Scope::BookmarkBrowser,
        command: Command::Cancel, // close-ish; real dispatch handles delete
        label: "delete",
    },
];

/// Footer hints shown while the confirm modal is open. Actual key dispatch
/// is handled by `screens::confirm::handle_key` (y / n / Enter / Esc).
const CONFIRM_MODAL_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Char('y'), KeyModifiers::NONE),
        scope: Scope::ConfirmModal,
        command: Command::Confirm,
        label: "yes",
    },
    CommandBinding {
        key: key(KeyCode::Char('n'), KeyModifiers::NONE),
        scope: Scope::ConfirmModal,
        command: Command::Cancel,
        label: "no",
    },
    CommandBinding {
        key: key(KeyCode::Enter, KeyModifiers::NONE),
        scope: Scope::ConfirmModal,
        command: Command::Confirm,
        label: "yes",
    },
    CommandBinding {
        key: key(KeyCode::Esc, KeyModifiers::NONE),
        scope: Scope::ConfirmModal,
        command: Command::Cancel,
        label: "no",
    },
];

/// Footer hints shown while the label prompt modal is open. Actual key
/// dispatch is handled by `screens::label_prompt::handle_key`.
const LABEL_PROMPT_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Esc, KeyModifiers::NONE),
        scope: Scope::LabelPrompt,
        command: Command::Cancel,
        label: "close",
    },
    CommandBinding {
        key: key(KeyCode::Enter, KeyModifiers::NONE),
        scope: Scope::LabelPrompt,
        command: Command::Confirm,
        label: "set",
    },
    CommandBinding {
        key: key(KeyCode::Char('j'), KeyModifiers::NONE),
        scope: Scope::LabelPrompt,
        command: Command::SelectNextSession,
        label: "next",
    },
    CommandBinding {
        key: key(KeyCode::Char('k'), KeyModifiers::NONE),
        scope: Scope::LabelPrompt,
        command: Command::SelectPrevSession,
        label: "prev",
    },
    CommandBinding {
        key: key(KeyCode::Tab, KeyModifiers::NONE),
        scope: Scope::LabelPrompt,
        command: Command::Cancel, // footer-hint only; real dispatch toggles mode
        label: "mode",
    },
];

/// Returns the bindings registered for a given scope. Modal scopes return
/// an empty slice so the footer renders a stable (empty) row until later
/// waves fill them in.
pub fn commands_for_scope(scope: Scope) -> &'static [CommandBinding] {
    match scope {
        Scope::Global => GLOBAL_BINDINGS,
        Scope::MainScreen => MAIN_SCREEN_BINDINGS,
        Scope::SearchModal => SEARCH_MODAL_BINDINGS,
        Scope::FindBar => FIND_BAR_BINDINGS,
        Scope::BookmarkBrowser => BOOKMARK_BROWSER_BINDINGS,
        Scope::ConfirmModal => CONFIRM_MODAL_BINDINGS,
        Scope::LabelPrompt => LABEL_PROMPT_BINDINGS,
        _ => &[],
    }
}

/// Look up a key in the given scope, falling back to Global. Returns the
/// matched command, or None if no binding applies.
pub fn lookup(scope: Scope, ev: KeyEvent) -> Option<Command> {
    // Filter only Release — terminals (and PTYs like `expect`) sometimes deliver
    // typed characters as `Repeat` instead of `Press`, and dropping those
    // breaks both navigation and scroll bindings. Mirrors the same fix already
    // applied to the search modal (commit 5f365a5).
    if ev.kind == crossterm::event::KeyEventKind::Release {
        return None;
    }
    let matches = |b: &&CommandBinding| {
        b.key.code == ev.code && b.key.modifiers == ev.modifiers
    };
    commands_for_scope(scope)
        .iter()
        .find(matches)
        .or_else(|| commands_for_scope(Scope::Global).iter().find(matches))
        .map(|b| b.command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_quits_globally() {
        let ev = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(lookup(Scope::MainScreen, ev), Some(Command::Quit));
        assert_eq!(lookup(Scope::Global, ev), Some(Command::Quit));
    }

    #[test]
    fn ctrl_c_quits_globally() {
        let ev = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(lookup(Scope::MainScreen, ev), Some(Command::Quit));
    }

    #[test]
    fn unknown_key_returns_none() {
        let ev = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE);
        assert!(lookup(Scope::MainScreen, ev).is_none());
    }

    #[test]
    fn release_events_are_ignored() {
        let ev = KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert!(lookup(Scope::MainScreen, ev).is_none());
    }

    #[test]
    fn repeat_events_are_dispatched() {
        // Regression: some terminals/PTYs (notably `expect`) deliver typed
        // characters as `Repeat` rather than `Press`. The lookup must accept
        // them or scroll/navigation bindings silently break.
        let ev = KeyEvent {
            code: KeyCode::Char('G'),
            modifiers: KeyModifiers::SHIFT,
            kind: crossterm::event::KeyEventKind::Repeat,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert_eq!(lookup(Scope::MainScreen, ev), Some(Command::ScrollBottom));
    }

    #[test]
    fn repeat_j_moves_to_next_session() {
        // Sibling regression: sidebar nav must also survive Repeat-kind events.
        let ev = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Repeat,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert_eq!(lookup(Scope::MainScreen, ev), Some(Command::SelectNextSession));
    }

    #[test]
    fn global_scope_returns_two_quit_bindings() {
        let bindings = commands_for_scope(Scope::Global);
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().all(|b| b.command == Command::Quit));
    }

    #[test]
    fn j_and_down_both_select_next_session() {
        let j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(lookup(Scope::MainScreen, j), Some(Command::SelectNextSession));
        assert_eq!(lookup(Scope::MainScreen, down), Some(Command::SelectNextSession));
    }

    #[test]
    fn k_and_up_both_select_prev_session() {
        let k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(lookup(Scope::MainScreen, k), Some(Command::SelectPrevSession));
        assert_eq!(lookup(Scope::MainScreen, up), Some(Command::SelectPrevSession));
    }

    #[test]
    fn g_scrolls_top_and_shift_g_scrolls_bottom() {
        let g = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
        let big_g = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(lookup(Scope::MainScreen, g), Some(Command::ScrollTop));
        assert_eq!(lookup(Scope::MainScreen, big_g), Some(Command::ScrollBottom));
    }

    #[test]
    fn page_down_and_ctrl_d_both_scroll_half_page_down() {
        let pgdn = KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE);
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(lookup(Scope::MainScreen, pgdn), Some(Command::ScrollDownHalf));
        assert_eq!(lookup(Scope::MainScreen, ctrl_d), Some(Command::ScrollDownHalf));
    }

    #[test]
    fn page_up_and_ctrl_u_both_scroll_half_page_up() {
        let pgup = KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE);
        let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(lookup(Scope::MainScreen, pgup), Some(Command::ScrollUpHalf));
        assert_eq!(lookup(Scope::MainScreen, ctrl_u), Some(Command::ScrollUpHalf));
    }

    #[test]
    fn question_mark_opens_help() {
        let q = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(lookup(Scope::MainScreen, q), Some(Command::OpenHelp));
    }
}
