"""``threadhop`` CLI subcommand handlers.

One file per verb. Each handler is callable as
``cmd_<verb>(args) -> int`` and is the single point the dispatcher
routes to from the argparse tree.
"""

from .bookmark import cmd_bookmark
from .changelog import cmd_changelog
from .copy import cmd_copy
from .future import cmd_future
from .peek import cmd_peek
from .prepare import cmd_prepare
from .receive import cmd_receive
from .search import cmd_search
from .tag import cmd_tag
from .update import cmd_update

__all__ = [
    "cmd_bookmark",
    "cmd_changelog",
    "cmd_copy",
    "cmd_future",
    "cmd_peek",
    "cmd_prepare",
    "cmd_receive",
    "cmd_search",
    "cmd_tag",
    "cmd_update",
]
