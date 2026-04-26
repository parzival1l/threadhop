"""``threadhop observe`` — start, stop, or single-pass the observer sidecar.

This module also owns the small set of CLI-only observer helpers
(``_refresh_observer_state``, ``_print_observer_result``,
``_stop_observer_session``, ``_stop_all_observers``). The TUI
re-imports ``_refresh_observer_state`` from here because it shares the
exact same stale-PID correction logic.
"""

from __future__ import annotations

import os
import signal
import sys

from ...observation import observer
from ...observation.observer_state import _refresh_observer_state
from ...session.detection import (
    CLAUDE_PROJECTS,
    find_session_path,
    get_active_claude_session_ids,
)
from ...storage import db
from ..bootstrap import cli_bootstrap
from ..helpers import _resolve_cli_session

__all__ = [
    "_refresh_observer_state",
    "_print_observer_result",
    "_stop_observer_session",
    "_stop_all_observers",
    "cmd_observe",
]


def _print_observer_result(result: dict, *, prefix: str = "observer") -> None:
    message = result.get("message")
    if message:
        print(f"[{prefix}] {message}")
    if prefix == "observer" and result.get("status") == "extracted":
        print(
            f"[{prefix}] cursor {result['source_byte_offset']} bytes; "
            f"{result['entry_count']} observations on file"
        )
    if result.get("status") == "failed" and result.get("stderr"):
        print(f"[{prefix}] stderr: {result['stderr']}", file=sys.stderr)


def _stop_observer_session(conn, session_id: str) -> int:
    state = db.get_observation_state(conn, session_id)
    if state is not None and state.get("status") == "running":
        pid = state.get("observer_pid")
        if pid is not None and not observer._pid_is_alive(int(pid)):
            db.set_observer_stopped(conn, session_id)
            print(
                f"Observer PID {pid} for session {session_id} was stale; "
                "state corrected to stopped."
            )
            return 0

    state = _refresh_observer_state(conn, session_id)
    if state is None or state.get("observer_pid") is None:
        print(f"Observer is not running for session {session_id}.")
        return 0

    pid = int(state["observer_pid"])
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        db.set_observer_stopped(conn, session_id)
        print(
            f"Observer PID {pid} for session {session_id} was stale; "
            "state corrected to stopped."
        )
        return 0
    print(f"Sent SIGTERM to observer for session {session_id} (pid {pid}).")
    return 0


def _stop_all_observers(conn) -> int:
    rows = db.get_running_observers(conn)
    if not rows:
        print("No running observers.")
        return 0

    stopped = 0
    stale = 0
    for row in rows:
        session_id = row["session_id"]
        pid = int(row["observer_pid"])
        try:
            os.kill(pid, signal.SIGTERM)
            stopped += 1
        except ProcessLookupError:
            db.set_observer_stopped(conn, session_id)
            stale += 1
    print(f"Sent SIGTERM to {stopped} observer(s).")
    if stale:
        print(f"Corrected {stale} stale observer PID(s).")
    return 0


def cmd_observe(args) -> int:
    """Run or manage the per-session observer sidecar."""
    with cli_bootstrap() as ctx:
        conn = ctx.conn

        if args.stop_all:
            return _stop_all_observers(conn)

        rc = _resolve_cli_session(args)
        if rc != 0:
            return rc

        if args.stop:
            return _stop_observer_session(conn, args.session)

        session_path = find_session_path(args.session)
        if session_path is None:
            print(
                f"threadhop observe: no transcript found for session "
                f"{args.session} under {CLAUDE_PROJECTS}.",
                file=sys.stderr,
            )
            return 1

        db.upsert_session(
            conn, args.session, str(session_path),
            project=session_path.parent.name,
        )
        state = _refresh_observer_state(conn, args.session)
        if args.reset and state is not None and state.get("observer_pid") is not None:
            pid = int(state["observer_pid"])
            print(
                f"threadhop observe: cannot reset session {args.session} while "
                f"observer pid {pid} is still running. Stop it first.",
                file=sys.stderr,
            )
            return 1
        if args.reset:
            removed = db.delete_observation_state(conn, args.session)
            conn.commit()
            obs_path = db.OBS_DIR / f"{args.session}.jsonl"
            wiped = False
            if obs_path.exists():
                obs_path.unlink()
                wiped = True
            print(
                f"reset: removed {removed} state row(s)"
                + (f", deleted {obs_path}" if wiped else "")
            )

        if args.once:
            result = observer.observe_session(
                conn, args.session,
                batch_threshold=args.batch_threshold,
                claude_bin=args.claude_bin,
                model=args.model,
                timeout=args.timeout,
            )
            _print_observer_result(result)
            if result.get("obs_path"):
                print(f"  obs file: {result['obs_path']}")
            return 1 if result.get("status") == "failed" else 0

        state = _refresh_observer_state(conn, args.session)
        if state is not None and state.get("observer_pid") is not None:
            pid = int(state["observer_pid"])
            print(
                f"Observer already running for session {args.session} "
                f"(pid {pid})."
            )
            return 0

        print(
            f"Observing session {args.session} via {args.watch_backend} "
            f"watcher (batch threshold {args.batch_threshold})."
        )
        result = observer.observe_sidecar(
            conn,
            args.session,
            source_path=session_path,
            batch_threshold=args.batch_threshold,
            poll_interval_sec=args.poll_interval,
            watch_backend=args.watch_backend,
            claude_bin=args.claude_bin,
            model=args.model,
            timeout=args.timeout,
            session_active_fn=lambda sid: sid in get_active_claude_session_ids(),
            on_result=_print_observer_result,
            on_reflector_result=lambda r: _print_observer_result(
                r, prefix="reflector"
            ),
        )

    status = result.get("status")
    if status == "already_running":
        print(result.get("message", "Observer already running."))
        return 0
    if status == "no_source":
        print(result.get("message", "No source JSONL found."), file=sys.stderr)
        return 1
    if status == "source_gone":
        print(f"Observer stopped for session {args.session}: source transcript disappeared.")
        return 0
    if status == "session_ended":
        print(f"Observer stopped for session {args.session}: Claude session ended.")
        return 0
    if status == "stopped":
        print(f"Observer stopped for session {args.session}.")
        return 0
    if status == "failed":
        return 1
    return 0
