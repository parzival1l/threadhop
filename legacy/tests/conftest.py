"""Pytest configuration and shared fixtures for ThreadHop tests.

Makes the project root importable so tests can `import db` without packaging,
and provides fixtures for temp DB / config / project-tree isolation so no test
ever touches ~/.config/threadhop or ~/.claude/projects.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

# --- Import path -----------------------------------------------------------
_ROOT = Path(__file__).resolve().parent.parent
if str(_ROOT) not in sys.path:
    sys.path.insert(0, str(_ROOT))


# --- Fixtures --------------------------------------------------------------

@pytest.fixture
def config_path(tmp_path: Path) -> Path:
    """Path to a temp config.json (not yet created). Each test owns its own."""
    return tmp_path / "config.json"


@pytest.fixture
def projects_dir(tmp_path: Path) -> Path:
    """Empty temp directory standing in for ~/.claude/projects."""
    d = tmp_path / "projects"
    d.mkdir()
    return d


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    """Path to a temp sessions.db file (not yet created)."""
    return tmp_path / "sessions.db"


@pytest.fixture
def conn(db_path: Path):
    """Initialized DB connection pointed at a temp DB, schema applied."""
    from threadhop_core.storage import db as db_mod

    c = db_mod.init_db(db_path)
    try:
        yield c
    finally:
        c.close()


@pytest.fixture
def write_config(config_path: Path):
    """Helper: write a dict as JSON to config_path, return the raw text."""
    def _write(data: dict) -> str:
        raw = json.dumps(data)
        config_path.write_text(raw)
        return raw
    return _write
