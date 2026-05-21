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
    BookmarkModal,
    KanbanModal,
    HelpOverlay,
    LabelPrompt,
    Confirm,
}

/// High-level action a binding fires. Wave E grows this to cover sidebar
/// navigation and transcript scrolling; later waves add OpenSearch /
/// ToggleBookmark / etc.
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
];

/// Returns the bindings registered for a given scope. Modal scopes return
/// an empty slice so the footer renders a stable (empty) row until later
/// waves fill them in.
pub fn commands_for_scope(scope: Scope) -> &'static [CommandBinding] {
    match scope {
        Scope::Global => GLOBAL_BINDINGS,
        Scope::MainScreen => MAIN_SCREEN_BINDINGS,
        _ => &[],
    }
}

/// Look up a key in the given scope, falling back to Global. Returns the
/// matched command, or None if no binding applies.
pub fn lookup(scope: Scope, ev: KeyEvent) -> Option<Command> {
    // Normalise to Press — crossterm reports Release/Repeat events too on
    // some terminals, and we don't want to fire bindings on Release.
    if ev.kind != crossterm::event::KeyEventKind::Press {
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
