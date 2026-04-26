"""``ClaudeSessions`` — the Textual App.

This module owns the cross-session context-manager App: layout,
refresh loop, keybindings, session discovery, command-registry
dispatch, and ``run_tui`` (the entrypoint the CLI calls when no
subcommand is given).

CSS lives in external ``.tcss`` stylesheets under ``css/`` so the App
body stays free of inline CSS blocks. ``CSS_PATH`` is resolved
relative to this file by Textual.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path

from rich.markup import escape
from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.screen import ModalScreen  # noqa: F401 — historical import surface
from textual.widgets import Header, Input, ListView, Static, TextArea
from textual.worker import Worker

from threadhop_core import indexer
from threadhop_core.cli.export_cleanup import (
    EXPORT_RETENTION_DAYS_DEFAULT,
    cleanup_export_markdown_files,
)
from threadhop_core.config.loader import (
    CONFIG_FILE,
    load_config,
    save_app_config,
)
from threadhop_core.config.update_check import _check_for_update
from threadhop_core.observation.observer_state import _refresh_observer_state
from threadhop_core.session.detection import (
    CLAUDE_PROJECTS,
    detect_project_from_cwd,
    get_active_claude_session_ids,
)
from threadhop_core.storage import db

from .constants import (
    COMMAND_TAG_RE,
    LOCAL_COMMAND_RE,
    MAX_SESSIONS,
    OBSERVER_SUBPROCESS_SIGNATURES,
    REFRESH_INTERVAL,
    SPINNER_FRAMES,
    SPINNER_INTERVAL,
    STATUS_CYCLE,
    STATUS_LABELS,
    STATUS_ORDER,
    STATUS_RANK,
    SYSTEM_REMINDER_RE,
)
from .keybindings import (
    COMMAND_REGISTRY,
    SCOPE_FIND,
    SCOPE_GLOBAL,
    SCOPE_REPLY,
    SCOPE_SELECTION,
    SCOPE_SESSION_LIST,
    SCOPE_TRANSCRIPT,
)
from .screens.bookmark import BookmarkBrowserScreen
from .screens.confirm import ConfirmScreen
from .screens.help import HelpScreen
from .screens.kanban import KanbanScreen
from .screens.label_prompt import LabelPromptScreen
from .screens.search import SearchScreen
from .theme import get_available_themes
from .utils import (
    app_bindings_from_registry,
    build_observe_command,
    copy_to_clipboard,
)
from .widgets.contextual_footer import ContextualFooter
from .widgets.find_bar import FindBar
from .widgets.session_list import SessionItem, SessionStatusHeader
from .widgets.transcript import TranscriptView


# CSS lives in external Textual stylesheet files under ``css/`` (one per
# surface). Resolved relative to this file by Textual via ``CSS_PATH``.
_TUI_CSS_DIR = Path(__file__).resolve().parent / "css"
_TUI_CSS_FILES = [
    str(_TUI_CSS_DIR / "app.tcss"),
    str(_TUI_CSS_DIR / "search.tcss"),
    str(_TUI_CSS_DIR / "bookmark.tcss"),
    str(_TUI_CSS_DIR / "label_prompt.tcss"),
    str(_TUI_CSS_DIR / "confirm.tcss"),
    str(_TUI_CSS_DIR / "help.tcss"),
    str(_TUI_CSS_DIR / "contextual_footer.tcss"),
    str(_TUI_CSS_DIR / "kanban.tcss"),
]


class ClaudeSessions(App):
    """Cross-session context manager for Claude Code"""

    TITLE = "ThreadHop"
    ENABLE_COMMAND_PALETTE = False

    # OpenCode-style native select-to-copy. Textual >=0.86 supports
    # click-and-drag text selection inside widgets and pushes the
    # selection to the OS clipboard via OSC 52 on mouse-up; AUTO_COPY
    # = True is the explicit opt-in. ALLOW_SELECT is True by default
    # but pinned here so a future Textual upgrade flipping the default
    # doesn't silently regress this UX.
    #
    # ``copy_to_clipboard`` is overridden below so the OSC 52 path is
    # joined by a subprocess pbcopy/xclip fallback — OSC 52 requires
    # the terminal to opt-in (iTerm2 has "Allow apps to copy to
    # clipboard" off by default), and falling back through subprocess
    # makes select-to-copy work regardless of terminal config.
    ALLOW_SELECT = True
    AUTO_COPY = True

    CSS_PATH = _TUI_CSS_FILES

    # Bindings come from the shared command registry (ADR-017). Add new
    # App-level commands to COMMAND_REGISTRY above, not here — footer and
    # help overlay both read from the same list.
    #
    # Kanban is a mockup binding deliberately left out of COMMAND_REGISTRY
    # so the help overlay + footer don't advertise an unvalidated feature.
    # Promote to the registry once the view earns its keep.
    BINDINGS = app_bindings_from_registry() + [
        Binding("B", "open_kanban", "Kanban board", show=False, priority=True),
    ]

    SIDEBAR_MIN = 20
    SIDEBAR_MAX = 60
    SIDEBAR_STEP = 4
    SIDEBAR_DEFAULT = 36

    def __init__(self, project_filter: str | None = None, days: int = 10):
        super().__init__()
        self.project_filter = project_filter
        self.days_filter = days
        self.sessions = []
        self._selected_session_id = None
        self._spinner_frame = 0
        self._renaming_session_id = None
        self._show_archived = False
        # Debounce timer for the in-transcript find bar so fast typing
        # collapses into a single highlight pass.
        self._find_timer = None
        self._footer_note: str | None = None
        self._footer_note_timer = None

        # Open the SQLite store and run the one-time config.json → SQLite
        # migration (ADR-001). Both calls are idempotent — init_db applies
        # pending schema migrations, and migrate_config_json_to_sqlite is
        # guarded by a settings flag so only the first ever launch does
        # real work.
        self.conn = db.init_db()
        db.migrate_config_json_to_sqlite(
            self.conn, CONFIG_FILE, CLAUDE_PROJECTS
        )

        self.config = load_config(self.conn)
        # Defensive defaults: load_config always returns these, but guard
        # in case a future code path bypasses it.
        if "session_names" not in self.config:
            self.config["session_names"] = {}
        if "session_order" not in self.config:
            self.config["session_order"] = []
        # Register OpenCode-derived themes (ADR-portable Radix 12-step
        # scale, both dark/light variants). Done before the saved-theme
        # lookup so a config that names "opencode-dark" resolves cleanly
        # on first launch.
        try:
            from threadhop_core.tui.theme import load_opencode_themes
            for theme in load_opencode_themes():
                self.register_theme(theme)
        except Exception:
            pass

        saved_theme = self.config.get("theme", "opencode-dark")
        if saved_theme in self.available_themes:
            self.theme = saved_theme

    def notify(self, message, **kwargs):
        if str(message).strip().lower() == "ok":
            return
        super().notify(message, **kwargs)

    def compose(self) -> ComposeResult:
        yield Header(show_clock=True)
        yield ListView(id="session-list")
        yield Vertical(
            FindBar(id="find-bar"),
            TranscriptView(id="transcript-scroll"),
            id="transcript-column",
        )
        yield Vertical(TextArea(id="reply-input"), id="input-container")
        yield ContextualFooter(id="contextual-footer")

    def copy_to_clipboard(self, text: str) -> None:
        """Push ``text`` to the OS clipboard via OSC 52 *and* subprocess.

        Textual's default ``copy_to_clipboard`` writes an OSC 52 escape
        sequence to the terminal — that's the right thing for SSH
        sessions and supports any terminal that opts in. But several
        common macOS / Linux terminals (iTerm2 with default settings,
        many tmux configs without ``set -g set-clipboard on``) drop
        OSC 52 silently, which makes Textual's "AUTO_COPY on mouse-up
        of a text selection" feel like it's broken — the user selects
        text, releases the mouse, and nothing lands on the clipboard.

        Falling back through the module-level ``copy_to_clipboard``
        helper (pbcopy on macOS, xclip on Linux) makes select-to-copy
        work regardless of terminal opt-in. We call both: OSC 52 for
        the SSH-friendly path, then subprocess as belt-and-braces. If
        both succeed, the subprocess write happens last and wins —
        same end state, just more robust.
        """
        try:
            super().copy_to_clipboard(text)
        except Exception:
            # OSC 52 emit can fail on exotic drivers; the subprocess
            # fallback below still has a shot.
            pass
        try:
            copy_to_clipboard(text)
        except Exception:
            pass

    def on_mount(self) -> None:
        # Apply persisted sidebar width (defaults to CSS value of 36).
        width = self.config.get("sidebar_width", self.SIDEBAR_DEFAULT)
        width = max(self.SIDEBAR_MIN, min(self.SIDEBAR_MAX, int(width)))
        self._apply_sidebar_width(width)

        # Find bar is hidden until the user invokes Ctrl+F or jumps in
        # from the cross-session SearchScreen. Setting ``display`` (not
        # a CSS class) keeps the bar out of the layout flow entirely,
        # so the transcript fills the column while it's inactive.
        try:
            self.query_one("#find-bar", FindBar).display = False
        except Exception:
            pass

        self.update_titles()
        self.load_sessions()
        self.query_one("#session-list", ListView).focus()
        self.set_interval(REFRESH_INTERVAL, self.auto_refresh_background)
        self._spinner_frame = 0
        self.set_interval(SPINNER_INTERVAL, self.animate_spinners)
        self._run_export_cleanup()
        # Prime the contextual footer now that all widgets are mounted
        # and the sidebar has focus. on_descendant_focus also covers
        # this, but calling refresh_footer directly makes startup
        # deterministic in tests that don't emit focus events.
        self.refresh_footer()

        # 24h startup update check (ADR-027). Gates (env / TTY / cache /
        # not-inside-claude) run inside `_check_for_update`; if a newer
        # release is out, surface it as a transient toast. Wrapped in a
        # broad try/except because a version check must never break the
        # TUI — the helper already swallows network errors, this catches
        # anything else.
        try:
            update_info = _check_for_update()
        except Exception:
            update_info = None
        if update_info is not None:
            self.notify(
                f"ThreadHop {update_info.latest} available — "
                "run `threadhop update`.",
                title="Update available",
                severity="information",
                timeout=10,
            )

    def update_titles(self) -> None:
        filter_info = ""
        if self.project_filter:
            # Show just the last part of the project dir name for readability
            short_name = self.project_filter.rsplit("-", 1)[-1]
            filter_info += f" [{short_name}]"
        else:
            filter_info += " [all]"
        filter_info += f" ({self.days_filter}d)"
        self.query_one("#session-list").border_title = (
            f"Sessions{filter_info} [J/K]reorder"
        )
        self.query_one("#transcript-scroll").border_title = "Transcript"
        # The input container has no top border in the OpenCode-style
        # layout, so a border-title would render to nothing. Reply
        # keybind hints live in ContextualFooter instead.

    def animate_spinners(self) -> None:
        self._spinner_frame = (self._spinner_frame + 1) % len(SPINNER_FRAMES)
        list_view = self.query_one("#session-list", ListView)
        for item in list_view.children:
            if isinstance(item, SessionItem):
                item.update_spinner(self._spinner_frame)

    def auto_refresh_background(self) -> None:
        self.run_worker(
            self._gather_session_data, exclusive=True, group="refresh", thread=True
        )

    def on_worker_state_changed(self, event: Worker.StateChanged) -> None:
        if (
            event.worker.group == "refresh"
            and event.worker.is_finished
            and not event.worker.is_cancelled
        ):
            if event.worker.result:
                self._apply_session_data(event.worker.result)

    def _get_active_claude_sessions(self) -> set[str]:
        """Detect which session IDs have a running claude process on macOS/Linux.

        Returns a set of active session IDs by scanning running claude processes.
        For `claude -r <id>`, we get the ID directly from args.
        For `claude -r` (no ID) or `claude -c`, we match by CWD to find the
        most recent session in that project.
        """
        return get_active_claude_session_ids()

    def _gather_session_data(self) -> dict:
        """Gather session data from JSONL files"""
        sessions = []
        cutoff = (
            datetime.now() - timedelta(days=self.days_filter)
        ).timestamp()
        active_session_ids = self._get_active_claude_sessions()

        try:
            for project_dir in CLAUDE_PROJECTS.iterdir():
                if not project_dir.is_dir():
                    continue
                project_name = project_dir.name

                # Project filter: substring match on directory name
                if self.project_filter:
                    if self.project_filter.lower() not in project_name.lower():
                        continue

                for jsonl in project_dir.glob("*.jsonl"):
                    if jsonl.name.startswith("agent-"):
                        continue
                    stat = jsonl.stat()

                    # Days filter: skip files older than cutoff
                    if stat.st_mtime < cutoff:
                        continue

                    custom_title = None
                    ai_title = None
                    first_user_msg = None
                    session_cwd = None
                    is_working = False
                    last_msg_type = None
                    has_pending_tool = False
                    user_turn_count = 0

                    try:
                        with open(jsonl) as f:
                            for line in f:
                                try:
                                    msg = json.loads(line)
                                    msg_type = msg.get("type")
                                    if msg_type == "custom-title":
                                        custom_title = msg.get("customTitle", "")
                                    elif msg_type == "ai-title":
                                        ai_title = msg.get("aiTitle", "")
                                    elif msg_type == "user" and not first_user_msg:
                                        if "toolUseResult" not in msg:
                                            content = msg.get("message", {}).get(
                                                "content", ""
                                            )
                                            if isinstance(content, list):
                                                text_parts = [
                                                    b.get("text", "")
                                                    for b in content
                                                    if isinstance(b, dict)
                                                    and b.get("type") == "text"
                                                ]
                                                content = " ".join(text_parts)
                                            if isinstance(content, str) and content.strip():
                                                text = SYSTEM_REMINDER_RE.sub("", content)
                                                text = LOCAL_COMMAND_RE.sub("", text)
                                                text = COMMAND_TAG_RE.sub(" ", text)
                                                text = " ".join(text.split())
                                                if text:
                                                    first_user_msg = text[:50]
                                    if "cwd" in msg and not session_cwd:
                                        session_cwd = msg.get("cwd")
                                    if msg_type == "assistant":
                                        content = msg.get("message", {}).get(
                                            "content", []
                                        )
                                        has_pending_tool = any(
                                            b.get("type") == "tool_use"
                                            for b in content
                                            if isinstance(b, dict)
                                        )
                                        last_msg_type = "assistant"
                                    elif msg_type == "user":
                                        if msg.get("toolUseResult"):
                                            has_pending_tool = False
                                            last_msg_type = "tool_result"
                                        else:
                                            last_msg_type = "user"
                                            # Count only genuine user prompts
                                            # (tool results also have type=user;
                                            # ADR-003 assistant chunks share a
                                            # message.id so counting assistant
                                            # lines would massively inflate).
                                            user_turn_count += 1
                                except:
                                    pass
                        # Session is "active" if a claude process is running for it
                        session_id = jsonl.stem
                        has_process = session_id in active_session_ids
                        # "Working" = active process AND currently processing
                        recently_modified = (
                            datetime.now().timestamp() - stat.st_mtime < 300
                        )
                        is_working = has_process and recently_modified and (
                            (has_pending_tool and last_msg_type == "assistant")
                            or last_msg_type == "user"
                        )
                    except:
                        pass

                    # Skip observer/reflector subprocess sessions. Every
                    # `claude -p` call spawned by the observation pipeline
                    # opens a fresh Claude Code session whose first user
                    # message is the prompt template itself — they are
                    # extractor traffic, not user-facing conversations.
                    if first_user_msg and first_user_msg.startswith(
                        OBSERVER_SUBPROCESS_SIGNATURES
                    ):
                        continue

                    # Title priority: custom > ai > first message
                    session_title = custom_title or ai_title or first_user_msg
                    if session_cwd:
                        project_name = session_cwd.replace(str(Path.home()), "~")

                    sessions.append(
                        {
                            "path": str(jsonl),
                            "project": project_name,
                            "cwd": session_cwd,
                            "session_id": jsonl.stem,
                            "created": stat.st_ctime,
                            "modified": stat.st_mtime,
                            "title": session_title,
                            # Kept separate from ``title`` so surfaces
                            # that deliberately avoid ai-titles (e.g.
                            # the Kanban) can pick just the user-authored
                            # rename without the AI-auto-summarize line
                            # slipping in as a fallback.
                            "custom_title": custom_title or "",
                            # ai-title kept alongside so the Kanban can
                            # mirror what `claude -r` shows in Claude
                            # Code's own session picker: user-rename
                            # wins, then AI-generated title, then the
                            # first user message. Sidebar continues to
                            # use the `title` composite for backward
                            # compat.
                            "ai_title": ai_title or "",
                            "first_user_msg": first_user_msg,
                            "is_active": has_process,
                            "is_working": is_working,
                            # user prompts × 2 ≈ exchange count (one
                            # user msg + one assistant reply per turn);
                            # see KanbanScreen meta line.
                            "turn_count": user_turn_count * 2,
                        }
                    )
        except:
            pass

        # --- Incremental indexing (task #9) ---
        # Piggyback on the 5s refresh cycle: for each discovered session,
        # index only the bytes appended since the last run.  Errors in one
        # session's indexing must not affect others or the refresh cycle.
        for s in sessions:
            try:
                indexer.index_session_incremental(
                    self.conn, s["session_id"], s["path"],
                )
            except Exception:
                pass

        return {"sessions": sessions}

    def _apply_session_data(self, data: dict, force_rebuild: bool = False) -> None:
        self.sessions = data["sessions"]

        for s in self.sessions:
            s["path"] = Path(s["path"])

        # Keep the `sessions` table in step with the filesystem scan so
        # user actions (rename, reorder, last_viewed) that target any
        # currently-visible session can UPDATE a row that actually exists.
        # upsert_session intentionally leaves user-owned columns
        # (custom_name, sort_order, last_viewed, status) alone.
        try:
            with db.transaction(self.conn):
                for s in self.sessions:
                    db.upsert_session(
                        self.conn,
                        session_id=s["session_id"],
                        session_path=str(s["path"]),
                        project=s.get("project"),
                        cwd=s.get("cwd"),
                        created_at=s.get("created"),
                        modified_at=s.get("modified"),
                    )
        except Exception:
            # A DB hiccup shouldn't break the TUI — we'll retry on the
            # next refresh tick. Reads still work because the TUI uses
            # in-memory self.sessions for display.
            pass

        # Stamp each session dict with its persisted sidebar metadata in
        # one bulk read after the upsert so new sessions get the DEFAULT
        # 'active' status and the ADR-021 observation bit without
        # per-session queries during the 5-second refresh cycle.
        try:
            sidebar_state = db.get_session_sidebar_metadata(self.conn)
            for s in self.sessions:
                state = sidebar_state.get(s["session_id"], {})
                s["status"] = state.get("status", "active")
                s["has_observations"] = bool(
                    state.get("has_observations", False)
                )
        except Exception:
            for s in self.sessions:
                s.setdefault("status", "active")
                s.setdefault("has_observations", False)

        self._apply_stable_ordering()
        self._update_session_list(force_rebuild)
        self.refresh_transcript()

        # If the Kanban modal is open, push the same fresh data into it
        # so board cards stay in step with the sidebar. The screen stack
        # is a list; the top is the active screen. We check the type
        # rather than storing a reference so the screen is fully owned
        # by Textual's screen stack and GC'd on dismiss without extra
        # cleanup on our side.
        try:
            top = self.screen
        except Exception:
            top = None
        if isinstance(top, KanbanScreen):
            try:
                top.update_sessions(
                    self.sessions,
                    self.config.get("session_names", {}) or {},
                )
            except Exception:
                # A render hiccup in the modal must not break the 5s
                # refresh loop for the rest of the app.
                pass

    def _apply_stable_ordering(self) -> None:
        if "session_order" not in self.config:
            self.config["session_order"] = []

        saved_order = self.config["session_order"]
        session_map = {s["session_id"]: s for s in self.sessions}
        current_ids = set(session_map.keys())
        saved_ids = set(saved_order)

        new_ids = current_ids - saved_ids
        new_sessions = [session_map[sid] for sid in new_ids]
        new_sessions.sort(key=lambda x: x["modified"], reverse=True)
        new_order = [s["session_id"] for s in new_sessions]

        existing_order = [sid for sid in saved_order if sid in current_ids]
        final_order = new_order + existing_order

        if final_order != saved_order:
            self.config["session_order"] = final_order
            # Session ordering lives in SQLite per ADR-001.
            try:
                db.set_session_order(self.conn, final_order)
            except Exception:
                pass

        order_index = {sid: i for i, sid in enumerate(final_order)}
        # Primary key: status rank (ADR-004 grouping). Secondary key: the
        # user's manual sort order. An unknown status sorts after all known
        # ones so a typo or future value never vanishes from the list.
        self.sessions.sort(
            key=lambda x: (
                STATUS_RANK.get(x.get("status", "active"), len(STATUS_ORDER)),
                order_index.get(x["session_id"], 999999),
            )
        )

    def _update_session_list(self, force_rebuild: bool = False) -> None:
        all_sessions = self.sessions[:MAX_SESSIONS]

        # Split into active (non-archived) and archived groups.
        active_sessions = [s for s in all_sessions if s.get("status") != "archived"]
        archived_sessions = [s for s in all_sessions if s.get("status") == "archived"]

        if self._show_archived:
            display_sessions = active_sessions + archived_sessions
        else:
            display_sessions = active_sessions

        # Bucket sessions by status. Preserve the in-memory order inside
        # each bucket — the sort in _apply_stable_ordering already put them
        # in the right order.
        groups: dict[str, list[dict]] = {}
        for s in display_sessions:
            groups.setdefault(s.get("status", "active"), []).append(s)

        # Build a flat (kind, value) plan for the ListView children. Headers
        # interleave with sessions; only non-empty groups get a header, so
        # the sidebar never shows an empty "Done" divider.
        plan: list[tuple[str, object]] = []
        for status in STATUS_ORDER:
            bucket = groups.get(status)
            if not bucket:
                continue
            plan.append(("header", status))
            for s in bucket:
                plan.append(("session", s))
        # Catch-all for statuses outside STATUS_ORDER (shouldn't happen, but
        # safe): render them under their raw label at the bottom.
        for status, bucket in groups.items():
            if status in STATUS_RANK or not bucket:
                continue
            plan.append(("header", status))
            for s in bucket:
                plan.append(("session", s))

        list_view = self.query_one("#session-list", ListView)

        # Build fingerprints that capture both structure (headers) and
        # order (session ids). Any change — status flip, new session,
        # reorder, header added/removed — triggers a full rebuild.
        def _fingerprint_children() -> list[tuple[str, str]]:
            out: list[tuple[str, str]] = []
            for item in list_view.children:
                if isinstance(item, SessionStatusHeader):
                    out.append(("h", item.status))
                elif isinstance(item, SessionItem):
                    out.append(("s", item.session_data.get("session_id", "")))
            return out

        new_fp: list[tuple[str, str]] = [
            ("h", val) if kind == "header" else ("s", val.get("session_id", ""))  # type: ignore[union-attr]
            for kind, val in plan
        ]
        current_fp = _fingerprint_children()

        if force_rebuild or current_fp != new_fp:
            # Preserve selection across rebuild. Prefer the explicit
            # tracker; fall back to whatever is currently highlighted.
            current_session_id = self._selected_session_id
            if (
                not current_session_id
                and list_view.highlighted_child
                and isinstance(list_view.highlighted_child, SessionItem)
            ):
                current_session_id = list_view.highlighted_child.session_data.get(
                    "session_id"
                )

            list_view.clear()
            target_index: int | None = None
            first_session_index: int | None = None
            for i, (kind, val) in enumerate(plan):
                if kind == "header":
                    list_view.append(SessionStatusHeader(val))  # type: ignore[arg-type]
                    continue
                session = val  # type: ignore[assignment]
                session_id = session.get("session_id", "")  # type: ignore[union-attr]
                custom_name = self.config.get("session_names", {}).get(session_id)
                last_viewed = self.config.get("last_viewed", {}).get(session_id, 0)
                is_unread = session["modified"] > last_viewed  # type: ignore[index]
                list_view.append(
                    SessionItem(session, custom_name, is_unread, self._spinner_frame)  # type: ignore[arg-type]
                )
                if first_session_index is None:
                    first_session_index = i
                if session_id == current_session_id:
                    target_index = i

            # If the previously-selected session is gone, land on the first
            # real session (skip headers — they're disabled so a header
            # index would just get bounced past anyway, but being explicit
            # avoids a visible flicker).
            if target_index is None:
                target_index = first_session_index

            if target_index is not None:
                final_index = target_index

                def restore_highlight():
                    list_view.index = final_index

                self.call_after_refresh(restore_highlight)
        else:
            session_map = {s.get("session_id"): s for s in display_sessions}
            for item in list_view.children:
                if isinstance(item, SessionItem):
                    sid = item.session_data.get("session_id")
                    if sid in session_map:
                        item.session_data.update(session_map[sid])
                        item.refresh_label()

    def refresh_transcript(self) -> None:
        transcript = self.query_one("#transcript-scroll", TranscriptView)

        # Foreign-session lock: the user jumped from search to a session
        # that isn't in the sidebar. The sidebar highlight still points
        # at the ORIGINAL session, so reloading from it here would yank
        # the transcript panel back to that original — wiping out what
        # the user is actually reading. Skip the refresh in that case.
        if transcript._foreign_session_path is not None:
            return

        list_view = self.query_one("#session-list", ListView)
        if list_view.highlighted_child and isinstance(
            list_view.highlighted_child, SessionItem
        ):
            # Schedule the async load
            self.call_later(
                transcript.load_transcript,
                list_view.highlighted_child.session_data["path"],
            )

    def load_sessions(self, force_rebuild: bool = False) -> None:
        data = self._gather_session_data()
        self._apply_session_data(data, force_rebuild)

    async def on_list_view_selected(self, event: ListView.Selected) -> None:
        if isinstance(event.item, SessionItem):
            transcript = self.query_one("#transcript-scroll", TranscriptView)

            # Explicit sidebar selection (click / Enter) always ends a
            # foreign-session search jump, even when the user re-selects
            # the row that was already highlighted — Highlighted doesn't
            # fire in that case, but Selected does.
            #
            # Keep ``_find_query`` alive so the find bar applies to the
            # reselected transcript too; just reset the match cursor.
            transcript._foreign_session_path = None
            transcript._active_highlight_uuid = None
            transcript._active_highlight_terms = None
            if transcript._find_query:
                transcript._find_current = -1

            await transcript.load_transcript(event.item.session_data["path"])

    async def on_list_view_highlighted(self, event: ListView.Highlighted) -> None:
        if isinstance(event.item, SessionItem):
            session_id = event.item.session_data.get("session_id")
            self._selected_session_id = session_id
            transcript = self.query_one("#transcript-scroll", TranscriptView)

            # Manual sidebar navigation ends any in-progress search-jump
            # context. Clear the foreign-session lock AND the active
            # highlight state so the 5s auto-refresh can resume and new
            # reloads don't try to re-apply stale highlights. The
            # search-flow sets these fresh after this handler runs, via
            # queue_scroll_to_uuid, so same-sidebar jumps aren't broken.
            #
            # ``_find_query`` is DELIBERATELY preserved — the find bar
            # follows the user across session switches so the same term
            # keeps highlighting in each transcript they browse. Reset
            # the match cursor so the re-run lands on the first match
            # in the new transcript rather than a stale index.
            transcript._foreign_session_path = None
            transcript._active_highlight_uuid = None
            transcript._active_highlight_terms = None
            if transcript._find_query:
                transcript._find_current = -1

            await transcript.load_transcript(event.item.session_data["path"])

            if "last_viewed" not in self.config:
                self.config["last_viewed"] = {}
            now_ts = datetime.now().timestamp()
            self.config["last_viewed"][session_id] = now_ts
            # last_viewed lives in SQLite per ADR-001.
            try:
                db.set_last_viewed(self.conn, session_id, now_ts)
            except Exception:
                pass

            if event.item.is_unread:
                event.item.is_unread = False
                # query_one can race with mount: a freshly-appended
                # SessionItem may not have composed its Static child yet
                # when the highlight watcher fires.
                try:
                    label = event.item.query_one(".session-label", Static)
                    label.remove_class("unread")
                except Exception:
                    pass

    def action_refresh(self) -> None:
        self.load_sessions()
        self.notify("Refreshed session list")

    def action_toggle_theme_next(self) -> None:
        if not self._input_has_focus():
            self._cycle_theme(1)

    def action_toggle_theme_prev(self) -> None:
        if not self._input_has_focus():
            self._cycle_theme(-1)

    def _cycle_theme(self, direction: int) -> None:
        # ``available_themes`` is the live registry — it includes both the
        # built-ins from Textual *and* the OpenCode-derived themes we
        # registered in __init__. ``get_available_themes`` returns only
        # the static built-in list, so prefer the live source.
        themes = list(self.available_themes.keys())
        try:
            current_idx = themes.index(self.theme)
            next_idx = (current_idx + direction) % len(themes)
        except ValueError:
            next_idx = 0
        self.theme = themes[next_idx]
        self.config["theme"] = self.theme
        # Theme stays in config.json (app-level setting per ADR-001).
        save_app_config(self.config)
        self.notify(f"Theme: {self.theme}")

    def _mirror_custom_title_to_jsonl(
        self, session_id: str, new_name: str
    ) -> None:
        """Append a ``custom-title`` line to the session's JSONL.

        Same shape Claude Code's ``/rename`` produces, so all JSONL
        readers (including Claude Code itself and the next ThreadHop
        refresh tick at tui.py:3776) see the rename. Safe to call
        concurrently with Claude Code's own writes — POSIX guarantees
        single-write appends are atomic, and JSONL is line-addressable,
        so an interleaved Claude Code append lands on a separate line.

        Best-effort: a missing file, permissions hiccup, or unusual
        filesystem state must not take the TUI down. SQLite remains
        the authoritative store for ThreadHop's sidebar rendering.
        """
        session = next(
            (s for s in self.sessions if s.get("session_id") == session_id),
            None,
        )
        if session is None:
            return
        session_path = session.get("path")
        if not session_path:
            return
        try:
            path = Path(session_path)
        except Exception:
            return
        if not path.is_file():
            return
        entry = {
            "type": "custom-title",
            "customTitle": new_name,
            "sessionId": session_id,
            "timestamp": (
                datetime.now(timezone.utc)
                .isoformat(timespec="milliseconds")
                .replace("+00:00", "Z")
            ),
        }
        try:
            with path.open("a") as f:
                f.write(json.dumps(entry) + "\n")
        except OSError:
            # Filesystem errors are surfaced via notify so the user
            # knows the JSONL mirror didn't land, but SQLite has the
            # rename either way.
            self.notify(
                "Rename saved locally but not mirrored to JSONL",
                severity="warning",
            )

    def action_rename_session(self) -> None:
        if self._input_has_focus():
            return
        # `n` doubles as next-match while find mode is active so the
        # Ctrl+F workflow stays keyboard-only without colliding with
        # the rename shortcut the rest of the time.
        if self._find_is_active():
            self.action_find_next()
            return
        list_view = self.query_one("#session-list", ListView)
        if not list_view.highlighted_child or not isinstance(
            list_view.highlighted_child, SessionItem
        ):
            return

        session = list_view.highlighted_child.session_data
        session_id = session.get("session_id", "")
        current_name = self.config.get("session_names", {}).get(session_id, "")

        text_area = self.query_one("#reply-input", TextArea)
        text_area.clear()
        text_area.insert(current_name)
        text_area.focus()
        self.notify(f"Renaming session (current: {current_name or 'auto'})")
        self._renaming_session_id = session_id

    def action_copy_session_id(self) -> None:
        """Copy 'claude -r <session_id>' to clipboard"""
        list_view = self.query_one("#session-list", ListView)
        if not list_view.highlighted_child or not isinstance(
            list_view.highlighted_child, SessionItem
        ):
            return
        session_id = list_view.highlighted_child.session_data.get("session_id", "")
        cmd = f"claude -r {session_id}"
        try:
            if not copy_to_clipboard(cmd):
                raise RuntimeError("clipboard unavailable")
            self.notify(f"Copied: {cmd}")
        except Exception:
            # Fallback: just show the command
            self.notify(f"Resume: {cmd}", timeout=10)

    def _highlighted_session_item(self) -> SessionItem | None:
        list_view = self.query_one("#session-list", ListView)
        item = list_view.highlighted_child
        if isinstance(item, SessionItem):
            return item
        return None

    def _spawn_observer(self, session_id: str) -> bool:
        try:
            subprocess.Popen(
                build_observe_command(session_id),
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
            return True
        except Exception as e:
            self.notify(f"Failed to start observer: {e}", severity="error")
            return False

    def _refresh_session_observation(self, session_id: str) -> dict | None:
        try:
            return _refresh_observer_state(self.conn, session_id)
        except Exception:
            return None

    def _confirm_observer_start(self, session_id: str, prompt: str, success_message: str) -> None:
        def _after(confirm: bool | None) -> None:
            if not confirm:
                return
            state = self._refresh_session_observation(session_id)
            if state is not None and state.get("observer_pid") is not None:
                self.notify("Observer already running")
                return
            if self._spawn_observer(session_id):
                self.notify(success_message)

        self.push_screen(ConfirmScreen(prompt), _after)

    def action_observe_session(self) -> None:
        if self._input_has_focus():
            return
        item = self._highlighted_session_item()
        if item is None:
            return
        session = item.session_data
        session_id = session.get("session_id", "")
        state = self._refresh_session_observation(session_id)
        is_observed = bool(session.get("has_observations"))
        if state is not None and int(state.get("entry_count") or 0) > 0:
            is_observed = True
        if is_observed:
            obs_path = state.get("obs_path") if state else None
            if obs_path and copy_to_clipboard(str(obs_path)):
                self.notify("Observation path copied")
            elif obs_path:
                self.notify(str(obs_path), timeout=10)
            else:
                self.notify("Observation path unavailable", severity="warning")
            return

        if state is not None and state.get("observer_pid") is not None:
            self.notify("Observer already running")
            return
        self._confirm_observer_start(
            session_id,
            "No observations yet. Start observing? (y/n)",
            "Observer starting in background",
        )

    def action_resume_observation(self) -> None:
        if self._input_has_focus():
            return
        item = self._highlighted_session_item()
        if item is None:
            return
        session = item.session_data
        session_id = session.get("session_id", "")
        state = self._refresh_session_observation(session_id)
        entry_count = 0
        if state is not None:
            entry_count = int(state.get("entry_count") or 0)
        if entry_count <= 0:
            self.notify("No observations to resume", severity="warning")
            return
        if state is not None and state.get("observer_pid") is not None:
            self.notify("Observer already running")
            return
        self._confirm_observer_start(
            session_id,
            "Resume observation from last offset? (y/n)",
            "Observation resumed in background",
        )

    def _input_has_focus(self) -> bool:
        if self.query_one("#reply-input", TextArea).has_focus:
            return True
        # The find-bar Input counts as an active text input too — stops
        # top-level bindings like `q`, `/`, `n` from firing while the
        # user is typing a query in the bar.
        try:
            if self.query_one("#find-input", Input).has_focus:
                return True
        except Exception:
            pass
        return False

    def _find_bar(self) -> "FindBar | None":
        try:
            return self.query_one("#find-bar", FindBar)
        except Exception:
            return None

    def _find_is_active(self) -> bool:
        bar = self._find_bar()
        return bool(bar and bar.display)

    def _current_scopes(self) -> list[str]:
        """Resolve which registry scopes are live right now.

        Ordered from outer to inner (``global`` first, most-specific
        last) so the contextual footer and any downstream consumer
        can give precedence to the innermost scope when a key appears
        in more than one.
        """
        scopes = [SCOPE_GLOBAL]
        focused = self.focused

        # Selection mode is inside the transcript but displaces its
        # regular bindings, so it's surfaced as its own scope first.
        try:
            transcript = self.query_one("#transcript-scroll", TranscriptView)
        except Exception:
            transcript = None
        in_selection = bool(transcript and transcript._selection_mode)

        # Find bar is modeless — it can be up while focus is elsewhere.
        # Surface it as its own scope whenever it's visible.
        if self._find_is_active():
            scopes.append(SCOPE_FIND)

        if focused is not None:
            fid = getattr(focused, "id", None)
            if fid == "reply-input":
                scopes.append(SCOPE_REPLY)
            elif fid == "session-list":
                scopes.append(SCOPE_SESSION_LIST)
            elif fid == "find-input":
                # Already covered by SCOPE_FIND above.
                pass
            elif transcript is not None and focused is transcript:
                scopes.append(SCOPE_SELECTION if in_selection else SCOPE_TRANSCRIPT)

        if in_selection and SCOPE_SELECTION not in scopes:
            scopes.append(SCOPE_SELECTION)
        return scopes

    def refresh_footer(self) -> None:
        """Re-render the contextual footer for the current scope stack.

        Called whenever focus moves, selection mode toggles, or the
        find bar opens/closes — the footer picks up the new scope set
        and redraws. No-ops if the footer hasn't mounted yet.
        """
        try:
            footer = self.query_one("#contextual-footer", ContextualFooter)
        except Exception:
            return
        footer.render_for(self._current_scopes(), note=self._footer_note)

    def _set_footer_note(self, message: str | None, *, timeout: float = 8.0) -> None:
        """Show a transient note in the footer without using a toast."""
        self._footer_note = message
        if self._footer_note_timer is not None:
            try:
                self._footer_note_timer.stop()
            except Exception:
                pass
            self._footer_note_timer = None
        if message and timeout > 0:
            self._footer_note_timer = self.set_timer(
                timeout, lambda: self._set_footer_note(None, timeout=0)
            )
        self.refresh_footer()

    def _run_export_cleanup(self) -> None:
        """Best-effort cleanup of stale markdown exports at TUI startup."""
        result = cleanup_export_markdown_files(
            retention_days=self.config.get(
                "export_retention_days",
                EXPORT_RETENTION_DAYS_DEFAULT,
            ),
        )
        try:
            self.log(result.debug_message())
        except Exception:
            pass
        message = result.footer_message()
        if message:
            self._set_footer_note(message)

    def on_descendant_focus(self, event) -> None:
        self.refresh_footer()

    def on_descendant_blur(self, event) -> None:
        self.refresh_footer()

    def action_open_help(self) -> None:
        """Show the context-aware help overlay.

        Skips when an input widget has focus so `?` still types
        normally while composing a reply or a search query.
        """
        if self._input_has_focus():
            return
        self.push_screen(HelpScreen())

    def action_maybe_quit(self) -> None:
        if not self._input_has_focus():
            self.exit()

    def action_maybe_refresh(self) -> None:
        if not self._input_has_focus():
            self.load_sessions()
            self.notify("Refreshed session list")

    def action_start_reply(self) -> None:
        """Focus the reply input"""
        text_area = self.query_one("#reply-input", TextArea)
        if not text_area.has_focus:
            list_view = self.query_one("#session-list", ListView)
            if list_view.highlighted_child and isinstance(
                list_view.highlighted_child, SessionItem
            ):
                self._selected_session_id = (
                    list_view.highlighted_child.session_data.get("session_id")
                )
            text_area.focus()

    def action_open_search(self) -> None:
        """Open the real-time FTS5 search panel (task #14).

        Skips when the reply input has focus so `/` types normally while
        the user is composing a message. Dismissing with a selection
        routes through `_jump_to_search_result` to switch sessions and
        scroll to the source message.
        """
        if self._input_has_focus():
            return
        self.push_screen(
            SearchScreen(
                self.conn,
                config=self.config,
                current_session_id=self._selected_session_id,
            ),
            self._on_search_dismissed,
        )

    def action_open_bookmarks(self) -> None:
        """Open the bookmark browser modal (task #18).

        Skips while a text input holds focus so `b` still types when the
        user is composing a reply / filtering search. Dismiss payload is
        ``(session_id, message_uuid)``; we reuse the search jump path so
        bookmarks in out-of-sidebar sessions load just like search hits.
        """
        if self._input_has_focus():
            return
        self.push_screen(
            BookmarkBrowserScreen(self.conn),
            self._on_bookmark_dismissed,
        )

    def _on_bookmark_dismissed(self, result) -> None:
        """Route a bookmark pick through the same jump path as search
        results. ``None`` means the user hit Escape without picking."""
        if result is None:
            return
        try:
            session_id, message_uuid = result
        except (TypeError, ValueError):
            return
        self._jump_to_search_result(session_id, message_uuid, search_terms=None)

    def action_open_kanban(self) -> None:
        """Open the Kanban board modal (mockup).

        Skips while a text input holds focus so `B` still types when the
        user is composing a reply. Hands the current sessions list + the
        custom-name map to the screen so it renders consistently with the
        sidebar from the very first frame.
        """
        if self._input_has_focus():
            return
        custom_names = self.config.get("session_names", {}) or {}
        self.push_screen(
            KanbanScreen(self.sessions, custom_names),
            self._on_kanban_dismissed,
        )

    def _on_kanban_dismissed(self, session_id) -> None:
        """Enter on a Kanban card dismisses with the picked session_id.

        We reload the sidebar selection to that session and let the
        normal ListView.Highlighted handler drive the transcript load —
        that path also stamps last_viewed and clears the unread flag,
        which we'd otherwise have to duplicate here.
        """
        if not session_id:
            return
        list_view = self.query_one("#session-list", ListView)
        for idx, item in enumerate(list_view.children):
            if (
                isinstance(item, SessionItem)
                and item.session_data.get("session_id") == session_id
            ):
                list_view.index = idx
                list_view.focus()
                return

    def bookmark_toggle_selection(self) -> None:
        """Selection-mode `space`: toggle a bookmark on each selected
        message. Widgets whose uuid isn't in the indexed ``messages``
        table yet (synthetic, pre-index, or a race with the 5 s index
        cycle) are skipped rather than crashing the FK. Duplicates
        within the same selection (multiple widgets resolving to the
        same turn uuid — e.g. tool widgets pinned to the turn's
        canonical id) are collapsed so one press produces one toggle
        per underlying row."""
        # TODO: when the TUI grows a bookmark/research picker, route that flow
        # through ``db.upsert_bookmark`` so chat commands and the TUI share one
        # deterministic ingest path. Selection-mode space intentionally keeps
        # the legacy toggle semantics for the plain built-in bookmark kind.
        transcript = self.query_one("#transcript-scroll", TranscriptView)
        selected = transcript.get_selected_messages()
        if not selected:
            return
        created = 0
        removed = 0
        skipped = 0
        seen: set[str] = set()
        with db.transaction(self.conn):
            for widget in selected:
                uuid = getattr(widget, "_uuid", None)
                if not uuid:
                    skipped += 1
                    continue
                if uuid in seen:
                    continue
                seen.add(uuid)
                # Pre-check the FK target. A missing row means the
                # indexer hasn't caught up or the widget was emitted
                # with an id the indexer doesn't store — skip rather
                # than let IntegrityError bubble up through on_key.
                row = self.conn.execute(
                    "SELECT 1 FROM messages WHERE uuid = ? LIMIT 1",
                    (uuid,),
                ).fetchone()
                if row is None:
                    skipped += 1
                    continue
                result = db.toggle_bookmark(self.conn, uuid)
                if result is None:
                    removed += 1
                else:
                    created += 1
        parts = []
        if created:
            parts.append(f"{created} bookmarked")
        if removed:
            parts.append(f"{removed} removed")
        if skipped and not parts:
            parts.append(f"{skipped} skipped (not indexed yet)")
        self.notify(", ".join(parts) if parts else "No bookmarks changed")

    def bookmark_prompt_label(self) -> None:
        """Selection-mode `L`: prompt for a note on the (single) focused
        message. For range selections we note the cursor message only —
        bulk note editing is an anti-pattern for a short free-text field."""
        transcript = self.query_one("#transcript-scroll", TranscriptView)
        messages = transcript._get_message_widgets()
        if not transcript._selection_mode or not messages:
            return
        idx = transcript._selected_index
        if idx < 0 or idx >= len(messages):
            return
        widget = messages[idx]
        uuid = getattr(widget, "_uuid", None)
        if not uuid:
            self.notify("Message has no uuid — cannot bookmark", severity="warning")
            return

        existing = db.get_bookmark(self.conn, uuid)
        current = existing["note"] if existing and existing.get("note") else ""

        def _apply(new_note: str | None) -> None:
            if new_note is None:
                return
            # If the message isn't bookmarked yet, create one first so we have
            # an id to annotate. This makes `L` on an unbookmarked message
            # behave like "bookmark + note in one step".
            row = db.get_bookmark(self.conn, uuid)
            if row is None:
                row = db.toggle_bookmark(self.conn, uuid)
                if row is None:
                    # Extremely unlikely — the toggle raced a delete.
                    self.notify("Could not create bookmark", severity="error")
                    return
            db.set_bookmark_note(self.conn, row["id"], new_note)
            shown = (new_note or "").strip() or "(no note)"
            self.notify(f"Bookmark note: {shown}")

        self.push_screen(LabelPromptScreen(current), _apply)

    def action_open_find(self) -> None:
        """Toggle the persistent in-transcript find bar.

        Pressing Ctrl+F from anywhere in the app pops the bar open,
        focuses the input, and (if it already contains a query) re-runs
        the find so highlights reflect the current transcript. Pressing
        Ctrl+F again with the bar already open refocuses the input —
        never closes the bar (Escape / × do that) so the query isn't
        accidentally lost.
        """
        bar = self._find_bar()
        if bar is None:
            return
        bar.display = True
        input_widget = bar.query_one("#find-input", Input)
        input_widget.focus()
        existing = (input_widget.value or "").strip()
        if existing:
            self._run_find(input_widget.value)
        self.refresh_footer()

    def action_find_next(self) -> None:
        if not self._find_is_active():
            return
        transcript = self.query_one("#transcript-scroll", TranscriptView)
        current, total = transcript.next_match()
        self._update_find_status(current, total)

    def action_find_prev(self) -> None:
        if not self._find_is_active():
            return
        transcript = self.query_one("#transcript-scroll", TranscriptView)
        current, total = transcript.prev_match()
        self._update_find_status(current, total)

    def _run_find(self, query: str, anchor_uuid: str | None = None) -> None:
        """Apply ``query`` as the find-bar term and refresh the counter."""
        transcript = self.query_one("#transcript-scroll", TranscriptView)
        current, total = transcript.activate_find(query, anchor_uuid=anchor_uuid)
        self._update_find_status(current, total)

    def _update_find_status(self, current: int, total: int) -> None:
        bar = self._find_bar()
        if bar is None:
            return
        try:
            query = bar.query_one("#find-input", Input).value or ""
        except Exception:
            query = ""
        bar.update_status(current, total, bool(query.strip()))

    def _close_find(self) -> None:
        """Dismiss the find bar and drop all highlights.

        Force-reloads the transcript so the inline-highlight rewrites
        (which replace Markdown bodies with plain-text Rich renderables)
        are reverted on all widgets. Focus lands back on the sidebar —
        the typical resting position after a find session.
        """
        bar = self._find_bar()
        transcript = self.query_one("#transcript-scroll", TranscriptView)
        transcript.clear_find_state()
        transcript._active_highlight_uuid = None
        transcript._active_highlight_terms = None
        if bar is not None:
            try:
                bar.query_one("#find-input", Input).value = ""
            except Exception:
                pass
            bar.update_status(0, 0, False)
            bar.display = False
        if transcript.current_path:
            self.call_later(transcript.load_transcript, transcript.current_path, True)
        try:
            self.query_one("#session-list", ListView).focus()
        except Exception:
            pass
        self.refresh_footer()

    def on_input_changed(self, event) -> None:
        """Debounce keystrokes in the find bar into a single highlight pass."""
        try:
            if event.input.id != "find-input":
                return
        except AttributeError:
            return
        if self._find_timer is not None:
            try:
                self._find_timer.stop()
            except Exception:
                pass
        value = event.value
        self._find_timer = self.set_timer(
            0.1, lambda v=value: self._run_find(v)
        )

    def on_input_submitted(self, event) -> None:
        """Enter in the find input = jump to the next match."""
        try:
            if event.input.id != "find-input":
                return
        except AttributeError:
            return
        event.stop()
        self.action_find_next()

    def _on_search_dismissed(self, result) -> None:
        """Dismiss callback: the user either selected a row or pressed Esc."""
        if result is None:
            return
        # Legacy 2-tuple tolerated so stale callers don't break.
        try:
            if len(result) == 3:
                session_id, message_uuid, terms = result
            else:
                session_id, message_uuid = result
                terms = None
        except (TypeError, ValueError):
            return

        # Pre-activate find mode BEFORE the jump so the post-load hook
        # in TranscriptView.load_transcript applies highlights across
        # every matching widget (not just the jump target) as soon as
        # the transcript finishes loading. The anchor_uuid parameter is
        # passed through via the pending-scroll path so the match
        # cursor lands on the result the user actually clicked.
        query = " ".join(terms) if terms else ""
        transcript = self.query_one("#transcript-scroll", TranscriptView)
        bar = self._find_bar()
        if query and bar is not None:
            bar.display = True
            input_widget = bar.query_one("#find-input", Input)
            # Set the find-mode state directly so the transcript reload
            # sees it immediately; also mirror it in the visible Input
            # so the user can edit. Suppress the debounce re-run since
            # activate_find on load will handle the initial highlight.
            transcript._find_query = query
            if self._find_timer is not None:
                try:
                    self._find_timer.stop()
                except Exception:
                    pass
            # Setting Input.value triggers Changed → debounced _run_find;
            # that's fine — it just re-runs the same query after load,
            # keeping state consistent if the user edits the value next.
            input_widget.value = query

        self._jump_to_search_result(session_id, message_uuid, terms)

    def _jump_to_search_result(
        self,
        session_id: str,
        message_uuid: str,
        search_terms: list[str] | None = None,
    ) -> None:
        """Switch to ``session_id`` and scroll its transcript to ``message_uuid``.

        Three cases:
          1. Session is already the selected one → scroll directly.
          2. Session is in the sidebar → change ``list_view.index`` and
             queue the scroll; the normal highlight handler loads the
             transcript then applies the pending scroll.
          3. Session isn't in the sidebar (older than ``--days``, or
             archive hidden) → look up its path from the DB and load
             the transcript directly so search is useful across ALL
             indexed history, not just what's visible in the sidebar.
        """
        list_view = self.query_one("#session-list", ListView)
        transcript = self.query_one("#transcript-scroll", TranscriptView)

        target_idx: int | None = None
        for i, item in enumerate(list_view.children):
            if (
                isinstance(item, SessionItem)
                and item.session_data.get("session_id") == session_id
            ):
                target_idx = i
                break

        if target_idx is not None:
            # Case 1 & 2: session is in the sidebar. Clear any stale
            # foreign-session lock from a previous cross-project jump.
            transcript._foreign_session_path = None
            if list_view.index == target_idx:
                # Same session already loaded — no reload will fire, so
                # the post-load find hook won't run. If find mode is
                # live, apply highlights across every matching widget
                # right here and anchor the cursor on the jump target.
                # Otherwise fall back to the legacy single-widget
                # highlight path (keeps behavior identical when find
                # mode is off).
                transcript._pending_scroll_uuid = None
                transcript._pending_scroll_terms = None
                if transcript._find_query:
                    current, total = transcript.activate_find(
                        transcript._find_query,
                        anchor_uuid=message_uuid,
                    )
                    self._update_find_status(current, total)
                elif not transcript._scroll_to_uuid(message_uuid, search_terms):
                    self.notify(
                        "Message not found in loaded transcript",
                        severity="warning",
                    )
            else:
                transcript.queue_scroll_to_uuid(message_uuid, search_terms)
                list_view.index = target_idx
            list_view.focus()
            return

        # Case 3: session is outside the current sidebar view. Fall back
        # to a direct transcript load using the DB-recorded session_path.
        row = db.get_session(self.conn, session_id)
        if not row or not row.get("session_path"):
            self.notify(
                "Session metadata not found in DB",
                severity="error",
            )
            return
        session_path = Path(row["session_path"])
        if not session_path.exists():
            self.notify(
                "Session file no longer exists on disk",
                severity="warning",
            )
            return

        self._selected_session_id = session_id
        # Lock the foreign session in so the 5s refresh tick doesn't
        # reload the sidebar's selection over the top of this panel.
        # Cleared by the user selecting anything in the sidebar.
        transcript._foreign_session_path = session_path
        transcript.queue_scroll_to_uuid(message_uuid, search_terms)
        # load_transcript is async; schedule and let the pending scroll
        # fire at the end of the load.
        self.call_later(transcript.load_transcript, session_path)

        # Put the foreign session's identity in the transcript border
        # title so the user knows the panel is showing a session that
        # isn't in their sidebar. Gets reset to "Transcript" by the
        # normal selection-exit flow when they click back into a
        # sidebar session.
        name = row.get("custom_name") or row.get("project") or session_id[:8]
        project = row.get("project") or ""
        label = f"Transcript ── [{project}] {name}" if project else f"Transcript ── {name}"
        transcript.border_title = label + "  (from search — not in sidebar)"
        self.notify(
            f"Jumped to out-of-view session: {name}",
            timeout=4,
        )

    def check_action(self, action: str, parameters):
        """Disable the priority Enter binding while a modal is up OR the
        find-bar input has focus.

        Returning False tells Textual the binding doesn't match, so the
        key event passes through to whatever owns focus (the modal, or
        the find input's own Submitted handler).
        """
        if action == "start_reply_or_send":
            if len(self.screen_stack) > 1:
                return False
            try:
                if self.query_one("#find-input", Input).has_focus:
                    return False
            except Exception:
                pass
        return True

    def action_start_reply_or_send(self) -> None:
        """If in list/transcript, go to reply. If in reply, send message."""
        text_area = self.query_one("#reply-input", TextArea)
        if text_area.has_focus:
            self._submit_reply()
        else:
            list_view = self.query_one("#session-list", ListView)
            if list_view.highlighted_child and isinstance(
                list_view.highlighted_child, SessionItem
            ):
                self._selected_session_id = (
                    list_view.highlighted_child.session_data.get("session_id")
                )
            text_area.focus()

    def action_insert_newline(self) -> None:
        text_area = self.query_one("#reply-input", TextArea)
        if text_area.has_focus:
            text_area.insert("\n")

    def action_cancel_reply(self) -> None:
        # Escape closes the find bar first if it's up (Ctrl+F semantics):
        # users expect it to peel off the overlay rather than clear the
        # reply buffer underneath.
        if self._find_is_active():
            self._close_find()
            return
        text_area = self.query_one("#reply-input", TextArea)
        text_area.clear()
        self._renaming_session_id = None
        self.query_one("#session-list", ListView).focus()

    def action_nav_down_from_reply(self) -> None:
        list_view = self.query_one("#session-list", ListView)
        list_view.action_cursor_down()
        if list_view.highlighted_child and isinstance(
            list_view.highlighted_child, SessionItem
        ):
            self._selected_session_id = list_view.highlighted_child.session_data.get(
                "session_id"
            )

    def action_nav_up_from_reply(self) -> None:
        list_view = self.query_one("#session-list", ListView)
        list_view.action_cursor_up()
        if list_view.highlighted_child and isinstance(
            list_view.highlighted_child, SessionItem
        ):
            self._selected_session_id = list_view.highlighted_child.session_data.get(
                "session_id"
            )

    def _submit_reply(self) -> None:
        """Send message to selected session via claude CLI"""
        text_area = self.query_one("#reply-input", TextArea)
        input_text = text_area.text.strip()

        # Check if we're in rename mode
        if self._renaming_session_id:
            session_id = self._renaming_session_id
            self._renaming_session_id = None
            if input_text:
                self.config["session_names"][session_id] = input_text
            else:
                self.config["session_names"].pop(session_id, None)
            # Custom names live in SQLite per ADR-001.
            try:
                db.set_custom_name(self.conn, session_id, input_text or None)
            except Exception:
                pass
            # Mirror SET (not CLEAR) into the session JSONL as a
            # `custom-title` line — same shape Claude Code's /rename
            # writes, which `_gather_session_data` already reads at
            # tui.py:3776. This closes the loop so a name set here
            # shows up in Claude Code's own session surfaces too. On
            # CLEAR we deliberately DO NOT write — that lets the user
            # "drop my local override" without clobbering whatever
            # name Claude Code has on its side (last-write-wins means
            # an empty string would win, which is never what you want).
            if input_text:
                self._mirror_custom_title_to_jsonl(session_id, input_text)
            text_area.clear()
            self.query_one("#session-list", ListView).focus()
            self.load_sessions(force_rebuild=True)
            self.notify(
                f"Renamed to: {input_text}" if input_text else "Name cleared"
            )
            return

        if not input_text:
            return

        # Find the selected session
        list_view = self.query_one("#session-list", ListView)
        if not list_view.highlighted_child or not isinstance(
            list_view.highlighted_child, SessionItem
        ):
            self.notify("No session selected", severity="error")
            return

        session = list_view.highlighted_child.session_data
        session_id = session.get("session_id", "")
        session_cwd = session.get("cwd")

        # Clear input and show sending state
        text_area.clear()
        text_area.focus()
        self.notify("Sending...", timeout=2)

        # Mark session as working immediately
        session["is_working"] = True
        if isinstance(list_view.highlighted_child, SessionItem):
            list_view.highlighted_child.session_data["is_working"] = True
            try:
                label = list_view.highlighted_child.query_one(".session-label", Static)
                label.add_class("working")
                list_view.highlighted_child.update_spinner(self._spinner_frame)
            except:
                pass

        # Run claude CLI in background worker
        # --resume requires running from the project's CWD
        def send_message():
            cmd = ["claude", "-p", "--resume", session_id]
            result = subprocess.run(
                cmd,
                input=input_text,
                capture_output=True,
                text=True,
                cwd=session_cwd or None,
                timeout=300,
            )
            return result

        def on_reply_done(worker: Worker) -> None:
            if worker.is_finished and not worker.is_cancelled:
                result = worker.result
                if result and result.returncode == 0:
                    self.notify("Sent", timeout=2)
                else:
                    err = result.stderr[:100] if result else "Unknown error"
                    self.notify(f"Send failed: {err}", severity="error")
                # Refresh transcript to show new messages
                self.set_timer(1.0, self.refresh_transcript)

        worker = self.run_worker(send_message, thread=True, group="reply")
        worker.on_state_changed = lambda _: (
            on_reply_done(worker) if worker.is_finished else None
        )

    def action_focus_transcript(self) -> None:
        self.query_one("#transcript-scroll", TranscriptView).focus()

    def action_focus_list(self) -> None:
        self.query_one("#session-list", ListView).focus()

    def action_cursor_down(self) -> None:
        if self._input_has_focus():
            return
        self.query_one("#session-list", ListView).action_cursor_down()

    def action_cursor_up(self) -> None:
        if self._input_has_focus():
            return
        self.query_one("#session-list", ListView).action_cursor_up()

    def action_move_session_up(self) -> None:
        if not self._input_has_focus():
            self._move_session(-1)

    def action_move_session_down(self) -> None:
        if not self._input_has_focus():
            self._move_session(1)

    def _move_session(self, direction: int) -> None:
        list_view = self.query_one("#session-list", ListView)
        if not list_view.highlighted_child or not isinstance(
            list_view.highlighted_child, SessionItem
        ):
            return

        session_id = list_view.highlighted_child.session_data.get("session_id")
        if not session_id:
            return

        order = self.config.get("session_order", [])
        if session_id not in order:
            return

        visible_ids = [s["session_id"] for s in self.sessions[:MAX_SESSIONS]]
        if session_id not in visible_ids:
            return

        visible_idx = visible_ids.index(session_id)
        target_visible_idx = visible_idx + direction

        if target_visible_idx < 0 or target_visible_idx >= len(visible_ids):
            return

        swap_with_id = visible_ids[target_visible_idx]

        current_order_idx = order.index(session_id)
        swap_order_idx = order.index(swap_with_id)
        order[current_order_idx], order[swap_order_idx] = (
            order[swap_order_idx],
            order[current_order_idx],
        )

        self.config["session_order"] = order
        # Manual reorder lives in SQLite per ADR-001.
        try:
            db.set_session_order(self.conn, order)
        except Exception:
            pass

        order_index = {sid: i for i, sid in enumerate(order)}
        self.sessions.sort(key=lambda x: order_index.get(x["session_id"], 999999))

        self._selected_session_id = session_id
        self._update_session_list(force_rebuild=True)

        def restore_position():
            for i, item in enumerate(list_view.children):
                if (
                    isinstance(item, SessionItem)
                    and item.session_data.get("session_id") == session_id
                ):
                    list_view.index = i
                    break

        self.call_after_refresh(restore_position)

    def action_cycle_status_forward(self) -> None:
        if not self._input_has_focus():
            self._cycle_session_status(1)

    def action_cycle_status_backward(self) -> None:
        if not self._input_has_focus():
            self._cycle_session_status(-1)

    def _cycle_session_status(self, direction: int) -> None:
        list_view = self.query_one("#session-list", ListView)
        if not list_view.highlighted_child or not isinstance(
            list_view.highlighted_child, SessionItem
        ):
            return

        session = list_view.highlighted_child.session_data
        session_id = session.get("session_id")
        if not session_id:
            return

        current_status = session.get("status", "active")
        try:
            idx = STATUS_CYCLE.index(current_status)
        except ValueError:
            # Status not in cycle (e.g. archived) — don't cycle it.
            return

        new_status = STATUS_CYCLE[(idx + direction) % len(STATUS_CYCLE)]

        # Persist to SQLite immediately.
        try:
            db.set_session_status(self.conn, session_id, new_status)
        except Exception:
            self.notify("Failed to update status", severity="error")
            return

        # Update in-memory state and re-render.
        session["status"] = new_status
        self._selected_session_id = session_id
        self._update_session_list(force_rebuild=True)

        # Restore highlight to the same session after rebuild.
        def restore_position():
            for i, item in enumerate(list_view.children):
                if (
                    isinstance(item, SessionItem)
                    and item.session_data.get("session_id") == session_id
                ):
                    list_view.index = i
                    break

        self.call_after_refresh(restore_position)
        self.notify(f"Status: {STATUS_LABELS.get(new_status, new_status)}")

    def action_archive_session(self) -> None:
        """Toggle selected session between archived and active status"""
        if self._input_has_focus():
            return
        list_view = self.query_one("#session-list", ListView)
        if not list_view.highlighted_child or not isinstance(
            list_view.highlighted_child, SessionItem
        ):
            return
        session = list_view.highlighted_child.session_data
        session_id = session.get("session_id")
        if not session_id:
            return
        current_status = session.get("status", "active")
        if current_status == "archived":
            new_status = "active"
        else:
            new_status = "archived"
        try:
            db.set_session_status(self.conn, session_id, new_status)
        except Exception:
            self.notify("Failed to update session status", severity="error")
            return
        # Update in-memory status and re-render.
        session["status"] = new_status
        self._selected_session_id = session_id
        self._update_session_list(force_rebuild=True)

        def restore_position():
            for i, item in enumerate(list_view.children):
                if (
                    isinstance(item, SessionItem)
                    and item.session_data.get("session_id") == session_id
                ):
                    list_view.index = i
                    break

        self.call_after_refresh(restore_position)
        if new_status == "archived":
            self.notify("Session archived")
        else:
            self.notify("Session restored")

    def action_toggle_archive_view(self) -> None:
        """Toggle visibility of archived sessions in the sidebar"""
        if self._input_has_focus():
            return
        self._show_archived = not self._show_archived
        if self._show_archived:
            self.notify("Showing archived sessions")
        else:
            self.notify("Hiding archived sessions")
        self._update_session_list(force_rebuild=True)

    def _apply_sidebar_width(self, width: int) -> None:
        """Set grid columns to the given sidebar width."""
        self.screen.styles.grid_columns = f"{width} 1fr"

    def action_shrink_sidebar(self) -> None:
        if self._input_has_focus():
            return
        current = self.config.get("sidebar_width", self.SIDEBAR_DEFAULT)
        new_width = max(self.SIDEBAR_MIN, int(current) - self.SIDEBAR_STEP)
        self._apply_sidebar_width(new_width)
        self.config["sidebar_width"] = new_width
        save_app_config(self.config)

    def action_grow_sidebar(self) -> None:
        if self._input_has_focus():
            return
        current = self.config.get("sidebar_width", self.SIDEBAR_DEFAULT)
        new_width = min(self.SIDEBAR_MAX, int(current) + self.SIDEBAR_STEP)
        self._apply_sidebar_width(new_width)
        self.config["sidebar_width"] = new_width
        save_app_config(self.config)

    def action_scroll_transcript_up(self) -> None:
        self.query_one("#transcript-scroll", TranscriptView).scroll_page_up(
            animate=False
        )

    def action_scroll_transcript_down(self) -> None:
        self.query_one("#transcript-scroll", TranscriptView).scroll_page_down(
            animate=False
        )

    def action_scroll_transcript_home(self) -> None:
        self.query_one("#transcript-scroll", TranscriptView).scroll_home(
            animate=False
        )

    def action_scroll_transcript_end(self) -> None:
        self.query_one("#transcript-scroll", TranscriptView).scroll_end(
            animate=False
        )


def run_tui(project: str | None, days: int, all_projects: bool) -> int:
    if not project and not all_projects:
        detected = detect_project_from_cwd()
        if detected:
            project = detected
    app = ClaudeSessions(project_filter=project, days=days)
    app.run()
    return 0
