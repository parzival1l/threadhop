from __future__ import annotations

import importlib.machinery
import importlib.util
from pathlib import Path
from types import SimpleNamespace

from threadhop_core.storage import db


def _load_threadhop_module():
    # See test_tui_observation_indicator._load_threadhop_module for why this
    # returns `tui` rather than the executed script: `TranscriptView` lives
    # in tui.py, and loading the script file merely primes `sys.modules` so
    # `import tui` can resolve its `from threadhop import *`.
    import sys as _sys
    path = Path(__file__).resolve().parent.parent / "threadhop"
    loader = importlib.machinery.SourceFileLoader("threadhop_app", str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    _sys.modules.setdefault(loader.name, module)
    loader.exec_module(module)
    import tui  # noqa: PLC0415 — deferred until the script is registered.
    return tui


def _dummy_transcript_view(module, conn):
    dummy = SimpleNamespace(app=SimpleNamespace(conn=conn))
    dummy._format_observation_header = (
        lambda entry_count, obs_path: module.TranscriptView._format_observation_header(
            dummy, entry_count, obs_path
        )
    )
    return dummy


def test_format_observation_header_uses_tilde_for_home_path(conn):
    module = _load_threadhop_module()
    dummy = _dummy_transcript_view(module, conn)

    obs_path = str(Path.home() / ".config" / "threadhop" / "observations" / "abc123.jsonl")

    header = module.TranscriptView._format_observation_header(dummy, 12, obs_path)

    assert header == "─── 🗒 12 observations · ~/.config/threadhop/observations/abc123.jsonl ───"


def test_get_observation_header_text_reads_state_from_sqlite(conn, tmp_path: Path):
    module = _load_threadhop_module()
    dummy = _dummy_transcript_view(module, conn)

    db.upsert_session(conn, "sess-1", str(tmp_path / "sess-1.jsonl"))
    db.upsert_observation_state(
        conn,
        "sess-1",
        str(tmp_path / "sess-1.jsonl"),
        str(Path.home() / ".config" / "threadhop" / "observations" / "sess-1.jsonl"),
        source_byte_offset=128,
        entry_count=3,
    )

    header = module.TranscriptView._get_observation_header_text(dummy, "sess-1")

    assert header == "─── 🗒 3 observations · ~/.config/threadhop/observations/sess-1.jsonl ───"
    assert module.TranscriptView._get_observation_header_text(dummy, "missing") is None
