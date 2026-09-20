"""Recent-search history helpers.

Persisted in ``~/.config/threadhop/config.json`` as the ``recent_searches``
list. The TUI's search panel uses these to surface recent queries; the
helpers dedupe, trim, and clear in place. Persistence is delegated back
to the config loader so all writes funnel through the same JSON-write
path (atomic-ish, swallows OSError).

``save_app_config`` is imported at module load (rather than the lazy
import the original script used) so tests can monkeypatch it on this
module — patching through the loader module would miss because each
call would re-resolve the global.
"""

from __future__ import annotations

from ..config.loader import save_app_config

MAX_RECENT_SEARCHES = 8


def get_recent_searches(config: dict | None) -> list[str]:
    """Return deduped recent searches from app config."""
    if not isinstance(config, dict):
        return []
    raw = config.get("recent_searches", [])
    if not isinstance(raw, list):
        return []

    seen: set[str] = set()
    cleaned: list[str] = []
    for item in raw:
        if not isinstance(item, str):
            continue
        query = item.strip()
        if not query or query in seen:
            continue
        seen.add(query)
        cleaned.append(query)
    return cleaned


def save_recent_search(config: dict | None, raw_query: str) -> list[str]:
    """Persist one recent search at the front of config.json."""
    if not isinstance(config, dict):
        return []
    query = raw_query.strip()
    if not query:
        return get_recent_searches(config)

    recent = [q for q in get_recent_searches(config) if q != query]
    recent.insert(0, query)
    config["recent_searches"] = recent[:MAX_RECENT_SEARCHES]
    save_app_config(config)
    return list(config["recent_searches"])


def clear_recent_searches(config: dict | None) -> None:
    """Remove persisted recent searches from config.json."""
    if not isinstance(config, dict):
        return
    config["recent_searches"] = []
    save_app_config(config)

