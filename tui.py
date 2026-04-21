"""ThreadHop Textual UI.

Imported lazily from `threadhop` so CLI dispatch can parse args before
importing Textual.
"""

from rich.console import Group
from rich.markdown import Markdown
from rich.markup import escape
from rich.text import Text
from textual.app import App, ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widgets import Header, Input, ListItem, ListView, Static, TextArea
from textual.worker import Worker
from time import perf_counter

import threadhop as _core
from threadhop import *  # noqa: F401,F403

globals().update({
    name: value
    for name, value in _core.__dict__.items()
    if name.startswith("_") and not name.startswith("__")
})


def commands_for_scope(scope: str) -> list[Command]:
    return [c for c in COMMAND_REGISTRY if c.scope == scope]


def format_command_keys(cmd: Command) -> str:
    """Render a command's keys as a slash-separated list for display."""
    return "/".join(format_key(k) for k in cmd.keys)


def app_bindings_from_registry() -> list[Binding]:
    """Derive App-level ``Binding`` entries from the registry.

    Every Command with an ``action`` is bound at the App level. The
    scope field is advisory for display — app bindings fire globally,
    but each action already gates itself on focus / mode state (see
    e.g. ``_input_has_focus`` in action handlers). Widget-local keys
    (selection mode, find bar, search modal) are deliberately skipped
    because their widgets handle the key in ``on_key`` and stop the
    event before it reaches an app binding.
    """
    bindings: list[Binding] = []
    for cmd in COMMAND_REGISTRY:
        if cmd.action is None:
            continue
        for key in cmd.keys:
            bindings.append(
                Binding(
                    key,
                    cmd.action,
                    cmd.description,
                    show=False,
                    priority=cmd.priority,
                )
            )
    # Escape on the reply input action is the app-level cancel_reply;
    # it doubles as "close find bar" when the bar is up (see
    # action_cancel_reply). Registered via the SCOPE_REPLY entry above.
    return bindings


def format_age(timestamp: float) -> str:
    """Format a timestamp as a human-readable age string."""
    age = datetime.now().timestamp() - timestamp
    if age < 60:
        return f"{int(age)}s"
    elif age < 3600:
        return f"{int(age / 60)}m"
    elif age < 86400:
        return f"{int(age / 3600)}h"
    else:
        return f"{int(age / 86400)}d"


def _supports_observation_emoji() -> bool:
    """Return whether the current terminal can safely encode the emoji marker."""
    if os.environ.get("THREADHOP_ASCII_OBSERVATION_MARKER") == "1":
        return False
    encoding = sys.stdout.encoding or "utf-8"
    try:
        OBSERVATION_MARKER.encode(encoding)
    except (LookupError, UnicodeEncodeError):
        return False
    return True


def _observation_marker_text() -> Text:
    """Return the ADR-021 observed-session marker."""
    if _supports_observation_emoji():
        return Text(OBSERVATION_MARKER, style="dim")
    return Text(OBSERVATION_MARKER_FALLBACK, style="dim cyan")


def _session_display_name(session_data: dict, custom_name: str | None) -> str:
    """Resolve the user-facing session label before sidebar truncation."""
    title = session_data.get("title", "")
    if custom_name:
        return custom_name
    if title:
        return title
    return session_data["project"]

def copy_to_clipboard(text: str) -> bool:
    """Copy text to the system clipboard."""
    try:
        if sys.platform == "darwin":
            subprocess.run(["pbcopy"], input=text.encode(), check=True)
        else:
            subprocess.run(
                ["xclip", "-selection", "clipboard"],
                input=text.encode(),
                check=True,
            )
        return True
    except Exception:
        return False


def build_observe_command(session_id: str) -> list[str]:
    """Spawn the CLI sidecar through the current executable script."""
    return [str(Path(_core.__file__).resolve()), "observe", "--session", session_id]


def render_session_label_text(
    session_data: dict,
    *,
    custom_name: str | None = None,
    spinner_frame: int = 0,
) -> Text:
    """Build the Rich label for one sidebar session row."""
    is_working = session_data.get("is_working", False)
    is_active = session_data.get("is_active", False)
    if is_working:
        status = SPINNER_FRAMES[spinner_frame % len(SPINNER_FRAMES)]
    elif is_active:
        status = "●"
    else:
        status = "○"

    display = _session_display_name(session_data, custom_name)
    indicator = (
        _observation_marker_text()
        if session_data.get("has_observations")
        else None
    )
    reserved_width = 0
    if indicator is not None:
        reserved_width = 1 + indicator.cell_len
    name_width = max(DISPLAY_NAME_WIDTH - reserved_width, 0)

    middle = Text(display[:name_width])
    if indicator is not None:
        middle.append(" ")
        middle.append_text(indicator)
    if middle.cell_len < DISPLAY_NAME_WIDTH:
        middle.append(" " * (DISPLAY_NAME_WIDTH - middle.cell_len))

    age_str = format_age(session_data["modified"])
    text = Text()
    text.append(f"{status} ")
    text.append_text(middle)
    text.append(f" {age_str:>4}")
    return text


class SessionItem(ListItem):
    """A session in the list"""

    def __init__(
        self,
        session_data: dict,
        custom_name: str | None = None,
        is_unread: bool = False,
        spinner_frame: int = 0,
    ):
        self.session_data = session_data
        self.custom_name = custom_name
        self.is_unread = is_unread
        self.spinner_frame = spinner_frame
        super().__init__()

    def compose(self) -> ComposeResult:
        label = Static(self._render_label(), classes="session-label")
        self._sync_label_classes(label)
        yield label

    def _render_label(self) -> Text:
        return render_session_label_text(
            self.session_data,
            custom_name=self.custom_name,
            spinner_frame=self.spinner_frame,
        )

    def _sync_label_classes(self, label: Static) -> None:
        if self.is_unread:
            label.add_class("unread")
        else:
            label.remove_class("unread")

        if self.session_data.get("is_working", False):
            label.add_class("working")
        else:
            label.remove_class("working")

        if (
            self.session_data.get("is_active", False)
            and not self.session_data.get("is_working", False)
        ):
            label.add_class("active")
        else:
            label.remove_class("active")

        if self.session_data.get("status") == "archived":
            label.add_class("archived")
        else:
            label.remove_class("archived")

    def refresh_label(self) -> None:
        try:
            label = self.query_one(".session-label", Static)
            label.update(self._render_label())
            self._sync_label_classes(label)
        except Exception:
            pass

    def update_spinner(self, frame: int) -> None:
        """Update spinner animation frame"""
        if not self.session_data.get("is_working"):
            return
        self.spinner_frame = frame
        self.refresh_label()



class SessionStatusHeader(ListItem):
    """Non-selectable divider between status groups in the sidebar (ADR-004).

    Rendered as a dim rule with the status label. `disabled=True` tells
    ListView to skip over it when the cursor moves with j/k — so users see
    the grouping but never land on a header.
    """

    def __init__(self, status: str):
        self.status = status
        super().__init__(disabled=True)

    def compose(self) -> ComposeResult:
        label = STATUS_LABELS.get(self.status, self.status)
        yield Static(f"── {label} ", classes="status-header")


class _SelectableMessage(Static):
    """Base for message widgets that participate in selection mode.

    can_focus is deliberately False — focus stays on the parent
    TranscriptView so its on_key handler receives all keypresses.
    Highlighting is driven purely by the .message-selected CSS class.
    """
    can_focus = False


class UserMessage(_SelectableMessage):
    """A user message in the transcript"""
    pass


class AssistantMessage(_SelectableMessage):
    """An assistant message in the transcript"""
    pass


class ToolMessage(_SelectableMessage):
    """A tool call/result in the transcript"""
    pass


class ObservationInfoHeader(Static):
    """Passive observation metadata shown above the transcript."""

    can_focus = False


class TranscriptView(VerticalScroll):
    """Shows full conversation from selected session"""

    can_focus = True

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.current_path = None
        self._last_mtime = 0
        self._selection_mode = False
        self._selected_index = -1
        self._range_mode = False
        self._range_anchor = -1
        # Queued by the search-result dismiss handler: applied at the end
        # of the next load_transcript() call so the target widget exists
        # by the time we try to scroll to it.
        self._pending_scroll_uuid: str | None = None
        # Search terms to inline-highlight when _scroll_to_uuid runs.
        # Populated alongside _pending_scroll_uuid by the search dismiss
        # handler so the match position is visible inside long messages.
        self._pending_scroll_terms: list[str] | None = None

        # Active highlight state — survives transcript reloads so the
        # 5-second refresh cycle doesn't wipe out search highlights when
        # the session file's mtime changes (e.g. claude is actively
        # writing to it). Set in _scroll_to_uuid, cleared by a sidebar
        # selection (App.on_list_view_highlighted).
        self._active_highlight_uuid: str | None = None
        self._active_highlight_terms: list[str] | None = None

        # Foreign-session lock: non-None while showing a transcript
        # whose source session isn't in the sidebar (cross-project or
        # out-of-view search jump). Tells App.refresh_transcript to
        # leave this transcript alone on auto-refresh ticks — otherwise
        # the refresh would reload whatever the sidebar highlights and
        # yank the user back to the original view.
        self._foreign_session_path: Path | None = None

        # Find-in-transcript state (Ctrl+F-style persistent bar).
        # Non-empty _find_query means find mode is active — all
        # transcript reloads re-apply highlights across every matching
        # message widget, and n/N navigation cycles _find_current
        # through _find_matches (widget-index positions). Cleared by
        # App._close_find which also force-reloads to drop the inline
        # highlight rewrites and restore Markdown formatting.
        self._find_query: str = ""
        self._find_matches: list[int] = []
        self._find_current: int = -1

    def _format_observation_header(self, entry_count: int, obs_path: str) -> str:
        """Return the passive transcript header for observed sessions."""
        noun = "observation" if entry_count == 1 else "observations"
        try:
            display_path = str(Path(obs_path).expanduser())
            home = str(Path.home())
            if display_path == home:
                display_path = "~"
            elif display_path.startswith(home + os.sep):
                display_path = "~" + display_path[len(home):]
        except Exception:
            display_path = obs_path
        return f"─── 🗒 {entry_count} {noun} · {display_path} ───"

    def _get_observation_header_text(self, session_id: str) -> str | None:
        """Look up observation metadata for the current transcript."""
        try:
            state = db.get_observation_state(self.app.conn, session_id)
        except Exception:
            return None

        if not state:
            return None

        entry_count = int(state.get("entry_count") or 0)
        obs_path = str(state.get("obs_path") or "").strip()
        if entry_count <= 0 or not obs_path:
            return None

        return self._format_observation_header(entry_count, obs_path)

    def _get_message_widgets(self):
        """Return all message widgets in order."""
        return [
            c for c in self.children
            if isinstance(c, (UserMessage, AssistantMessage, ToolMessage))
        ]

    def on_key(self, event) -> None:
        """Handle keys for message selection and range selection modes."""
        if event.key == "m":
            if self._selection_mode:
                self._exit_selection_mode()
            else:
                self._enter_selection_mode()
            event.stop()
            event.prevent_default()
        elif self._selection_mode:
            if event.key == "v":
                if self._range_mode:
                    self._exit_range_mode()
                else:
                    self._enter_range_mode()
                event.stop()
                event.prevent_default()
            elif event.key in ("j", "down"):
                self._select_next()
                event.stop()
                event.prevent_default()
            elif event.key in ("k", "up"):
                self._select_previous()
                event.stop()
                event.prevent_default()
            elif event.key == "y":
                self._copy_selection()
                event.stop()
                event.prevent_default()
            elif event.key == "e":
                self._export_selection()
                event.stop()
                event.prevent_default()
            elif event.key == "space":
                self.app.bookmark_toggle_selection()
                event.stop()
                event.prevent_default()
            elif event.key == "L":
                self.app.bookmark_prompt_label()
                event.stop()
                event.prevent_default()
            elif event.key == "B":
                self.app.bookmark_prompt_category()
                event.stop()
                event.prevent_default()
            elif event.key == "escape":
                if self._range_mode:
                    self._exit_range_mode()
                else:
                    self._exit_selection_mode()
                event.stop()
                event.prevent_default()

    def _enter_selection_mode(self):
        messages = self._get_message_widgets()
        if not messages:
            return
        self._selection_mode = True
        # Start at the last message — recent context is usually what you want
        self._selected_index = len(messages) - 1
        self._update_selection(messages)
        self.app.notify(
            "Selection mode: j/k move, v range, y copy, e export, "
            "space bookmark, B category, L note, m/Esc exit"
        )
        # Nudge the contextual footer — selection mode has its own scope.
        try:
            self.app.refresh_footer()
        except Exception:
            pass

    def _exit_selection_mode(self):
        self._range_mode = False
        self._range_anchor = -1
        self._clear_selection()
        self._selection_mode = False
        self._selected_index = -1
        self.border_title = "Transcript"
        try:
            self.app.refresh_footer()
        except Exception:
            pass

    def _enter_range_mode(self):
        self._range_mode = True
        self._range_anchor = self._selected_index
        self._update_selection()
        self.app.notify("Range select: j/k extend, v/Esc cancel")

    def _exit_range_mode(self):
        self._range_mode = False
        self._range_anchor = -1
        self._update_selection()

    def get_selected_messages(self):
        """Return the list of message widgets in the current selection.

        Single selection mode returns a one-element list; range mode
        returns all messages between the anchor and cursor (inclusive).
        Useful for downstream copy (task #12) and export (task #13).
        """
        messages = self._get_message_widgets()
        if not messages or not self._selection_mode:
            return []
        if self._range_mode and self._range_anchor >= 0:
            lo = min(self._range_anchor, self._selected_index)
            hi = max(self._range_anchor, self._selected_index)
            return messages[lo:hi + 1]
        if 0 <= self._selected_index < len(messages):
            return [messages[self._selected_index]]
        return []

    def _select_next(self):
        messages = self._get_message_widgets()
        if not messages:
            return
        self._selected_index = min(self._selected_index + 1, len(messages) - 1)
        self._update_selection(messages)

    def _select_previous(self):
        messages = self._get_message_widgets()
        if not messages:
            return
        self._selected_index = max(self._selected_index - 1, 0)
        self._update_selection(messages)

    def _update_selection(self, messages=None):
        if messages is None:
            messages = self._get_message_widgets()

        if self._range_mode and self._range_anchor >= 0:
            lo = min(self._range_anchor, self._selected_index)
            hi = max(self._range_anchor, self._selected_index)
        else:
            lo = hi = -1  # no range

        for i, widget in enumerate(messages):
            # Cursor position highlight (single selected message)
            if i == self._selected_index:
                widget.add_class("message-selected")
                widget.scroll_visible()
            else:
                widget.remove_class("message-selected")
            # Range highlight (all messages between anchor and cursor)
            if lo <= i <= hi:
                widget.add_class("message-range-selected")
            else:
                widget.remove_class("message-range-selected")

        # Update position counter in border title
        total = len(messages)
        pos = self._selected_index + 1
        if self._range_mode and self._range_anchor >= 0:
            count = hi - lo + 1
            self.border_title = (
                f"Transcript ── RANGE {count} selected "
                f"({pos}/{total}) (j/k extend, v/Esc cancel)"
            )
        else:
            self.border_title = f"Transcript ── SELECT {pos}/{total} (j/k move, v range, y/e copy/export, space bookmark, B category, L note, m/Esc exit)"

    def _clear_selection(self):
        for widget in self._get_message_widgets():
            widget.remove_class("message-selected")
            widget.remove_class("message-range-selected")

    def _copy_selection(self):
        """Copy selected messages to clipboard with source labels (ADR-008)."""
        selected = self.get_selected_messages()
        if not selected:
            return

        # Resolve session metadata from the app
        app = self.app
        session_id = app._selected_session_id
        session_data = next(
            (s for s in app.sessions if s.get("session_id") == session_id),
            None,
        )

        if session_data:
            custom_name = app.config.get("session_names", {}).get(session_id)
            session_name = (
                custom_name
                or session_data.get("title")
                or session_data.get("project", "unknown")
            )
            cwd = session_data.get("project", "")
        else:
            session_name = "unknown"
            cwd = ""

        # Use the first selected message's timestamp for the label
        first_ts = next(
            (getattr(w, "_timestamp", None) for w in selected
             if getattr(w, "_timestamp", None)),
            None,
        )
        ts_str = ""
        if first_ts:
            try:
                dt = datetime.fromisoformat(first_ts.replace("Z", "+00:00"))
                ts_str = dt.strftime("%Y-%m-%d %H:%M")
            except (ValueError, AttributeError):
                pass

        # Build source label — contract parsed by /threadhop:context
        label_parts = [f'[From "{session_name}"']
        if cwd:
            label_parts.append(f" — {cwd}")
        if ts_str:
            label_parts.append(f" — {ts_str}")
        label_parts.append("]")
        label = "".join(label_parts)

        # Build message content
        lines = [label]
        for w in selected:
            raw = getattr(w, "_raw_text", "")
            if isinstance(w, UserMessage):
                lines.append(f"User: {raw}")
            elif isinstance(w, AssistantMessage):
                lines.append(f"Claude: {raw}")
            elif isinstance(w, ToolMessage):
                lines.append(raw)

        text = "\n".join(lines)

        # Copy to clipboard via pbcopy (macOS)
        try:
            subprocess.run(["pbcopy"], input=text.encode(), check=True)
            count = len(selected)
            noun = "message" if count == 1 else "messages"
            app.notify(f"Copied {count} {noun} to clipboard")
        except Exception:
            app.notify("Failed to copy to clipboard", severity="error")

    def _export_selection(self):
        """Export selected messages to a temp markdown file (ADR-008).

        Writes to /tmp/threadhop/<session_id>-<timestamp>.md. ThreadHop
        prunes stale exports on TUI startup, but these remain ephemeral
        reference files rather than permanent exports. Users reference
        them from other sessions via: Read /tmp/threadhop/<file>.md
        """
        selected = self.get_selected_messages()
        if not selected:
            return

        app = self.app
        session_id = app._selected_session_id or "unknown"
        session_data = next(
            (s for s in app.sessions if s.get("session_id") == session_id),
            None,
        )

        if session_data:
            custom_name = app.config.get("session_names", {}).get(session_id)
            session_name = (
                custom_name
                or session_data.get("title")
                or session_data.get("project", "unknown")
            )
            project = session_data.get("project", "")
            cwd = session_data.get("cwd", "")
        else:
            session_name = "unknown"
            project = ""
            cwd = ""

        # Build markdown content
        ts_now = datetime.now().strftime("%Y%m%d-%H%M%S")
        lines = [
            f"# Exported from: {session_name}",
            "",
            "| Field | Value |",
            "| --- | --- |",
            f"| Session ID | `{session_id}` |",
        ]
        if project:
            lines.append(f"| Project | {project} |")
        if cwd:
            lines.append(f"| CWD | `{cwd}` |")
        lines.append(f"| Exported | {datetime.now().strftime('%Y-%m-%d %H:%M:%S')} |")
        lines.append(f"| Messages | {len(selected)} |")
        lines.append("")
        lines.append("---")
        lines.append("")

        for w in selected:
            raw = getattr(w, "_raw_text", "")
            ts = getattr(w, "_timestamp", None)
            ts_str = ""
            if ts:
                try:
                    dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
                    ts_str = dt.strftime("%Y-%m-%d %H:%M:%S")
                except (ValueError, AttributeError):
                    pass

            if isinstance(w, UserMessage):
                role_label = "User"
            elif isinstance(w, AssistantMessage):
                role_label = "Claude"
            else:
                role_label = "Tool"

            if ts_str:
                lines.append(f"### {role_label} — {ts_str}")
            else:
                lines.append(f"### {role_label}")
            lines.append("")
            lines.append(raw)
            lines.append("")

        content = "\n".join(lines)

        export_dir = EXPORT_DIR
        try:
            os.makedirs(export_dir, exist_ok=True)
            export_path = export_dir / f"{session_id}-{ts_now}.md"
            export_path.write_text(content)

            count = len(selected)
            noun = "message" if count == 1 else "messages"

            # Copy the export path to clipboard so the user can paste it
            # directly into another session (e.g. "Read /tmp/threadhop/...")
            try:
                subprocess.run(
                    ["pbcopy"], input=str(export_path).encode(), check=True,
                )
            except Exception:
                pass

            app.notify(
                f"Exported {count} {noun} → {export_path} (path copied)",
                timeout=10,
            )
        except Exception as e:
            app.notify(f"Export failed: {e}", severity="error")

    async def load_transcript(self, session_path: Path, force: bool = False):
        """Load and display full conversation from transcript"""
        if not session_path or not session_path.exists():
            if self._selection_mode:
                self._exit_selection_mode()
            await self._clear_and_show("No transcript available")
            return

        try:
            mtime = session_path.stat().st_mtime
            if (
                not force
                and session_path == self.current_path
                and mtime == self._last_mtime
            ):
                # File unchanged — keep selection mode alive
                return
            self._last_mtime = mtime
        except:
            pass

        # Transcript is actually reloading — exit selection mode now
        if self._selection_mode:
            self._exit_selection_mode()

        self.current_path = session_path

        messages = self._parse_messages(session_path)
        observation_header = self._get_observation_header_text(session_path.stem)

        # Remove all existing message widgets (await ensures completion)
        await self._remove_all_messages()

        if not messages:
            await self.mount(AssistantMessage("No messages yet"))
            return

        # Build widgets with visual boundaries
        widgets = []
        if observation_header:
            widgets.append(
                ObservationInfoHeader(
                    observation_header,
                    classes="observation-info-header",
                )
            )
        tool_batch = []

        def flush_tools():
            if not tool_batch:
                return
            lines = Text()
            first_ts = None
            first_uuid = None
            raw_parts = []
            for j, (tr, tc, ts, tu) in enumerate(tool_batch):
                if first_ts is None:
                    first_ts = ts
                if first_uuid is None:
                    first_uuid = tu
                if tr == "tool":
                    lines.append("⚙ ", style="dim yellow")
                    lines.append(str(tc) if tc else "", style="dim")
                    raw_parts.append(f"⚙ {tc}")
                else:
                    lines.append("  ↳ ", style="dim green")
                    lines.append(str(tc) if tc else "", style="dim green")
                    raw_parts.append(f"  ↳ {tc}")
                if j < len(tool_batch) - 1:
                    lines.append("\n")
            w = ToolMessage(lines)
            w._raw_text = "\n".join(raw_parts)
            w._timestamp = first_ts
            w._uuid = first_uuid
            widgets.append(w)
            tool_batch.clear()

        for role, content, timestamp, uuid in messages:
            if role in ("tool", "tool_result"):
                tool_batch.append((role, content, timestamp, uuid))
                continue

            flush_tools()

            if role == "user":
                user_text = Text()
                user_text.append("You\n", style="bold cyan")
                user_text.append(str(content) if content else "")
                w = UserMessage(user_text)
                w._raw_text = str(content) if content else ""
                w._timestamp = timestamp
                w._uuid = uuid
                widgets.append(w)
            else:
                header = Text()
                header.append("Claude\n", style="bold green")
                renderables = [header]
                if content:
                    try:
                        renderables.append(Markdown(str(content)))
                    except:
                        renderables.append(Text(str(content)))
                w = AssistantMessage(Group(*renderables))
                w._raw_text = str(content) if content else ""
                w._timestamp = timestamp
                w._uuid = uuid
                widgets.append(w)

        flush_tools()

        # Await mount to ensure all widgets are in the DOM before scrolling
        await self.mount_all(widgets)

        # Decide the post-mount scroll behavior, in priority order:
        #
        # 1. Find mode is active → re-apply highlights across every
        #    matching widget and park the cursor on the current match
        #    (or the pending jump-target uuid, if any).
        # 2. Search just handed us a jump-to-source → honor it.
        # 3. A previous jump is still "active" (highlight state survived
        #    into this reload, e.g. the 5-second refresh hit while the
        #    user is still reading a match) → re-apply the highlight
        #    so it isn't lost on every tick.
        # 4. Normal reload → scroll to the bottom (live tail).
        if self._find_query:
            target_uuid = self._pending_scroll_uuid
            self._pending_scroll_uuid = None
            self._pending_scroll_terms = None
            self._apply_find_highlights(self._find_query, anchor_uuid=target_uuid)
            self._report_find_status()
        elif self._pending_scroll_uuid is not None:
            target = self._pending_scroll_uuid
            terms = self._pending_scroll_terms
            self._pending_scroll_uuid = None
            self._pending_scroll_terms = None
            self._scroll_to_uuid(target, terms)
        elif (
            self._active_highlight_uuid is not None
            and self._active_highlight_terms
        ):
            self._scroll_to_uuid(
                self._active_highlight_uuid,
                self._active_highlight_terms,
            )
        else:
            self.scroll_end(animate=False)

    def queue_scroll_to_uuid(
        self,
        uuid: str,
        search_terms: list[str] | None = None,
    ) -> None:
        """Remember a uuid to scroll into view on the next load_transcript().

        When ``search_terms`` is supplied, every occurrence of each term
        in the target widget gets highlighted so the match is visible
        inside long messages.
        """
        self._pending_scroll_uuid = uuid
        self._pending_scroll_terms = search_terms

    def _scroll_to_uuid(
        self,
        uuid: str,
        search_terms: list[str] | None = None,
    ) -> bool:
        """Scroll to the widget with matching ``_uuid``.

        If ``search_terms`` are supplied, the widget's content is
        rebuilt with each term highlighted in black-on-yellow so the
        exact match position is visible even inside long messages. We
        also try to scroll past the widget's top proportionally to the
        first match's line offset in the raw text, so a match deep in
        a long message isn't hidden below the fold. Approximate — line
        wrapping means it can overshoot or undershoot by a few rows.
        """
        for widget in self._get_message_widgets():
            if getattr(widget, "_uuid", None) == uuid:
                match_line = 0
                if search_terms:
                    # Rebuild with highlights and get back the exact
                    # rendered-line offset of the first match (computed
                    # by Rich at the widget's current width).
                    match_line = self._rebuild_widget_with_highlights(
                        widget, search_terms
                    )
                widget.scroll_visible(animate=False, top=True)
                widget.add_class("message-selected")

                if match_line > 2:
                    # A couple lines of breathing room so the message
                    # header / preceding context stays visible above
                    # the match.
                    self.scroll_to(
                        y=self.scroll_y + match_line - 2,
                        animate=False,
                    )

                def clear_flash(w=widget):
                    try:
                        w.remove_class("message-selected")
                    except Exception:
                        pass

                self.set_timer(1.5, clear_flash)

                # Remember so we can re-apply after a refresh-driven
                # reload (file mtime changed) without losing the match
                # anchor. Cleared by sidebar navigation.
                self._active_highlight_uuid = uuid
                self._active_highlight_terms = (
                    list(search_terms) if search_terms else None
                )
                return True
        return False

    def _rebuild_widget_with_highlights(
        self,
        widget,
        search_terms: list[str],
    ) -> int:
        """Re-render a message widget with search terms highlighted inline.

        Returns the 0-based rendered-line index of the first search
        match in the rebuilt widget, computed by Rich at the widget's
        current width. Returns 0 if the widget is empty or the line
        count couldn't be computed.

        Trade-off: AssistantMessage originally renders its body through
        Rich's Markdown. To inject highlights reliably we replace the
        Markdown block with a plain Text body for the single widget —
        that loses fenced-code / heading formatting on the jumped-to
        message, but gains a precise visual anchor at the match. The
        formatting comes back on the next transcript reload.
        """
        raw = getattr(widget, "_raw_text", "") or ""
        if not raw:
            return 0

        body = Text(raw)
        for term in search_terms:
            if not term:
                continue
            try:
                body.highlight_regex(
                    rf"(?i){re.escape(term)}",
                    style="bold black on yellow",
                )
            except Exception:
                continue

        if isinstance(widget, UserMessage):
            new_renderable = Text.assemble(
                Text("You\n", style="bold cyan"),
                body,
            )
        elif isinstance(widget, AssistantMessage):
            new_renderable = Group(
                Text("Claude\n", style="bold green"),
                body,
            )
        else:
            # ToolMessage: body is already a simple Text block.
            new_renderable = body

        try:
            widget.update(new_renderable)
        except Exception:
            pass

        # Compute the rendered-line offset of the first match using
        # Rich's own renderer at the widget's current width. This is
        # the same layout engine Textual uses internally, so the line
        # count reflects the actual on-screen layout (wrap, padding,
        # indentation). Widget padding added by CSS isn't counted here
        # — that introduces a ±1 row error we absorb with the -2 line
        # breathing-room offset in the caller.
        width = widget.size.width
        if width <= 0:
            width = self.size.width or 80
        try:
            from rich.console import Console

            console = Console(
                width=width,
                color_system=None,
                legacy_windows=False,
                force_terminal=False,
                record=False,
                tab_size=4,
            )
            options = console.options.update_width(width)
            lines = console.render_lines(new_renderable, options, pad=False)
            lowest = -1
            for term in search_terms:
                if not term:
                    continue
                needle = term.lower()
                for i, line in enumerate(lines):
                    line_text = "".join(s.text for s in line).lower()
                    if needle in line_text:
                        if lowest < 0 or i < lowest:
                            lowest = i
                        break
            return max(lowest, 0)
        except Exception:
            return 0

    def activate_find(
        self,
        query: str,
        anchor_uuid: str | None = None,
    ) -> tuple[int, int]:
        """Turn on find mode for ``query`` and return ``(current, total)``.

        Walks every message widget, rebuilding any that contain the term
        so matches are visible inline, and records the list of matching
        widget indices in ``_find_matches``. If ``anchor_uuid`` is given
        and belongs to a matching widget, cursor starts on that match —
        otherwise on the first match. ``current`` is 1-based (0 if no
        matches); ``total`` is the number of matching widgets.
        """
        self._find_query = query or ""
        if not self._find_query.strip():
            self._find_matches = []
            self._find_current = -1
            return 0, 0
        self._apply_find_highlights(self._find_query, anchor_uuid=anchor_uuid)
        total = len(self._find_matches)
        current = (self._find_current + 1) if self._find_current >= 0 else 0
        return current, total

    def next_match(self) -> tuple[int, int]:
        """Advance to the next match (wraps). ``(0, 0)`` when none exist."""
        if not self._find_matches:
            return 0, 0
        self._find_current = (self._find_current + 1) % len(self._find_matches)
        self._scroll_to_match_index(self._find_current)
        return self._find_current + 1, len(self._find_matches)

    def prev_match(self) -> tuple[int, int]:
        """Step back to the previous match (wraps). ``(0, 0)`` when none."""
        if not self._find_matches:
            return 0, 0
        self._find_current = (self._find_current - 1) % len(self._find_matches)
        self._scroll_to_match_index(self._find_current)
        return self._find_current + 1, len(self._find_matches)

    def clear_find_state(self) -> None:
        """Drop find mode state. Caller must force-reload the transcript
        to restore the pre-highlight rendering (Markdown, etc.)."""
        self._find_query = ""
        self._find_matches = []
        self._find_current = -1

    def _apply_find_highlights(
        self,
        query: str,
        anchor_uuid: str | None = None,
    ) -> None:
        """Highlight every occurrence of ``query`` across all messages
        and rebuild ``_find_matches`` / ``_find_current``.

        Case-insensitive substring match against each widget's
        ``_raw_text``. When ``anchor_uuid`` lands on a matching widget
        the cursor snaps there; otherwise we clamp the previous cursor
        into range (so widget-add on reload doesn't yank focus) or fall
        back to the first match.
        """
        if not query:
            self._find_matches = []
            self._find_current = -1
            return

        try:
            pattern = re.compile(re.escape(query), re.IGNORECASE)
        except re.error:
            self._find_matches = []
            self._find_current = -1
            return

        widgets = self._get_message_widgets()
        matches: list[int] = []
        for idx, widget in enumerate(widgets):
            raw = getattr(widget, "_raw_text", "") or ""
            if not raw:
                continue
            if not pattern.search(raw):
                continue
            matches.append(idx)
            self._rebuild_widget_with_highlights(widget, [query])

        self._find_matches = matches

        if not matches:
            self._find_current = -1
            return

        anchor_pos = -1
        if anchor_uuid:
            for pos, idx in enumerate(matches):
                if idx < len(widgets) and getattr(widgets[idx], "_uuid", None) == anchor_uuid:
                    anchor_pos = pos
                    break

        if anchor_pos >= 0:
            self._find_current = anchor_pos
        elif self._find_current >= 0:
            self._find_current = min(self._find_current, len(matches) - 1)
        else:
            self._find_current = 0

        self._scroll_to_match_index(self._find_current)

    def _scroll_to_match_index(self, match_pos: int) -> None:
        """Scroll so the widget at ``_find_matches[match_pos]`` is on screen."""
        if not self._find_matches:
            return
        if match_pos < 0 or match_pos >= len(self._find_matches):
            return
        widgets = self._get_message_widgets()
        widget_idx = self._find_matches[match_pos]
        if widget_idx >= len(widgets):
            return
        widget = widgets[widget_idx]
        try:
            widget.scroll_visible(animate=False, top=True)
        except Exception:
            pass

    def _report_find_status(self) -> None:
        """Push the current match counter to the find bar."""
        try:
            find_bar = self.app.query_one("#find-bar", FindBar)
        except Exception:
            return
        total = len(self._find_matches)
        current = self._find_current + 1 if total else 0
        find_bar.update_status(current, total, bool(self._find_query.strip()))

    async def _remove_all_messages(self):
        """Remove all message widgets, awaiting completion"""
        existing = list(self.children)
        if existing:
            await self.remove_children()

    async def _clear_and_show(self, text: str):
        await self._remove_all_messages()
        await self.mount(AssistantMessage(text))

    def _parse_messages(
        self, session_path: Path
    ) -> list[tuple[str, str, str | None, str | None]]:
        """Parse JSONL file and extract displayable messages.

        Returns list of (role, content, timestamp, uuid) tuples.

        Assistant rows: consecutive lines sharing the same
        ``message.id`` (streaming chunks of one logical reply) are
        merged into a single tuple tagged with the first chunk's uuid —
        matching the indexer's rule (ADR-003) so the bookmarks FK into
        ``messages.uuid`` can never see an id the indexer never stored.

        Tool and tool_result rows: inherit the enclosing turn's
        canonical uuid (the same first-chunk uuid). The indexer folds
        tool-use abbreviations into the turn's single row and never
        indexes tool_result user lines at all, so only the turn's uuid
        is guaranteed present in ``messages``. Pinning tool widgets to
        that id means bookmarking any of them toggles the turn's
        bookmark — the same semantics the deferred turn-as-unit
        rendering will make visually explicit (see UI-IMPROVEMENTS.md).

        Genuine user rows: keep the line's own uuid; a real human
        message ends the preceding assistant turn and flushes the
        pending group. ``toolUseResult`` user lines do NOT flush —
        they belong to the turn in progress.
        """
        messages = []
        # Pending assistant group accumulates text across streaming
        # chunks sharing a message.id, and anchors the turn's canonical
        # uuid / timestamp for any tool or tool_result widgets emitted
        # while the group is open. Flushed on a genuine user message,
        # a differing message.id, or EOF.
        pending_mid: str | None = None
        pending_uuid: str | None = None
        pending_ts: str | None = None
        pending_text_parts: list[str] = []

        def flush_assistant() -> None:
            nonlocal pending_mid, pending_uuid, pending_ts, pending_text_parts
            if pending_text_parts and pending_uuid is not None:
                messages.append((
                    "assistant",
                    " ".join(pending_text_parts),
                    pending_ts,
                    pending_uuid,
                ))
            pending_mid = None
            pending_uuid = None
            pending_ts = None
            pending_text_parts = []

        try:
            with open(session_path) as f:
                for line in f:
                    try:
                        msg = json.loads(line)
                        msg_type = msg.get("type")
                        timestamp = msg.get("timestamp")
                        uuid = msg.get("uuid")

                        # Only process user and assistant messages
                        if msg_type not in ("user", "assistant"):
                            continue

                        if msg_type == "user":
                            if "toolUseResult" in msg:
                                # Mid-turn event — attach to the
                                # pending assistant turn, don't flush.
                                tool_result = msg.get("toolUseResult")
                                if isinstance(tool_result, dict):
                                    result_info = self._format_tool_result(tool_result)
                                    if result_info:
                                        anchor_uuid = pending_uuid or uuid
                                        anchor_ts = pending_ts or timestamp
                                        messages.append((
                                            "tool_result",
                                            result_info,
                                            anchor_ts,
                                            anchor_uuid,
                                        ))
                            else:
                                # Genuine human message — ends the turn.
                                flush_assistant()
                                content = msg.get("message", {}).get("content", "")
                                if isinstance(content, list):
                                    text_parts = [
                                        b.get("text", "")
                                        for b in content
                                        if b.get("type") == "text"
                                    ]
                                    content = " ".join(text_parts)
                                if content and isinstance(content, str):
                                    # Strip system-reminder tags
                                    content = SYSTEM_REMINDER_RE.sub("", content).strip()
                                    if content:
                                        messages.append(("user", content, timestamp, uuid))

                        elif msg_type == "assistant":
                            mid = msg.get("message", {}).get("id")
                            # New logical message → flush the previous
                            # group and anchor on this line's uuid.
                            if mid is None or mid != pending_mid:
                                flush_assistant()
                                pending_mid = mid
                                pending_uuid = uuid
                                pending_ts = timestamp

                            content = msg.get("message", {}).get("content", [])
                            if isinstance(content, list):
                                for block in content:
                                    if block.get("type") == "text":
                                        text = block.get("text", "")
                                        # Strip system-reminder tags
                                        text = SYSTEM_REMINDER_RE.sub("", text).strip()
                                        if text:
                                            pending_text_parts.append(text)
                                    elif block.get("type") == "tool_use":
                                        tool_name = block.get("name", "Unknown")
                                        tool_input = block.get("input", {})
                                        tool_desc = self._format_tool_use(
                                            tool_name, tool_input
                                        )
                                        # Pin tool widget to the turn's
                                        # canonical uuid so it resolves
                                        # to the same messages row the
                                        # indexer stores for this turn.
                                        messages.append((
                                            "tool",
                                            tool_desc,
                                            pending_ts or timestamp,
                                            pending_uuid or uuid,
                                        ))
                    except (json.JSONDecodeError, KeyError):
                        pass
            flush_assistant()
        except Exception:
            pass
        return messages

    def _format_tool_use(self, tool_name: str, tool_input: dict) -> str:
        """Format tool use for display"""
        if tool_name == "Read":
            path = tool_input.get("file_path", "")
            return f"Reading {Path(path).name}" if path else "Reading file"
        elif tool_name == "Write":
            path = tool_input.get("file_path", "")
            return f"Writing {Path(path).name}" if path else "Writing file"
        elif tool_name == "Edit":
            path = tool_input.get("file_path", "")
            return f"Editing {Path(path).name}" if path else "Editing file"
        elif tool_name == "Bash":
            cmd = tool_input.get("command", "")
            short_cmd = cmd.split()[0] if cmd else "command"
            return f"Running {short_cmd}"
        elif tool_name == "Glob":
            pattern = tool_input.get("pattern", "")
            return f"Searching for {pattern}" if pattern else "Searching files"
        elif tool_name == "Grep":
            pattern = tool_input.get("pattern", "")
            return f"Searching for '{pattern}'" if pattern else "Searching content"
        elif tool_name == "Agent":
            desc = tool_input.get("description", "")
            return f"Agent: {desc}" if desc else "Running agent"
        elif tool_name == "WebFetch":
            url = tool_input.get("url", "")
            return f"Fetching {url[:50]}..." if len(url) > 50 else f"Fetching {url}"
        elif tool_name == "WebSearch":
            query = tool_input.get("query", "")
            return f"Searching web for '{query}'"
        elif tool_name == "TodoWrite":
            return "Updating todo list"
        else:
            return f"{tool_name}"

    def _format_tool_result(self, result: dict) -> str | None:
        """Format tool result for display"""
        if isinstance(result, str):
            if "Error" in result:
                return f"Error: {result[:80]}..."
            return None

        if "numFiles" in result:
            num = result.get("numFiles", 0)
            return f"Found {num} file{'s' if num != 1 else ''}"

        if "additions" in result or "deletions" in result:
            add = result.get("additions", 0)
            delete = result.get("deletions", 0)
            return f"+{add}/-{delete} lines"

        if "numLines" in result:
            return f"{result['numLines']} lines"

        if "numMatches" in result:
            return f"{result['numMatches']} matches"

        if "status" in result:
            status = result.get("status", "")
            if status == "completed":
                return "Completed"
            return status

        return None


# --- Real-time search panel (task #14, ADR-002 / ADR-007) -----------------
#
# FTS5 prefix matching against the `messages` table populated by the
# indexer. Each keystroke in the search input debounces (~80 ms) and
# re-runs the query. Results render as a ListView of SearchResultItem
# rows; dismissing the modal with a selection jumps the main TUI to the
# source message.
#
# Filter syntax parsed from the raw query string:
#   project:<name>    — substring match against sessions.project
#   session:current   — restrict to the active transcript session
#   since:/until:     — YYYY-MM-DD or full ISO timestamp range filters
#   user:             — only role = 'user'
#   assistant:        — only role = 'assistant'
# Remaining whitespace-separated tokens become FTS5 prefix terms.

# Sentinel characters bracketing matched spans in the SQL `snippet()`
# output. Using control chars avoids collisions with user text. Parsed
# back out by `_render_snippet` into Rich Text with a highlight style.
_FTS_MATCH_START = "\x01"
_FTS_MATCH_END = "\x02"


@dataclass(frozen=True)
class SearchQuerySpec:
    """Parsed search input plus derived filter metadata."""

    raw: str
    fts_expr: str
    terms: tuple[str, ...]
    plain_query: str
    role: str | None = None
    project: str | None = None
    session_id: str | None = None
    since_ts: str | None = None
    until_ts: str | None = None
    until_is_exclusive: bool = False


@dataclass(frozen=True)
class SearchResultPage:
    """One page of results plus metadata for the search modal."""

    rows: list[dict]
    total_count: int
    limit: int
    offset: int
    elapsed_ms: float
    used_fuzzy_fallback: bool = False

    @property
    def loaded_count(self) -> int:
        return self.offset + len(self.rows)

    @property
    def has_more(self) -> bool:
        return self.loaded_count < self.total_count


def _parse_search_date(value: str, *, upper_bound: bool) -> tuple[str | None, bool]:
    """Parse a date/date-time filter into an ISO UTC bound.

    Returns ``(iso_utc, is_exclusive)``. Date-only ``until:YYYY-MM-DD``
    becomes the next day's midnight and is treated as exclusive so the
    whole day is included.
    """
    raw = (value or "").strip()
    if not raw:
        return None, False

    try:
        if len(raw) == 10:
            dt = datetime.fromisoformat(raw).replace(tzinfo=timezone.utc)
            if upper_bound:
                dt += timedelta(days=1)
                return dt.strftime("%Y-%m-%dT%H:%M:%SZ"), True
            return dt.strftime("%Y-%m-%dT%H:%M:%SZ"), False

        dt = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError:
        return None, False

    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    else:
        dt = dt.astimezone(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ"), False


def _build_fts_query(
    raw: str,
    *,
    current_session_id: str | None = None,
) -> SearchQuerySpec:
    """Parse raw input into a structured search query spec."""
    tokens = raw.strip().split()
    role: str | None = None
    project: str | None = None
    session_id: str | None = None
    since_ts: str | None = None
    until_ts: str | None = None
    until_is_exclusive = False
    terms: list[str] = []

    for tok in tokens:
        low = tok.lower()
        if low == "user:":
            role = "user"
        elif low == "assistant:":
            role = "assistant"
        elif low.startswith("project:"):
            val = tok[len("project:"):].strip()
            if val:
                project = val
        elif low.startswith("session:"):
            val = tok[len("session:"):].strip()
            if val:
                if val.lower() == "current":
                    session_id = current_session_id
                else:
                    session_id = val
        elif low.startswith("since:"):
            parsed, _ = _parse_search_date(tok[len("since:"):], upper_bound=False)
            if parsed:
                since_ts = parsed
        elif low.startswith("until:"):
            parsed, exclusive = _parse_search_date(
                tok[len("until:"):],
                upper_bound=True,
            )
            if parsed:
                until_ts = parsed
                until_is_exclusive = exclusive
        else:
            # Keep only word chars (letters/digits/underscore). FTS5's
            # unicode61 tokenizer is fine with these, and stripping
            # punctuation prevents syntax errors at MATCH time.
            clean = re.sub(r"[^\w]", "", tok)
            if clean:
                terms.append(clean)

    return SearchQuerySpec(
        raw=raw.strip(),
        fts_expr=" ".join(f"{term}*" for term in terms),
        terms=tuple(terms),
        plain_query=" ".join(terms).lower(),
        role=role,
        project=project,
        session_id=session_id,
        since_ts=since_ts,
        until_ts=until_ts,
        until_is_exclusive=until_is_exclusive,
    )


def _build_search_filters(spec: SearchQuerySpec) -> tuple[list[str], dict]:
    """Return shared SQL filter clauses for FTS and filter-only search."""
    clauses: list[str] = []
    params: dict[str, str] = {}

    if spec.role:
        clauses.append("m.role = :role")
        params["role"] = spec.role
    if spec.project:
        clauses.append("s.project LIKE :project")
        params["project"] = f"%{spec.project}%"
    if spec.session_id:
        clauses.append("m.session_id = :session_id")
        params["session_id"] = spec.session_id
    if spec.since_ts:
        clauses.append("m.timestamp >= :since_ts")
        params["since_ts"] = spec.since_ts
    if spec.until_ts:
        op = "<" if spec.until_is_exclusive else "<="
        clauses.append(f"m.timestamp {op} :until_ts")
        params["until_ts"] = spec.until_ts

    return clauses, params


def _search_trigram_fallback_page(
    conn: sqlite3.Connection,
    spec: SearchQuerySpec,
    *,
    limit: int,
    offset: int,
    where_sql: str,
    base_params: dict,
    timer_start: float,
) -> SearchResultPage:
    """Return a paged fuzzy-fallback result set for a zero-hit query."""
    trigram_expr = search_queries._build_trigram_match_expr(list(spec.terms))
    if not trigram_expr:
        return SearchResultPage(
            rows=[],
            total_count=0,
            limit=limit,
            offset=offset,
            elapsed_ms=(perf_counter() - timer_start) * 1000,
            used_fuzzy_fallback=False,
        )

    candidate_sql = (
        "SELECT m.uuid AS uuid, m.session_id AS session_id, "
        "m.role AS role, m.timestamp AS timestamp, "
        "m.text AS text, "
        "s.custom_name AS custom_name, s.project AS project, "
        "s.session_path AS session_path, "
        "bm25(messages_fts_trigram) AS trigram_rank "
        "FROM messages_fts_trigram "
        "JOIN messages m ON m.rowid = messages_fts_trigram.rowid "
        "LEFT JOIN sessions s ON s.session_id = m.session_id "
        "WHERE messages_fts_trigram MATCH :q"
        f"{where_sql} "
        "ORDER BY trigram_rank "
        "LIMIT :candidate_lim"
    )
    params = dict(base_params)
    params["q"] = trigram_expr
    params["candidate_lim"] = max(limit * 4, 200)

    try:
        candidates = db.query_all(conn, candidate_sql, params)
    except sqlite3.OperationalError:
        return SearchResultPage(
            rows=[],
            total_count=0,
            limit=limit,
            offset=offset,
            elapsed_ms=(perf_counter() - timer_start) * 1000,
            used_fuzzy_fallback=False,
        )

    scored: list[tuple[float, float, dict, str | None]] = []
    for row in candidates:
        result = search_queries._score_candidate_row(row, list(spec.terms))
        if result is None:
            continue
        score, highlight_term = result
        trigram_rank = float(row.get("trigram_rank") or 0.0)
        scored.append((score, trigram_rank, row, highlight_term))

    scored.sort(key=lambda item: (-item[0], item[1], item[2].get("timestamp") or ""))

    all_rows: list[dict] = []
    for score, _, row, highlight_term in scored:
        all_rows.append(
            {
                "uuid": row.get("uuid"),
                "session_id": row.get("session_id"),
                "role": row.get("role"),
                "timestamp": row.get("timestamp"),
                "snippet": search_queries._build_fallback_snippet(
                    str(row.get("text") or ""),
                    highlight_term,
                ),
                "custom_name": row.get("custom_name"),
                "project": row.get("project"),
                "session_path": row.get("session_path"),
                "score": score,
            }
        )

    page_rows = all_rows[offset:offset + limit]
    return SearchResultPage(
        rows=page_rows,
        total_count=len(all_rows),
        limit=limit,
        offset=offset,
        elapsed_ms=(perf_counter() - timer_start) * 1000,
        used_fuzzy_fallback=bool(all_rows),
    )


def search_messages(
    conn: sqlite3.Connection,
    raw_query: str,
    limit: int = SEARCH_PAGE_SIZE,
    offset: int = 0,
    *,
    current_session_id: str | None = None,
) -> SearchResultPage:
    """Run the FTS5 search for the search panel.

    Returns paged rows with keys: uuid, session_id, role, timestamp,
    snippet, custom_name, project, session_path. ``snippet`` contains
    _FTS_MATCH_START / _FTS_MATCH_END sentinels around each match span.
    """
    spec = _build_fts_query(raw_query, current_session_id=current_session_id)
    where_clauses, base_params = _build_search_filters(spec)
    where_sql = ""
    if where_clauses:
        where_sql = " AND " + " AND ".join(where_clauses)

    timer_start = perf_counter()

    # No search terms: allow a filter-only query so typing just
    # `project:foo` or `session:current` lists recent matching messages.
    if not spec.fts_expr:
        if not where_clauses:
            return SearchResultPage(
                rows=[],
                total_count=0,
                limit=limit,
                offset=offset,
                elapsed_ms=0.0,
            )

        count_sql = (
            "SELECT COUNT(*) AS total_count "
            "FROM messages m "
            "LEFT JOIN sessions s ON s.session_id = m.session_id "
            f"WHERE 1=1{where_sql}"
        )
        page_sql = (
            "SELECT m.uuid AS uuid, m.session_id AS session_id, "
            "m.role AS role, m.timestamp AS timestamp, "
            "substr(m.text, 1, 160) AS snippet, "
            "s.custom_name AS custom_name, s.project AS project, "
            "s.session_path AS session_path "
            "FROM messages m "
            "LEFT JOIN sessions s ON s.session_id = m.session_id "
            f"WHERE 1=1{where_sql} "
            "ORDER BY m.timestamp DESC "
            "LIMIT :lim OFFSET :off"
        )
        params = dict(base_params)
        params["lim"] = limit
        params["off"] = offset
        try:
            total_row = db.query_one(conn, count_sql, base_params) or {}
            rows = db.query_all(conn, page_sql, params)
        except sqlite3.OperationalError:
            return SearchResultPage(
                rows=[],
                total_count=0,
                limit=limit,
                offset=offset,
                elapsed_ms=0.0,
            )

        return SearchResultPage(
            rows=rows,
            total_count=int(total_row.get("total_count") or 0),
            limit=limit,
            offset=offset,
            elapsed_ms=(perf_counter() - timer_start) * 1000,
        )

    recent_cutoff = (
        datetime.now(timezone.utc) - timedelta(days=30)
    ).strftime("%Y-%m-%dT%H:%M:%SZ")
    normalized_text_sql = " ".join(
        """
        lower(
            replace(
                replace(
                    replace(
                        replace(
                            replace(
                                replace(
                                    replace(' ' || m.text || ' ', char(10), ' '),
                                    '.', ' '
                                ),
                                ',', ' '
                            ),
                            ';', ' '
                        ),
                        ':', ' '
                    ),
                    '!', ' '
                ),
                '?', ' '
            )
        )
        """.split()
    )
    base_from_sql = (
        "FROM messages_fts "
        "JOIN messages m ON m.rowid = messages_fts.rowid "
        "LEFT JOIN sessions s ON s.session_id = m.session_id "
        "WHERE messages_fts MATCH :q"
        f"{where_sql}"
    )
    count_sql = "SELECT COUNT(*) AS total_count " + base_from_sql
    page_sql = (
        "SELECT m.uuid AS uuid, m.session_id AS session_id, "
        "m.role AS role, m.timestamp AS timestamp, "
        "snippet(messages_fts, 0, :mstart, :mend, '…', 16) AS snippet, "
        "s.custom_name AS custom_name, s.project AS project, "
        "s.session_path AS session_path "
        + base_from_sql
        + " ORDER BY "
        "CASE "
        "WHEN :plain_query = '' THEN 0 "
        f"WHEN instr({normalized_text_sql}, ' ' || :plain_query || ' ') > 0 THEN 0 "
        "ELSE 1 "
        "END ASC, "
        "CASE WHEN m.timestamp >= :recent_cutoff THEN 0 ELSE 1 END ASC, "
        "bm25(messages_fts) ASC, "
        "m.timestamp DESC "
        "LIMIT :lim OFFSET :off"
    )
    params = dict(base_params)
    params.update({
        "q": spec.fts_expr,
        "mstart": _FTS_MATCH_START,
        "mend": _FTS_MATCH_END,
        "plain_query": spec.plain_query,
        "recent_cutoff": recent_cutoff,
    })
    params["lim"] = limit
    params["off"] = offset

    try:
        count_params = {
            k: v
            for k, v in params.items()
            if k not in {"lim", "off", "mstart", "mend", "plain_query", "recent_cutoff"}
        }
        total_row = db.query_one(conn, count_sql, count_params) or {}
        total_count = int(total_row.get("total_count") or 0)
        rows = db.query_all(conn, page_sql, params)
    except sqlite3.OperationalError:
        # Malformed expressions still slip through on edge cases
        # (e.g. a bare `*`). Surface as "no results" rather than crash.
        return SearchResultPage(
            rows=[],
            total_count=0,
            limit=limit,
            offset=offset,
            elapsed_ms=0.0,
        )

    if total_count == 0 and spec.terms:
        return _search_trigram_fallback_page(
            conn,
            spec,
            limit=limit,
            offset=offset,
            where_sql=where_sql,
            base_params=base_params,
            timer_start=timer_start,
        )

    return SearchResultPage(
        rows=rows,
        total_count=total_count,
        limit=limit,
        offset=offset,
        elapsed_ms=(perf_counter() - timer_start) * 1000,
    )


def _render_snippet(snippet: str) -> Text:
    """Convert an FTS5 snippet with sentinel-wrapped matches to Rich Text."""
    out = Text()
    remaining = snippet or ""
    while True:
        i = remaining.find(search_queries.FTS_MATCH_START)
        if i < 0:
            out.append(remaining)
            break
        out.append(remaining[:i])
        remaining = remaining[i + len(search_queries.FTS_MATCH_START):]
        j = remaining.find(search_queries.FTS_MATCH_END)
        if j < 0:
            # Unterminated — highlight the rest and stop.
            out.append(remaining, style="bold black on yellow")
            break
        out.append(remaining[:j], style="bold black on yellow")
        remaining = remaining[j + len(search_queries.FTS_MATCH_END):]
    return out


class SearchResultItem(ListItem):
    """One row in the search-results list.

    Holds the raw result dict so the dismiss handler can pull the
    session_id + uuid for jump-to-source without another DB lookup.
    """

    def __init__(self, row: dict):
        self.result = row
        super().__init__()

    def compose(self) -> ComposeResult:
        role = self.result.get("role", "")
        role_icon = "▶" if role == "user" else "●"
        role_style = "bold cyan" if role == "user" else "bold green"

        # Snippet line: colored role marker + highlighted text span.
        snippet_line = Text()
        snippet_line.append(f"{role_icon} ", style=role_style)
        snippet_line.append_text(
            _render_snippet(self.result.get("snippet") or "")
        )

        # Metadata line: session name · project · formatted timestamp.
        session_name = (
            self.result.get("custom_name")
            or self.result.get("project")
            or (self.result.get("session_id", "") or "")[:8]
        )
        project = self.result.get("project") or ""
        ts_raw = self.result.get("timestamp") or ""
        ts_str = ts_raw[:16]
        try:
            dt = datetime.fromisoformat(ts_raw.replace("Z", "+00:00"))
            ts_str = dt.strftime("%Y-%m-%d %H:%M")
        except (ValueError, AttributeError):
            pass

        meta = Text()
        meta.append("  ", style="dim")
        meta.append(str(session_name), style="bold")
        if project and project != session_name:
            meta.append("  ·  ", style="dim")
            meta.append(str(project), style="cyan")
        meta.append("  ·  ", style="dim")
        meta.append(ts_str, style="dim")

        yield Static(Group(snippet_line, meta), classes="search-result")


class RecentSearchItem(ListItem):
    """One row in the recent-search list shown for an empty query."""

    def __init__(self, query: str):
        self.query = query
        super().__init__()

    def compose(self) -> ComposeResult:
        label = Text()
        label.append("Recent  ", style="dim")
        label.append(self.query, style="bold")
        yield Static(label, classes="search-result")


class SearchScreen(ModalScreen):
    """Modal full-text search over indexed messages (task #14).

    Focus stays on the Input; arrow keys (and Ctrl+N/Ctrl+P) navigate
    the results list without losing typing focus — matches common
    fuzzy-finder UX (fzf, Helm). Enter returns ``(session_id, uuid)``
    to the dismiss callback; Escape dismisses with ``None``.
    """

    # Debounce delay between keystroke and the FTS query. 120 ms leaves
    # enough slack for the extra COUNT() query and lazy-paging status
    # updates without making fast typing feel sticky.
    DEBOUNCE_SECONDS = 0.12

    # The app binds `enter` with priority=True to start_reply_or_send, and
    # app priority bindings fire even while a modal is up. Re-bind enter
    # here with priority so the modal wins and Input.Submitted actually
    # reaches our handler.
    BINDINGS = [
        Binding("enter", "open_result", "Open", priority=True, show=False),
    ]

    CSS = """
    /* Explicitly reset layout here — the app's top-level
       `Screen { layout: grid; grid-columns: 36 1fr; ... }` rule cascades
       to ModalScreen subclasses too, which would otherwise squeeze the
       modal into the 36-col first grid cell. */
    SearchScreen {
        layout: vertical;
        align: center top;
        background: $background 70%;
    }

    #search-container {
        width: 90%;
        max-width: 140;
        height: 85%;
        margin-top: 1;
        background: $surface;
        border: thick $accent;
        layout: vertical;
    }

    #search-input {
        margin: 0;
        border: none;
        background: $boost;
    }

    #search-input:focus {
        background: $boost;
    }

    #search-results {
        height: 1fr;
        background: $surface;
    }

    #search-status {
        height: 1;
        padding: 0 1;
        color: $text-muted;
        background: $boost;
    }

    #search-help {
        height: 1;
        padding: 0 1;
        color: $text-muted;
        background: $boost;
        text-style: italic;
    }

    .search-result {
        padding: 0 1;
    }

    SearchResultItem {
        padding: 0;
    }
    """

    def __init__(
        self,
        conn: sqlite3.Connection,
        *,
        config: dict | None = None,
        current_session_id: str | None = None,
    ):
        super().__init__()
        self.conn = conn
        self.config = config
        self.current_session_id = current_session_id
        # Single outstanding debounce timer. Each new keystroke stops
        # the previous timer before scheduling the next one, so only
        # the final keystroke in a typing burst triggers a query.
        self._search_timer = None
        self._active_query = ""
        self._loaded_count = 0
        self._total_count = 0
        self._last_elapsed_ms = 0.0
        self._loading_more = False

    def compose(self) -> ComposeResult:
        with Vertical(id="search-container") as container:
            container.border_title = "Search"
            yield Input(
                placeholder=(
                    "Search — prefix first, fuzzy fallback; "
                    "filters: project: session:current since: until:"
                ),
                id="search-input",
            )
            yield Static("Type to search…", id="search-status")
            yield ListView(id="search-results")
            yield Static(
                "↑/↓ or PgUp/PgDn navigate • Enter jump • Ctrl+X clear history • Esc close",
                id="search-help",
            )

    def on_mount(self) -> None:
        self.query_one("#search-input", Input).focus()
        self._show_empty_state()

    def on_input_changed(self, event) -> None:
        """Debounced re-run on every keystroke."""
        try:
            if event.input.id != "search-input":
                return
        except AttributeError:
            return
        if self._search_timer is not None:
            try:
                self._search_timer.stop()
            except Exception:
                pass
        value = event.value
        self._search_timer = self.set_timer(
            self.DEBOUNCE_SECONDS, lambda v=value: self._execute_search(v)
        )

    def _execute_search(self, raw_query: str) -> None:
        self._run_search(raw_query, reset=True)

    def _show_empty_state(self) -> None:
        results_list = self.query_one("#search-results", ListView)
        status = self.query_one("#search-status", Static)
        self._active_query = ""
        self._loaded_count = 0
        self._total_count = 0
        self._last_elapsed_ms = 0.0

        results_list.clear()
        recent = get_recent_searches(self.config)
        if not recent:
            status.update("Type to search…")
            return

        for query in recent:
            results_list.append(RecentSearchItem(query))
        status.update(f"Recent searches ({len(recent)})")

    def _update_status(self, page: SearchResultPage, *, raw_query: str) -> None:
        status = self.query_one("#search-status", Static)
        if not raw_query.strip():
            self._show_empty_state()
            return

        # Pad dynamic numbers so the status row doesn't jitter as you type:
        # elapsed_ms grows 4.2 → 10.7 → 100.3 chars, and `loaded` grows
        # 1 → 10 → 100 as you scroll. Terminal fonts are monospace, so the
        # only thing moving the layout is the format-spec width itself.
        if page.total_count == 0:
            status.update(f"No results  •  {page.elapsed_ms:>6.1f} ms")
            return

        noun = "result" if page.total_count == 1 else "results"
        loaded = min(self._loaded_count, self._total_count)
        count_width = len(str(page.total_count))
        parts = [
            f"{loaded:>{count_width}} of {page.total_count} {noun}",
            f"{page.elapsed_ms:>6.1f} ms",
        ]
        if page.used_fuzzy_fallback:
            parts.append("fuzzy fallback")
        if loaded < page.total_count:
            parts.append("more load as you scroll")
        status.update("  •  ".join(parts))

    def _run_search(self, raw_query: str, *, reset: bool) -> None:
        results_list = self.query_one("#search-results", ListView)

        raw = raw_query.strip()
        if not raw:
            self._show_empty_state()
            return

        try:
            offset = 0 if reset or raw != self._active_query else self._loaded_count
            page = search_messages(
                self.conn,
                raw,
                limit=SEARCH_PAGE_SIZE,
                offset=offset,
                current_session_id=self.current_session_id,
            )
        except Exception as e:  # noqa: BLE001
            self._loading_more = False
            results_list.clear()
            self.query_one("#search-status", Static).update(f"Search error: {e}")
            return

        if reset or raw != self._active_query:
            results_list.clear()
            self._active_query = raw
            self._loaded_count = 0

        for row in page.rows:
            results_list.append(SearchResultItem(row))

        if reset and page.rows:
            results_list.index = 0

        self._loaded_count = offset + len(page.rows)
        self._total_count = page.total_count
        self._last_elapsed_ms = page.elapsed_ms
        self._update_status(page, raw_query=raw)
        self._loading_more = False

    def _load_more_results(self) -> None:
        if self._loading_more:
            return
        if not self._active_query:
            return
        if self._loaded_count >= self._total_count:
            return
        self._loading_more = True
        self._run_search(self._active_query, reset=False)

    def on_key(self, event) -> None:
        """Intercept nav / Escape keys while the Input holds focus.

        Arrow keys and Ctrl+N/P aren't bound by Input so they bubble up
        to here. Escape isn't bound by Input either. Enter IS bound by
        Input (fires Submitted), so it's handled via on_input_submitted
        below rather than here.
        """
        key = event.key
        if key == "escape":
            event.stop()
            event.prevent_default()
            self.dismiss(None)
        elif key in ("down", "ctrl+n"):
            event.stop()
            event.prevent_default()
            self._move_selection(1)
        elif key in ("up", "ctrl+p"):
            event.stop()
            event.prevent_default()
            self._move_selection(-1)
        elif key == "pagedown":
            event.stop()
            event.prevent_default()
            self._move_selection(10)
        elif key == "pageup":
            event.stop()
            event.prevent_default()
            self._move_selection(-10)
        elif key == "ctrl+x":
            event.stop()
            event.prevent_default()
            self.action_clear_search_history()

    def action_clear_search_history(self) -> None:
        input_widget = self.query_one("#search-input", Input)
        if (input_widget.value or "").strip():
            input_widget.value = ""
            return
        clear_recent_searches(self.config)
        self._show_empty_state()

    def action_open_result(self) -> None:
        """Enter → open the highlighted result.

        Fires via the screen-level priority Enter binding. As a safety
        net, ``on_input_submitted`` below catches the Input.Submitted
        message too — either path lands at _open_selected.
        """
        self._open_selected()

    def on_input_submitted(self, event) -> None:
        """Safety net: if Input's own enter binding fires first, its
        Submitted message bubbles up to here. Treat it the same as the
        screen-level binding — dismiss with the highlighted row.
        """
        try:
            if event.input.id != "search-input":
                return
        except AttributeError:
            return
        event.stop()
        self._open_selected()

    def _move_selection(self, delta: int) -> None:
        lv = self.query_one("#search-results", ListView)
        count = len(lv.children)
        if count == 0:
            return
        if lv.index is None:
            lv.index = 0 if delta > 0 else count - 1
            return
        new_idx = max(0, min(count - 1, lv.index + delta))
        lv.index = new_idx
        if count and new_idx >= count - 3:
            self._load_more_results()

    def _open_selected(self) -> None:
        """Dismiss with (session_id, uuid, search_terms).

        Reads ``children[index]`` directly rather than ``highlighted_child``
        because ``highlighted_child`` may not be synchronously up to date
        after a programmatic ``index`` change — the reactive update lands
        on the next message cycle, not before we're called.

        ``search_terms`` is the raw query parsed into prefix-stripped
        words so the caller can inline-highlight matches in the target
        widget. Filter tokens (``project:``, ``user:``, ``assistant:``)
        are not included.
        """
        lv = self.query_one("#search-results", ListView)
        if not lv.children:
            return
        idx = lv.index if lv.index is not None else 0
        if idx < 0 or idx >= len(lv.children):
            idx = 0
        item = lv.children[idx]
        if isinstance(item, RecentSearchItem):
            input_widget = self.query_one("#search-input", Input)
            input_widget.value = item.query
            input_widget.focus()
            return
        if not isinstance(item, SearchResultItem):
            return

        raw_query = ""
        try:
            raw_query = self.query_one("#search-input", Input).value or ""
        except Exception:
            pass
        spec = _build_fts_query(
            raw_query,
            current_session_id=self.current_session_id,
        )
        terms = list(spec.terms)
        save_recent_search(self.config, raw_query)

        row = item.result
        self.dismiss((row["session_id"], row["uuid"], terms))


class BookmarkItem(ListItem):
    """One row in the bookmark browser.

    Layout mirrors SearchResultItem so the two modals feel like siblings:
    category / note / role marker + snippet, then session · project · timestamp.
    """

    def __init__(self, row: dict):
        self.row = row
        super().__init__()

    def compose(self) -> ComposeResult:
        role = self.row.get("role", "")
        role_icon = "▶" if role == "user" else "●"
        role_style = "bold cyan" if role == "user" else "bold green"
        note = (self.row.get("note") or self.row.get("label") or "").strip()
        category = (self.row.get("category_name") or "").strip()

        first_line = Text()
        first_line.append("★ ", style="bold yellow")
        if category:
            first_line.append(f"[{category}]", style="bold yellow")
            first_line.append("  ", style="dim")
        if note:
            first_line.append(note, style="bold")
            first_line.append("  ", style="dim")
        first_line.append(f"{role_icon} ", style=role_style)
        # Trim the stored text to a single-line snippet — the full body
        # is reachable via Enter → jump.
        raw_text = (self.row.get("text") or "").strip().replace("\n", " ")
        snippet = raw_text[:160] + ("…" if len(raw_text) > 160 else "")
        first_line.append(snippet)

        session_name = (
            self.row.get("custom_name")
            or self.row.get("project")
            or (self.row.get("session_id", "") or "")[:8]
        )
        project = self.row.get("project") or ""
        ts_raw = self.row.get("timestamp") or ""
        ts_str = ts_raw[:16]
        try:
            dt = datetime.fromisoformat(ts_raw.replace("Z", "+00:00"))
            ts_str = dt.strftime("%Y-%m-%d %H:%M")
        except (ValueError, AttributeError):
            pass

        meta = Text()
        meta.append("  ", style="dim")
        meta.append(str(session_name), style="bold")
        if project and project != session_name:
            meta.append("  ·  ", style="dim")
            meta.append(str(project), style="cyan")
        meta.append("  ·  ", style="dim")
        meta.append(ts_str, style="dim")

        yield Static(Group(first_line, meta), classes="bookmark-item")


class BookmarkBrowserScreen(ModalScreen):
    """Modal bookmark browser (task #18).

    Shares the SearchScreen layout — filter Input on top, ListView of
    results, status + hint lines at the bottom — so keyboard muscle
    memory carries over. Filter runs case-insensitive substring matches
    over note / kind / message body / project / custom name via
    ``db.list_bookmarks``.

    Dismiss payload:
      - ``(session_id, message_uuid)`` when the user hits Enter on a row
      - ``None`` on Escape

    The caller routes the payload through ``_jump_to_search_result``
    which handles all three cases (same session, sidebar session,
    cross-project/out-of-view session).
    """

    DEBOUNCE_SECONDS = 0.08

    BINDINGS = [
        Binding("enter", "open_result", "Open", priority=True, show=False),
    ]

    CSS = """
    BookmarkBrowserScreen {
        layout: vertical;
        align: center top;
        background: $background 70%;
    }

    #bookmark-container {
        width: 90%;
        max-width: 140;
        height: 85%;
        margin-top: 1;
        background: $surface;
        border: thick $accent;
        layout: vertical;
    }

    #bookmark-input {
        margin: 0;
        border: none;
        background: $boost;
    }

    #bookmark-input:focus {
        background: $boost;
    }

    #bookmark-results {
        height: 1fr;
        background: $surface;
    }

    #bookmark-status {
        height: 1;
        padding: 0 1;
        color: $text-muted;
        background: $boost;
    }

    #bookmark-help {
        height: 1;
        padding: 0 1;
        color: $text-muted;
        background: $boost;
        text-style: italic;
    }

    .bookmark-item {
        padding: 0 1;
    }

    BookmarkItem {
        padding: 0;
    }
    """

    def __init__(self, conn: sqlite3.Connection):
        super().__init__()
        self.conn = conn
        self._filter_timer = None

    def compose(self) -> ComposeResult:
        with Vertical(id="bookmark-container") as container:
            container.border_title = "Bookmarks"
            yield Input(
                placeholder="Filter bookmarks by category, note, text, or session…",
                id="bookmark-input",
            )
            yield Static("", id="bookmark-status")
            yield ListView(id="bookmark-results")
            yield Static(
                "↑/↓ navigate • Enter jump • L note • d delete • Esc close",
                id="bookmark-help",
            )

    def on_mount(self) -> None:
        self._refresh_results("")
        self.query_one("#bookmark-input", Input).focus()

    def on_input_changed(self, event) -> None:
        try:
            if event.input.id != "bookmark-input":
                return
        except AttributeError:
            return
        if self._filter_timer is not None:
            try:
                self._filter_timer.stop()
            except Exception:
                pass
        value = event.value
        self._filter_timer = self.set_timer(
            self.DEBOUNCE_SECONDS, lambda v=value: self._refresh_results(v)
        )

    def _refresh_results(self, raw_query: str) -> None:
        """Reload the results list from the DB. Preserves cursor position
        when possible so in-place edits (note / delete) don't jump the
        user back to the top."""
        results_list = self.query_one("#bookmark-results", ListView)
        status = self.query_one("#bookmark-status", Static)

        prev_idx = results_list.index
        results_list.clear()

        query = raw_query.strip() or None
        try:
            rows = db.list_bookmarks(self.conn, query=query, limit=200)
        except Exception as e:  # noqa: BLE001
            status.update(f"Bookmark query error: {e}")
            return

        for row in rows:
            results_list.append(BookmarkItem(row))

        count = len(rows)
        if count == 0:
            status.update("No bookmarks" if query is None else "No bookmarks match")
        else:
            noun = "bookmark" if count == 1 else "bookmarks"
            status.update(
                f"{count} {noun}" + (" (showing first 200)" if count >= 200 else "")
            )

        # Restore cursor if still in range — otherwise clamp.
        if count:
            if prev_idx is None:
                results_list.index = 0
            else:
                results_list.index = min(prev_idx, count - 1)

    def on_key(self, event) -> None:
        key = event.key
        if key == "escape":
            event.stop()
            event.prevent_default()
            self.dismiss(None)
        elif key in ("down", "ctrl+n"):
            event.stop()
            event.prevent_default()
            self._move_selection(1)
        elif key in ("up", "ctrl+p"):
            event.stop()
            event.prevent_default()
            self._move_selection(-1)
        elif key == "L":
            event.stop()
            event.prevent_default()
            self._edit_note_selected()
        elif key == "d":
            event.stop()
            event.prevent_default()
            self._delete_selected()

    def action_open_result(self) -> None:
        self._open_selected()

    def on_input_submitted(self, event) -> None:
        try:
            if event.input.id != "bookmark-input":
                return
        except AttributeError:
            return
        event.stop()
        self._open_selected()

    def _move_selection(self, delta: int) -> None:
        lv = self.query_one("#bookmark-results", ListView)
        count = len(lv.children)
        if count == 0:
            return
        if lv.index is None:
            lv.index = 0 if delta > 0 else count - 1
            return
        lv.index = max(0, min(count - 1, lv.index + delta))

    def _current_row(self) -> dict | None:
        lv = self.query_one("#bookmark-results", ListView)
        if not lv.children:
            return None
        idx = lv.index if lv.index is not None else 0
        if idx < 0 or idx >= len(lv.children):
            idx = 0
        item = lv.children[idx]
        if not isinstance(item, BookmarkItem):
            return None
        return item.row

    def _open_selected(self) -> None:
        row = self._current_row()
        if row is None:
            return
        self.dismiss((row["session_id"], row["message_uuid"]))

    def _edit_note_selected(self) -> None:
        row = self._current_row()
        if row is None:
            return
        bookmark_id = row["id"]
        current = row.get("note") or ""

        def _apply(new_note: str | None) -> None:
            if new_note is None:
                return
            try:
                db.set_bookmark_note(self.conn, bookmark_id, new_note)
            except Exception as e:  # noqa: BLE001
                self.app.notify(f"Note update failed: {e}", severity="error")
                return
            # Refresh in place so the edited note renders immediately.
            raw = self.query_one("#bookmark-input", Input).value or ""
            self._refresh_results(raw)
            shown = new_note.strip() or "(no note)"
            self.app.notify(f"Bookmark note: {shown}")

        self.app.push_screen(LabelPromptScreen(current), _apply)

    def _delete_selected(self) -> None:
        row = self._current_row()
        if row is None:
            return
        try:
            db.delete_bookmark(self.conn, row["id"])
        except Exception as e:  # noqa: BLE001
            self.app.notify(f"Delete failed: {e}", severity="error")
            return
        raw = self.query_one("#bookmark-input", Input).value or ""
        self._refresh_results(raw)
        self.app.notify("Bookmark removed")


class LabelPromptScreen(ModalScreen):
    """Tiny modal that captures a single line of text.

    Used both when setting a note from selection mode and when editing one from
    the browser. Returns the submitted string on Enter, or ``None`` on Escape.
    The caller decides whether an empty submission means "clear" (bookmark
    browser does that via ``set_bookmark_note``'s blank-collapses-to-NULL
    behaviour).
    """

    BINDINGS = [
        Binding("enter", "submit", "Save", priority=True, show=False),
        Binding("escape", "cancel", "Cancel", priority=True, show=False),
    ]

    CSS = """
    LabelPromptScreen {
        layout: vertical;
        align: center middle;
        background: $background 70%;
    }

    #label-container {
        width: 70;
        max-width: 90%;
        height: auto;
        background: $surface;
        border: thick $accent;
        padding: 1 2;
        layout: vertical;
    }

    #label-title {
        height: 1;
        text-style: bold;
        color: $text;
    }

    #label-input {
        margin-top: 1;
    }

    #label-hint {
        height: 1;
        margin-top: 1;
        color: $text-muted;
        text-style: italic;
    }
    """

    def __init__(self, initial: str = ""):
        super().__init__()
        self._initial = initial

    def compose(self) -> ComposeResult:
        with Vertical(id="label-container"):
            yield Static("Bookmark note", id="label-title")
            yield Input(
                value=self._initial,
                placeholder="Note (blank = clear)",
                id="label-input",
            )
            yield Static("Enter to save • Esc to cancel", id="label-hint")

    def on_mount(self) -> None:
        self.query_one("#label-input", Input).focus()

    def action_submit(self) -> None:
        value = self.query_one("#label-input", Input).value or ""
        self.dismiss(value)

    def action_cancel(self) -> None:
        self.dismiss(None)

    def on_input_submitted(self, event) -> None:
        try:
            if event.input.id != "label-input":
                return
        except AttributeError:
            return
        event.stop()
        self.action_submit()


class ConfirmScreen(ModalScreen):
    """Minimal yes/no confirmation modal for TUI actions."""

    BINDINGS = [
        Binding("y", "confirm", "Yes", priority=True, show=False),
        Binding("n", "cancel", "No", priority=True, show=False),
        Binding("enter", "confirm", "Yes", priority=True, show=False),
        Binding("escape", "cancel", "No", priority=True, show=False),
    ]

    CSS = """
    ConfirmScreen {
        layout: vertical;
        align: center middle;
        background: $background 70%;
    }

    #confirm-container {
        width: 72;
        max-width: 90%;
        height: auto;
        background: $surface;
        border: thick $accent;
        padding: 1 2;
        layout: vertical;
    }

    #confirm-title {
        color: $text;
        text-style: bold;
    }

    #confirm-hint {
        height: 1;
        margin-top: 1;
        color: $text-muted;
        text-style: italic;
    }
    """

    def __init__(self, title: str):
        super().__init__()
        self._title = title

    def compose(self) -> ComposeResult:
        with Vertical(id="confirm-container"):
            yield Static(self._title, id="confirm-title")
            yield Static("y/Enter = yes • n/Esc = no", id="confirm-hint")

    def action_confirm(self) -> None:
        self.dismiss(True)

    def action_cancel(self) -> None:
        self.dismiss(False)


class BookmarkCategoryPickerScreen(ModalScreen):
    """Minimal category picker stub for future TUI integration.

    This deliberately stays small for now: users can type a category name and
    ThreadHop will reuse the shared bookmark-ingest primitive. TODO: replace
    this with a searchable picker + note capture flow.
    """

    BINDINGS = [
        Binding("enter", "submit", "Save", priority=True, show=False),
        Binding("escape", "cancel", "Cancel", priority=True, show=False),
    ]

    CSS = """
    BookmarkCategoryPickerScreen {
        layout: vertical;
        align: center middle;
        background: $background 70%;
    }

    #bookmark-category-container {
        width: 76;
        max-width: 90%;
        height: auto;
        background: $surface;
        border: thick $accent;
        padding: 1 2;
        layout: vertical;
    }

    #bookmark-category-title {
        height: 1;
        text-style: bold;
        color: $text;
    }

    #bookmark-category-input {
        margin-top: 1;
    }

    #bookmark-category-hint {
        height: auto;
        margin-top: 1;
        color: $text-muted;
    }
    """

    def __init__(self, suggestions: list[str]):
        super().__init__()
        self._suggestions = suggestions

    def compose(self) -> ComposeResult:
        hint = ", ".join(self._suggestions) if self._suggestions else "bookmark, research"
        with Vertical(id="bookmark-category-container"):
            yield Static("Bookmark category", id="bookmark-category-title")
            yield Input(
                value=db.DEFAULT_BOOKMARK_CATEGORY,
                placeholder="Category name",
                id="bookmark-category-input",
            )
            yield Static(
                "Built-ins and known categories: "
                f"{hint}\nTODO: replace this stub with a searchable picker + note field.",
                id="bookmark-category-hint",
            )

    def on_mount(self) -> None:
        self.query_one("#bookmark-category-input", Input).focus()

    def action_submit(self) -> None:
        value = self.query_one("#bookmark-category-input", Input).value or ""
        self.dismiss(value.strip() or db.DEFAULT_BOOKMARK_CATEGORY)

    def action_cancel(self) -> None:
        self.dismiss(None)

    def on_input_submitted(self, event) -> None:
        try:
            if event.input.id != "bookmark-category-input":
                return
        except AttributeError:
            return
        event.stop()
        self.action_submit()


class HelpScreen(ModalScreen):
    """Full-app help overlay grouped by scope (ADR-017, task #42).

    Mirrors the search modal layout so the two discoverability surfaces
    feel consistent. Reads entirely from ``COMMAND_REGISTRY`` — anything
    new added there appears here automatically.
    """

    BINDINGS = [
        Binding("escape", "close", "Close", priority=True),
        Binding("?", "close", "Close", priority=True),
        Binding("q", "close", "Close", priority=True),
    ]

    CSS = """
    HelpScreen {
        layout: vertical;
        align: center top;
        background: $background 70%;
    }

    #help-container {
        width: 90%;
        max-width: 120;
        height: 85%;
        margin-top: 1;
        background: $surface;
        border: thick $accent;
        layout: vertical;
    }

    #help-title {
        height: 1;
        padding: 0 1;
        background: $boost;
        color: $text;
        text-style: bold;
    }

    #help-body {
        height: 1fr;
        padding: 1 2;
    }

    #help-hint {
        height: 1;
        padding: 0 1;
        color: $text-muted;
        background: $boost;
        text-style: italic;
    }

    .help-scope {
        margin-top: 1;
        text-style: bold;
        color: $accent;
    }

    .help-scope:first-of-type {
        margin-top: 0;
    }
    """

    def compose(self) -> ComposeResult:
        with Vertical(id="help-container") as container:
            container.border_title = "Help — keys grouped by context"
            yield Static("ThreadHop commands", id="help-title")
            yield VerticalScroll(id="help-body")
            yield Static(
                "Esc / ? / q close • keys are grouped by where they're live",
                id="help-hint",
            )

    def on_mount(self) -> None:
        body = self.query_one("#help-body", VerticalScroll)
        for scope in SCOPE_ORDER:
            commands = commands_for_scope(scope)
            if not commands:
                continue
            body.mount(Static(SCOPE_LABELS[scope], classes="help-scope"))
            # Width of the keys column is computed per-scope so shorter
            # sections don't pay for a long key name in another scope.
            key_width = max(len(format_command_keys(c)) for c in commands)
            key_width = max(key_width, 6)
            for cmd in commands:
                keys = format_command_keys(cmd).ljust(key_width)
                body.mount(Static(f"  {keys}   {cmd.description}"))

    def action_close(self) -> None:
        self.dismiss(None)


class ContextualFooter(Static):
    """Single-line footer that shows scope-relevant hints from the registry.

    The stock ``Footer`` widget lists every visible binding at once, which
    turns into noise once the app has focus-aware and mode-specific
    commands. Instead we read ``COMMAND_REGISTRY`` and render only
    commands whose scope matches the caller-supplied context, preferring
    those marked ``footer=True``.
    """

    DEFAULT_CSS = """
    ContextualFooter {
        dock: bottom;
        height: 1;
        background: $boost;
        color: $text-muted;
        padding: 0 1;
    }
    ContextualFooter > .footer-sep {
        color: $text-disabled;
    }
    """

    def __init__(self, *args, **kwargs):
        super().__init__("", *args, **kwargs)
        self._last_render: tuple[tuple[str, ...], str | None] = ((), None)

    def render_for(self, scopes: list[str], note: str | None = None) -> None:
        """Rebuild the footer content for the given list of active scopes.

        Scopes are evaluated in the order passed — the registry entries
        for later scopes win when a key appears in multiple (e.g. Enter
        is Reply/Send globally but Send in the reply scope).
        """
        scope_tuple = tuple(scopes)
        signature = (scope_tuple, note)
        if signature == self._last_render:
            # Footer already reflects the right context — skip the rebuild.
            return
        self._last_render = signature

        # Keep the help shortcut pinned on the far left so it stays
        # discoverable regardless of current focus.
        parts: list[str] = []
        seen_keys: set[tuple[str, ...]] = set()

        def add(cmd: Command) -> None:
            if cmd.keys in seen_keys:
                return
            seen_keys.add(cmd.keys)
            parts.append(f"[b]{format_command_keys(cmd)}[/b] {cmd.description}")

        # Global "?" first, so it's always in the same spot.
        for cmd in commands_for_scope(SCOPE_GLOBAL):
            if cmd.keys == ("?",):
                add(cmd)
                break

        for scope in scopes:
            for cmd in commands_for_scope(scope):
                if cmd.footer and cmd.keys != ("?",):
                    add(cmd)

        if note:
            parts.append(f"[dim]{escape(note)}[/dim]")

        if not parts:
            self.update("")
            return
        sep = "  [dim]·[/dim]  "
        self.update(sep.join(parts))


class FindBar(Horizontal):
    """Persistent Ctrl+F-style find bar shown above the transcript.

    Stays open until the user dismisses it (Esc, ×, or Ctrl+F again), so
    the active query remains editable and the match counter remains
    visible across session switches. Unlike ``SearchScreen`` (which
    issues FTS5 queries across ALL indexed sessions), this bar operates
    purely on widgets already rendered in the current ``TranscriptView``
    — it's a within-transcript "find in page", not a cross-session
    search.

    Hidden by default via ``display = False``; App.action_open_find
    toggles it visible and focuses the input.
    """

    def compose(self) -> ComposeResult:
        yield Input(
            placeholder="Find in transcript… (Enter=next, ↑/↓ prev/next, Esc=close)",
            id="find-input",
        )
        yield Static("", id="find-status")
        yield Static("[×]", id="find-close")

    def update_status(self, current: int, total: int, has_query: bool) -> None:
        """Set the match counter. ``has_query=False`` blanks the status."""
        status = self.query_one("#find-status", Static)
        if not has_query:
            status.update("")
        elif total == 0:
            status.update("No matches")
        elif total == 1:
            status.update("1 match")
        else:
            status.update(f"{current} of {total} matches")

    def on_key(self, event) -> None:
        """Arrow / Escape bubble up from the Input — keep them here.

        Up/Down navigate matches without leaving the input. Escape
        closes the bar and clears highlights. Enter is handled by the
        app's ``on_input_submitted`` because Input fires Submitted on
        Enter, not a raw key event here.
        """
        key = event.key
        if key == "escape":
            event.stop()
            event.prevent_default()
            self.app._close_find()
        elif key == "down":
            event.stop()
            event.prevent_default()
            self.app.action_find_next()
        elif key == "up":
            event.stop()
            event.prevent_default()
            self.app.action_find_prev()

    def on_click(self, event) -> None:
        """Click on the × static to dismiss.

        Textual's Click event bubbles up the widget tree; checking the
        event's widget id here lets the whole bar act as a click host
        without extra message plumbing.
        """
        try:
            target = getattr(event, "widget", None)
            if target is not None and target.id == "find-close":
                event.stop()
                self.app._close_find()
        except Exception:
            pass


class ClaudeSessions(App):
    """Cross-session context manager for Claude Code"""

    TITLE = "ThreadHop"
    ENABLE_COMMAND_PALETTE = False

    CSS = """
    Screen {
        layout: grid;
        grid-size: 2 2;
        grid-columns: 36 1fr;
        grid-rows: 1fr auto;
        grid-gutter: 0;
    }

    /* Idle panels all carry the same muted border so section boundaries
       stay visible without competing. The focused panel is promoted to
       $accent, giving the eye a single clear target — this is the TUI
       analogue of focus-elevation via shadow on the web. */
    #session-list {
        height: 100%;
        border: solid $panel;
        border-title-color: $text;
    }

    #session-list:focus-within {
        border: solid $accent;
    }

    #transcript-column {
        height: 100%;
        layout: vertical;
    }

    #transcript-scroll {
        height: 1fr;
        border: solid $panel;
        border-title-color: $text;
    }

    #transcript-scroll:focus-within {
        border: solid $accent;
    }

    ObservationInfoHeader {
        padding: 0 1;
        margin: 0;
        color: $text-muted;
        background: $panel;
    }

    FindBar {
        height: 1;
        background: $boost;
        layout: horizontal;
    }

    #find-input {
        width: 1fr;
        height: 1;
        border: none;
        padding: 0 1;
        background: $boost;
    }

    #find-input:focus {
        background: $surface;
    }

    #find-status {
        width: auto;
        height: 1;
        padding: 0 2;
        color: $text-muted;
    }

    /* 5 cells wide (was 3) so the mouse target is comfortable without
       pushing the status counter. Visible glyph stays centered. */
    #find-close {
        width: 5;
        height: 1;
        content-align: center middle;
        color: $error;
        background: $error 15%;
    }

    #find-close:hover {
        background: $error 30%;
    }

    #input-container {
        column-span: 2;
        border: solid $panel;
        border-title-color: $text;
        height: auto;
        min-height: 3;
        max-height: 10;
    }

    #input-container:focus-within {
        border: solid $accent;
    }

    #reply-input {
        background: $surface;
        height: auto;
        min-height: 1;
        max-height: 8;
    }

    #reply-input:focus {
        background: $boost;
    }

    UserMessage {
        padding: 1 1;
        margin: 1 0 0 0;
        background: $primary-background 15%;
        border-left: thick $accent;
    }

    AssistantMessage {
        padding: 1 1;
        margin: 1 0 0 0;
        border-left: thick $success;
    }

    ToolMessage {
        padding: 0 1 0 3;
    }

    .session-label {
        padding: 0 1;
    }

    .status-header {
        padding: 0 1;
        color: $text-muted;
        text-style: italic;
    }

    /* Keep header backgrounds flat so a disabled header can't be mistaken
       for a focusable row, even if Textual briefly parks the cursor on it
       before the disabled-skip logic fires. */
    SessionStatusHeader, SessionStatusHeader:disabled {
        background: transparent;
    }

    ListView:focus > SessionStatusHeader.--highlight {
        background: transparent;
    }

    .session-label.unread {
        color: $warning;
        text-style: bold;
    }

    .session-label.active {
        color: $accent;
    }

    .session-label.working {
        color: $success;
    }

    .session-label.archived {
        color: $text-muted;
        text-style: italic;
    }

    ListView > ListItem.--highlight {
        background: $boost;
    }

    ListView:focus > ListItem.--highlight {
        background: $accent;
    }

    /* Selection highlight — must override per-type border-left */
    UserMessage.message-selected,
    AssistantMessage.message-selected,
    ToolMessage.message-selected {
        background: $warning 20%;
        border-left: thick $warning;
        tint: $warning 8%;
    }

    /* Range selection highlight — softer tint for non-cursor messages */
    UserMessage.message-range-selected,
    AssistantMessage.message-range-selected,
    ToolMessage.message-range-selected {
        background: $accent 15%;
        border-left: thick $accent;
        tint: $accent 5%;
    }

    /* Cursor within a range — keep the stronger warning highlight */
    UserMessage.message-selected.message-range-selected,
    AssistantMessage.message-selected.message-range-selected,
    ToolMessage.message-selected.message-range-selected {
        background: $warning 20%;
        border-left: thick $warning;
        tint: $warning 8%;
    }

    ToastRack {
        margin-bottom: 2;
    }
    """

    # Bindings come from the shared command registry (ADR-017). Add new
    # App-level commands to COMMAND_REGISTRY above, not here — footer and
    # help overlay both read from the same list.
    BINDINGS = app_bindings_from_registry()

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
        saved_theme = self.config.get("theme", "textual-dark")
        themes = get_available_themes()
        if saved_theme in themes:
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
        self.query_one("#input-container").border_title = (
            "Reply (Enter=send, Alt+Enter=newline, Alt+J/K=nav, Esc=cancel)"
        )

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
                            "first_user_msg": first_user_msg,
                            "is_active": has_process,
                            "is_working": is_working,
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
        themes = get_available_themes()
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

    def bookmark_prompt_category(self) -> None:
        """Selection-mode `B`: minimal category picker stub.

        Reuses the same create primitive as the CLI/skill path. TODO: capture
        note text and replace the free-text input with a searchable picker.
        """
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

        session_id = self._selected_session_id
        if not session_id:
            self.notify("No session selected — cannot bookmark", severity="warning")
            return
        session_path = None
        if session_id:
            session = next(
                (row for row in self.sessions if row.get("session_id") == session_id),
                None,
            )
            if session is not None:
                session_path = session.get("path")

        categories = [row["name"] for row in db.list_bookmark_categories(self.conn)]

        def _apply(category_name: str | None) -> None:
            if not category_name:
                return
            try:
                row = bookmark_ops.add_bookmark(
                    self.conn,
                    session_id,
                    uuid,
                    category_name,
                    session_path=session_path,
                )
            except Exception as e:  # noqa: BLE001
                self.notify(f"Bookmark failed: {e}", severity="error")
                return
            self.notify(
                f"Bookmarked in category '{row['category_name']}'"
            )

        self.push_screen(BookmarkCategoryPickerScreen(categories), _apply)

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

        existing = db.get_bookmark(
            self.conn,
            uuid,
            category_name=db.DEFAULT_BOOKMARK_CATEGORY,
        )
        current = existing["note"] if existing and existing.get("note") else ""

        def _apply(new_note: str | None) -> None:
            if new_note is None:
                return
            # If the message isn't bookmarked yet, create one first so we have
            # an id to annotate. This makes `L` on an unbookmarked message
            # behave like "bookmark + note in one step".
            row = db.get_bookmark(
                self.conn,
                uuid,
                category_name=db.DEFAULT_BOOKMARK_CATEGORY,
            )
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
