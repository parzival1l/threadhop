"""CLI helper tests for ``threadhop config``."""

from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace

import pytest


@pytest.fixture
def threadhop_ns() -> dict:
    """Expose the config command + loader namespaces for monkeypatching.

    Phase 3 split the script into ``threadhop_core/cli/commands/config.py``
    plus ``threadhop_core/config/loader.py``. The handler reads
    ``CONFIG_FILE`` through the loader's globals, so ``_point_config_at_tmp``
    has to retarget both modules. The fixture exposes
    ``cmd_config`` directly so call sites stay unchanged.
    """
    from threadhop_core.cli.commands import config as config_cmd
    from threadhop_core.config import loader as loader_mod

    return {
        "cmd_config": config_cmd.cmd_config,
        "_loader": loader_mod,
        "_cmd_module": config_cmd,
    }


def _point_config_at_tmp(threadhop_ns: dict, tmp_path: Path) -> Path:
    config_dir = tmp_path / "threadhop-config"
    config_file = config_dir / "config.json"
    loader_mod = threadhop_ns["_loader"]
    cmd_mod = threadhop_ns["_cmd_module"]
    # Both modules cache the path constants — keep them in lockstep so
    # save_app_config (loader) and the get/set output (cmd) agree.
    loader_mod.CONFIG_DIR = config_dir
    loader_mod.CONFIG_FILE = config_file
    cmd_mod.CONFIG_FILE = config_file
    threadhop_ns["CONFIG_DIR"] = config_dir
    threadhop_ns["CONFIG_FILE"] = config_file
    return config_file


def test_get_observe_enabled_defaults_false(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    _point_config_at_tmp(threadhop_ns, tmp_path)

    rc = threadhop_ns["cmd_config"](
        SimpleNamespace(config_command="get", key="observe.enabled")
    )

    assert rc == 0
    assert capsys.readouterr().out.strip() == "false"


def test_set_observe_enabled_persists_bool(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    config_file = _point_config_at_tmp(threadhop_ns, tmp_path)

    rc = threadhop_ns["cmd_config"](
        SimpleNamespace(
            config_command="set",
            key="observe.enabled",
            value="true",
        )
    )

    assert rc == 0
    assert capsys.readouterr().out.strip() == "observe.enabled = true"
    assert json.loads(config_file.read_text()) == {"observe.enabled": True}


def test_set_observe_enabled_rejects_invalid_values(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    _point_config_at_tmp(threadhop_ns, tmp_path)

    rc = threadhop_ns["cmd_config"](
        SimpleNamespace(
            config_command="set",
            key="observe.enabled",
            value="maybe",
        )
    )

    assert rc == 2
    assert "observe.enabled expects true/false" in capsys.readouterr().err


def test_set_observe_enabled_preserves_existing_app_config(
    tmp_path: Path,
    capsys: pytest.CaptureFixture[str],
    threadhop_ns: dict,
):
    config_file = _point_config_at_tmp(threadhop_ns, tmp_path)
    config_file.parent.mkdir(parents=True, exist_ok=True)
    config_file.write_text(json.dumps({
        "theme": "textual-light",
        "sidebar_width": 42,
        "export_retention_days": 14,
    }))

    rc = threadhop_ns["cmd_config"](
        SimpleNamespace(
            config_command="set",
            key="observe.enabled",
            value="true",
        )
    )

    assert rc == 0
    assert capsys.readouterr().out.strip() == "observe.enabled = true"
    assert json.loads(config_file.read_text()) == {
        "theme": "textual-light",
        "sidebar_width": 42,
        "export_retention_days": 14,
        "observe.enabled": True,
    }
