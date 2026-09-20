"""Startup cleanup of exported markdown files under ``/tmp/threadhop``.

The TUI's selection-mode export feature drops markdown files into
``/tmp/threadhop``. Without sweeping, those accumulate forever. This
module is the sweep: it preserves anything modified within the grace
period (so a half-written export isn't yanked) and anything currently
open (best-effort, via ``lsof +D``).
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path

EXPORT_DIR = Path("/tmp/threadhop")
EXPORT_RETENTION_DAYS_DEFAULT = 7
EXPORT_RECENT_GRACE_PERIOD = timedelta(hours=1)


@dataclass(frozen=True)
class ExportCleanupResult:
    """Summary of a startup cleanup pass for exported markdown files."""

    scanned: int = 0
    deleted: int = 0
    skipped_recent: int = 0
    skipped_open: int = 0
    errors: int = 0
    directory_missing: bool = False
    retention_days: int = EXPORT_RETENTION_DAYS_DEFAULT
    recent_grace_seconds: int = int(EXPORT_RECENT_GRACE_PERIOD.total_seconds())

    def debug_message(self) -> str:
        grace_hours = max(1, int(self.recent_grace_seconds / 3600))
        if self.directory_missing:
            return (
                "Export cleanup: skipped "
                f"({EXPORT_DIR} missing, retention={self.retention_days}d, "
                f"grace={grace_hours}h)"
            )
        return (
            "Export cleanup: "
            f"removed {self.deleted}, scanned {self.scanned}, "
            f"skipped_recent {self.skipped_recent}, "
            f"skipped_open {self.skipped_open}, errors {self.errors} "
            f"(retention={self.retention_days}d)"
        )

    def footer_message(self) -> str | None:
        if self.errors:
            noun = "error" if self.errors == 1 else "errors"
            return f"Export cleanup: {self.errors} {noun}"
        if self.deleted:
            noun = "stale export" if self.deleted == 1 else "stale exports"
            return f"Cleaned {self.deleted} {noun}"
        return None


def _coerce_retention_days(value) -> int:
    """Return a non-negative retention window in days."""
    try:
        return max(0, int(value))
    except (TypeError, ValueError):
        return EXPORT_RETENTION_DAYS_DEFAULT


def _open_export_paths(export_dir: Path) -> set[Path]:
    """Best-effort detection of open export files under ``export_dir``.

    Uses ``lsof +D`` on macOS. Failures degrade to an empty set so
    cleanup stays non-blocking.
    """
    try:
        proc = subprocess.run(
            ["lsof", "+D", str(export_dir), "-Fn"],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError:
        return set()

    open_paths: set[Path] = set()
    for line in proc.stdout.splitlines():
        if not line.startswith("n"):
            continue
        try:
            path = Path(line[1:]).resolve()
        except Exception:
            continue
        if path.parent == export_dir.resolve() and path.suffix == ".md":
            open_paths.add(path)
    return open_paths


def cleanup_export_markdown_files(
    export_dir: Path = EXPORT_DIR,
    *,
    retention_days: int = EXPORT_RETENTION_DAYS_DEFAULT,
    recent_grace_period: timedelta = EXPORT_RECENT_GRACE_PERIOD,
    now: datetime | None = None,
    open_paths: set[Path] | None = None,
) -> ExportCleanupResult:
    """Delete stale exported markdown files from ``export_dir``.

    Files modified within ``recent_grace_period`` are always preserved,
    even when they exceed the configured retention window. Open-file
    detection is best-effort and non-fatal.
    """
    retention_days = _coerce_retention_days(retention_days)
    now = now or datetime.now()
    recent_cutoff = (now - recent_grace_period).timestamp()
    retention_cutoff = (now - timedelta(days=retention_days)).timestamp()

    if not export_dir.exists() or not export_dir.is_dir():
        return ExportCleanupResult(
            directory_missing=True,
            retention_days=retention_days,
            recent_grace_seconds=int(recent_grace_period.total_seconds()),
        )

    resolved_dir = export_dir.resolve()
    resolved_open_paths = (
        {p.resolve() for p in open_paths}
        if open_paths is not None else _open_export_paths(export_dir)
    )

    scanned = 0
    deleted = 0
    skipped_recent = 0
    skipped_open = 0
    errors = 0

    for path in export_dir.glob("*.md"):
        scanned += 1
        try:
            stat = path.stat()
            resolved_path = path.resolve()
        except OSError:
            errors += 1
            continue

        if resolved_path.parent != resolved_dir:
            continue
        if stat.st_mtime >= recent_cutoff:
            skipped_recent += 1
            continue
        if resolved_path in resolved_open_paths:
            skipped_open += 1
            continue
        if stat.st_mtime >= retention_cutoff:
            continue

        try:
            path.unlink()
            deleted += 1
        except FileNotFoundError:
            continue
        except OSError:
            errors += 1

    return ExportCleanupResult(
        scanned=scanned,
        deleted=deleted,
        skipped_recent=skipped_recent,
        skipped_open=skipped_open,
        errors=errors,
        retention_days=retention_days,
        recent_grace_seconds=int(recent_grace_period.total_seconds()),
    )
