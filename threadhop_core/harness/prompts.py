"""Prompt template loader.

All bundled prompts live in ``<repo_root>/prompts/*.md``. This module is
the single point of resolution; previously each caller computed
``Path(__file__).resolve().parents[N] / "prompts" / ...`` with ``N``
depending on its own depth in the tree, which broke during the Phase 1
reorganization (observer/reflector use ``parents[2]``, handoff used
``parents[1]``).
"""
from __future__ import annotations

from functools import lru_cache
from pathlib import Path

# threadhop_core/harness/prompts.py
#   parents[0] = harness
#   parents[1] = threadhop_core
#   parents[2] = repo root
PROMPTS_DIR = Path(__file__).resolve().parents[2] / "prompts"


@lru_cache(maxsize=None)
def load_prompt(name: str) -> str:
    """Load ``prompts/<name>.md`` and return its text. Cached after first read."""
    path = PROMPTS_DIR / f"{name}.md"
    return path.read_text(encoding="utf-8")


def prompt_path(name: str) -> Path:
    """Return the on-disk :class:`Path` to ``prompts/<name>.md``.

    Useful for callers that still accept a ``prompt_path`` override
    parameter (observer / reflector / handoff all do — tests inject a
    custom prompt for some scenarios). When no override is given they
    fall back to this default.
    """
    return PROMPTS_DIR / f"{name}.md"
