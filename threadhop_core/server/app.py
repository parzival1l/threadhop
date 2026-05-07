"""FastAPI sidecar exposing the indexed sessions DB as a read-only HTTP API.

This is a parallel facade to the CLI — both call into the same
``threadhop_core`` modules. Endpoints live here (not as new CLI subcommands)
when their consumer is a UI rather than a human at a terminal.

v0 surface — three read endpoints driving the t3code library route:
    GET /projects                         -> [ProjectSummary]
    GET /sessions?project=<cwd>           -> [SessionSummary]
    GET /sessions/{session_id}/messages   -> [MessageRow]

Plus /healthz for the consumer to check liveness before issuing real reads.

Indexing happens once on startup via ``index_all`` — restart the sidecar
to pick up sessions created since boot. A watcher will land in v1 (see
the design notes in this branch's PR description).
"""

from __future__ import annotations

from contextlib import asynccontextmanager

from fastapi import FastAPI, HTTPException
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel

from .. import indexer
from ..storage import db


class ProjectSummary(BaseModel):
    cwd: str
    session_count: int
    last_modified: float | None


class SessionSummary(BaseModel):
    session_id: str
    session_path: str
    project: str | None
    cwd: str | None
    custom_name: str | None
    status: str
    modified_at: float | None
    created_at: float | None


class MessageRow(BaseModel):
    uuid: str
    session_id: str
    role: str
    text: str
    timestamp: str | None
    parent_uuid: str | None


@asynccontextmanager
async def _lifespan(app: FastAPI):
    conn = db.init_db()
    indexer.index_all(conn)
    app.state.conn = conn
    try:
        yield
    finally:
        conn.close()


def create_app() -> FastAPI:
    app = FastAPI(
        title="ThreadHop sidecar",
        version="0.1.0",
        lifespan=_lifespan,
    )

    # The t3code dev UI runs on http://localhost:5733 (Vite) and the
    # production app loads from a file:// origin. Allowing all origins
    # is fine for a localhost-only sidecar; lock this down if the
    # sidecar ever binds to non-loopback.
    app.add_middleware(
        CORSMiddleware,
        allow_origins=["*"],
        allow_methods=["GET"],
        allow_headers=["*"],
    )

    @app.get("/healthz")
    def healthz() -> dict[str, str]:
        return {"status": "ok"}

    @app.get("/projects", response_model=list[ProjectSummary])
    def list_projects() -> list[ProjectSummary]:
        rows = db.query_all(
            app.state.conn,
            """
            SELECT cwd,
                   COUNT(*) AS session_count,
                   MAX(modified_at) AS last_modified
            FROM sessions
            WHERE cwd IS NOT NULL AND cwd <> ''
            GROUP BY cwd
            ORDER BY last_modified DESC NULLS LAST
            """,
        )
        return [
            ProjectSummary(
                cwd=row["cwd"],
                session_count=row["session_count"],
                last_modified=row["last_modified"],
            )
            for row in rows
        ]

    @app.get("/sessions", response_model=list[SessionSummary])
    def list_sessions(project: str) -> list[SessionSummary]:
        rows = db.query_all(
            app.state.conn,
            """
            SELECT session_id, session_path, project, cwd, custom_name,
                   status, modified_at, created_at
            FROM sessions
            WHERE cwd = ?
            ORDER BY modified_at DESC NULLS LAST
            """,
            (project,),
        )
        return [SessionSummary(**row) for row in rows]

    @app.get("/sessions/{session_id}/messages", response_model=list[MessageRow])
    def list_messages(session_id: str) -> list[MessageRow]:
        if db.query_one(
            app.state.conn,
            "SELECT 1 FROM sessions WHERE session_id = ?",
            (session_id,),
        ) is None:
            raise HTTPException(status_code=404, detail="session not found")
        rows = db.query_all(
            app.state.conn,
            """
            SELECT uuid, session_id, role, text, timestamp, parent_uuid
            FROM messages
            WHERE session_id = ? AND is_sidechain = 0
            ORDER BY timestamp ASC
            """,
            (session_id,),
        )
        return [MessageRow(**row) for row in rows]

    return app
