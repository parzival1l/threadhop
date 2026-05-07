"""``threadhop serve`` — run the HTTP sidecar (FastAPI/uvicorn).

The sidecar lets external UIs (e.g. the t3code library route) read
the indexed sessions DB over HTTP without re-implementing the storage
layer. See ``threadhop_core/server/app.py`` for the endpoint surface.
"""

from __future__ import annotations

import argparse


def cmd_serve(args: argparse.Namespace) -> int:
    # uvicorn + fastapi are heavy imports; keep them inside the handler
    # so other CLI subcommands stay snappy.
    import uvicorn  # noqa: PLC0415

    from ...server.app import create_app  # noqa: PLC0415

    uvicorn.run(
        create_app(),
        host=args.host,
        port=args.port,
        log_level=args.log_level,
    )
    return 0
