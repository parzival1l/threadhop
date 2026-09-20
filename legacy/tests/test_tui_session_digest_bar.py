"""Pilot-driven tests for ``SessionDigestBar``.

The widget's job is to render whatever it's handed via ``set_digest``,
full stop. The extractor that produces those digests is exercised
separately in ``test_session_digest.py``.

Tests are sync and drive Textual's async ``run_test()`` harness via
``asyncio.run`` so we don't pull in ``pytest-asyncio`` for the suite.
"""

from __future__ import annotations

import asyncio

from textual.app import App, ComposeResult
from textual.widgets import Static

from threadhop_core.session.digest import RecapEntry, SessionDigest
from threadhop_core.tui.widgets.session_digest_bar import SessionDigestBar


class _Harness(App):
    def compose(self) -> ComposeResult:
        yield SessionDigestBar(id="bar")


def _texts(bar: SessionDigestBar) -> list[str]:
    out: list[str] = []
    for s in bar.query(Static):
        r = s.render()
        out.append(r.plain if hasattr(r, "plain") else str(r))
    return out


def _run(coro):
    """Run an async test body under asyncio without pytest-asyncio."""
    return asyncio.run(coro)


def test_bar_starts_with_placeholder():
    async def body():
        async with _Harness().run_test() as pilot:
            bar = pilot.app.query_one("#bar", SessionDigestBar)
            assert "Select a session" in " ".join(_texts(bar))

    _run(body())


def test_bar_renders_full_digest():
    digest = SessionDigest(
        session_id="abc12345-deadbeef",
        custom_title="Workshop tag: feedback",
        slug="brave-otter",
        branch="dev",
        duration_seconds=2 * 3600 + 14 * 60,  # 2h 14m
        recap=[
            RecapEntry(label="Started", timestamp=None, text="redesign kanban"),
            RecapEntry(
                label="Recently",
                timestamp=None,
                text="just finished input box restyle",
            ),
        ],
        pr_number=73,
        pr_repository="parzival1l/threadhop",
        files_touched={"a.py", "b.py", "c.py"},
        total_input_tokens=47_200,
        total_output_tokens=12_000,
        total_cache_read_tokens=3_500_000,
        total_cache_creation_tokens=180_000,
        latest_turn_input_tokens=50_000,  # latest prompt was ~50k tokens
        context_window=1_000_000,         # opus-4-7[1m]
        models_used=["claude-opus-4-7", "claude-haiku-4-5"],
        permission_mode="acceptEdits",
        client_version="2.1.131",
    )

    async def body():
        async with _Harness().run_test() as pilot:
            bar = pilot.app.query_one("#bar", SessionDigestBar)
            bar.set_digest(digest)
            await pilot.pause()
            rendered = "\n".join(_texts(bar))

            # Identity
            assert "Workshop tag: feedback" in rendered
            assert "brave-otter" in rendered
            assert "⎇ dev" in rendered
            assert "2h 14m" in rendered

            # Recap (no "Last asked" — the most recent turn is in the
            # transcript pane right next to the bar).
            assert "Recap" in rendered
            assert "Started" in rendered
            assert "redesign kanban" in rendered
            assert "Recently" in rendered
            assert "Last asked" not in rendered

            # Outputs
            assert "PR #73" in rendered
            assert "3 files" in rendered

            # Context: fill headline + cumulative totals + cache + models.
            assert "Context" in rendered
            # Latest turn fill: 50.0k / 1.0M · 5% used
            assert "50.0k" in rendered
            assert "1.0M" in rendered
            assert "5% used" in rendered
            # Cumulative billed input = 47.2k + 3.5M + 180k = 3.7M.
            # Output cumulative = 12.0k.
            assert "input 3.7M" in rendered
            assert "output 12.0k" in rendered
            # Cache hit = 3.5M / (3.5M + 180k) ≈ 95%
            assert "cache 95% hit" in rendered
            # Models stay as friendly labels.
            assert "Opus 4.7" in rendered
            assert "Haiku 4.5" in rendered

            # Footer
            assert "acceptEdits" in rendered
            assert "v2.1.131" in rendered
            assert "claude -r abc12345" in rendered

    _run(body())


def test_bar_collapses_empty_sections():
    digest = SessionDigest(
        session_id="short-session",
        recap=[RecapEntry(label="Started", timestamp=None, text="quick question")],
    )

    async def body():
        async with _Harness().run_test() as pilot:
            bar = pilot.app.query_one("#bar", SessionDigestBar)
            bar.set_digest(digest)
            await pilot.pause()
            rendered = "\n".join(_texts(bar))

            assert "Started" in rendered
            assert "quick question" in rendered
            # Section headers should not appear when their sources are empty.
            assert "Outputs" not in rendered
            assert "Context" not in rendered

    _run(body())


def test_bar_replaces_old_digest_on_set():
    first = SessionDigest(
        session_id="first",
        custom_title="First session",
        recap=[RecapEntry(label="Started", timestamp=None, text="alpha")],
    )
    second = SessionDigest(
        session_id="second",
        custom_title="Second session",
        recap=[RecapEntry(label="Started", timestamp=None, text="beta")],
    )

    async def body():
        async with _Harness().run_test() as pilot:
            bar = pilot.app.query_one("#bar", SessionDigestBar)
            bar.set_digest(first)
            await pilot.pause()
            bar.set_digest(second)
            await pilot.pause()
            rendered = "\n".join(_texts(bar))

            assert "Second session" in rendered
            assert "First session" not in rendered
            assert "beta" in rendered
            assert "alpha" not in rendered

    _run(body())


def test_bar_set_digest_none_resets_to_placeholder():
    digest = SessionDigest(
        session_id="x",
        custom_title="X",
        recap=[RecapEntry(label="Started", timestamp=None, text="hi")],
    )

    async def body():
        async with _Harness().run_test() as pilot:
            bar = pilot.app.query_one("#bar", SessionDigestBar)
            bar.set_digest(digest)
            await pilot.pause()
            bar.set_digest(None)
            await pilot.pause()
            rendered = " ".join(_texts(bar))
            assert "Select a session" in rendered
            assert "X" not in rendered

    _run(body())
