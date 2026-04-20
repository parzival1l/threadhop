"""Tests for startup cleanup of exported markdown files."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import json
import os
import sys
from datetime import datetime, timedelta
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
THREADHOP = ROOT / "threadhop"


def _load_threadhop_module():
    loader = importlib.machinery.SourceFileLoader(
        "threadhop_test_module", str(THREADHOP)
    )
    spec = importlib.util.spec_from_loader(loader.name, loader)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[loader.name] = module
    loader.exec_module(module)
    return module


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

    monkeypatch.setattr(threadhop_mod, "CONFIG_FILE", config_path)
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

    monkeypatch.setattr(threadhop_mod, "CONFIG_FILE", config_path)
    monkeypatch.setattr(threadhop_mod.db, "get_custom_names", lambda conn: {})
    monkeypatch.setattr(threadhop_mod.db, "get_session_order", lambda conn: [])
    monkeypatch.setattr(threadhop_mod.db, "get_last_viewed", lambda conn: {})

    config = threadhop_mod.load_config(conn=None)

    assert config["export_retention_days"] == 14
