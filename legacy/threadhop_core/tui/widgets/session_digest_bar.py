"""Right-hand SessionDigestBar — passive context column for the focused session.

The bar mirrors OpenCode's right-side info column (header → context →
LSP → footer) but with ThreadHop-specific content: identity, recap
timeline, outputs, token usage, and a small footer with model + perm
mode + resume command.

The bar is *passive*: it never holds focus, it only updates when the
sidebar selection or the digest data changes. The owning App calls
``set_digest(SessionDigest | None)`` whenever:

  - the user highlights / selects a different session row, or
  - the 5-second auto-refresh re-reads the active session's JSONL.

When ``set_digest(None)`` is called the bar shows a muted placeholder.
"""

from __future__ import annotations

from datetime import datetime

from textual.app import ComposeResult
from textual.containers import Vertical, VerticalScroll
from textual.widgets import Static

from ...session.digest import RecapEntry, SessionDigest
from ..utils import format_age


def _fmt_tokens(n: int) -> str:
    if n >= 1_000_000:
        return f"{n / 1_000_000:.1f}M"
    if n >= 1_000:
        return f"{n / 1_000:.1f}k"
    return str(n)


def _fmt_duration(seconds: int | None) -> str:
    if seconds is None or seconds <= 0:
        return ""
    if seconds < 60:
        return f"{seconds}s"
    if seconds < 3600:
        return f"{seconds // 60}m"
    if seconds < 86400:
        h = seconds // 3600
        m = (seconds % 3600) // 60
        return f"{h}h {m}m" if m else f"{h}h"
    d = seconds // 86400
    h = (seconds % 86400) // 3600
    return f"{d}d {h}h" if h else f"{d}d"


def _short_model(model: str) -> str:
    """Render long model identifiers as compact display labels.

    ``claude-opus-4-7[1m]`` → ``Opus 4.7 1m``; ``claude-haiku-4-5`` →
    ``Haiku 4.5``. Unrecognized strings are passed through with a
    length cap so a single odd model name can't blow the column width.
    """
    raw = model.strip()
    if not raw:
        return raw
    lower = raw.lower()
    suffix = ""
    body = raw
    if "[" in lower and lower.endswith("]"):
        body, _, tail = raw.rpartition("[")
        suffix = " " + tail.rstrip("]")
        lower = body.lower()
    for family, label in (
        ("opus", "Opus"),
        ("sonnet", "Sonnet"),
        ("haiku", "Haiku"),
    ):
        if family in lower:
            digits = "".join(ch for ch in lower.split(family, 1)[1] if ch.isdigit() or ch == "-")
            digits = digits.strip("-").replace("--", "-")
            version = ""
            if digits:
                parts = digits.split("-")
                if parts[0]:
                    version = parts[0]
                if len(parts) > 1 and parts[1]:
                    version = f"{version}.{parts[1]}"
            return f"{label}{(' ' + version) if version else ''}{suffix}"
    if len(raw) > 18:
        return raw[:17] + "…"
    return raw


def _iso_to_age(ts: str | None) -> str:
    if not ts:
        return ""
    try:
        dt = datetime.fromisoformat(str(ts).replace("Z", "+00:00"))
    except Exception:
        return ""
    return format_age(dt.timestamp())


class SessionDigestBar(VerticalScroll):
    """Right-hand bar with identity / recap / outputs / context / footer."""

    DEFAULT_CSS = ""  # all styling lives in css/session_digest.tcss

    def __init__(self, *, id: str | None = None) -> None:
        super().__init__(id=id)
        self._digest: SessionDigest | None = None
        self.can_focus = False

    def compose(self) -> ComposeResult:
        with Vertical(id="digest-empty"):
            yield Static(
                "Select a session to see its digest.",
                classes="digest-placeholder",
            )

    def set_digest(self, digest: SessionDigest | None) -> None:
        """Replace the bar contents with a fresh digest (or empty state)."""
        self._digest = digest
        # Tear down whatever was there and re-mount.
        for child in list(self.children):
            child.remove()
        if digest is None:
            self.mount(Static("Select a session to see its digest.",
                              classes="digest-placeholder"))
            return
        self._mount_identity(digest)
        if digest.recap:
            self._mount_recap(digest.recap)
        if self._has_outputs(digest):
            self._mount_outputs(digest)
        if self._has_context(digest):
            self._mount_context(digest)
        self._mount_footer(digest)

    # ------------------------------------------------------------------ helpers

    @staticmethod
    def _has_outputs(d: SessionDigest) -> bool:
        return bool(d.pr_number or d.files_touched)

    @staticmethod
    def _has_context(d: SessionDigest) -> bool:
        return d.total_tokens > 0 or bool(d.models_used)

    def _mount_identity(self, d: SessionDigest) -> None:
        block = Vertical(classes="digest-block digest-identity")
        self.mount(block)
        block.mount(Static(d.title, classes="digest-title"))
        meta_bits: list[str] = []
        if d.slug:
            meta_bits.append(d.slug)
        if meta_bits:
            block.mount(Static(" · ".join(meta_bits), classes="digest-slug"))
        chip_parts: list[str] = []
        if d.branch:
            chip_parts.append(f"⎇ {d.branch}")
        dur = _fmt_duration(d.duration_seconds)
        if dur:
            chip_parts.append(dur)
        if chip_parts:
            block.mount(Static("  ·  ".join(chip_parts), classes="digest-chips"))

    def _mount_recap(self, recap: list[RecapEntry]) -> None:
        block = Vertical(classes="digest-block digest-recap")
        self.mount(block)
        block.mount(Static("Recap", classes="digest-section-header"))
        for entry in recap:
            band = Vertical(classes="digest-recap-band")
            block.mount(band)
            label = entry.label
            age = _iso_to_age(entry.timestamp)
            if age:
                label = f"{label} · {age} ago"
            band.mount(Static(label, classes="digest-band-label"))
            band.mount(Static(entry.text, classes="digest-band-body"))

    def _mount_outputs(self, d: SessionDigest) -> None:
        block = Vertical(classes="digest-block digest-outputs")
        self.mount(block)
        block.mount(Static("Outputs", classes="digest-section-header"))
        if d.pr_number:
            line = f"↗ PR #{d.pr_number}"
            if d.pr_repository:
                line += f"  {d.pr_repository}"
            block.mount(Static(line, classes="digest-output-row digest-pr"))
        if d.files_touched_count:
            label = f"✎ {d.files_touched_count} file"
            if d.files_touched_count != 1:
                label += "s"
            block.mount(Static(label, classes="digest-output-row"))

    def _mount_context(self, d: SessionDigest) -> None:
        block = Vertical(classes="digest-block digest-context")
        self.mount(block)
        block.mount(Static("Context", classes="digest-section-header"))

        # Headline: latest turn's prompt size against the model's
        # context window. This is the single most actionable token
        # number — it tells the user how close they are to a compact.
        fill_ratio = d.context_fill_ratio
        if fill_ratio is not None:
            fill_pct = fill_ratio * 100
            block.mount(Static(
                f"{_fmt_tokens(d.latest_turn_input_tokens)} / "
                f"{_fmt_tokens(d.context_window)}  ·  {fill_pct:.0f}% used",
                classes="digest-context-row digest-context-fill",
            ))

        # Cumulative session totals — independent of context-window
        # pressure. ``total_input_tokens_billed`` includes cached reads
        # and cache-creation so the user sees the real input weight,
        # not just the uncached slice.
        in_total = d.total_input_tokens_billed
        out_total = d.total_output_tokens
        if in_total or out_total:
            block.mount(Static(
                f"input {_fmt_tokens(in_total)}  ·  output {_fmt_tokens(out_total)}",
                classes="digest-context-row",
            ))

        # Cache effectiveness as a single ratio line; the cached/uncached
        # split is implicit in this number (95% hit ⇒ most input was
        # cached).
        if d.cache_hit_ratio is not None:
            block.mount(Static(
                f"cache {d.cache_hit_ratio * 100:.0f}% hit",
                classes="digest-context-row",
            ))

        if d.models_used:
            labels = [_short_model(m) for m in d.models_used]
            # Filter out the synthetic placeholder so the model line stays useful.
            labels = [l for l in labels if l and l != "<synthetic>"]
            if labels:
                block.mount(Static(
                    " · ".join(labels),
                    classes="digest-context-row digest-models",
                ))

    def _mount_footer(self, d: SessionDigest) -> None:
        block = Vertical(classes="digest-block digest-footer")
        self.mount(block)
        block.mount(Static("─" * 8, classes="digest-divider"))
        chip_parts: list[str] = []
        if d.permission_mode:
            chip_parts.append(d.permission_mode)
        if d.client_version:
            chip_parts.append(f"v{d.client_version}")
        if chip_parts:
            block.mount(Static("  ·  ".join(chip_parts), classes="digest-footer-chips"))
        # Resume command — copy-able mental affordance even though the
        # bar itself isn't focusable. Always show it; it's the one
        # action-shaped artifact the bar carries.
        sid = d.session_id
        short = sid[:8] if len(sid) > 8 else sid
        block.mount(Static(
            f"claude -r {short}…",
            classes="digest-footer-resume",
        ))


__all__ = ["SessionDigestBar"]
