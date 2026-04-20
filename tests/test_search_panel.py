"""Tests for the hardened search panel query helpers."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import sys
from pathlib import Path

import db


def _load_threadhop_module():
    root = Path(__file__).resolve().parents[1]
    path = root / "threadhop"
    module_name = "threadhop_script_test"
    loader = importlib.machinery.SourceFileLoader(module_name, str(path))
    spec = importlib.util.spec_from_loader(module_name, loader)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    loader.exec_module(module)
    return module


threadhop = _load_threadhop_module()


def _seed_message(
    conn,
    *,
    session_id: str,
    uuid: str,
    text: str,
    timestamp: str,
    project: str = "atlas",
    role: str = "user",
) -> None:
    db.upsert_session(
        conn,
        session_id,
        f"/tmp/{session_id}.jsonl",
        project=project,
        created_at=0,
        modified_at=0,
    )
    db.execute(
        conn,
        """
        INSERT INTO messages (
            uuid, session_id, role, text, timestamp, cwd, parent_uuid, is_sidechain
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        """,
        (uuid, session_id, role, text, timestamp, "/tmp", None, 0),
    )


def test_build_fts_query_supports_scope_and_date_filters():
    spec = threadhop._build_fts_query(
        "project:atlas session:current since:2026-04-01 until:2026-04-10 "
        "user: rate-limit retries",
        current_session_id="sess-current",
    )

    assert spec.role == "user"
    assert spec.project == "atlas"
    assert spec.session_id == "sess-current"
    assert spec.terms == ("ratelimit", "retries")
    assert spec.fts_expr == "ratelimit* retries*"
    assert spec.since_ts == "2026-04-01T00:00:00Z"
    assert spec.until_ts == "2026-04-11T00:00:00Z"
    assert spec.until_is_exclusive is True


def test_search_messages_pages_current_session_results(conn):
    _seed_message(
        conn,
        session_id="sess-a",
        uuid="a1",
        text="alpha planning note",
        timestamp="2026-04-18T10:00:00Z",
    )
    _seed_message(
        conn,
        session_id="sess-a",
        uuid="a2",
        text="alpha implementation detail",
        timestamp="2026-04-18T11:00:00Z",
    )
    _seed_message(
        conn,
        session_id="sess-a",
        uuid="a3",
        text="alpha verification task",
        timestamp="2026-04-18T12:00:00Z",
    )
    _seed_message(
        conn,
        session_id="sess-b",
        uuid="b1",
        text="alpha from another session",
        timestamp="2026-04-18T13:00:00Z",
    )

    page1 = threadhop.search_messages(
        conn,
        "session:current alpha",
        limit=2,
        offset=0,
        current_session_id="sess-a",
    )
    page2 = threadhop.search_messages(
        conn,
        "session:current alpha",
        limit=2,
        offset=2,
        current_session_id="sess-a",
    )

    assert page1.total_count == 3
    assert [row["uuid"] for row in page1.rows] == ["a3", "a2"]
    assert page1.has_more is True
    assert all(row["session_id"] == "sess-a" for row in page1.rows)

    assert [row["uuid"] for row in page2.rows] == ["a1"]
    assert page2.has_more is False
    assert page2.loaded_count == 3


def test_search_messages_boosts_exact_phrase_over_recent_prefix_match(conn):
    _seed_message(
        conn,
        session_id="sess-old",
        uuid="old-exact",
        text="We should rate limit retries in the gateway.",
        timestamp="2026-03-01T12:00:00Z",
    )
    _seed_message(
        conn,
        session_id="sess-new",
        uuid="new-prefix",
        text="The rate limiting policy was updated yesterday.",
        timestamp="2026-04-19T12:00:00Z",
    )

    page = threadhop.search_messages(conn, "rate limit", limit=10)

    assert page.total_count == 2
    assert [row["uuid"] for row in page.rows[:2]] == ["old-exact", "new-prefix"]
    assert page.elapsed_ms >= 0


def test_recent_search_helpers_dedupe_trim_and_clear(monkeypatch):
    writes: list[list[str]] = []
    monkeypatch.setattr(
        threadhop,
        "save_app_config",
        lambda cfg: writes.append(list(cfg.get("recent_searches", []))),
    )

    config = {
        "theme": "textual-dark",
        "recent_searches": ["alpha", "beta", "alpha", " "],
    }

    assert threadhop.get_recent_searches(config) == ["alpha", "beta"]

    threadhop.save_recent_search(config, "beta")
    assert config["recent_searches"] == ["beta", "alpha"]

    for i in range(threadhop.MAX_RECENT_SEARCHES + 3):
        threadhop.save_recent_search(config, f"query-{i}")

    assert len(config["recent_searches"]) == threadhop.MAX_RECENT_SEARCHES

    threadhop.clear_recent_searches(config)
    assert config["recent_searches"] == []
    assert writes
