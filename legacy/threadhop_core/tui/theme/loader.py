"""Convert OpenCode JSON theme files into ``textual.theme.Theme`` objects.

OpenCode themes follow the schema documented at
https://opencode.ai/theme.json — a ``defs`` table of named hex colors
plus a ``theme`` table mapping semantic roles to those defs. Every
theme exposes both a ``dark`` and ``light`` variant, so each JSON file
yields two Textual themes.

We expose a small set of extra CSS variables (``$step1``..``$step12``,
``$text-muted``, ``$border-subtle``, ``$border-active``, ``$success``,
``$warning``, ``$error``) so the app's stylesheet can reach for the
12-step scale directly without re-deriving it from the resolved
primary/secondary/accent triple.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Iterable

from textual.theme import Theme


OPENCODE_THEMES_DIR = Path(__file__).parent / "vendored"


# Roles we forward to Textual's Theme constructor. Keys are OpenCode
# theme-table names; values are Textual Theme constructor kwargs.
_TEXTUAL_ROLES: dict[str, str] = {
    "primary":         "primary",
    "secondary":       "secondary",
    "accent":          "accent",
    "background":      "background",
    "backgroundPanel": "panel",
    "backgroundElement": "surface",
    "text":            "foreground",
    "success":         "success",
    "warning":         "warning",
    "error":           "error",
}

# Roles we expose only as CSS variables ($name) — Textual derives some of
# these itself, but OpenCode-defined values are typically more deliberate
# than Textual's auto-derivation, so we override.
_EXTRA_VARIABLES: dict[str, str] = {
    "textMuted":     "text-muted",
    "border":        "border-color",
    "borderActive":  "border-active",
    "borderSubtle":  "border-subtle",
    "info":          "info",
    "diffAdded":     "diff-added",
    "diffRemoved":   "diff-removed",
    "diffAddedBg":   "diff-added-bg",
    "diffRemovedBg": "diff-removed-bg",
    "syntaxKeyword":  "syntax-keyword",
    "syntaxFunction": "syntax-function",
    "syntaxString":   "syntax-string",
    "syntaxComment":  "syntax-comment",
    "syntaxType":     "syntax-type",
    "syntaxNumber":   "syntax-number",
    "syntaxVariable": "syntax-variable",
}


def _resolve(value: str, defs: dict[str, str]) -> str:
    """Resolve a theme-table value: either a literal hex or a defs key."""
    if value.startswith("#"):
        return value
    return defs.get(value, value)


def _build(name: str, raw: dict, *, dark: bool) -> Theme:
    defs = raw["defs"]
    table = raw["theme"]
    variant = "dark" if dark else "light"

    def get(role: str) -> str | None:
        slot = table.get(role)
        if slot is None:
            return None
        return _resolve(slot[variant], defs)

    # Step1..Step12 follow the convention darkStep1, darkStep2, ...
    step_prefix = "dark" if dark else "light"
    variables: dict[str, str] = {}
    for i in range(1, 13):
        key = f"{step_prefix}Step{i}"
        if key in defs:
            variables[f"step{i}"] = defs[key]

    for opencode_name, css_name in _EXTRA_VARIABLES.items():
        value = get(opencode_name)
        if value is not None:
            variables[css_name] = value

    kwargs = {"name": name, "dark": dark, "variables": variables}
    for opencode_name, textual_name in _TEXTUAL_ROLES.items():
        value = get(opencode_name)
        if value is not None:
            kwargs[textual_name] = value

    # Theme requires ``primary`` to be non-None. OpenCode's schema always
    # provides one; if it's absent we'd rather raise than ship a broken
    # theme silently.
    if "primary" not in kwargs:
        raise ValueError(f"theme {name!r} has no primary color")

    return Theme(**kwargs)


def load_opencode_themes(
    directory: Path | None = None,
) -> Iterable[Theme]:
    """Yield Textual ``Theme`` objects for every JSON in ``directory``.

    Each file produces two themes: ``<name>-dark`` and ``<name>-light``.
    """
    directory = directory or OPENCODE_THEMES_DIR
    for path in sorted(directory.glob("*.json")):
        raw = json.loads(path.read_text())
        if "theme" not in raw or "defs" not in raw:
            continue
        stem = path.stem
        yield _build(f"{stem}-dark", raw, dark=True)
        yield _build(f"{stem}-light", raw, dark=False)
