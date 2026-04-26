"""App-level configuration loader.

Owns the ``~/.config/threadhop/config.json`` read/write surface plus the
helpers the ``threadhop config`` CLI uses to coerce raw string values
into typed config entries. Session-level state (custom names, ordering,
last-viewed) lives in SQLite per ADR-001 and is merged into the returned
config dict purely for backwards-compatibility with the TUI's read paths.
"""

from __future__ import annotations

import json
from pathlib import Path

from ..cli.export_cleanup import EXPORT_RETENTION_DAYS_DEFAULT

CONFIG_DIR = Path.home() / ".config" / "threadhop"
CONFIG_FILE = CONFIG_DIR / "config.json"

# App-level keys that live in config.json post-migration. Session-level
# state (session_names, session_order, last_viewed) lives in SQLite per
# ADR-001. Anything unknown is preserved on save so future settings
# (sidebar_width, export_retention_days, observe.enabled, user-added keys)
# don't get silently dropped.
APP_CONFIG_KEYS = {
    "theme",
    "sidebar_width",
    "export_retention_days",
    "observe.enabled",
}

# Subset of keys writable by the ``threadhop config set`` CLI surface.
CLI_CONFIG_KEYS = {"observe.enabled"}


def _coerce_retention_days(value) -> int:
    """Return a non-negative retention window in days."""
    try:
        return max(0, int(value))
    except (TypeError, ValueError):
        return EXPORT_RETENTION_DAYS_DEFAULT


def load_config(conn):
    """Return the combined in-memory config.

    App-level keys (`theme`, `sidebar_width`, `export_retention_days`,
    `observe.enabled`) come from config.json.
    Session-level keys (`session_names`, `session_order`, `last_viewed`)
    come from the SQLite `sessions` table — this is the ADR-001 split.

    The returned dict keeps the same shape the TUI has always read so
    the read paths (SessionItem construction, unread detection, reorder
    logic) stay unchanged; only the persistence layer moved.
    """
    # 1. App-level from JSON.
    config: dict = {}
    try:
        if CONFIG_FILE.exists():
            raw = json.loads(CONFIG_FILE.read_text())
            if isinstance(raw, dict):
                config.update(raw)
    except (OSError, json.JSONDecodeError):
        pass

    # Strip any legacy session-level keys that may linger (old config,
    # partially-migrated state). SQLite is the source of truth now.
    for key in ("session_names", "session_order", "last_viewed"):
        config.pop(key, None)

    config.setdefault("theme", "textual-dark")
    config["export_retention_days"] = _coerce_retention_days(
        config.get("export_retention_days", EXPORT_RETENTION_DAYS_DEFAULT)
    )
    config.setdefault("observe.enabled", False)

    # 2. Session-level from SQLite.
    try:
        # Local import to avoid a config <-> storage cycle at module import.
        from ..storage import db  # noqa: PLC0415
        config["session_names"] = db.get_custom_names(conn)
        config["session_order"] = db.get_session_order(conn)
        config["last_viewed"] = db.get_last_viewed(conn)
    except Exception:
        # A broken DB shouldn't prevent the app from starting; fall back
        # to empty session state and let the user keep browsing.
        config["session_names"] = {}
        config["session_order"] = []
        config["last_viewed"] = {}

    return config


def save_app_config(config) -> bool:
    """Persist only app-level keys (theme, sidebar_width,
    export_retention_days, observe.enabled, unknown extras) to config.json.

    Session-level keys are deliberately excluded — they live in SQLite
    now and are written through at each mutation site (rename, reorder,
    view) rather than dumped wholesale here.

    Returns ``True`` when the write succeeded, ``False`` on ``OSError``.
    Existing call sites that do not check the return value keep their
    legacy best-effort behaviour.
    """
    try:
        CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        app_only = {
            k: v for k, v in config.items()
            if k not in ("session_names", "session_order", "last_viewed")
        }
        CONFIG_FILE.write_text(json.dumps(app_only, indent=2))
        return True
    except OSError:
        return False


def _load_app_config_file() -> dict:
    """Return the raw config.json object, or {} if unreadable/missing."""
    try:
        if CONFIG_FILE.exists():
            raw = json.loads(CONFIG_FILE.read_text())
            if isinstance(raw, dict):
                return raw
    except (OSError, json.JSONDecodeError):
        pass
    return {}


def _parse_boolish(value: str) -> bool | None:
    """Parse common CLI spellings for a boolean config value."""
    lowered = value.strip().lower()
    if lowered in {"1", "true", "yes", "on"}:
        return True
    if lowered in {"0", "false", "no", "off"}:
        return False
    return None


def _config_value_to_text(value) -> str:
    """Render a config value back to simple CLI output."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if value is None:
        return "null"
    if isinstance(value, (int, float, str)):
        return str(value)
    return json.dumps(value)


def _coerce_config_value(key: str, raw_value: str):
    """Validate and normalize supported config values."""
    if key == "observe.enabled":
        parsed = _parse_boolish(raw_value)
        if parsed is None:
            raise ValueError(
                "observe.enabled expects true/false (also accepts 1/0, on/off, yes/no)."
            )
        return parsed
    raise ValueError(f"Unsupported config key: {key}")
