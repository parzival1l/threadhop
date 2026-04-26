"""``TranscriptView`` — the main message-list panel for the active session.

Owns selection mode, range mode, find-in-transcript state, observation
header rendering, and the JSONL → message-widget translation. Mounted by
``ClaudeSessions.compose`` with id ``transcript-scroll``.

The host App (``ClaudeSessions``) drives loads via ``load_transcript``
and reacts to selection-mode events through methods like
``bookmark_toggle_selection``. Any newly-introduced cross-class hook
should go through the same App-mediated path rather than reaching for
sibling widgets directly — this keeps the transcript widget portable.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
from datetime import datetime
from pathlib import Path

from rich.console import Group
from rich.markdown import Markdown
from rich.markup import escape
from rich.text import Text
from textual.containers import VerticalScroll
from textual.widgets import ListView

from threadhop_core import indexer
from threadhop_core.cli.export_cleanup import EXPORT_DIR
from threadhop_core.storage import db

from ..constants import SYSTEM_REMINDER_RE
from ..utils import format_msg_clock
from .find_bar import FindBar
from .messages import (
    AssistantMessage,
    CommandPill,
    ObservationInfoHeader,
    ToolMessage,
    UserMessage,
)


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
            if isinstance(c, (UserMessage, AssistantMessage, ToolMessage, CommandPill))
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
            "space bookmark, L note, m/Esc exit"
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
            self.border_title = f"Transcript ── SELECT {pos}/{total} (j/k move, v range, y/e copy/export, space bookmark, L note, m/Esc exit)"

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
            elif isinstance(w, CommandPill):
                # Pills are already prefixed with their kind marker
                # (▶ /foo or ✦ skill:bar) — copy the pill as-is so a
                # pasted transcript reads naturally.
                lines.append(raw)
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
            elif isinstance(w, CommandPill):
                role_label = "Command"
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

            # Slash-command invocations and skill-load banners collapse
            # into a one-line CommandPill. Without this, every ``/foo``
            # invocation renders as a "You: <command-name>/foo</command-name>"
            # message, and every skill load drops the entire skill
            # markdown body into the transcript as a fat user message.
            # See CommandPill's docstring and indexer.classify_user_text
            # for the classification rules.
            if role == "command":
                pill = Text()
                pill.append("▶ ", style="dim cyan")
                pill.append(str(content), style="cyan")
                clock = format_msg_clock(timestamp)
                if clock:
                    pill.append(f"  ·  {clock}", style="dim")
                w = CommandPill(pill)
                w._raw_text = f"▶ {content}"
                w._timestamp = timestamp
                w._uuid = uuid
                widgets.append(w)
                continue

            if role == "skill_load":
                pill = Text()
                pill.append("✦ ", style="dim magenta")
                pill.append("skill loaded: ", style="dim")
                pill.append(str(content), style="magenta")
                clock = format_msg_clock(timestamp)
                if clock:
                    pill.append(f"  ·  {clock}", style="dim")
                w = CommandPill(pill)
                w._raw_text = f"✦ skill loaded: {content}"
                w._timestamp = timestamp
                w._uuid = uuid
                widgets.append(w)
                continue

            # Build a 1-line role header with optional dim timestamp,
            # then render the body as Markdown so user-pasted code
            # fences, lists, and inline code render the same way they
            # do for assistant messages. The try/except falls back to
            # plain Text if Markdown trips on the input — same defensive
            # pattern that was already in place for assistant messages.
            if role == "user":
                role_label, role_style, msg_class = "You", "bold cyan", UserMessage
            else:
                role_label, role_style, msg_class = "Claude", "bold green", AssistantMessage

            header = Text()
            header.append(role_label, style=role_style)
            clock = format_msg_clock(timestamp)
            if clock:
                header.append(f"  ·  {clock}", style="dim")

            renderables = [header]
            if content:
                try:
                    renderables.append(Markdown(str(content)))
                except Exception:
                    renderables.append(Text(str(content)))

            w = msg_class(Group(*renderables))
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
                                    # ``classify_user_text`` separates real
                                    # user prose from slash-command markup
                                    # and skill-load banners. The pill
                                    # roles render as compact CommandPill
                                    # widgets (see load_transcript) so the
                                    # transcript isn't drowned in skill
                                    # markdown bodies — see CommandPill's
                                    # docstring for the full rationale.
                                    kind, text = indexer.classify_user_text(content)
                                    if kind == "command":
                                        messages.append((
                                            "command", text, timestamp, uuid,
                                        ))
                                    elif kind == "skill_load":
                                        messages.append((
                                            "skill_load", text, timestamp, uuid,
                                        ))
                                    elif kind == "user" and text:
                                        messages.append((
                                            "user", text, timestamp, uuid,
                                        ))

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

