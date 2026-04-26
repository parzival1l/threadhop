"""Real-time full-text search modal (task #14, ADR-002 / ADR-007).

FTS5 prefix matching against the ``messages`` table populated by the
indexer. Each keystroke debounces (~80 ms) and re-runs the query.
Results render as a ``ListView`` of ``SearchResultItem`` rows; dismissing
the modal with a selection jumps the main TUI to the source message.

Filter syntax parsed from the raw query string:
  - ``project:<name>``   substring match against ``sessions.project``
  - ``session:current``  restrict to the active transcript session
  - ``since:`` / ``until:``  ``YYYY-MM-DD`` or ISO range filters
  - ``user:`` / ``assistant:``  role filters
  - remaining whitespace-separated tokens become FTS5 prefix terms.
"""

from __future__ import annotations

import re
import sqlite3
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from time import perf_counter

from rich.console import Group
from rich.markup import escape
from rich.text import Text
from textual.app import ComposeResult
from textual.binding import Binding
from textual.containers import Vertical
from textual.screen import ModalScreen
from textual.widgets import Input, ListItem, ListView, Static

from threadhop_core.storage import db
from threadhop_core.storage import search_queries
from threadhop_core.storage.recent_searches import (
    clear_recent_searches,
    get_recent_searches,
    save_recent_search,
)

from ..constants import SEARCH_PAGE_SIZE


# Sentinel characters bracketing matched spans in the SQL `snippet()`
# output. Using control chars avoids collisions with user text. Parsed
# back out by `_render_snippet` into Rich Text with a highlight style.
_FTS_MATCH_START = "\x01"
_FTS_MATCH_END = "\x02"


@dataclass(frozen=True)
class SearchQuerySpec:
    """Parsed search input plus derived filter metadata."""

    raw: str
    fts_expr: str
    terms: tuple[str, ...]
    plain_query: str
    role: str | None = None
    project: str | None = None
    session_id: str | None = None
    since_ts: str | None = None
    until_ts: str | None = None
    until_is_exclusive: bool = False


@dataclass(frozen=True)
class SearchResultPage:
    """One page of results plus metadata for the search modal."""

    rows: list[dict]
    total_count: int
    limit: int
    offset: int
    elapsed_ms: float
    used_fuzzy_fallback: bool = False

    @property
    def loaded_count(self) -> int:
        return self.offset + len(self.rows)

    @property
    def has_more(self) -> bool:
        return self.loaded_count < self.total_count


def _parse_search_date(value: str, *, upper_bound: bool) -> tuple[str | None, bool]:
    """Parse a date/date-time filter into an ISO UTC bound.

    Returns ``(iso_utc, is_exclusive)``. Date-only ``until:YYYY-MM-DD``
    becomes the next day's midnight and is treated as exclusive so the
    whole day is included.
    """
    raw = (value or "").strip()
    if not raw:
        return None, False

    try:
        if len(raw) == 10:
            dt = datetime.fromisoformat(raw).replace(tzinfo=timezone.utc)
            if upper_bound:
                dt += timedelta(days=1)
                return dt.strftime("%Y-%m-%dT%H:%M:%SZ"), True
            return dt.strftime("%Y-%m-%dT%H:%M:%SZ"), False

        dt = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError:
        return None, False

    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    else:
        dt = dt.astimezone(timezone.utc)
    return dt.strftime("%Y-%m-%dT%H:%M:%SZ"), False


def _build_fts_query(
    raw: str,
    *,
    current_session_id: str | None = None,
) -> SearchQuerySpec:
    """Parse raw input into a structured search query spec."""
    tokens = raw.strip().split()
    role: str | None = None
    project: str | None = None
    session_id: str | None = None
    since_ts: str | None = None
    until_ts: str | None = None
    until_is_exclusive = False
    terms: list[str] = []

    for tok in tokens:
        low = tok.lower()
        if low == "user:":
            role = "user"
        elif low == "assistant:":
            role = "assistant"
        elif low.startswith("project:"):
            val = tok[len("project:"):].strip()
            if val:
                project = val
        elif low.startswith("session:"):
            val = tok[len("session:"):].strip()
            if val:
                if val.lower() == "current":
                    session_id = current_session_id
                else:
                    session_id = val
        elif low.startswith("since:"):
            parsed, _ = _parse_search_date(tok[len("since:"):], upper_bound=False)
            if parsed:
                since_ts = parsed
        elif low.startswith("until:"):
            parsed, exclusive = _parse_search_date(
                tok[len("until:"):],
                upper_bound=True,
            )
            if parsed:
                until_ts = parsed
                until_is_exclusive = exclusive
        else:
            # Keep only word chars (letters/digits/underscore). FTS5's
            # unicode61 tokenizer is fine with these, and stripping
            # punctuation prevents syntax errors at MATCH time.
            clean = re.sub(r"[^\w]", "", tok)
            if clean:
                terms.append(clean)

    return SearchQuerySpec(
        raw=raw.strip(),
        fts_expr=" ".join(f"{term}*" for term in terms),
        terms=tuple(terms),
        plain_query=" ".join(terms).lower(),
        role=role,
        project=project,
        session_id=session_id,
        since_ts=since_ts,
        until_ts=until_ts,
        until_is_exclusive=until_is_exclusive,
    )


def _build_search_filters(spec: SearchQuerySpec) -> tuple[list[str], dict]:
    """Return shared SQL filter clauses for FTS and filter-only search."""
    clauses: list[str] = []
    params: dict[str, str] = {}

    if spec.role:
        clauses.append("m.role = :role")
        params["role"] = spec.role
    if spec.project:
        clauses.append("s.project LIKE :project")
        params["project"] = f"%{spec.project}%"
    if spec.session_id:
        clauses.append("m.session_id = :session_id")
        params["session_id"] = spec.session_id
    if spec.since_ts:
        clauses.append("m.timestamp >= :since_ts")
        params["since_ts"] = spec.since_ts
    if spec.until_ts:
        op = "<" if spec.until_is_exclusive else "<="
        clauses.append(f"m.timestamp {op} :until_ts")
        params["until_ts"] = spec.until_ts

    return clauses, params


def _search_trigram_fallback_page(
    conn: sqlite3.Connection,
    spec: SearchQuerySpec,
    *,
    limit: int,
    offset: int,
    where_sql: str,
    base_params: dict,
    timer_start: float,
) -> SearchResultPage:
    """Return a paged fuzzy-fallback result set for a zero-hit query."""
    trigram_expr = search_queries._build_trigram_match_expr(list(spec.terms))
    if not trigram_expr:
        return SearchResultPage(
            rows=[],
            total_count=0,
            limit=limit,
            offset=offset,
            elapsed_ms=(perf_counter() - timer_start) * 1000,
            used_fuzzy_fallback=False,
        )

    candidate_sql = (
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
        f"{where_sql} "
        "ORDER BY trigram_rank "
        "LIMIT :candidate_lim"
    )
    params = dict(base_params)
    params["q"] = trigram_expr
    params["candidate_lim"] = max(limit * 4, 200)

    try:
        candidates = db.query_all(conn, candidate_sql, params)
    except sqlite3.OperationalError:
        return SearchResultPage(
            rows=[],
            total_count=0,
            limit=limit,
            offset=offset,
            elapsed_ms=(perf_counter() - timer_start) * 1000,
            used_fuzzy_fallback=False,
        )

    scored: list[tuple[float, float, dict, str | None]] = []
    for row in candidates:
        result = search_queries._score_candidate_row(row, list(spec.terms))
        if result is None:
            continue
        score, highlight_term = result
        trigram_rank = float(row.get("trigram_rank") or 0.0)
        scored.append((score, trigram_rank, row, highlight_term))

    scored.sort(key=lambda item: (-item[0], item[1], item[2].get("timestamp") or ""))

    all_rows: list[dict] = []
    for score, _, row, highlight_term in scored:
        all_rows.append(
            {
                "uuid": row.get("uuid"),
                "session_id": row.get("session_id"),
                "role": row.get("role"),
                "timestamp": row.get("timestamp"),
                "snippet": search_queries._build_fallback_snippet(
                    str(row.get("text") or ""),
                    highlight_term,
                ),
                "custom_name": row.get("custom_name"),
                "project": row.get("project"),
                "session_path": row.get("session_path"),
                "score": score,
            }
        )

    page_rows = all_rows[offset:offset + limit]
    return SearchResultPage(
        rows=page_rows,
        total_count=len(all_rows),
        limit=limit,
        offset=offset,
        elapsed_ms=(perf_counter() - timer_start) * 1000,
        used_fuzzy_fallback=bool(all_rows),
    )


def search_messages(
    conn: sqlite3.Connection,
    raw_query: str,
    limit: int = SEARCH_PAGE_SIZE,
    offset: int = 0,
    *,
    current_session_id: str | None = None,
) -> SearchResultPage:
    """Run the FTS5 search for the search panel.

    Returns paged rows with keys: uuid, session_id, role, timestamp,
    snippet, custom_name, project, session_path. ``snippet`` contains
    _FTS_MATCH_START / _FTS_MATCH_END sentinels around each match span.
    """
    spec = _build_fts_query(raw_query, current_session_id=current_session_id)
    where_clauses, base_params = _build_search_filters(spec)
    where_sql = ""
    if where_clauses:
        where_sql = " AND " + " AND ".join(where_clauses)

    timer_start = perf_counter()

    # No search terms: allow a filter-only query so typing just
    # `project:foo` or `session:current` lists recent matching messages.
    if not spec.fts_expr:
        if not where_clauses:
            return SearchResultPage(
                rows=[],
                total_count=0,
                limit=limit,
                offset=offset,
                elapsed_ms=0.0,
            )

        count_sql = (
            "SELECT COUNT(*) AS total_count "
            "FROM messages m "
            "LEFT JOIN sessions s ON s.session_id = m.session_id "
            f"WHERE 1=1{where_sql}"
        )
        page_sql = (
            "SELECT m.uuid AS uuid, m.session_id AS session_id, "
            "m.role AS role, m.timestamp AS timestamp, "
            "substr(m.text, 1, 160) AS snippet, "
            "s.custom_name AS custom_name, s.project AS project, "
            "s.session_path AS session_path "
            "FROM messages m "
            "LEFT JOIN sessions s ON s.session_id = m.session_id "
            f"WHERE 1=1{where_sql} "
            "ORDER BY m.timestamp DESC "
            "LIMIT :lim OFFSET :off"
        )
        params = dict(base_params)
        params["lim"] = limit
        params["off"] = offset
        try:
            total_row = db.query_one(conn, count_sql, base_params) or {}
            rows = db.query_all(conn, page_sql, params)
        except sqlite3.OperationalError:
            return SearchResultPage(
                rows=[],
                total_count=0,
                limit=limit,
                offset=offset,
                elapsed_ms=0.0,
            )

        return SearchResultPage(
            rows=rows,
            total_count=int(total_row.get("total_count") or 0),
            limit=limit,
            offset=offset,
            elapsed_ms=(perf_counter() - timer_start) * 1000,
        )

    recent_cutoff = (
        datetime.now(timezone.utc) - timedelta(days=30)
    ).strftime("%Y-%m-%dT%H:%M:%SZ")
    normalized_text_sql = " ".join(
        """
        lower(
            replace(
                replace(
                    replace(
                        replace(
                            replace(
                                replace(
                                    replace(' ' || m.text || ' ', char(10), ' '),
                                    '.', ' '
                                ),
                                ',', ' '
                            ),
                            ';', ' '
                        ),
                        ':', ' '
                    ),
                    '!', ' '
                ),
                '?', ' '
            )
        )
        """.split()
    )
    base_from_sql = (
        "FROM messages_fts "
        "JOIN messages m ON m.rowid = messages_fts.rowid "
        "LEFT JOIN sessions s ON s.session_id = m.session_id "
        "WHERE messages_fts MATCH :q"
        f"{where_sql}"
    )
    count_sql = "SELECT COUNT(*) AS total_count " + base_from_sql
    page_sql = (
        "SELECT m.uuid AS uuid, m.session_id AS session_id, "
        "m.role AS role, m.timestamp AS timestamp, "
        "snippet(messages_fts, 0, :mstart, :mend, '…', 16) AS snippet, "
        "s.custom_name AS custom_name, s.project AS project, "
        "s.session_path AS session_path "
        + base_from_sql
        + " ORDER BY "
        "CASE "
        "WHEN :plain_query = '' THEN 0 "
        f"WHEN instr({normalized_text_sql}, ' ' || :plain_query || ' ') > 0 THEN 0 "
        "ELSE 1 "
        "END ASC, "
        "CASE WHEN m.timestamp >= :recent_cutoff THEN 0 ELSE 1 END ASC, "
        "bm25(messages_fts) ASC, "
        "m.timestamp DESC "
        "LIMIT :lim OFFSET :off"
    )
    params = dict(base_params)
    params.update({
        "q": spec.fts_expr,
        "mstart": _FTS_MATCH_START,
        "mend": _FTS_MATCH_END,
        "plain_query": spec.plain_query,
        "recent_cutoff": recent_cutoff,
    })
    params["lim"] = limit
    params["off"] = offset

    try:
        count_params = {
            k: v
            for k, v in params.items()
            if k not in {"lim", "off", "mstart", "mend", "plain_query", "recent_cutoff"}
        }
        total_row = db.query_one(conn, count_sql, count_params) or {}
        total_count = int(total_row.get("total_count") or 0)
        rows = db.query_all(conn, page_sql, params)
    except sqlite3.OperationalError:
        # Malformed expressions still slip through on edge cases
        # (e.g. a bare `*`). Surface as "no results" rather than crash.
        return SearchResultPage(
            rows=[],
            total_count=0,
            limit=limit,
            offset=offset,
            elapsed_ms=0.0,
        )

    if total_count == 0 and spec.terms:
        return _search_trigram_fallback_page(
            conn,
            spec,
            limit=limit,
            offset=offset,
            where_sql=where_sql,
            base_params=base_params,
            timer_start=timer_start,
        )

    return SearchResultPage(
        rows=rows,
        total_count=total_count,
        limit=limit,
        offset=offset,
        elapsed_ms=(perf_counter() - timer_start) * 1000,
    )


def _render_snippet(snippet: str) -> Text:
    """Convert an FTS5 snippet with sentinel-wrapped matches to Rich Text."""
    out = Text()
    remaining = snippet or ""
    while True:
        i = remaining.find(search_queries.FTS_MATCH_START)
        if i < 0:
            out.append(remaining)
            break
        out.append(remaining[:i])
        remaining = remaining[i + len(search_queries.FTS_MATCH_START):]
        j = remaining.find(search_queries.FTS_MATCH_END)
        if j < 0:
            # Unterminated — highlight the rest and stop.
            out.append(remaining, style="bold black on yellow")
            break
        out.append(remaining[:j], style="bold black on yellow")
        remaining = remaining[j + len(search_queries.FTS_MATCH_END):]
    return out


class SearchResultItem(ListItem):
    """One row in the search-results list.

    Holds the raw result dict so the dismiss handler can pull the
    session_id + uuid for jump-to-source without another DB lookup.
    """

    def __init__(self, row: dict):
        self.result = row
        super().__init__()

    def compose(self) -> ComposeResult:
        role = self.result.get("role", "")
        role_icon = "▶" if role == "user" else "●"
        role_style = "bold cyan" if role == "user" else "bold green"

        # Snippet line: colored role marker + highlighted text span.
        snippet_line = Text()
        snippet_line.append(f"{role_icon} ", style=role_style)
        snippet_line.append_text(
            _render_snippet(self.result.get("snippet") or "")
        )

        # Metadata line: session name · project · formatted timestamp.
        session_name = (
            self.result.get("custom_name")
            or self.result.get("project")
            or (self.result.get("session_id", "") or "")[:8]
        )
        project = self.result.get("project") or ""
        ts_raw = self.result.get("timestamp") or ""
        ts_str = ts_raw[:16]
        try:
            dt = datetime.fromisoformat(ts_raw.replace("Z", "+00:00"))
            ts_str = dt.strftime("%Y-%m-%d %H:%M")
        except (ValueError, AttributeError):
            pass

        meta = Text()
        meta.append("  ", style="dim")
        meta.append(str(session_name), style="bold")
        if project and project != session_name:
            meta.append("  ·  ", style="dim")
            meta.append(str(project), style="cyan")
        meta.append("  ·  ", style="dim")
        meta.append(ts_str, style="dim")

        yield Static(Group(snippet_line, meta), classes="search-result")


class RecentSearchItem(ListItem):
    """One row in the recent-search list shown for an empty query."""

    def __init__(self, query: str):
        self.query = query
        super().__init__()

    def compose(self) -> ComposeResult:
        label = Text()
        label.append("Recent  ", style="dim")
        label.append(self.query, style="bold")
        yield Static(label, classes="search-result")


class SearchScreen(ModalScreen):
    """Modal full-text search over indexed messages (task #14).

    Focus stays on the Input; arrow keys (and Ctrl+N/Ctrl+P) navigate
    the results list without losing typing focus — matches common
    fuzzy-finder UX (fzf, Helm). Enter returns ``(session_id, uuid)``
    to the dismiss callback; Escape dismisses with ``None``.
    """

    # Debounce delay between keystroke and the FTS query. 120 ms leaves
    # enough slack for the extra COUNT() query and lazy-paging status
    # updates without making fast typing feel sticky.
    DEBOUNCE_SECONDS = 0.12

    # The app binds `enter` with priority=True to start_reply_or_send, and
    # app priority bindings fire even while a modal is up. Re-bind enter
    # here with priority so the modal wins and Input.Submitted actually
    # reaches our handler.
    BINDINGS = [
        Binding("enter", "open_result", "Open", priority=True, show=False),
    ]


    def __init__(
        self,
        conn: sqlite3.Connection,
        *,
        config: dict | None = None,
        current_session_id: str | None = None,
    ):
        super().__init__()
        self.conn = conn
        self.config = config
        self.current_session_id = current_session_id
        # Single outstanding debounce timer. Each new keystroke stops
        # the previous timer before scheduling the next one, so only
        # the final keystroke in a typing burst triggers a query.
        self._search_timer = None
        self._active_query = ""
        self._loaded_count = 0
        self._total_count = 0
        self._last_elapsed_ms = 0.0
        self._loading_more = False

    def compose(self) -> ComposeResult:
        with Vertical(id="search-container") as container:
            container.border_title = "Search"
            yield Input(
                placeholder=(
                    "Search — prefix first, fuzzy fallback; "
                    "filters: project: session:current since: until:"
                ),
                id="search-input",
            )
            yield Static("Type to search…", id="search-status")
            yield ListView(id="search-results")
            yield Static(
                "↑/↓ or PgUp/PgDn navigate • Enter jump • Ctrl+X clear history • Esc close",
                id="search-help",
            )

    def on_mount(self) -> None:
        self.query_one("#search-input", Input).focus()
        self._show_empty_state()

    def on_input_changed(self, event) -> None:
        """Debounced re-run on every keystroke."""
        try:
            if event.input.id != "search-input":
                return
        except AttributeError:
            return
        if self._search_timer is not None:
            try:
                self._search_timer.stop()
            except Exception:
                pass
        value = event.value
        self._search_timer = self.set_timer(
            self.DEBOUNCE_SECONDS, lambda v=value: self._execute_search(v)
        )

    def _execute_search(self, raw_query: str) -> None:
        self._run_search(raw_query, reset=True)

    def _show_empty_state(self) -> None:
        results_list = self.query_one("#search-results", ListView)
        status = self.query_one("#search-status", Static)
        self._active_query = ""
        self._loaded_count = 0
        self._total_count = 0
        self._last_elapsed_ms = 0.0

        results_list.clear()
        recent = get_recent_searches(self.config)
        if not recent:
            status.update("Type to search…")
            return

        for query in recent:
            results_list.append(RecentSearchItem(query))
        status.update(f"Recent searches ({len(recent)})")

    def _update_status(self, page: SearchResultPage, *, raw_query: str) -> None:
        status = self.query_one("#search-status", Static)
        if not raw_query.strip():
            self._show_empty_state()
            return

        # Pad dynamic numbers so the status row doesn't jitter as you type:
        # elapsed_ms grows 4.2 → 10.7 → 100.3 chars, and `loaded` grows
        # 1 → 10 → 100 as you scroll. Terminal fonts are monospace, so the
        # only thing moving the layout is the format-spec width itself.
        if page.total_count == 0:
            status.update(f"No results  •  {page.elapsed_ms:>6.1f} ms")
            return

        noun = "result" if page.total_count == 1 else "results"
        loaded = min(self._loaded_count, self._total_count)
        count_width = len(str(page.total_count))
        parts = [
            f"{loaded:>{count_width}} of {page.total_count} {noun}",
            f"{page.elapsed_ms:>6.1f} ms",
        ]
        if page.used_fuzzy_fallback:
            parts.append("fuzzy fallback")
        if loaded < page.total_count:
            parts.append("more load as you scroll")
        status.update("  •  ".join(parts))

    def _run_search(self, raw_query: str, *, reset: bool) -> None:
        results_list = self.query_one("#search-results", ListView)

        raw = raw_query.strip()
        if not raw:
            self._show_empty_state()
            return

        try:
            offset = 0 if reset or raw != self._active_query else self._loaded_count
            page = search_messages(
                self.conn,
                raw,
                limit=SEARCH_PAGE_SIZE,
                offset=offset,
                current_session_id=self.current_session_id,
            )
        except Exception as e:  # noqa: BLE001
            self._loading_more = False
            results_list.clear()
            self.query_one("#search-status", Static).update(f"Search error: {e}")
            return

        if reset or raw != self._active_query:
            results_list.clear()
            self._active_query = raw
            self._loaded_count = 0

        for row in page.rows:
            results_list.append(SearchResultItem(row))

        if reset and page.rows:
            results_list.index = 0

        self._loaded_count = offset + len(page.rows)
        self._total_count = page.total_count
        self._last_elapsed_ms = page.elapsed_ms
        self._update_status(page, raw_query=raw)
        self._loading_more = False

    def _load_more_results(self) -> None:
        if self._loading_more:
            return
        if not self._active_query:
            return
        if self._loaded_count >= self._total_count:
            return
        self._loading_more = True
        self._run_search(self._active_query, reset=False)

    def on_key(self, event) -> None:
        """Intercept nav / Escape keys while the Input holds focus.

        Arrow keys and Ctrl+N/P aren't bound by Input so they bubble up
        to here. Escape isn't bound by Input either. Enter IS bound by
        Input (fires Submitted), so it's handled via on_input_submitted
        below rather than here.
        """
        key = event.key
        if key == "escape":
            event.stop()
            event.prevent_default()
            self.dismiss(None)
        elif key in ("down", "ctrl+n"):
            event.stop()
            event.prevent_default()
            self._move_selection(1)
        elif key in ("up", "ctrl+p"):
            event.stop()
            event.prevent_default()
            self._move_selection(-1)
        elif key == "pagedown":
            event.stop()
            event.prevent_default()
            self._move_selection(10)
        elif key == "pageup":
            event.stop()
            event.prevent_default()
            self._move_selection(-10)
        elif key == "ctrl+x":
            event.stop()
            event.prevent_default()
            self.action_clear_search_history()

    def action_clear_search_history(self) -> None:
        input_widget = self.query_one("#search-input", Input)
        if (input_widget.value or "").strip():
            input_widget.value = ""
            return
        clear_recent_searches(self.config)
        self._show_empty_state()

    def action_open_result(self) -> None:
        """Enter → open the highlighted result.

        Fires via the screen-level priority Enter binding. As a safety
        net, ``on_input_submitted`` below catches the Input.Submitted
        message too — either path lands at _open_selected.
        """
        self._open_selected()

    def on_input_submitted(self, event) -> None:
        """Safety net: if Input's own enter binding fires first, its
        Submitted message bubbles up to here. Treat it the same as the
        screen-level binding — dismiss with the highlighted row.
        """
        try:
            if event.input.id != "search-input":
                return
        except AttributeError:
            return
        event.stop()
        self._open_selected()

    def _move_selection(self, delta: int) -> None:
        lv = self.query_one("#search-results", ListView)
        count = len(lv.children)
        if count == 0:
            return
        if lv.index is None:
            lv.index = 0 if delta > 0 else count - 1
            return
        new_idx = max(0, min(count - 1, lv.index + delta))
        lv.index = new_idx
        if count and new_idx >= count - 3:
            self._load_more_results()

    def _open_selected(self) -> None:
        """Dismiss with (session_id, uuid, search_terms).

        Reads ``children[index]`` directly rather than ``highlighted_child``
        because ``highlighted_child`` may not be synchronously up to date
        after a programmatic ``index`` change — the reactive update lands
        on the next message cycle, not before we're called.

        ``search_terms`` is the raw query parsed into prefix-stripped
        words so the caller can inline-highlight matches in the target
        widget. Filter tokens (``project:``, ``user:``, ``assistant:``)
        are not included.
        """
        lv = self.query_one("#search-results", ListView)
        if not lv.children:
            return
        idx = lv.index if lv.index is not None else 0
        if idx < 0 or idx >= len(lv.children):
            idx = 0
        item = lv.children[idx]
        if isinstance(item, RecentSearchItem):
            input_widget = self.query_one("#search-input", Input)
            input_widget.value = item.query
            input_widget.focus()
            return
        if not isinstance(item, SearchResultItem):
            return

        raw_query = ""
        try:
            raw_query = self.query_one("#search-input", Input).value or ""
        except Exception:
            pass
        spec = _build_fts_query(
            raw_query,
            current_session_id=self.current_session_id,
        )
        terms = list(spec.terms)
        save_recent_search(self.config, raw_query)

        row = item.result
        self.dismiss((row["session_id"], row["uuid"], terms))

