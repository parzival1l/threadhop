"""Themes vendored from anomalyco/opencode (MIT) and adapted to Textual.

See ``themes/vendored/README.md`` for attribution. The OpenCode JSON
schema follows a Radix-style 12-step scale plus semantic accents; the
loader maps that into Textual's ``Theme`` plus a ``variables`` overlay
so app CSS can reach for ``$step6``, ``$text-muted``, ``$opencode-border``,
etc. directly.
"""

from .loader import load_opencode_themes, OPENCODE_THEMES_DIR


def get_available_themes() -> list[str]:
    """Return Textual's built-in theme names.

    Falls back to the dark/light pair when ``BUILTIN_THEMES`` is absent
    on older Textual versions.
    """
    try:
        from textual.theme import BUILTIN_THEMES  # noqa: PLC0415

        return list(BUILTIN_THEMES.keys())
    except Exception:  # pragma: no cover — defensive against API drift
        return ["textual-dark", "textual-light"]


__all__ = [
    "load_opencode_themes",
    "OPENCODE_THEMES_DIR",
    "get_available_themes",
]
