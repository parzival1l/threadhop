//! Command + Scope registry. Mirrors `threadhop_core/tui/keybindings.py`.
//!
//! Wave A only wires Quit (`q`, `Ctrl+C`). Later waves grow the enums and
//! per-scope binding tables without rewriting the lookup contract.

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

/// High-level action a binding fires. Wave A defines only Quit; later waves
/// add NextSession / PrevSession / OpenSearch / ToggleBookmark / etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Quit,
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

/// Returns the bindings registered for a given scope. Wave A only populates
/// `Global` — the other scopes return an empty slice so the footer renders a
/// stable (empty) row until later waves fill them in.
pub fn commands_for_scope(scope: Scope) -> &'static [CommandBinding] {
    match scope {
        Scope::Global => GLOBAL_BINDINGS,
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
}
