"""Search helpers shared by the TUI search panel.

Keeps the query/parser logic free of Textual imports so the prefix path
and fuzzy fallback can be unit-tested directly.
"""

from __future__ import annotations

import re
import sqlite3
from typing import Any

from . import db


# Sentinel characters bracketing matched spans in snippet text. The TUI
# converts these to highlighted Rich spans when rendering results.
FTS_MATCH_START = "\x01"
FTS_MATCH_END = "\x02"

_FALLBACK_MIN_SCORE = 0.55
_FALLBACK_CANDIDATE_LIMIT = 200


def _parse_search_query(raw: str) -> tuple[list[str], str | None, str | None]:
    """Parse raw input into (search_terms, role_filter, project_filter)."""
    tokens = raw.strip().split()
    role: str | None = None
    project: str | None = None
    terms: list[str] = []
    for tok in tokens:
        low = tok.lower()
        if low == "user:":
            role = "user"
        elif low == "assistant:":
            role = "assistant"
        elif low.startswith("project:"):
            val = tok[len("project:"):]
            if val:
                project = val
        else:
            clean = re.sub(r"[^\w]", "", tok)
            if clean:
                terms.append(clean)
    return terms, role, project


def _build_prefix_fts_query(terms: list[str]) -> str:
    """Return an AND-ed FTS5 prefix expression for the supplied terms."""
    return " ".join(term + "*" for term in terms)


def _apply_message_filters(
    sql: str,
    params: dict[str, Any],
    *,
    role: str | None,
    project: str | None,
) -> tuple[str, dict[str, Any]]:
    if role:
        sql += " AND m.role = :role"
        params["role"] = role
    if project:
        sql += " AND s.project LIKE :project"
        params["project"] = f"%{project}%"
    return sql, params


def _search_filter_only(
    conn: sqlite3.Connection,
    *,
    role: str | None,
    project: str | None,
    limit: int,
) -> list[dict[str, Any]]:
    sql = (
        "SELECT m.uuid AS uuid, m.session_id AS session_id, "
        "m.role AS role, m.timestamp AS timestamp, "
        "substr(m.text, 1, 160) AS snippet, "
        "s.custom_name AS custom_name, s.project AS project, "
        "s.session_path AS session_path "
        "FROM messages m "
        "LEFT JOIN sessions s ON s.session_id = m.session_id "
        "WHERE 1=1"
    )
    params: dict[str, Any] = {}
    sql, params = _apply_message_filters(sql, params, role=role, project=project)
    sql += " ORDER BY m.timestamp DESC LIMIT :lim"
    params["lim"] = limit
    return db.query_all(conn, sql, params)


def _search_prefix(
    conn: sqlite3.Connection,
    *,
    fts_expr: str,
    role: str | None,
    project: str | None,
    limit: int,
) -> list[dict[str, Any]]:
    sql = (
        "SELECT m.uuid AS uuid, m.session_id AS session_id, "
        "m.role AS role, m.timestamp AS timestamp, "
        "snippet(messages_fts, 0, :mstart, :mend, '…', 16) AS snippet, "
        "s.custom_name AS custom_name, s.project AS project, "
        "s.session_path AS session_path "
        "FROM messages_fts "
        "JOIN messages m ON m.rowid = messages_fts.rowid "
        "LEFT JOIN sessions s ON s.session_id = m.session_id "
        "WHERE messages_fts MATCH :q"
    )
    params: dict[str, Any] = {
        "q": fts_expr,
        "mstart": FTS_MATCH_START,
        "mend": FTS_MATCH_END,
    }
    sql, params = _apply_message_filters(sql, params, role=role, project=project)
    sql += " ORDER BY rank LIMIT :lim"
    params["lim"] = limit
    return db.query_all(conn, sql, params)


def _term_trigrams(term: str) -> set[str]:
    low = term.lower()
    if len(low) < 3:
        return set()
    return {low[i:i + 3] for i in range(len(low) - 2)}


def _build_trigram_match_expr(terms: list[str]) -> str:
    trigrams: set[str] = set()
    for term in terms:
        trigrams.update(_term_trigrams(term))
    return " OR ".join(sorted(trigrams))


def _search_trigram_candidates(
    conn: sqlite3.Connection,
    *,
    trigram_expr: str,
    role: str | None,
    project: str | None,
    limit: int,
) -> list[dict[str, Any]]:
    sql = (
        "SELECT m.uuid AS uuid, m.session_id AS session_id, "
        "m.role AS role, m.timestamp AS timestamp, "
        "m.text AS text, "
        "s.custom_name AS custom_name, s.project AS project, "
        "s.session_path AS session_path, "
        "bm25(messages_fts_trigram) AS trigram_rank "
        "FROM messages_fts_trigram "
        "JOIN messages m ON m.rowid = messages_fts_trigram.rowid "
        "LEFT JOIN sessions s ON s.session_id = m.session_id "
        "WHERE messages_fts_trigram MATCH :q"
    )
    params: dict[str, Any] = {"q": trigram_expr}
    sql, params = _apply_message_filters(sql, params, role=role, project=project)
    sql += " ORDER BY trigram_rank LIMIT :lim"
    params["lim"] = limit
    return db.query_all(conn, sql, params)


def _similarity(query_term: str, candidate_word: str) -> float:
    query = query_term.lower()
    candidate = candidate_word.lower()
    if not query or not candidate:
        return 0.0
    if query == candidate:
        return 1.0
    if query in candidate:
        return max(0.8, len(query) / len(candidate))
    if candidate in query:
        return max(0.7, len(candidate) / len(query))

    q_trigrams = _term_trigrams(query)
    c_trigrams = _term_trigrams(candidate)
    if not q_trigrams or not c_trigrams:
        return 0.0
    overlap = len(q_trigrams & c_trigrams)
    if overlap == 0:
        return 0.0
    return (2.0 * overlap) / (len(q_trigrams) + len(c_trigrams))


def _score_candidate_row(
    row: dict[str, Any],
    query_terms: list[str],
) -> tuple[float, str | None] | None:
    text = str(row.get("text") or "")
    words = re.findall(r"\w+", text)
    if not words:
        return None

    scores: list[float] = []
    best_word: str | None = None
    best_word_score = 0.0

    for term in query_terms:
        best_score = 0.0
        best_for_term: str | None = None
        for word in words:
            score = _similarity(term, word)
            if score > best_score:
                best_score = score
                best_for_term = word
            if best_score >= 1.0:
                break
        if best_score < _FALLBACK_MIN_SCORE:
            return None
        scores.append(best_score)
        if best_score > best_word_score:
            best_word = best_for_term
            best_word_score = best_score

    if not scores:
        return None
    return sum(scores) / len(scores), best_word


def _build_fallback_snippet(text: str, highlight_term: str | None) -> str:
    if not text:
        return ""
    if not highlight_term:
        return text[:160]

    match = re.search(re.escape(highlight_term), text, flags=re.IGNORECASE)
    if not match:
        return text[:160]

    start = max(0, match.start() - 60)
    end = min(len(text), max(start + 160, match.end() + 60))
    snippet = text[start:end]
    rel_start = match.start() - start
    rel_end = match.end() - start

    parts = []
    if start > 0:
        parts.append("…")
    parts.append(snippet[:rel_start])
    parts.append(FTS_MATCH_START)
    parts.append(snippet[rel_start:rel_end])
    parts.append(FTS_MATCH_END)
    parts.append(snippet[rel_end:])
    if end < len(text):
        parts.append("…")
    return "".join(parts)


def _search_trigram_fallback(
    conn: sqlite3.Connection,
    *,
    query_terms: list[str],
    role: str | None,
    project: str | None,
    limit: int,
) -> list[dict[str, Any]]:
    trigram_expr = _build_trigram_match_expr(query_terms)
    if not trigram_expr:
        return []

    candidates = _search_trigram_candidates(
        conn,
        trigram_expr=trigram_expr,
        role=role,
        project=project,
        limit=max(limit * 4, _FALLBACK_CANDIDATE_LIMIT),
    )

    scored: list[tuple[float, float, dict[str, Any], str | None]] = []
    for row in candidates:
        result = _score_candidate_row(row, query_terms)
        if result is None:
            continue
        score, highlight_term = result
        trigram_rank = float(row.get("trigram_rank") or 0.0)
        scored.append((score, trigram_rank, row, highlight_term))

    scored.sort(key=lambda item: (-item[0], item[1], item[2].get("timestamp") or ""))

    results: list[dict[str, Any]] = []
    for score, _, row, highlight_term in scored[:limit]:
        results.append(
            {
                "uuid": row.get("uuid"),
                "session_id": row.get("session_id"),
                "role": row.get("role"),
                "timestamp": row.get("timestamp"),
                "snippet": _build_fallback_snippet(
                    str(row.get("text") or ""),
                    highlight_term,
                ),
                "custom_name": row.get("custom_name"),
                "project": row.get("project"),
                "session_path": row.get("session_path"),
                "score": score,
            }
        )
    return results


def search_messages(
    conn: sqlite3.Connection,
    raw_query: str,
    limit: int = 50,
) -> tuple[list[dict[str, Any]], bool]:
    """Run the search-panel query.

    Returns ``(rows, used_fuzzy_fallback)``. The trigram path only runs
    when the primary FTS5 prefix query returns zero rows.
    """
    query_terms, role, project = _parse_search_query(raw_query)

    if not query_terms:
        if not role and not project:
            return [], False
        try:
            return _search_filter_only(
                conn,
                role=role,
                project=project,
                limit=limit,
            ), False
        except sqlite3.OperationalError:
            return [], False

    try:
        rows = _search_prefix(
            conn,
            fts_expr=_build_prefix_fts_query(query_terms),
            role=role,
            project=project,
            limit=limit,
        )
    except sqlite3.OperationalError:
        rows = []

    if rows:
        return rows, False

    try:
        fallback_rows = _search_trigram_fallback(
            conn,
            query_terms=query_terms,
            role=role,
            project=project,
            limit=limit,
        )
    except sqlite3.OperationalError:
        fallback_rows = []

    return fallback_rows, bool(fallback_rows)
