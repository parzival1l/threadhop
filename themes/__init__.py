"""Themes vendored from anomalyco/opencode (MIT) and adapted to Textual.

See ``themes/vendored/README.md`` for attribution. The OpenCode JSON
schema follows a Radix-style 12-step scale plus semantic accents; the
loader maps that into Textual's ``Theme`` plus a ``variables`` overlay
so app CSS can reach for ``$step6``, ``$text-muted``, ``$opencode-border``,
etc. directly.
"""

from .loader import load_opencode_themes, OPENCODE_THEMES_DIR

__all__ = ["load_opencode_themes", "OPENCODE_THEMES_DIR"]
