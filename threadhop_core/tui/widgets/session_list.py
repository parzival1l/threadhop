"""Sidebar widgets: ``SessionItem`` rows and ``SessionStatusHeader`` dividers.

The host App composes these into a ``ListView`` with id ``session-list``.
The status-rank-aware grouping logic lives on the App; these widgets
just render whatever they're handed.
"""

from __future__ import annotations

from rich.text import Text
from textual.app import ComposeResult
from textual.widgets import ListItem, Static

from ..constants import STATUS_LABELS


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
        # Lazy import — render_session_label_text lives in the legacy
        # tui module while the App body still owns it. Once the App
        # extraction lands, this will switch to ``..utils``.
        from tui import render_session_label_text  # noqa: WPS433

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


__all__ = ["SessionItem", "SessionStatusHeader"]
