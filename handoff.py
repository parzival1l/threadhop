"""Handoff brief builder — task #26, ADR-012 / ADR-016 / ADR-018 / ADR-020.

``/threadhop:handoff <session_id> [--full]`` produces a compressed brief
that lets a new Claude Code session pick up where an older one left off.

Per ADR-018, the observer is the core function — there is no separate
"compress raw JSONL" path. This module orchestrates:

  1. Run ``observer.observe_session`` on the target session. The observer
     itself decides whether to read from byte 0 (first run) or resume
     from the recorded ``source_byte_offset`` (catch-up on new bytes).
     We drop its batch threshold to 1 — for handoff we want whatever is
     extractable, even from a short tail.
  2. Read the per-session observation file
     (``~/.config/threadhop/observations/<session_id>.jsonl``). Per
     ADR-020 this file is unified: observer entries AND reflector
     ``type: "conflict"`` entries live together.
  3. Format the entries:
       * **direct** (short sets, no ``--full``) — pure Python markdown
         grouped by type. No LLM call, deterministic.
       * **polish** (large sets, no ``--full``) — Haiku sub-agent via
         ``claude -p`` compresses/cleans the direct form.
       * **full** (``--full``) — Haiku sub-agent plus the cleaned
         transcript, emitting a comprehensive handoff with rationale
         and verbatim excerpts.

On any polish failure (missing prompt, timeout, non-zero exit) we fall
back to the direct formatter so the caller still gets something useful.

Public API::

    build_handoff(conn, session_id, *, full=False, ...) -> dict
"""

from __future__ import annotations

import json
import shutil
import sqlite3
import subprocess
from pathlib import Path
from typing import Any, Callable

import db
import indexer
import observer
import reflector


# --- Configuration --------------------------------------------------------

# Above this observation count we prefer the Haiku polish pass even
# without ``--full``. The direct formatter is fine for ~30-50 line briefs,
# but once a session accumulates dozens of entries the output needs
# compression, deduplication, and ordering judgement a template can't do.
LARGE_SET_THRESHOLD = 40

# Polish subprocess timeout. Longer than the observer's default because
# ``--full`` passes the full cleaned transcript alongside observations,
# which inflates the prompt considerably.
DEFAULT_POLISH_TIMEOUT_SEC = 240.0

# Observer batch threshold when invoked via handoff. The normal default
# (3) is designed for watch mode where skipping "2 turn" chunks avoids
# burning Haiku calls. Handoff is the user's last chance — run the
# observer even on a short tail, but still return "up_to_date" when
# nothing at all has been appended (turns == 0 path).
HANDOFF_BATCH_THRESHOLD = 1

# Location of the polish prompt. Bundled with the app alongside
# ``prompts/observer.md`` and ``prompts/reflector.md``.
POLISH_PROMPT_PATH = Path(__file__).resolve().parent / "prompts" / "handoff.md"

# Observation types in display order. Mirrors observer.py's ``_TYPE_ORDER``
# but reorders for a reader: decisions/ADRs first (the "what was chosen"),
# then TODOs (what's left), then done (context), then observations, then
# conflicts (warnings, intentionally last so they don't drown the brief).
_TYPE_ORDER = ("decision", "adr", "todo", "done", "observation", "conflict")

_TYPE_LABELS = {
    "decision": "Decisions",
    "adr": "ADRs",
    "todo": "Open TODOs",
    "done": "Completed",
    "observation": "Observations",
    "conflict": "Conflicts",
}


# --- Helpers --------------------------------------------------------------


def _read_observations(obs_path: Path) -> list[dict]:
    """Load all parseable JSONL entries from the observation file.

    Malformed lines are silently skipped — the file is append-only
    (ADR-020) so a half-written tail line on the first observer run
    shouldn't abort the whole read.
    """
    if not obs_path or not obs_path.is_file():
        return []
    entries: list[dict] = []
    with open(obs_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                entries.append(json.loads(line))
            except (json.JSONDecodeError, ValueError):
                continue
    return entries


def _group_by_type(entries: list[dict]) -> dict[str, list[dict]]:
    """Bucket entries by their ``type`` field. Unknown types fall into
    ``observation`` so nothing is silently dropped in the direct format.
    """
    groups: dict[str, list[dict]] = {}
    for e in entries:
        t = e.get("type") or "observation"
        if not isinstance(t, str):
            t = "observation"
        groups.setdefault(t, []).append(e)
    return groups


def _format_brief_direct(
    session_id: str,
    session_meta: dict | None,
    entries: list[dict],
) -> str:
    """Deterministic markdown brief — used for short sets with no ``--full``.

    The shape intentionally mirrors what the polish prompt is asked to
    produce so consumers of the brief don't have to care which path ran.
    """
    groups = _group_by_type(entries)
    lines: list[str] = []

    title = f"# Handoff — session {session_id[:12]}"
    project = (session_meta or {}).get("project")
    if project:
        title += f" · project `{project}`"
    lines.append(title)
    lines.append("")
    lines.append(
        f"Based on {len(entries)} observation"
        f"{'' if len(entries) == 1 else 's'} "
        "extracted by the ThreadHop observer."
    )
    lines.append("")

    for t in _TYPE_ORDER:
        items = groups.get(t, [])
        if not items:
            continue
        lines.append(f"## {_TYPE_LABELS[t]}")
        for e in items:
            lines.append(_bullet_for(t, e))
        lines.append("")

    # Defensive: catch any types the observer emitted that we don't know
    # about yet. Better to surface them than hide them.
    for t, items in groups.items():
        if t in _TYPE_ORDER or not items:
            continue
        lines.append(f"## {t.title()}")
        for e in items:
            lines.append(_bullet_for(t, e))
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def _bullet_for(type_name: str, entry: dict) -> str:
    """One bullet line for a single observation entry.

    Conflict entries get their linked session refs appended so the brief
    isn't just "there is a conflict" — the reader can jump to the other
    session if they want more detail.
    """
    text = (entry.get("text") or "").strip()
    context = (entry.get("context") or "").strip()
    bullet = f"- {text}" if text else "- (empty)"
    if context:
        bullet += f"  _({context})_"
    if type_name == "conflict":
        refs = entry.get("refs") or []
        if isinstance(refs, list) and refs:
            short = ", ".join(str(r)[:8] for r in refs)
            bullet += f"  — refs: {short}"
    return bullet


def _invoke_polish(
    prompt_template: str,
    observations_jsonl: str,
    transcript: str | None,
    mode: str,
    *,
    claude_bin: str = "claude",
    timeout: float = DEFAULT_POLISH_TIMEOUT_SEC,
) -> tuple[str, dict[str, Any]]:
    """Shell out to ``claude -p --model haiku`` to produce the polished brief.

    ``mode`` is ``"polish"`` (observations-only compression) or ``"full"``
    (observations + transcript, comprehensive output). The transcript is
    only passed through for ``"full"``; ``"polish"`` deliberately leaves
    the source transcript out so the sub-agent can't paraphrase beyond
    what the observer already extracted.

    Returns ``(brief_text, meta)``. ``brief_text`` is empty on failure;
    ``meta`` carries ``error`` (and possibly ``stderr``) in that case so
    the caller can message the user and fall back to direct formatting.
    """
    prompt_parts = [
        prompt_template.rstrip(),
        "",
        "---",
        "",
        f"## Mode\n\n{mode}",
        "",
        "## Observations",
        "",
        "<observations>",
        observations_jsonl.rstrip(),
        "</observations>",
    ]
    if transcript is not None and mode == "full":
        prompt_parts += [
            "",
            "## Cleaned transcript",
            "",
            "<transcript>",
            transcript.rstrip(),
            "</transcript>",
        ]
    prompt = "\n".join(prompt_parts)

    if shutil.which(claude_bin) is None and not Path(claude_bin).exists():
        return "", {"error": f"{claude_bin} not found on PATH"}

    try:
        proc = subprocess.run(
            [
                claude_bin, "-p", prompt,
                "--model", "haiku",
                "--permission-mode", "acceptEdits",
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return "", {"error": f"polish timed out after {timeout}s"}
    except OSError as e:
        return "", {"error": f"polish could not start: {e}"}

    if proc.returncode != 0:
        return "", {
            "error": f"polish exited {proc.returncode}",
            "stderr": (proc.stderr or "").strip(),
        }

    out = (proc.stdout or "").strip()
    if not out:
        return "", {"error": "polish returned empty output"}
    return out + "\n", {}


def _resolve_source_path(
    conn: sqlite3.Connection,
    session_id: str,
    explicit: str | Path | None,
) -> Path | None:
    """Same precedence as the observer's resolver: explicit → state → sessions.

    Used for the ``--full`` transcript read, which happens *after* the
    observer has already run, so any of the three levels may have been
    populated. Returns ``None`` if no path is known — callers skip the
    transcript step and the sub-agent works from observations alone.
    """
    if explicit is not None:
        return Path(explicit)
    state = db.get_observation_state(conn, session_id)
    if state is not None and state["source_path"]:
        return Path(state["source_path"])
    sess = db.get_session(conn, session_id)
    if sess is not None and sess.get("session_path"):
        return Path(sess["session_path"])
    return None


def _rendered_transcript(
    conn: sqlite3.Connection,
    session_id: str,
    source_path: str | Path | None,
) -> str | None:
    """Render the full source JSONL through the same pipeline the observer
    and FTS index use (indexer.parse_byte_range + observer._format_transcript).

    Guarantees the ``--full`` sub-agent reasons about the same view of the
    conversation the user sees in the TUI. Returns ``None`` if the source
    can't be located or read — the caller degrades to observations-only
    polish in that case.
    """
    resolved = _resolve_source_path(conn, session_id, source_path)
    if resolved is None or not resolved.exists():
        return None
    try:
        raw = resolved.read_bytes()
    except OSError:
        return None
    turns = indexer.parse_byte_range(raw, fallback_session_id=session_id)
    if not turns:
        return None
    return observer._format_transcript(turns)


# --- Main entry point -----------------------------------------------------


def build_handoff(
    conn: sqlite3.Connection,
    session_id: str,
    *,
    source_path: str | Path | None = None,
    full: bool = False,
    large_set_threshold: int = LARGE_SET_THRESHOLD,
    claude_bin: str = "claude",
    timeout: float = DEFAULT_POLISH_TIMEOUT_SEC,
    prompt_path: Path | None = None,
    observer_prompt_path: Path | None = None,
    observer_batch_threshold: int = HANDOFF_BATCH_THRESHOLD,
    observer_timeout: float = observer.DEFAULT_TIMEOUT_SEC,
    reflect_fn: Callable[..., dict[str, Any]] | None | str = "default",
    reflector_prompt_path: Path | None = None,
    reflector_timeout: float = reflector.DEFAULT_TIMEOUT_SEC,
) -> dict[str, Any]:
    """Produce a handoff brief for ``session_id``.

    Orchestration — per ADR-018 the observer is the only path to
    observations, so this always runs it first (which itself no-ops when
    the cursor is at EOF and there's nothing new to extract).

    Args:
        conn: Open DB connection with the schema applied.
        session_id: Claude Code session UUID to hand off.
        source_path: Optional source JSONL override. Normally resolved
            from ``observation_state`` / ``sessions`` rows by the observer.
        full: If True, use the Haiku sub-agent in "full" mode — produces
            a comprehensive handoff with rationale and transcript
            excerpts. Always invokes Haiku regardless of set size.
        large_set_threshold: If ``len(entries) > threshold`` and ``full``
            is False, the "polish" sub-agent path is used to compress the
            brief. Below this threshold, direct templating is used.
        claude_bin: ``claude`` binary name or path. Forwarded to both the
            observer and the polish call — tests swap in a shell shim.
        timeout: Polish subprocess timeout (seconds).
        prompt_path: Override the handoff polish prompt location.
        observer_prompt_path: Override the observer prompt location
            (forwarded to ``observe_session``).
        observer_batch_threshold: Threshold passed to ``observe_session``.
            Defaults to 1 — handoff wants maximum coverage.
        observer_timeout: Timeout passed to ``observe_session``.
        reflect_fn: Callable invoked after the observer returns, with
            signature ``(conn, session_id, *, claude_bin, timeout,
            prompt_path) -> dict``. Defaults to
            ``reflector.reflect_session`` per ADR-022 ("reflector runs as
            a follow-up step" for on-demand observer invocations). Pass
            ``None`` to skip the reflector pass entirely (debug / tests).
            Any exception raised is caught and logged into the returned
            dict's ``reflector_error`` key — a reflector failure never
            breaks the handoff.
        reflector_prompt_path: Override the reflector prompt location
            (forwarded to ``reflect_fn`` as ``prompt_path``).
        reflector_timeout: Timeout passed to ``reflect_fn``.

    Returns:
        Dict::

            {
              "status": "ok" | "no_source" | "no_observations" | "failed",
              "brief":  <str>,          # markdown brief (empty on error)
              "mode":   "direct" | "polish" | "full",
              "entry_count": <int>,     # observations used
              "obs_path": <str>,
              "observer_result": <dict | None>,  # observe_session return
              "message": <str>,         # human-readable status line
            }

        Never raises on expected failures — callers branch on ``status``
        and can still display ``brief`` on ``ok`` regardless of ``mode``.
    """
    prompt_path = prompt_path or POLISH_PROMPT_PATH

    # Resolve ``reflect_fn`` late so tests / callers can monkeypatch
    # ``reflector.reflect_session`` after module import and still affect
    # the default. Passing ``reflect_fn=None`` explicitly still disables
    # the reflector; passing any callable overrides it.
    if reflect_fn == "default":
        reflect_fn = reflector.reflect_session

    # --- Step 1: run the observer (catch-up or first-time extraction). ----
    observer_result = observer.observe_session(
        conn, session_id,
        source_path=source_path,
        batch_threshold=observer_batch_threshold,
        claude_bin=claude_bin,
        timeout=observer_timeout,
        prompt_path=observer_prompt_path,
    )

    obs_status = observer_result.get("status")

    # ``no_source`` is the only condition that stops the handoff cold —
    # with no transcript AND no prior observations there is literally
    # nothing to hand off. All other observer failures (failed, timeout)
    # are soft: any existing observations are still readable.
    if obs_status == "no_source":
        state = db.get_observation_state(conn, session_id)
        if state is None or int(state.get("entry_count") or 0) == 0:
            return {
                "status": "no_source",
                "brief": "",
                "mode": "direct",
                "entry_count": 0,
                "obs_path": observer_result.get("obs_path", ""),
                "observer_result": observer_result,
                "message": observer_result.get(
                    "message", "No source JSONL for this session."
                ),
            }

    # --- Step 2: run the reflector (ADR-022 on-demand follow-up). --------
    # Per ADR-022: "When the observer core function runs on-demand (e.g.,
    # threadhop handoff or threadhop conflicts), the reflector runs as a
    # follow-up step." The reflector compares new decisions from this
    # session against peers in the same project, and appends
    # ``type: "conflict"`` entries to the same observation JSONL — so
    # those conflicts show up in the brief we render below. The reflector
    # gates itself (no_decisions / no_project / up_to_date) so calling it
    # unconditionally here is safe and cheap.
    reflector_result: dict[str, Any] | None = None
    reflector_error: str | None = None
    if reflect_fn is not None and obs_status != "no_source":
        try:
            reflector_result = reflect_fn(
                conn, session_id,
                claude_bin=claude_bin,
                timeout=reflector_timeout,
                prompt_path=reflector_prompt_path,
            )
        except Exception as e:
            # A reflector failure MUST NOT break the handoff — the
            # observer's output is already on disk and usable. Record
            # the error and continue to the format step.
            reflector_error = f"{type(e).__name__}: {e}"

    # --- Step 3: locate observation file and read entries. ----------------
    obs_path_str = observer_result.get("obs_path") or ""
    if not obs_path_str:
        state = db.get_observation_state(conn, session_id)
        if state is not None:
            obs_path_str = state["obs_path"]
    obs_path = Path(obs_path_str) if obs_path_str else None
    entries = _read_observations(obs_path) if obs_path else []

    if not entries:
        # Observer ran but extracted nothing, and no prior observations
        # exist. Common for short or routine sessions.
        return _attach_reflector_info({
            "status": "no_observations",
            "brief": "",
            "mode": "direct",
            "entry_count": 0,
            "obs_path": str(obs_path) if obs_path else "",
            "observer_result": observer_result,
            "message": (
                "No observations extracted for this session. "
                "The transcript may be too short or contain no "
                "decisions/TODOs/observations worth surfacing."
            ),
        }, reflector_result, reflector_error)

    # --- Step 3: pick format path. ----------------------------------------
    session_meta = db.get_session(conn, session_id)
    use_polish = full or len(entries) > large_set_threshold

    if not use_polish:
        brief = _format_brief_direct(session_id, session_meta, entries)
        return _attach_reflector_info({
            "status": "ok",
            "brief": brief,
            "mode": "direct",
            "entry_count": len(entries),
            "obs_path": str(obs_path) if obs_path else "",
            "observer_result": observer_result,
            "message": (
                f"Formatted {len(entries)} observation"
                f"{'' if len(entries) == 1 else 's'} directly."
            ),
        }, reflector_result, reflector_error)

    # Polish via Haiku sub-agent. Any failure below falls back to the
    # direct formatter so the user still gets a usable brief.
    try:
        template = prompt_path.read_text()
    except OSError as e:
        return _attach_reflector_info(
            _fallback_direct(
                session_id, session_meta, entries, obs_path, observer_result,
                reason=f"handoff prompt missing ({e})",
            ),
            reflector_result, reflector_error,
        )

    transcript: str | None = None
    if full:
        transcript = _rendered_transcript(conn, session_id, source_path)

    obs_jsonl = "\n".join(json.dumps(e) for e in entries)
    mode_label = "full" if full else "polish"
    polished, meta = _invoke_polish(
        template, obs_jsonl, transcript, mode_label,
        claude_bin=claude_bin, timeout=timeout,
    )

    if not polished:
        reason = meta.get("error") or "polish returned no output"
        return _attach_reflector_info(
            _fallback_direct(
                session_id, session_meta, entries, obs_path, observer_result,
                reason=reason,
                stderr=meta.get("stderr"),
            ),
            reflector_result, reflector_error,
        )

    return _attach_reflector_info({
        "status": "ok",
        "brief": polished,
        "mode": mode_label,
        "entry_count": len(entries),
        "obs_path": str(obs_path),
        "observer_result": observer_result,
        "message": (
            f"Polished {len(entries)} observations via Haiku "
            f"({'full mode — with transcript excerpts' if full else 'compression mode'})."
        ),
    }, reflector_result, reflector_error)


def _attach_reflector_info(
    result: dict[str, Any],
    reflector_result: dict[str, Any] | None,
    reflector_error: str | None,
) -> dict[str, Any]:
    """Decorate a returned handoff dict with reflector outcome fields.

    Keeps the reflector state out of every return-site literal — each of
    ``build_handoff``'s exit paths just wraps its dict in this helper.
    We always attach ``reflector_result`` (possibly ``None`` when the
    reflector was disabled or skipped); ``reflector_error`` is only
    present when ``reflect_fn`` raised, so the CLI can surface that
    specifically without having to dig into the dict.
    """
    result["reflector_result"] = reflector_result
    if reflector_error is not None:
        result["reflector_error"] = reflector_error
    return result


def _fallback_direct(
    session_id: str,
    session_meta: dict | None,
    entries: list[dict],
    obs_path: Path,
    observer_result: dict,
    *,
    reason: str,
    stderr: str | None = None,
) -> dict[str, Any]:
    """Shared tail for polish-failure branches.

    Returns ``status="ok"`` because the user still gets a usable brief —
    ``message`` carries the reason so the CLI can surface it without the
    caller having to special-case the fallback.
    """
    brief = _format_brief_direct(session_id, session_meta, entries)
    msg = (
        f"Formatted {len(entries)} observations directly "
        f"(fallback: {reason})."
    )
    result = {
        "status": "ok",
        "brief": brief,
        "mode": "direct",
        "entry_count": len(entries),
        "obs_path": str(obs_path),
        "observer_result": observer_result,
        "message": msg,
    }
    if stderr:
        result["polish_stderr"] = stderr
    return result
