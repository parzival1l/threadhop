"""Tests for startup cleanup of exported markdown files."""

from __future__ import annotations

import json
import os
from datetime import datetime, timedelta
from pathlib import Path
from types import SimpleNamespace


def _load_threadhop_module() -> SimpleNamespace:
    """Return a namespace with the symbols this suite touches.

    Phase 3 split the script into ``threadhop_core``: the export-cleanup
    helpers live in ``threadhop_core/cli/export_cleanup`` and the
    config loader in ``threadhop_core/config/loader``. Keeping a
    namespace facade lets the existing tests reach
    ``threadhop_mod.cleanup_export_markdown_files`` and
    ``threadhop_mod.load_config`` without rewriting every call site.
    """
    from threadhop_core.cli import export_cleanup
    from threadhop_core.config import loader
    from threadhop_core.storage import db

    return SimpleNamespace(
        cleanup_export_markdown_files=export_cleanup.cleanup_export_markdown_files,
        load_config=loader.load_config,
        # Mutating these on the loader module is what
        # ``test_load_config_*`` does via monkeypatch.setattr; expose
        # the loader module directly so the existing patches still work.
        CONFIG_FILE=loader.CONFIG_FILE,
        db=db,
        _loader=loader,
    )


def _touch_with_mtime(path: Path, when: datetime) -> None:
    path.write_text(path.name)
    ts = when.timestamp()
    os.utime(path, (ts, ts))


def test_cleanup_deletes_only_stale_safe_exports(tmp_path: Path):
    threadhop_mod = _load_threadhop_module()
    export_dir = tmp_path / "threadhop"
    export_dir.mkdir()

    now = datetime(2026, 4, 20, 12, 0, 0)
    stale = export_dir / "stale.md"
    stale_open = export_dir / "stale-open.md"
    retained = export_dir / "retained.md"
    recent = export_dir / "recent.md"
    ignored = export_dir / "ignored.txt"

    _touch_with_mtime(stale, now - timedelta(days=9))
    _touch_with_mtime(stale_open, now - timedelta(days=10))
    _touch_with_mtime(retained, now - timedelta(days=3))
    _touch_with_mtime(recent, now - timedelta(minutes=30))
    _touch_with_mtime(ignored, now - timedelta(days=30))

    result = threadhop_mod.cleanup_export_markdown_files(
        export_dir,
        retention_days=7,
        now=now,
        open_paths={stale_open},
    )

    assert result.deleted == 1
    assert result.scanned == 4
    assert result.skipped_open == 1
    assert result.skipped_recent == 1
    assert result.errors == 0
    assert not stale.exists()
    assert stale_open.exists()
    assert retained.exists()
    assert recent.exists()
    assert ignored.exists()


def test_cleanup_missing_directory_is_a_no_op(tmp_path: Path):
    threadhop_mod = _load_threadhop_module()
    export_dir = tmp_path / "missing-threadhop"

    result = threadhop_mod.cleanup_export_markdown_files(export_dir)

    assert result.directory_missing is True
    assert result.deleted == 0
    assert result.scanned == 0
    assert result.footer_message() is None


def test_load_config_defaults_export_retention_days(
    tmp_path: Path, monkeypatch
):
    threadhop_mod = _load_threadhop_module()
    config_path = tmp_path / "config.json"
    config_path.write_text(json.dumps({"theme": "textual-light"}))

    monkeypatch.setattr(threadhop_mod._loader, "CONFIG_FILE", config_path)
    monkeypatch.setattr(threadhop_mod.db, "get_custom_names", lambda conn: {})
    monkeypatch.setattr(threadhop_mod.db, "get_session_order", lambda conn: [])
    monkeypatch.setattr(threadhop_mod.db, "get_last_viewed", lambda conn: {})

    config = threadhop_mod.load_config(conn=None)

    assert config["theme"] == "textual-light"
    assert config["export_retention_days"] == 7


def test_load_config_coerces_export_retention_days_from_json(
    tmp_path: Path, monkeypatch
):
    threadhop_mod = _load_threadhop_module()
    config_path = tmp_path / "config.json"
    config_path.write_text(json.dumps({"export_retention_days": "14"}))

    monkeypatch.setattr(threadhop_mod._loader, "CONFIG_FILE", config_path)
    monkeypatch.setattr(threadhop_mod.db, "get_custom_names", lambda conn: {})
    monkeypatch.setattr(threadhop_mod.db, "get_session_order", lambda conn: [])
    monkeypatch.setattr(threadhop_mod.db, "get_last_viewed", lambda conn: {})

    config = threadhop_mod.load_config(conn=None)

    assert config["export_retention_days"] == 14
