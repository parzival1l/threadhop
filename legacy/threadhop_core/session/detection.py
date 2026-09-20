"""macOS-only Claude session detection via ``ps`` and ``lsof``.

Two flavours of detection live here:

* **Process-wide scan** (``get_active_claude_session_ids``) — scrapes
  every running ``claude`` CLI process and resolves a session id for
  each. Used by the TUI to mark sessions as live.
* **Process-tree walk** (``detect_current_session_id``,
  ``_invoked_from_claude_code``) — walks the ancestors of
  ``os.getpid()`` looking for a ``claude`` CLI parent. Used by every
  CLI subcommand that auto-defaults ``--session`` and by the
  update-check gate (so plugin invocations and ``!threadhop`` bash
  passthroughs stay quiet — ADR-027).
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

CLAUDE_PROJECTS = Path.home() / ".claude" / "projects"


def _parse_claude_process_args(args: str) -> tuple[bool, str | None]:
    """Classify a `ps` args line as an interactive `claude` CLI process.

    Returns (is_interactive_claude_cli, explicit_session_id_if_any). Excludes
    Claude.app and IDE-embedded binaries, and non-interactive `-p/--print`
    invocations. Used by both `get_active_claude_session_ids()` (scans all
    processes) and `detect_current_session_id()` (walks parent chain).
    """
    if "/Claude.app/" in args or "/native-binary/" in args:
        return (False, None)
    if "claude" not in args:
        return (False, None)
    arg_parts = args.split()
    if not arg_parts:
        return (False, None)
    is_interactive = False
    explicit_id: str | None = None
    for i, a in enumerate(arg_parts):
        if a in ("-r", "--resume") and "-p" not in arg_parts:
            is_interactive = True
            if i + 1 < len(arg_parts):
                candidate = arg_parts[i + 1]
                if len(candidate) == 36 and candidate.count("-") == 4:
                    explicit_id = candidate
        elif a in ("-c", "--continue") and "-p" not in arg_parts:
            is_interactive = True
    if not is_interactive:
        if arg_parts[-1] == "claude" or (
            arg_parts[0].endswith("/claude")
            and "-p" not in arg_parts and "--print" not in arg_parts
        ):
            is_interactive = True
    return (is_interactive, explicit_id)


def _resolve_session_id_by_cwd(cwd: str) -> str | None:
    """Given a claude process CWD, return the most-recently-modified session id
    in the matching `~/.claude/projects/<encoded>` directory."""
    project_path = CLAUDE_PROJECTS / cwd.replace("/", "-")
    if not project_path.is_dir():
        return None
    best: str | None = None
    best_mtime = 0.0
    for jf in project_path.glob("*.jsonl"):
        if jf.name.startswith("agent-"):
            continue
        mt = jf.stat().st_mtime
        if mt > best_mtime:
            best_mtime = mt
            best = jf.stem
    return best


def get_active_claude_session_ids() -> set[str]:
    """Detect running interactive Claude session ids on macOS/Linux."""
    active_ids: set[str] = set()
    cwd_pids: dict[int, str | None] = {}  # pid -> cwd for processes without explicit session IDs

    try:
        result = subprocess.run(
            ["ps", "-eo", "pid,args"],
            capture_output=True, text=True, timeout=5,
        )
        for line in result.stdout.strip().split("\n"):
            line = line.strip()
            if not line:
                continue
            parts = line.split(None, 1)
            if len(parts) < 2:
                continue
            pid_str, args = parts
            is_interactive, explicit_id = _parse_claude_process_args(args)
            if not is_interactive:
                continue

            if explicit_id:
                active_ids.add(explicit_id)
            else:
                try:
                    cwd_pids[int(pid_str)] = None
                except ValueError:
                    pass

        if cwd_pids:
            pid_list = ",".join(str(p) for p in cwd_pids)
            result = subprocess.run(
                ["lsof", "-a", "-d", "cwd", "-p", pid_list, "-Fn"],
                capture_output=True, text=True, timeout=5,
            )
            current_pid: int | None = None
            for line in result.stdout.strip().split("\n"):
                if line.startswith("p"):
                    current_pid = int(line[1:])
                elif line.startswith("n") and current_pid:
                    cwd_pids[current_pid] = line[1:]

            for cwd in cwd_pids.values():
                if not cwd:
                    continue
                sid = _resolve_session_id_by_cwd(cwd)
                if sid:
                    active_ids.add(sid)
    except Exception:
        pass

    return active_ids


def _get_process_cwd(pid: int) -> str | None:
    """Resolve a process's working directory via `lsof`. macOS-only."""
    try:
        result = subprocess.run(
            ["lsof", "-a", "-d", "cwd", "-p", str(pid), "-Fn"],
            capture_output=True, text=True, timeout=5,
        )
    except Exception:
        return None
    for line in result.stdout.strip().split("\n"):
        if line.startswith("n"):
            return line[1:]
    return None


def detect_current_session_id() -> str | None:
    """Detect the Claude session id for the current terminal's process tree.

    Walks ancestors of `os.getpid()` looking for a `claude` CLI process. When
    one is found, resolves its session id from args (explicit `--resume <id>`)
    or its CWD (most-recently-modified JSONL in the matching project dir).
    Returns None if no claude ancestor is found or the id can't be resolved.

    macOS-only — uses `ps` and `lsof` (matches the rest of threadhop's
    detection surface). Same contract as `get_active_claude_session_ids()`,
    but scoped to the caller's process tree rather than every running claude.
    """
    try:
        result = subprocess.run(
            ["ps", "-eo", "pid,ppid,args"],
            capture_output=True, text=True, timeout=5,
        )
    except Exception:
        return None

    procs: dict[int, tuple[int, str]] = {}
    lines = result.stdout.strip().split("\n")
    for line in lines[1:]:  # skip header
        parts = line.strip().split(None, 2)
        if len(parts) < 3:
            continue
        try:
            pid = int(parts[0])
            ppid = int(parts[1])
        except ValueError:
            continue
        procs[pid] = (ppid, parts[2])

    pid = os.getpid()
    visited: set[int] = set()
    while pid and pid > 0 and pid not in visited:
        visited.add(pid)
        entry = procs.get(pid)
        if not entry:
            break
        ppid, args = entry
        is_claude, explicit_id = _parse_claude_process_args(args)
        if is_claude:
            if explicit_id:
                return explicit_id
            cwd = _get_process_cwd(pid)
            if cwd:
                sid = _resolve_session_id_by_cwd(cwd)
                if sid:
                    return sid
            return None
        pid = ppid
    return None


def find_session_path(session_id: str) -> Path | None:
    """Locate the JSONL transcript for a session ID under ~/.claude/projects/."""
    for path in CLAUDE_PROJECTS.glob(f"*/{session_id}.jsonl"):
        return path
    return None


def detect_project_from_cwd() -> str | None:
    """Auto-detect project by matching CWD to Claude's project directory names.

    Claude stores sessions in ~/.claude/projects/ with directory names like
    -Users-jane-Projects-my-app (CWD with / replaced by -).
    We check the CWD and its parents until we find a match.
    """
    cwd = Path.cwd()
    # Try CWD and its parents (e.g. if in a subdirectory of the project)
    for path in [cwd, *cwd.parents]:
        project_dir_name = str(path).replace("/", "-")
        if (CLAUDE_PROJECTS / project_dir_name).is_dir():
            return project_dir_name
        if path == Path.home():
            break
    return None


def _invoked_from_claude_code() -> bool:
    """True if a `claude` CLI process is an ancestor of this one.

    Shares the ``ps``-based parent-chain walk with
    ``detect_current_session_id`` but only cares about *presence* — we
    don't need to resolve a session id, just gate the update-check
    notice so plugin invocations (``/threadhop:tag``) and
    ``!threadhop …`` bash passthroughs stay quiet (ADR-027).
    """
    try:
        result = subprocess.run(
            ["ps", "-eo", "pid,ppid,args"],
            capture_output=True, text=True, timeout=5,
        )
    except Exception:
        return False

    procs: dict[int, tuple[int, str]] = {}
    for line in result.stdout.strip().split("\n")[1:]:  # skip header
        parts = line.strip().split(None, 2)
        if len(parts) < 3:
            continue
        try:
            pid = int(parts[0])
            ppid = int(parts[1])
        except ValueError:
            continue
        procs[pid] = (ppid, parts[2])

    pid = os.getpid()
    visited: set[int] = set()
    while pid and pid > 0 and pid not in visited:
        visited.add(pid)
        entry = procs.get(pid)
        if not entry:
            break
        ppid, args = entry
        is_claude, _ = _parse_claude_process_args(args)
        if is_claude:
            return True
        pid = ppid
    return False
