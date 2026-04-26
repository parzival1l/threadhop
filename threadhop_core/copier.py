"""Copy a cleaned session transcript to the macOS clipboard.

Backs both ``threadhop copy [N|all]`` (CLI) and ``/threadhop:copy`` (plugin
command). The plugin file is a one-line bash passthrough into this CLI,
matching the existing ``/threadhop:tag`` / ``/threadhop:bookmark`` pattern.

Design anchors:

* **Filter-then-count.** The CLI arg counts *rendered* turns (user +
  assistant prose, sidechains dropped, tool calls/results stripped), not
  raw JSONL lines. Counting what the user sees in the TUI keeps the
  mental model consistent.
* **Clipboard-only.** No stdout echo of the copied markdown — the whole
  point of this command is context reduction for paste into another
  session, so printing would defeat the purpose. A one-line status
  ("Copied N turns (≈W words) to clipboard.") confirms success without
  leaking the payload back into the source conversation.
* **Cleaning runs deterministically in code.** The transform reuses
  ``indexer.parse_messages(include_tool_calls=False)`` — the same
  cleaning pipeline the TUI renders — so paste recipients and search
  indices see a canonical form. See issue #63 for promoting the knob
  to ``config.json``.

Module name: ``copier`` rather than ``copy`` on purpose — pytest and
any transitive consumer of ``copy.deepcopy`` preload stdlib ``copy``
into ``sys.modules``, so a project-level ``copy.py`` loses the
import race. Naming also matches the existing noun-form convention
(``indexer``, ``observer``, ``reflector``). The CLI subcommand is
still ``threadhop copy``; only the Python module differs.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from datetime import datetime
from pathlib import Path
from typing import Iterator

from . import indexer


# Claude Code surfaces several harness-tooling wrappers as plain-text
# content inside user JSONL turns: ``!cmd`` passthroughs wrap their input
# and captured stdout/stderr in ``<bash-input>`` / ``<bash-stdout>`` /
# ``<bash-stderr>``; each of those chunks is also prefixed with a
# ``<local-command-caveat>`` telling the assistant not to respond; and
# slash-command invocations emit ``<command-name>`` / ``<command-message>``
# / ``<command-args>``. None of this is conversation — it's plumbing the
# user saw as UI chrome, not as words they typed. Strip on the way out
# so a pasted transcript reads like a chat, not a shell transcript.
#
# Scoped to this module — the indexer, TUI, search, and observer keep
# seeing the unfiltered bytes so session exploration still shows "you
# ran `!threadhop tag` here" as context.
HARNESS_TAG_RE = re.compile(
    r"<(bash-input|bash-stdout|bash-stderr"
    r"|local-command-caveat"
    r"|command-name|command-message|command-args)>"
    r".*?"
    r"</\1>",
    re.DOTALL,
)

# Fallback dump location when ``pbcopy`` fails. Matches
# ``EXPORT_DIR`` in the ``threadhop`` script (threadhop:85) so
# copy-fallback files live alongside intentional TUI exports.
EXPORT_DIR = Path("/tmp/threadhop")


# --- Argument parsing ----------------------------------------------------


def parse_count_arg(raw: str | None) -> int | None:
    """Translate the raw CLI arg to a count.

    ``None`` or empty string → 1 (default: last one turn).
    ``"all"`` / ``"ALL"`` / ``"All"`` → ``None`` sentinel (entire session).
    Positive integer → int.
    Everything else raises ``ValueError`` with a usage hint.

    A count larger than the session's turn total is handled by
    ``build_copy_markdown`` (silently caps at what's available) rather
    than here, so the CLI error path stays narrow.
    """
    if raw is None or raw == "":
        return 1
    normalized = raw.strip().lower()
    if normalized == "all":
        return None
    try:
        n = int(normalized)
    except ValueError:
        raise ValueError(
            f"expected a positive integer or 'all', got {raw!r}"
        )
    if n < 1:
        raise ValueError(
            f"count must be >= 1 (got {n}); use 'all' for the whole session"
        )
    return n


# --- Rendering -----------------------------------------------------------


def _iter_rendered_turns(session_path: Path) -> Iterator[tuple[str, str]]:
    """Yield ``(role_label, text)`` pairs in file order.

    Pipeline:
    1. ``indexer.parse_messages(include_tool_calls=False)`` — merges
       streaming chunks by ``message.id``, strips system reminders,
       skips ``toolUseResult`` user lines, drops thinking blocks,
       *omits* ``tool_use`` blocks entirely (not abbreviated).
    2. Drop ``is_sidechain`` rows — sub-agent exploration isn't what the
       human-visible conversation is about.
    3. Drop rows that reduced to empty text after cleaning.
    """
    for row in indexer.parse_messages(session_path, include_tool_calls=False):
        if row.get("is_sidechain"):
            continue
        text = (row.get("text") or "").strip()
        # Strip `!cmd` passthrough wrappers. If a turn was *only* bash
        # tooling (e.g. a bash-stdout echo from a prior `!threadhop copy`
        # invocation), it collapses to empty here and the turn is dropped
        # entirely — which is what we want.
        text = HARNESS_TAG_RE.sub("", text).strip()
        if not text:
            continue
        label = "User" if row["role"] == "user" else "Assistant"
        yield label, text


def build_copy_markdown(
    session_path: Path,
    count: int | None,
) -> tuple[str, int]:
    """Return ``(markdown, turn_count)``.

    ``count=None`` → every rendered turn in the session.
    ``count=N`` → last ``N`` rendered turns; silently capped at the
    session total if ``N`` exceeds what's available.
    """
    turns = list(_iter_rendered_turns(session_path))
    if count is not None:
        turns = turns[-count:]
    md = "\n\n".join(f"### {label}\n{text}" for label, text in turns)
    return md, len(turns)


def _word_count(s: str) -> int:
    return len(s.split())


# --- Clipboard + fallback ------------------------------------------------


def _try_pbcopy(text: str) -> bool:
    """Pipe ``text`` into ``pbcopy``. Return ``True`` on success."""
    try:
        subprocess.run(["pbcopy"], input=text.encode(), check=True)
        return True
    except (subprocess.CalledProcessError, FileNotFoundError, OSError):
        return False


def _dump_to_file(markdown: str, session_id: str) -> Path:
    """Write ``markdown`` to ``EXPORT_DIR`` and return the path.

    Mirrors the TUI's ``e`` export fallback (threadhop:699-711): same
    directory, similar naming shape so a user cleaning up ``/tmp/threadhop``
    sees copy-fallback files next to intentional exports.
    """
    os.makedirs(EXPORT_DIR, exist_ok=True)
    ts = datetime.now().strftime("%Y%m%d-%H%M%S")
    path = EXPORT_DIR / f"{session_id}-copy-{ts}.md"
    path.write_text(markdown)
    return path


# --- Entry point ---------------------------------------------------------


def run_copy(
    count_arg: str | None,
    session_id: str,
    session_path: Path,
) -> int:
    """CLI entry. Caller resolves ``session_id``/``session_path`` first
    (via the ``threadhop`` script's ``_resolve_cli_session`` +
    ``find_session_path`` helpers) so session discovery isn't duplicated.
    """
    try:
        count = parse_count_arg(count_arg)
    except ValueError as e:
        print(f"threadhop copy: {e}", file=sys.stderr)
        return 2

    markdown, turn_count = build_copy_markdown(session_path, count)

    if turn_count == 0:
        print(
            "threadhop copy: no user/assistant turns to copy "
            "(session may be empty or contain only tool output).",
            file=sys.stderr,
        )
        return 1

    noun = "turn" if turn_count == 1 else "turns"
    words = _word_count(markdown)

    if _try_pbcopy(markdown):
        print(f"✓ copied {turn_count} {noun} (≈{words} words) to clipboard.")
        return 0

    # pbcopy path failed — fall back to the TUI export UX: dump to
    # EXPORT_DIR and copy *the path* so the user still has something
    # actionable on the clipboard.
    dump_path = _dump_to_file(markdown, session_id)
    path_copied = _try_pbcopy(str(dump_path))
    path_note = " (path copied)" if path_copied else ""
    print(
        f"⚠ pbcopy unavailable; dumped {turn_count} {noun} → {dump_path}{path_note}",
        file=sys.stderr,
    )
    return 0
