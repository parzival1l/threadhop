//! Command + Scope registry. Mirrors `threadhop_core/tui/keybindings.py`.
//!
//! Phase 0 of the Rust TUI parity plan brings these bindings into 1:1 parity
//! with the Python TUI. The earlier Rust port wrote its own table from
//! scratch and drifted from Python (most visibly `t` for kanban, `b` for
//! toggle-bookmark, `g`/`Shift+G` for scroll). Per user directive, Python
//! parity wins over Vim/Helix/Lazygit norms — even where it conflicts.
//!
//! Bindings the audit flagged as Python-only but where Rust has no real
//! handler yet are wired as **no-op stubs**: pressing the key fires the
//! Command, the App's `handle_key` logs a `not implemented yet` warning and
//! surfaces a status_message so muscle memory works even without behaviour.
//! The real handlers land in their owning phase (A, B, …).
//!
//! Rust-only extensions that survive parity:
//!   * `Ctrl-c` — quit (KeyboardInterrupt muscle memory)
//!   * `c` (MainScreen) — open conflict viewer (Rust feature ahead of Python)

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// UI surface a key event is dispatched against. The same physical key can
/// mean different things in different scopes (e.g. `q` quits on Main, cancels
/// on a modal). `commands_for_scope` returns the bindings that apply.
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
    /// Phase 5: kanban-style status board modal — sessions grouped by status
    /// column, h/l moves between columns, j/k within a column.
    Kanban,
    /// Phase 5: conflict viewer modal — reflector-emitted cross-session
    /// decision conflicts, Enter / `r` marks resolved.
    ConflictViewer,
    /// Phase 0 reserves the selection-mode scope so the footer/help can
    /// advertise the bindings; real handlers land in Phase A.
    Selection,
}

/// High-level action a binding fires.
///
/// Phase 0 grows the variant set to cover every Python action; the App's
/// `handle_key` dispatch wires unimplemented variants to `tracing::warn!`
/// + status_message stubs.
#[allow(dead_code, clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    Quit,
    /// Move the sidebar selection one row down.
    SelectNextSession,
    /// Move the sidebar selection one row up.
    SelectPrevSession,
    /// Jump the transcript scroll to the top (`Home` per Python).
    ScrollTop,
    /// Jump the transcript scroll to the bottom (`End` per Python). The
    /// renderer clamps `u16::MAX` to the last line.
    ScrollBottom,
    /// Scroll the transcript down half a page (PageDown / Ctrl-d).
    ScrollDownHalf,
    /// Scroll the transcript up half a page (PageUp / Ctrl-u).
    ScrollUpHalf,
    /// Open the help overlay (`?`).
    OpenHelp,
    /// Confirm/activate current selection.
    Confirm,
    /// Open the FTS search modal (`/`).
    OpenSearchModal,
    /// Open the in-transcript find bar (`Ctrl-f` per Python parity).
    OpenFindBar,
    /// Close the find bar (footer hint while `Scope::FindBar` is active).
    CloseFindBar,
    /// Jump to the currently-highlighted find-bar match.
    JumpToCurrentMatch,
    /// Step to the next find-bar match.
    NextMatch,
    /// Step to the previous find-bar match.
    PrevMatch,
    // ---- Phase 4 ---------------------------------------------------------
    /// Toggle the bookmark on the message at `App::message_cursor`. Now
    /// bound to `Space` (Python's selection-mode toggle key); `b` belongs to
    /// `OpenBookmarkBrowser`.
    ToggleBookmark,
    /// Open the bookmark browser modal (Python: `b`).
    OpenBookmarkBrowser,
    /// Cycle the focused session's status forward (`s`).
    CycleSessionStatus,
    /// Cycle the focused session's status backward (`Shift+S`).
    CycleSessionStatusBack,
    /// Open the explicit label/status picker modal. No Python binding — the
    /// merged Rust modal stays reachable via `Shift+L` for the rename path.
    OpenLabelPrompt,
    /// Generic cancel (Esc / `n` in confirm modals).
    Cancel,
    /// Move the in-transcript message cursor down one message. Python has
    /// no analog; Phase 0 rebound from `Shift+J` to `Ctrl+J` to free
    /// `Shift+J` for `MoveSessionDown`.
    MoveCursorDown,
    /// Move the in-transcript message cursor up one message.
    MoveCursorUp,
    // ---- Phase 5 ---------------------------------------------------------
    /// Open the kanban (status board) modal. Python: `Shift+B`.
    OpenKanban,
    /// Open the conflict viewer modal. Rust-only extension; `c` survives.
    OpenConflictViewer,
    /// Mark the focused conflict as resolved.
    MarkConflictResolved,
    /// Move the kanban column focus left.
    KanbanColumnLeft,
    /// Move the kanban column focus right.
    KanbanColumnRight,
    /// Cycle the kanban item's status to the next column (Rust `m`; Python
    /// `Shift+Right` is the additional alias).
    KanbanMoveItem,
    /// Move the kanban item one column left (Python `Shift+Left`).
    KanbanMoveItemBack,
    // ---- Phase 0 additions (Python parity, no-op stubs) ------------------
    /// Refresh sessions (`r`). Stub.
    RefreshSessions,
    /// Cycle to next theme (`t`). Stub.
    ThemeNext,
    /// Cycle to previous theme (`Shift+T`). Stub.
    ThemePrev,
    /// Shrink the sidebar by one column (`[`). Stub.
    ShrinkSidebar,
    /// Grow the sidebar by one column (`]`). Stub.
    GrowSidebar,
    /// Rename the focused session (`n`). Stub.
    RenameSession,
    /// Copy the resume command for the focused session (`g`). Stub.
    CopyResumeCommand,
    /// Observe / copy observation path (`o`). Stub.
    ObserveSession,
    /// Resume observation (`Shift+O`). Stub.
    ResumeObservation,
    /// Archive the focused session (`a`). Stub.
    ArchiveSession,
    /// Toggle archived view (`Shift+A`). Stub.
    ToggleArchivedView,
    /// Reorder the focused session down (`Shift+J` / `Shift+Down`). Stub.
    MoveSessionDown,
    /// Reorder the focused session up (`Shift+K` / `Shift+Up`). Stub.
    MoveSessionUp,
    /// Focus the transcript pane from the session list (`l`/`Right`). Stub.
    FocusTranscript,
    /// Focus the session list from the transcript (`h`/`Left`). Stub.
    FocusList,
    /// Enter selection mode in the transcript (`m`). Stub.
    EnterSelectionMode,
    /// Edit the focused bookmark's note (`Shift+L`). Stub on MainScreen;
    /// real handler lives in the bookmark browser scope (Wave 2).
    EditBookmarkNote,
    // ---- Deferrals-cleanup pre-pop additions -----------------------------
    /// Open the bookmark-note prompt modal for editing the note on the
    /// currently-selected bookmark. Pre-pop reserves this variant so
    /// Worker D can wire the `L` handler in selection mode and the
    /// bookmark-browser modal without touching the enum again.
    OpenBookmarkNotePrompt,
    /// Toggle the folded/expanded state of the tool message under the
    /// transcript message cursor. Pre-pop reserves the variant; Worker E
    /// adds the key binding (`o` per the Phase C task 3 spec) and the
    /// dispatch branch that flips `App::expanded_tools`.
    ToggleToolFold,
}

/// Single binding row. `label` drives the contextual footer + help overlay.
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
    // Rust-only convenience — matches `KeyboardInterrupt` muscle memory.
    // Documented in the parity audit (§2.12.1) as a kept extension.
    CommandBinding {
        key: key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        scope: Scope::Global,
        command: Command::Quit,
        label: "quit",
    },
];

/// Bindings active on the main screen. Phase 0 rewrites this from the
/// Python `COMMAND_REGISTRY` mapping table. Each row also gets the Global
/// fallback (q / Ctrl-c) via `lookup`.
const MAIN_SCREEN_BINDINGS: &[CommandBinding] = &[
    // ---- Sidebar nav ----
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
    // ---- Scroll (Python uses Home/End; Vim Ctrl-u/Ctrl-d kept as Rust ext) ----
    CommandBinding {
        key: key(KeyCode::Home, KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ScrollTop,
        label: "top",
    },
    CommandBinding {
        key: key(KeyCode::End, KeyModifiers::NONE),
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
    // ---- Help / search / find (Python parity: Ctrl-f for find) ----
    CommandBinding {
        key: key(KeyCode::Char('?'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::OpenHelp,
        label: "help",
    },
    CommandBinding {
        key: key(KeyCode::Char('/'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::OpenSearchModal,
        label: "search",
    },
    CommandBinding {
        key: key(KeyCode::Char('f'), KeyModifiers::CONTROL),
        scope: Scope::MainScreen,
        command: Command::OpenFindBar,
        label: "find",
    },
    // ---- Bookmarks (Python: b = browse, Space = toggle) ----
    CommandBinding {
        key: key(KeyCode::Char('b'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::OpenBookmarkBrowser,
        label: "bookmarks",
    },
    CommandBinding {
        key: key(KeyCode::Char(' '), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ToggleBookmark,
        label: "toggle bookmark",
    },
    // ---- Kanban (Python: Shift+B opens the board) ----
    CommandBinding {
        key: key(KeyCode::Char('B'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::OpenKanban,
        label: "kanban",
    },
    // ---- Theme (Python: t/T) ----
    CommandBinding {
        key: key(KeyCode::Char('t'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ThemeNext,
        label: "next theme",
    },
    CommandBinding {
        key: key(KeyCode::Char('T'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::ThemePrev,
        label: "prev theme",
    },
    // ---- Sidebar width (Python: [ / ]) ----
    CommandBinding {
        key: key(KeyCode::Char('['), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ShrinkSidebar,
        label: "shrink sidebar",
    },
    CommandBinding {
        key: key(KeyCode::Char(']'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::GrowSidebar,
        label: "grow sidebar",
    },
    // ---- Session ops (Python parity) ----
    CommandBinding {
        key: key(KeyCode::Char('r'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::RefreshSessions,
        label: "refresh",
    },
    CommandBinding {
        key: key(KeyCode::Char('n'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::RenameSession,
        label: "rename",
    },
    CommandBinding {
        key: key(KeyCode::Char('g'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::CopyResumeCommand,
        label: "copy resume",
    },
    CommandBinding {
        key: key(KeyCode::Char('o'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ObserveSession,
        label: "observe",
    },
    CommandBinding {
        key: key(KeyCode::Char('O'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::ResumeObservation,
        label: "resume obs",
    },
    CommandBinding {
        key: key(KeyCode::Char('s'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::CycleSessionStatus,
        label: "status →",
    },
    CommandBinding {
        key: key(KeyCode::Char('S'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::CycleSessionStatusBack,
        label: "status ←",
    },
    CommandBinding {
        key: key(KeyCode::Char('a'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::ArchiveSession,
        label: "archive",
    },
    CommandBinding {
        key: key(KeyCode::Char('A'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::ToggleArchivedView,
        label: "show archived",
    },
    // ---- Session reorder (Python: J / Shift+Down, K / Shift+Up) ----
    CommandBinding {
        key: key(KeyCode::Char('J'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::MoveSessionDown,
        label: "session ↓",
    },
    CommandBinding {
        key: key(KeyCode::Down, KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::MoveSessionDown,
        label: "session ↓",
    },
    CommandBinding {
        key: key(KeyCode::Char('K'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::MoveSessionUp,
        label: "session ↑",
    },
    CommandBinding {
        key: key(KeyCode::Up, KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::MoveSessionUp,
        label: "session ↑",
    },
    // ---- Transcript msg-cursor (Rust-only; rebound off Shift+J/K to Ctrl+J/K
    //      so Shift+J/K can carry session reorder per Python) ----
    CommandBinding {
        key: key(KeyCode::Char('j'), KeyModifiers::CONTROL),
        scope: Scope::MainScreen,
        command: Command::MoveCursorDown,
        label: "msg ↓",
    },
    CommandBinding {
        key: key(KeyCode::Char('k'), KeyModifiers::CONTROL),
        scope: Scope::MainScreen,
        command: Command::MoveCursorUp,
        label: "msg ↑",
    },
    // ---- Selection mode + scope toggle (Phase A reserves these; Phase 0
    //      registers the binding so the footer/help reflect them) ----
    CommandBinding {
        key: key(KeyCode::Char('m'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::EnterSelectionMode,
        label: "select mode",
    },
    CommandBinding {
        key: key(KeyCode::Char('l'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::FocusTranscript,
        label: "focus transcript",
    },
    CommandBinding {
        key: key(KeyCode::Right, KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::FocusTranscript,
        label: "focus transcript",
    },
    CommandBinding {
        key: key(KeyCode::Char('h'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::FocusList,
        label: "focus list",
    },
    CommandBinding {
        key: key(KeyCode::Left, KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::FocusList,
        label: "focus list",
    },
    // ---- Rust-only conflict viewer (kept; Python has no analog) ----
    CommandBinding {
        key: key(KeyCode::Char('c'), KeyModifiers::NONE),
        scope: Scope::MainScreen,
        command: Command::OpenConflictViewer,
        label: "conflicts",
    },
    // ---- Label/rename modal opener (Rust-only; Python merges into `n`).
    //      Shift+L kept as a free key so users who want the modal still have
    //      it. ----
    CommandBinding {
        key: key(KeyCode::Char('L'), KeyModifiers::SHIFT),
        scope: Scope::MainScreen,
        command: Command::OpenLabelPrompt,
        label: "label",
    },
];

/// Footer hints shown while the search modal is open. The actual key
/// dispatch is handled directly by `screens::search::handle_key`.
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
    // Python parity: Ctrl-n/Ctrl-p mirror Up/Down inside the search modal.
    CommandBinding {
        key: key(KeyCode::Char('n'), KeyModifiers::CONTROL),
        scope: Scope::SearchModal,
        command: Command::SelectNextSession,
        label: "next hit",
    },
    CommandBinding {
        key: key(KeyCode::Char('p'), KeyModifiers::CONTROL),
        scope: Scope::SearchModal,
        command: Command::SelectPrevSession,
        label: "prev hit",
    },
    CommandBinding {
        key: key(KeyCode::PageDown, KeyModifiers::NONE),
        scope: Scope::SearchModal,
        command: Command::ScrollDownHalf,
        label: "jump down",
    },
    CommandBinding {
        key: key(KeyCode::PageUp, KeyModifiers::NONE),
        scope: Scope::SearchModal,
        command: Command::ScrollUpHalf,
        label: "jump up",
    },
    CommandBinding {
        key: key(KeyCode::Char('x'), KeyModifiers::CONTROL),
        scope: Scope::SearchModal,
        command: Command::Cancel,
        label: "clear",
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
        key: key(KeyCode::Down, KeyModifiers::NONE),
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
    CommandBinding {
        key: key(KeyCode::Up, KeyModifiers::NONE),
        scope: Scope::FindBar,
        command: Command::PrevMatch,
        label: "prev",
    },
];

/// Footer hints shown while the bookmark browser modal is open.
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
        key: key(KeyCode::Down, KeyModifiers::NONE),
        scope: Scope::BookmarkBrowser,
        command: Command::SelectNextSession,
        label: "next",
    },
    CommandBinding {
        key: key(KeyCode::Char('n'), KeyModifiers::CONTROL),
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
        key: key(KeyCode::Up, KeyModifiers::NONE),
        scope: Scope::BookmarkBrowser,
        command: Command::SelectPrevSession,
        label: "prev",
    },
    CommandBinding {
        key: key(KeyCode::Char('p'), KeyModifiers::CONTROL),
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
    CommandBinding {
        key: key(KeyCode::Char('L'), KeyModifiers::SHIFT),
        scope: Scope::BookmarkBrowser,
        command: Command::EditBookmarkNote,
        label: "edit note",
    },
];

/// Footer hints shown while the confirm modal is open.
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

/// Footer hints shown while the label prompt modal is open.
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

/// Footer hints shown while the kanban modal is open.
///
/// Python parity: Left/Right move between columns, Up/Down between cards,
/// Shift+Left / Shift+Right reorder cards across columns. h/j/k/l and `m`
/// kept as Rust-only Vim aliases.
const KANBAN_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Esc, KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::Cancel,
        label: "close",
    },
    CommandBinding {
        key: key(KeyCode::Enter, KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::Confirm,
        label: "open",
    },
    CommandBinding {
        key: key(KeyCode::Char('h'), KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::KanbanColumnLeft,
        label: "col ←",
    },
    CommandBinding {
        key: key(KeyCode::Left, KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::KanbanColumnLeft,
        label: "col ←",
    },
    CommandBinding {
        key: key(KeyCode::Char('l'), KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::KanbanColumnRight,
        label: "col →",
    },
    CommandBinding {
        key: key(KeyCode::Right, KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::KanbanColumnRight,
        label: "col →",
    },
    CommandBinding {
        key: key(KeyCode::Char('j'), KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::SelectNextSession,
        label: "row ↓",
    },
    CommandBinding {
        key: key(KeyCode::Down, KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::SelectNextSession,
        label: "row ↓",
    },
    CommandBinding {
        key: key(KeyCode::Char('k'), KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::SelectPrevSession,
        label: "row ↑",
    },
    CommandBinding {
        key: key(KeyCode::Up, KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::SelectPrevSession,
        label: "row ↑",
    },
    CommandBinding {
        key: key(KeyCode::Char('m'), KeyModifiers::NONE),
        scope: Scope::Kanban,
        command: Command::KanbanMoveItem,
        label: "move",
    },
    CommandBinding {
        key: key(KeyCode::Right, KeyModifiers::SHIFT),
        scope: Scope::Kanban,
        command: Command::KanbanMoveItem,
        label: "move →",
    },
    CommandBinding {
        key: key(KeyCode::Left, KeyModifiers::SHIFT),
        scope: Scope::Kanban,
        command: Command::KanbanMoveItemBack,
        label: "move ←",
    },
];

/// Footer hints shown while the conflict viewer modal is open.
const CONFLICT_VIEWER_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Esc, KeyModifiers::NONE),
        scope: Scope::ConflictViewer,
        command: Command::Cancel,
        label: "close",
    },
    CommandBinding {
        key: key(KeyCode::Enter, KeyModifiers::NONE),
        scope: Scope::ConflictViewer,
        command: Command::Confirm,
        label: "jump",
    },
    CommandBinding {
        key: key(KeyCode::Char('j'), KeyModifiers::NONE),
        scope: Scope::ConflictViewer,
        command: Command::SelectNextSession,
        label: "next",
    },
    CommandBinding {
        key: key(KeyCode::Char('k'), KeyModifiers::NONE),
        scope: Scope::ConflictViewer,
        command: Command::SelectPrevSession,
        label: "prev",
    },
    CommandBinding {
        key: key(KeyCode::Char('r'), KeyModifiers::NONE),
        scope: Scope::ConflictViewer,
        command: Command::MarkConflictResolved,
        label: "resolve",
    },
    CommandBinding {
        key: key(KeyCode::Char('t'), KeyModifiers::NONE),
        scope: Scope::ConflictViewer,
        command: Command::Cancel,
        label: "toggle resolved",
    },
];

/// Footer hints shown while the help overlay is open. Python parity adds `q`
/// as a close key (no quit-while-overlay-up surprise).
const HELP_OVERLAY_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Esc, KeyModifiers::NONE),
        scope: Scope::HelpOverlay,
        command: Command::Cancel,
        label: "close",
    },
    CommandBinding {
        key: key(KeyCode::Char('?'), KeyModifiers::NONE),
        scope: Scope::HelpOverlay,
        command: Command::OpenHelp,
        label: "close",
    },
    CommandBinding {
        key: key(KeyCode::Char('q'), KeyModifiers::NONE),
        scope: Scope::HelpOverlay,
        command: Command::Cancel,
        label: "close",
    },
];

/// Footer hints shown while selection mode is active in the transcript.
/// Phase A wires real handlers; Phase 0 reserves the bindings here so the
/// footer + help overlay advertise them.
const SELECTION_BINDINGS: &[CommandBinding] = &[
    CommandBinding {
        key: key(KeyCode::Char('j'), KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::SelectNextSession,
        label: "next msg",
    },
    CommandBinding {
        key: key(KeyCode::Down, KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::SelectNextSession,
        label: "next msg",
    },
    CommandBinding {
        key: key(KeyCode::Char('k'), KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::SelectPrevSession,
        label: "prev msg",
    },
    CommandBinding {
        key: key(KeyCode::Up, KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::SelectPrevSession,
        label: "prev msg",
    },
    CommandBinding {
        key: key(KeyCode::Char('v'), KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::Confirm,
        label: "range",
    },
    CommandBinding {
        key: key(KeyCode::Char('y'), KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::Confirm,
        label: "copy",
    },
    CommandBinding {
        key: key(KeyCode::Char('e'), KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::Confirm,
        label: "export",
    },
    CommandBinding {
        key: key(KeyCode::Char(' '), KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::ToggleBookmark,
        label: "bookmark",
    },
    CommandBinding {
        key: key(KeyCode::Char('L'), KeyModifiers::SHIFT),
        scope: Scope::Selection,
        command: Command::EditBookmarkNote,
        label: "edit note",
    },
    CommandBinding {
        key: key(KeyCode::Char('m'), KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::EnterSelectionMode,
        label: "exit select",
    },
    CommandBinding {
        key: key(KeyCode::Esc, KeyModifiers::NONE),
        scope: Scope::Selection,
        command: Command::Cancel,
        label: "exit select",
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
        Scope::Kanban => KANBAN_BINDINGS,
        Scope::ConflictViewer => CONFLICT_VIEWER_BINDINGS,
        Scope::HelpOverlay => HELP_OVERLAY_BINDINGS,
        Scope::Selection => SELECTION_BINDINGS,
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

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn q_quits_globally() {
        let e = ev(KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(lookup(Scope::MainScreen, e), Some(Command::Quit));
        assert_eq!(lookup(Scope::Global, e), Some(Command::Quit));
    }

    #[test]
    fn ctrl_c_quits_globally() {
        let e = ev(KeyCode::Char('c'), KeyModifiers::CONTROL);
        // Rust-only extension — Ctrl-c falls back to Global.
        // NB: Scope::MainScreen has `c` bound to OpenConflictViewer (no
        // modifier), but the lookup matches both code AND modifiers, so
        // Ctrl-c still falls through to Global.
        assert_eq!(lookup(Scope::MainScreen, e), Some(Command::Quit));
    }

    #[test]
    fn unknown_key_returns_none() {
        let e = ev(KeyCode::Char('z'), KeyModifiers::NONE);
        assert!(lookup(Scope::MainScreen, e).is_none());
    }

    #[test]
    fn release_events_are_ignored() {
        let e = KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Release,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert!(lookup(Scope::MainScreen, e).is_none());
    }

    #[test]
    fn repeat_events_are_dispatched() {
        // Some terminals/PTYs deliver typed characters as `Repeat`. The
        // lookup must accept them.
        let e = KeyEvent {
            code: KeyCode::End,
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Repeat,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert_eq!(lookup(Scope::MainScreen, e), Some(Command::ScrollBottom));
    }

    #[test]
    fn repeat_j_moves_to_next_session() {
        let e = KeyEvent {
            code: KeyCode::Char('j'),
            modifiers: KeyModifiers::NONE,
            kind: crossterm::event::KeyEventKind::Repeat,
            state: crossterm::event::KeyEventState::NONE,
        };
        assert_eq!(lookup(Scope::MainScreen, e), Some(Command::SelectNextSession));
    }

    #[test]
    fn global_scope_returns_two_quit_bindings() {
        let bindings = commands_for_scope(Scope::Global);
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().all(|b| b.command == Command::Quit));
    }

    #[test]
    fn j_and_down_both_select_next_session() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(Command::SelectNextSession)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Down, KeyModifiers::NONE)),
            Some(Command::SelectNextSession)
        );
    }

    #[test]
    fn k_and_up_both_select_prev_session() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(Command::SelectPrevSession)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Up, KeyModifiers::NONE)),
            Some(Command::SelectPrevSession)
        );
    }

    // ---- Phase 0 parity regressions: the actual rebinds -------------------

    #[test]
    fn home_scrolls_top_and_end_scrolls_bottom_python_parity() {
        // Python uses Home/End for scroll-top/bottom; g and Shift+G now have
        // different jobs (CopyResumeCommand, free).
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Home, KeyModifiers::NONE)),
            Some(Command::ScrollTop)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::End, KeyModifiers::NONE)),
            Some(Command::ScrollBottom)
        );
    }

    #[test]
    fn g_now_copies_resume_command_not_scrolls_top() {
        // Phase 0 parity: `g` is Python's CopyResumeCommand, not ScrollTop.
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('g'), KeyModifiers::NONE)),
            Some(Command::CopyResumeCommand)
        );
    }

    #[test]
    fn shift_g_is_unbound_post_phase_0() {
        // Python has no Shift+G binding — Rust's scroll-bottom moved to End.
        assert!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('G'), KeyModifiers::SHIFT)).is_none()
        );
    }

    #[test]
    fn b_now_opens_bookmark_browser_not_toggles() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('b'), KeyModifiers::NONE)),
            Some(Command::OpenBookmarkBrowser)
        );
    }

    #[test]
    fn space_now_toggles_bookmark() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(Command::ToggleBookmark)
        );
    }

    #[test]
    fn shift_b_now_opens_kanban_not_browser() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('B'), KeyModifiers::SHIFT)),
            Some(Command::OpenKanban)
        );
    }

    #[test]
    fn t_now_cycles_theme_not_opens_kanban() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('t'), KeyModifiers::NONE)),
            Some(Command::ThemeNext)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('T'), KeyModifiers::SHIFT)),
            Some(Command::ThemePrev)
        );
    }

    #[test]
    fn s_now_cycles_status_forward_not_opens_label_prompt() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('s'), KeyModifiers::NONE)),
            Some(Command::CycleSessionStatus)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('S'), KeyModifiers::SHIFT)),
            Some(Command::CycleSessionStatusBack)
        );
    }

    #[test]
    fn shift_j_and_shift_k_reorder_sessions_per_python() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('J'), KeyModifiers::SHIFT)),
            Some(Command::MoveSessionDown)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('K'), KeyModifiers::SHIFT)),
            Some(Command::MoveSessionUp)
        );
    }

    #[test]
    fn ctrl_j_and_ctrl_k_move_msg_cursor_post_phase_0() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('j'), KeyModifiers::CONTROL)),
            Some(Command::MoveCursorDown)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('k'), KeyModifiers::CONTROL)),
            Some(Command::MoveCursorUp)
        );
    }

    #[test]
    fn ctrl_f_opens_find_bar_python_parity() {
        // Python: Ctrl-f opens find-in-transcript. Plain `f` is no longer
        // bound on the main screen.
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('f'), KeyModifiers::CONTROL)),
            Some(Command::OpenFindBar)
        );
        assert!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('f'), KeyModifiers::NONE)).is_none(),
            "plain f should be unbound after Phase 0 — Ctrl-F is the Python parity binding"
        );
    }

    #[test]
    fn page_down_and_ctrl_d_both_scroll_half_page_down() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(Command::ScrollDownHalf)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(Command::ScrollDownHalf)
        );
    }

    #[test]
    fn page_up_and_ctrl_u_both_scroll_half_page_up() {
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::PageUp, KeyModifiers::NONE)),
            Some(Command::ScrollUpHalf)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            Some(Command::ScrollUpHalf)
        );
    }

    #[test]
    fn question_mark_opens_help() {
        let e = ev(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(lookup(Scope::MainScreen, e), Some(Command::OpenHelp));
    }

    // ---- Phase 0 stub bindings (Python-only actions Rust hasn't wired) ----

    #[test]
    fn phase_0_no_op_stubs_are_registered() {
        // Each of these is a Python binding we promised would at least
        // resolve to a Command after Phase 0 — the App's dispatch turns
        // them into a status_message stub.
        let cases: &[(KeyEvent, Command)] = &[
            (ev(KeyCode::Char('r'), KeyModifiers::NONE), Command::RefreshSessions),
            (ev(KeyCode::Char('n'), KeyModifiers::NONE), Command::RenameSession),
            (ev(KeyCode::Char('o'), KeyModifiers::NONE), Command::ObserveSession),
            (ev(KeyCode::Char('O'), KeyModifiers::SHIFT), Command::ResumeObservation),
            (ev(KeyCode::Char('a'), KeyModifiers::NONE), Command::ArchiveSession),
            (ev(KeyCode::Char('A'), KeyModifiers::SHIFT), Command::ToggleArchivedView),
            (ev(KeyCode::Char('['), KeyModifiers::NONE), Command::ShrinkSidebar),
            (ev(KeyCode::Char(']'), KeyModifiers::NONE), Command::GrowSidebar),
            (ev(KeyCode::Char('m'), KeyModifiers::NONE), Command::EnterSelectionMode),
            (ev(KeyCode::Char('l'), KeyModifiers::NONE), Command::FocusTranscript),
            (ev(KeyCode::Char('h'), KeyModifiers::NONE), Command::FocusList),
        ];
        for (key, expected) in cases {
            assert_eq!(
                lookup(Scope::MainScreen, *key),
                Some(*expected),
                "expected {expected:?} for {key:?}"
            );
        }
    }

    #[test]
    fn shift_session_reorder_arrow_aliases() {
        // Python registers both J / Shift+Down (and K / Shift+Up).
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Down, KeyModifiers::SHIFT)),
            Some(Command::MoveSessionDown)
        );
        assert_eq!(
            lookup(Scope::MainScreen, ev(KeyCode::Up, KeyModifiers::SHIFT)),
            Some(Command::MoveSessionUp)
        );
    }

    // ---- Modal-scope Python parity additions -----------------------------

    #[test]
    fn bookmark_browser_arrow_and_ctrl_aliases() {
        for k in [
            ev(KeyCode::Down, KeyModifiers::NONE),
            ev(KeyCode::Char('n'), KeyModifiers::CONTROL),
        ] {
            assert_eq!(
                lookup(Scope::BookmarkBrowser, k),
                Some(Command::SelectNextSession),
                "{k:?} should select next bookmark"
            );
        }
        for k in [
            ev(KeyCode::Up, KeyModifiers::NONE),
            ev(KeyCode::Char('p'), KeyModifiers::CONTROL),
        ] {
            assert_eq!(
                lookup(Scope::BookmarkBrowser, k),
                Some(Command::SelectPrevSession)
            );
        }
        assert_eq!(
            lookup(Scope::BookmarkBrowser, ev(KeyCode::Char('L'), KeyModifiers::SHIFT)),
            Some(Command::EditBookmarkNote)
        );
    }

    #[test]
    fn search_modal_ctrl_aliases_and_clear_and_paging() {
        for k in [
            ev(KeyCode::Char('n'), KeyModifiers::CONTROL),
            ev(KeyCode::Down, KeyModifiers::NONE),
        ] {
            assert_eq!(
                lookup(Scope::SearchModal, k),
                Some(Command::SelectNextSession)
            );
        }
        assert_eq!(
            lookup(Scope::SearchModal, ev(KeyCode::PageDown, KeyModifiers::NONE)),
            Some(Command::ScrollDownHalf)
        );
        assert_eq!(
            lookup(Scope::SearchModal, ev(KeyCode::Char('x'), KeyModifiers::CONTROL)),
            Some(Command::Cancel)
        );
    }

    #[test]
    fn find_bar_arrow_aliases() {
        assert_eq!(
            lookup(Scope::FindBar, ev(KeyCode::Down, KeyModifiers::NONE)),
            Some(Command::NextMatch)
        );
        assert_eq!(
            lookup(Scope::FindBar, ev(KeyCode::Up, KeyModifiers::NONE)),
            Some(Command::PrevMatch)
        );
    }

    #[test]
    fn kanban_arrow_and_shift_arrow_aliases() {
        // Python parity: Left/Right between columns, Shift+Left/Right reorder.
        assert_eq!(
            lookup(Scope::Kanban, ev(KeyCode::Left, KeyModifiers::NONE)),
            Some(Command::KanbanColumnLeft)
        );
        assert_eq!(
            lookup(Scope::Kanban, ev(KeyCode::Right, KeyModifiers::NONE)),
            Some(Command::KanbanColumnRight)
        );
        assert_eq!(
            lookup(Scope::Kanban, ev(KeyCode::Down, KeyModifiers::NONE)),
            Some(Command::SelectNextSession)
        );
        assert_eq!(
            lookup(Scope::Kanban, ev(KeyCode::Right, KeyModifiers::SHIFT)),
            Some(Command::KanbanMoveItem)
        );
        assert_eq!(
            lookup(Scope::Kanban, ev(KeyCode::Left, KeyModifiers::SHIFT)),
            Some(Command::KanbanMoveItemBack)
        );
    }

    #[test]
    fn help_overlay_q_closes_python_parity() {
        // Python's help overlay closes on Esc/?/q.
        assert_eq!(
            lookup(Scope::HelpOverlay, ev(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(Command::Cancel)
        );
    }

    #[test]
    fn selection_scope_reserves_python_bindings() {
        // Phase 0 reserves the selection-mode bindings so the footer/help
        // can advertise them; Phase A wires real handlers.
        assert_eq!(
            lookup(Scope::Selection, ev(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(Command::ToggleBookmark)
        );
        assert_eq!(
            lookup(Scope::Selection, ev(KeyCode::Char('v'), KeyModifiers::NONE)),
            Some(Command::Confirm)
        );
        assert_eq!(
            lookup(Scope::Selection, ev(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Command::Cancel)
        );
    }

    #[test]
    fn conflict_viewer_c_does_not_overlap_with_open_conflict_viewer() {
        // Rust-only `c` opens the viewer from MainScreen; inside the viewer,
        // `c` is unbound (no accidental nested-open).
        assert!(
            lookup(Scope::ConflictViewer, ev(KeyCode::Char('c'), KeyModifiers::NONE)).is_none()
        );
    }
}
