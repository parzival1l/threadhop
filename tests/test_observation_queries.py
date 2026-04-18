"""Tests for observation-backed CLI query helpers."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

import db
import observation_queries


@pytest.fixture
def obs_dir(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Redirect observation files into the test tempdir."""
    d = tmp_path / "observations"
    d.mkdir()
    monkeypatch.setattr(db, "OBS_DIR", d)
    return d


def _write_obs(obs_dir: Path, session_id: str, entries: list[dict]) -> Path:
    path = obs_dir / f"{session_id}.jsonl"
    path.write_text("".join(json.dumps(entry) + "\n" for entry in entries))
    return path


def _seed_session(conn, session_id: str, project: str | None = None) -> None:
    session_path = f"/tmp/{session_id}.jsonl"
    db.upsert_session(conn, session_id, session_path, project=project)


class TestRefreshUnprocessedObservations:
    def test_project_filter_only_catches_up_matching_tracked_sessions(
        self,
        conn,
        obs_dir: Path,
        monkeypatch: pytest.MonkeyPatch,
    ):
        _seed_session(conn, "alpha-1", "atlas")
        _seed_session(conn, "beta-1", "billing")
        db.upsert_observation_state(
            conn,
            "alpha-1",
            "/tmp/alpha-1.jsonl",
            str(obs_dir / "alpha-1.jsonl"),
        )
        db.upsert_observation_state(
            conn,
            "beta-1",
            "/tmp/beta-1.jsonl",
            str(obs_dir / "beta-1.jsonl"),
        )

        calls: list[str] = []

        def fake_observe_session(conn_, session_id, *, batch_threshold, **_kwargs):
            calls.append(session_id)
            return {
                "status": "up_to_date",
                "message": f"{session_id} caught up",
                "batch_threshold": batch_threshold,
            }

        monkeypatch.setattr(
            observation_queries.observer,
            "observe_session",
            fake_observe_session,
        )

        results = observation_queries.refresh_unprocessed_observations(
            conn,
            project="atl",
        )

        assert calls == ["alpha-1"]
        assert [result["session_id"] for result in results] == ["alpha-1"]


class TestListObservationEntries:
    def test_reads_all_files_and_sorts_newest_first(self, conn, obs_dir: Path):
        _seed_session(conn, "sess-a", "atlas")
        _seed_session(conn, "sess-b", "billing")
        _write_obs(
            obs_dir,
            "sess-a",
            [
                {
                    "type": "todo",
                    "text": "first todo",
                    "ts": "2026-04-17T10:00:00Z",
                },
                {
                    "type": "decision",
                    "text": "ignore me",
                    "ts": "2026-04-17T10:30:00Z",
                },
            ],
        )
        _write_obs(
            obs_dir,
            "sess-b",
            [
                {
                    "type": "todo",
                    "text": "newest todo",
                    "ts": "2026-04-17T11:00:00Z",
                },
            ],
        )
        _write_obs(
            obs_dir,
            "sess-c",
            [
                {
                    "type": "todo",
                    "text": "unknown project",
                    "ts": "2026-04-17T09:00:00Z",
                },
            ],
        )

        rows = observation_queries.list_observation_entries(
            conn,
            observation_type="todo",
        )

        assert [row["session"] for row in rows] == ["sess-b", "sess-a", "sess-c"]
        assert rows[0]["project"] == "billing"
        assert rows[1]["project"] == "atlas"
        assert rows[2]["project"] == ""
        assert [row["text"] for row in rows] == [
            "newest todo",
            "first todo",
            "unknown project",
        ]

    def test_project_filter_reads_only_matching_session_files(self, conn, obs_dir: Path):
        _seed_session(conn, "alpha-1", "atlas")
        _seed_session(conn, "beta-1", "billing")
        _write_obs(
            obs_dir,
            "alpha-1",
            [{"type": "todo", "text": "atlas todo", "ts": "2026-04-17T10:00:00Z"}],
        )
        _write_obs(
            obs_dir,
            "beta-1",
            [{"type": "todo", "text": "billing todo", "ts": "2026-04-17T11:00:00Z"}],
        )
        _write_obs(
            obs_dir,
            "rogue",
            [{"type": "todo", "text": "rogue todo", "ts": "2026-04-17T12:00:00Z"}],
        )

        rows = observation_queries.list_observation_entries(
            conn,
            observation_type="todo",
            project="atl",
        )

        assert rows == [
            {
                "project": "atlas",
                "session": "alpha-1",
                "timestamp": "2026-04-17T10:00:00Z",
                "text": "atlas todo",
                "type": "todo",
            }
        ]


class TestFormatEntryJson:
    def test_emits_compact_json_with_required_fields(self):
        line = observation_queries.format_entry_json(
            {
                "project": "atlas",
                "session": "sess-1",
                "timestamp": "2026-04-17T10:00:00Z",
                "text": "Ship todos query",
                "type": "todo",
            }
        )

        assert line == (
            '{"project":"atlas","session":"sess-1",'
            '"timestamp":"2026-04-17T10:00:00Z","text":"Ship todos query"}'
        )
