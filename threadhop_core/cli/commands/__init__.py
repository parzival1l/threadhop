"""``threadhop`` CLI subcommand handlers.

One file per verb. Each handler is callable as
``cmd_<verb>(args) -> int`` and is the single point the dispatcher
routes to from the argparse tree.
"""

from .bookmark import cmd_bookmark
from .changelog import cmd_changelog
from .config import cmd_config
from .conflicts import cmd_conflicts
from .copy import cmd_copy
from .decisions import cmd_decisions
from .future import cmd_future
from .handoff import cmd_handoff
from .observations import cmd_observations
from .observe import cmd_observe
from .tag import cmd_tag
from .todos import cmd_todos
from .update import cmd_update

__all__ = [
    "cmd_bookmark",
    "cmd_changelog",
    "cmd_config",
    "cmd_conflicts",
    "cmd_copy",
    "cmd_decisions",
    "cmd_future",
    "cmd_handoff",
    "cmd_observations",
    "cmd_observe",
    "cmd_tag",
    "cmd_todos",
    "cmd_update",
]
