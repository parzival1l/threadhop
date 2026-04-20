"""CLI helper tests for ``threadhop config``."""

from __future__ import annotations

import json
import runpy
import sys
import types
from pathlib import Path
from types import SimpleNamespace

import pytest


ROOT = Path(__file__).resolve().parent.parent
THREADHOP = ROOT / "threadhop"


@pytest.fixture
def threadhop_ns() -> dict:
    """Load the CLI script as a module namespace without executing main()."""
    class _DummyMeta(type):
        def __getattr__(cls, _name):
            return cls

    class _Dummy(metaclass=_DummyMeta):
        def __init__(self, *args, **kwargs) -> None:
            pass

        @classmethod
        def __class_getitem__(cls, _item):
            return cls

    rich_mod = types.ModuleType("rich")
    rich_console = types.ModuleType("rich.console")
    rich_console.Group = _Dummy
    rich_markdown = types.ModuleType("rich.markdown")
    rich_markdown.Markdown = _Dummy
    rich_markup = types.ModuleType("rich.markup")
    rich_markup.escape = lambda value: value
    rich_text = types.ModuleType("rich.text")
    rich_text.Text = _Dummy

    textual_mod = types.ModuleType("textual")
    textual_app = types.ModuleType("textual.app")
    textual_app.App = _Dummy
    textual_app.ComposeResult = _Dummy
    textual_binding = types.ModuleType("textual.binding")
    textual_binding.Binding = _Dummy
    textual_containers = types.ModuleType("textual.containers")
    textual_containers.Horizontal = _Dummy
    textual_containers.Vertical = _Dummy
    textual_containers.VerticalScroll = _Dummy
    textual_screen = types.ModuleType("textual.screen")
    textual_screen.ModalScreen = _Dummy
    textual_widgets = types.ModuleType("textual.widgets")
    textual_widgets.Header = _Dummy
    textual_widgets.Input = _Dummy
    textual_widgets.ListItem = _Dummy
    textual_widgets.ListView = _Dummy
    textual_widgets.Static = _Dummy
    textual_widgets.TextArea = _Dummy
    textual_worker = types.ModuleType("textual.worker")
    textual_worker.Worker = _Dummy

    sys.modules.setdefault("rich", rich_mod)
    sys.modules["rich.console"] = rich_console
    sys.modules["rich.markdown"] = rich_markdown
    sys.modules["rich.markup"] = rich_markup
    sys.modules["rich.text"] = rich_text
    sys.modules.setdefault("textual", textual_mod)
    sys.modules["textual.app"] = textual_app
    sys.modules["textual.binding"] = textual_binding
    sys.modules["textual.containers"] = textual_containers
    sys.modules["textual.screen"] = textual_screen
    sys.modules["textual.widgets"] = textual_widgets
    sys.modules["textual.worker"] = textual_worker
    return runpy.run_path(str(THREADHOP))


def _point_config_at_tmp(threadhop_ns: dict, tmp_path: Path) -> Path:
    config_dir = tmp_path / "threadhop-config"
    config_file = config_dir / "config.json"
    globals_dict = threadhop_ns["cmd_config"].__globals__
    globals_dict["CONFIG_DIR"] = config_dir
    globals_dict["CONFIG_FILE"] = config_file
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
