"""Claude CLI subprocess adapter.

This is the only place ThreadHop shells out to ``claude -p``. The three
callers (``observation.observer``, ``observation.reflector``, ``handoff``)
go through here so that adding alternative harnesses later (codex, gemini)
is a parallel adapter rather than a hunt-and-replace.

For now this is a single concrete adapter — there is no Harness Protocol
yet because we have only one adapter. The Protocol comes when the second
adapter arrives ("one adapter today, two adapters tomorrow").

Behavior contract for Phase 2 (refactor: behavior-preserving):

* The wrapper builds the canonical argv ``[claude_bin, "-p", prompt,
  "--model", model, "--permission-mode", permission_mode]`` and invokes
  ``subprocess.run`` with ``check=False, capture_output=True, text=True,
  timeout=timeout``.
* ``subprocess.TimeoutExpired`` and ``OSError`` are **not** caught here —
  every existing caller handles them with site-specific error dicts and we
  do not want to change that. ``FileNotFoundError`` (raised when the
  binary is missing) is a subclass of ``OSError`` and propagates the same
  way. Each caller already preflight-checks via ``shutil.which`` so the
  raise path is rarely hit in practice.
* The returned :class:`HarnessResult` dataclass mirrors the field names of
  ``subprocess.CompletedProcess`` (``returncode`` / ``stdout`` / ``stderr``)
  so existing call-site consumption code is unchanged.
"""
from __future__ import annotations

import subprocess
from dataclasses import dataclass
from typing import Optional


@dataclass(frozen=True)
class HarnessResult:
    """Outcome of a single ``claude -p`` invocation.

    Field names match :class:`subprocess.CompletedProcess` so callers that
    previously read ``proc.returncode`` / ``proc.stdout`` / ``proc.stderr``
    keep working unchanged.
    """

    returncode: int
    stdout: str
    stderr: str


def run_claude_p(
    prompt: str,
    *,
    model: str = "haiku",
    permission_mode: str = "acceptEdits",
    timeout: float = 180.0,
    claude_bin: str = "claude",
    extra_args: Optional[list[str]] = None,
) -> HarnessResult:
    """Run ``claude -p <prompt> --model <model> --permission-mode <mode>``.

    Captures stdout/stderr as text. ``subprocess.TimeoutExpired`` and
    ``OSError`` (including ``FileNotFoundError`` when ``claude_bin`` is
    missing) propagate to the caller — every existing call site has
    site-specific recovery logic and Phase 2 preserves that.

    ``extra_args`` is appended after the standard flags; reserved for
    callers that need to pass ``--resume <id>`` or similar in the future.
    """
    argv = [
        claude_bin, "-p", prompt,
        "--model", model,
        "--permission-mode", permission_mode,
    ]
    if extra_args:
        argv.extend(extra_args)
    proc = subprocess.run(
        argv,
        check=False,
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    return HarnessResult(
        returncode=proc.returncode,
        stdout=proc.stdout or "",
        stderr=proc.stderr or "",
    )
