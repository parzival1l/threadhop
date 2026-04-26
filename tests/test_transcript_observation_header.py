from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

from threadhop_core.storage import db
from threadhop_core.tui.widgets.transcript import TranscriptView


def _dummy_transcript_view(conn):
    dummy = SimpleNamespace(app=SimpleNamespace(conn=conn))
    dummy._format_observation_header = (
        lambda entry_count, obs_path: TranscriptView._format_observation_header(
            dummy, entry_count, obs_path
        )
    )
    return dummy


def test_format_observation_header_uses_tilde_for_home_path(conn):
    dummy = _dummy_transcript_view(conn)

    obs_path = str(Path.home() / ".config" / "threadhop" / "observations" / "abc123.jsonl")

    header = TranscriptView._format_observation_header(dummy, 12, obs_path)

    assert header == "─── 🗒 12 observations · ~/.config/threadhop/observations/abc123.jsonl ───"


def test_get_observation_header_text_reads_state_from_sqlite(conn, tmp_path: Path):
    dummy = _dummy_transcript_view(conn)

    db.upsert_session(conn, "sess-1", str(tmp_path / "sess-1.jsonl"))
    db.upsert_observation_state(
        conn,
        "sess-1",
        str(tmp_path / "sess-1.jsonl"),
        str(Path.home() / ".config" / "threadhop" / "observations" / "sess-1.jsonl"),
        source_byte_offset=128,
        entry_count=3,
    )

    header = TranscriptView._get_observation_header_text(dummy, "sess-1")

    assert header == "─── 🗒 3 observations · ~/.config/threadhop/observations/sess-1.jsonl ───"
    assert TranscriptView._get_observation_header_text(dummy, "missing") is None
