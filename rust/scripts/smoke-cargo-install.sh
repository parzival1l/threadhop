#!/usr/bin/env bash
# smoke-cargo-install.sh — Phase 6 task 6.6 smoke test.
#
# Builds the release binary and confirms it prints sensible --version and
# --help output. Does NOT actually invoke the TUI (would take over the
# terminal) or `cargo install` (heavyweight for CI). The goal is the
# minimum check: the release artifact links, exports clap's CLI, and
# answers --help without crashing.
#
# Usage:
#   bash rust/scripts/smoke-cargo-install.sh
#
# Exit codes:
#   0 — smoke passed
#   non-zero — at least one check failed

set -euo pipefail

# Resolve repo root from the script's location so the script can be invoked
# from anywhere (`bash rust/scripts/smoke-cargo-install.sh`,
# `(cd rust && bash scripts/smoke-cargo-install.sh)`, CI, etc.).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$RUST_DIR/target/release/threadhop-tui"

echo "==> building release binary"
(cd "$RUST_DIR" && cargo build --release -p threadhop-tui --quiet)

if [ ! -x "$BIN" ]; then
    echo "FAIL: release binary not found at $BIN" >&2
    exit 1
fi

echo "==> $BIN --version"
VERSION_OUT="$("$BIN" --version)"
echo "    $VERSION_OUT"
# Must contain the package name plus *some* version digit.
if ! grep -qE 'threadhop-tui[[:space:]]+[0-9]' <<<"$VERSION_OUT"; then
    echo "FAIL: --version output did not match expected shape" >&2
    echo "got: $VERSION_OUT" >&2
    exit 1
fi

echo "==> $BIN --help"
HELP_OUT="$("$BIN" --help)"
# Must list the three CLI flags we wired in Phase 6.
for needle in '--project' '--days' '--session'; do
    if ! grep -q -- "$needle" <<<"$HELP_OUT"; then
        echo "FAIL: --help did not mention $needle" >&2
        echo "---- help output ----" >&2
        echo "$HELP_OUT" >&2
        exit 1
    fi
done

echo "==> smoke test passed"
