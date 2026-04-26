"""Single-point CLI bootstrap.

Every DB-touching ``cmd_*`` handler used to repeat the same prologue
and epilogue: ``conn = db.init_db(); try: ... finally: conn.close()``.
A few of them forgot the ``finally`` and leaked the connection on
errors. ``cli_bootstrap()`` is the cure — one context manager that
opens the DB (which also runs migrations as a side-effect), yields a
``CLIContext``, and guarantees the close on every exit path.

Inspired by opencode's ``bootstrap(cb)`` pattern, kept intentionally
small. We do not pre-load app config: most handlers don't need it, and
when they do, ``ctx.config`` lazily loads on first access.
"""

from __future__ import annotations

import sqlite3
from contextlib import contextmanager
from dataclasses import dataclass, field
from typing import Iterator


@dataclass
class CLIContext:
    """Runtime context shared by ``cmd_*`` handlers.

    Held inside the ``cli_bootstrap()`` context manager so callers never
    need to remember the close. ``config`` is loaded on demand because
    a number of subcommands (``observe``, ``copy``, ``tag``) only need
    the DB.
    """

    conn: sqlite3.Connection
    _config: dict | None = field(default=None, repr=False)

    @property
    def config(self) -> dict:
        """App-level config dict, loaded once per bootstrap invocation."""
        if self._config is None:
            from ..config.loader import load_config  # noqa: PLC0415
            self._config = load_config(self.conn)
        return self._config


@contextmanager
def cli_bootstrap() -> Iterator[CLIContext]:
    """Open the DB (running pending migrations) and yield a ``CLIContext``.

    Closes the connection on exit, even when the body raises. The
    migrations live in ``db.init_db`` itself — no separate step needed.
    """
    from ..storage import db  # noqa: PLC0415
    conn = db.init_db()
    try:
        yield CLIContext(conn=conn)
    finally:
        conn.close()
