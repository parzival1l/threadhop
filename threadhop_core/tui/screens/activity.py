"""``ActivityInspectorScreen`` — explain active/working session state."""

from __future__ import annotations

from datetime import datetime
from pathlib import Path

from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.screen import ModalScreen
from textual.widgets import Static

from ..utils import format_age


def _short_path(value: object) -> str:
    if not value:
        return "unknown"
    text = str(value)
    try:
        home = str(Path.home())
        if text == home:
            return "~"
        if text.startswith(home + "/"):
            return "~" + text[len(home):]
    except Exception:
        pass
    return text


def _format_time(value: object) -> str:
    if not value:
        return "unknown"
    try:
        if isinstance(value, (int, float)):
            return datetime.fromtimestamp(float(value)).strftime("%Y-%m-%d %H:%M:%S")
        return datetime.fromisoformat(str(value).replace("Z", "+00:00")).strftime(
            "%Y-%m-%d %H:%M:%S"
        )
    except Exception:
        return str(value)


class ActivityInspectorScreen(ModalScreen):
    """Small modal that shows why the selected session has its status badge."""

    BINDINGS = [
        Binding("escape", "close", "Close", priority=True),
        Binding("i", "close", "Close", priority=True),
        Binding("q", "close", "Close", priority=True),
    ]

    def __init__(self, session: dict):
        super().__init__()
        self.session = session

    def compose(self) -> ComposeResult:
        with Vertical(id="activity-container"):
            yield Static("Activity", id="activity-title")
            yield VerticalScroll(id="activity-body")
            yield Static("esc to close", id="activity-hint")

    def on_mount(self) -> None:
        body = self.query_one("#activity-body", VerticalScroll)
        session_id = str(self.session.get("session_id") or "")
        title = self.session.get("title") or self.session.get("project") or "untitled"
        activity = self.session.get("activity") or {}

        state = "working" if self.session.get("is_working") else (
            "active" if self.session.get("is_active") else "inactive"
        )

        rows = [
            ("Session", session_id[:8] if session_id else "unknown"),
            ("Title", str(title)),
            ("State", state),
            ("Working reason", str(activity.get("working_reason") or "unknown")),
            (
                "Last transcript update",
                (
                    f"{_format_time(activity.get('last_transcript_update'))} "
                    f"({format_age(float(activity.get('last_transcript_update')))} ago)"
                    if activity.get("last_transcript_update")
                    else "unknown"
                ),
            ),
            ("CWD", _short_path(self.session.get("cwd"))),
            ("Transcript", _short_path(self.session.get("path"))),
            (
                "Last event",
                (
                    f"{activity.get('last_message_type') or 'unknown'} · "
                    f"{_format_time(activity.get('last_message_timestamp'))}"
                ),
            ),
        ]

        for label, value in rows:
            self._mount_row(body, label, value)

        body.mount(Static("Process evidence", classes="activity-section"))
        processes = activity.get("active_processes") or []
        if processes:
            for process in processes:
                pid = process.get("pid", "?")
                match = process.get("match_type", "match")
                reason = process.get("reason") or match
                cwd = _short_path(process.get("cwd"))
                body.mount(
                    Static(
                        f"pid {pid} · {match}\n{reason}\ncwd {cwd}",
                        classes="activity-block",
                    )
                )
        else:
            body.mount(
                Static(
                    "No interactive claude process matched this session.",
                    classes="activity-block",
                )
            )

        body.mount(Static("Tool-call evidence", classes="activity-section"))
        pending = activity.get("pending_tool_calls") or []
        if pending:
            for tool in pending:
                self._mount_tool(body, tool, prefix="pending")
        else:
            body.mount(
                Static(
                    "No pending tool_use in the latest assistant chunk.",
                    classes="activity-block",
                )
            )

        recent = activity.get("recent_tool_events") or []
        if recent:
            body.mount(Static("Recent tool_use blocks", classes="activity-section"))
            for tool in recent:
                self._mount_tool(body, tool, prefix="seen")

    def _mount_row(self, body: VerticalScroll, label: str, value: str) -> None:
        body.mount(
            Horizontal(
                Static(label, classes="activity-label"),
                Static(value, classes="activity-value"),
                classes="activity-row",
            )
        )

    def _mount_tool(self, body: VerticalScroll, tool: dict, *, prefix: str) -> None:
        name = tool.get("name") or "Unknown"
        summary = tool.get("summary") or name
        timestamp = _format_time(tool.get("timestamp"))
        tool_id = tool.get("id") or "no id"
        body.mount(
            Static(
                f"{prefix} · {name} · {timestamp}\n{summary}\n{tool_id}",
                classes="activity-block",
            )
        )

    def action_close(self) -> None:
        self.dismiss(None)


__all__ = ["ActivityInspectorScreen"]
