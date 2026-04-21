#!/usr/bin/env bash
# ThreadHop installer.
#
# Usage:
#   curl -LsSf https://raw.githubusercontent.com/parzival1l/threadhop/main/install.sh | bash
#
# Env overrides:
#   THREADHOP_DIR      where to clone the repo     (default: ~/.local/share/threadhop)
#   THREADHOP_BIN_DIR  where to symlink the entry  (default: ~/.local/bin)
#   THREADHOP_REF      git ref to check out        (default: main)

set -euo pipefail

readonly REPO_URL="https://github.com/parzival1l/threadhop.git"
readonly INSTALL_DIR="${THREADHOP_DIR:-$HOME/.local/share/threadhop}"
readonly BIN_DIR="${THREADHOP_BIN_DIR:-$HOME/.local/bin}"
readonly REF="${THREADHOP_REF:-main}"

log() { printf '==> %s\n' "$*" >&2; }
err() { printf 'error: %s\n' "$*" >&2; exit 1; }

if [[ "$(uname -s)" != "Darwin" ]]; then
    err "ThreadHop currently supports macOS only (detected $(uname -s))."
fi

command -v git >/dev/null 2>&1 \
    || err "git is required. Install via 'xcode-select --install' or Homebrew."

if ! command -v uv >/dev/null 2>&1; then
    log "uv not found — installing via the official Astral installer."
    curl -LsSf https://astral.sh/uv/install.sh | sh
    # uv lands in ~/.local/bin by default; surface it for this shell.
    export PATH="$HOME/.local/bin:$PATH"
    command -v uv >/dev/null 2>&1 \
        || err "uv install completed but 'uv' is not on PATH. Add ~/.local/bin to PATH and re-run."
fi

if [[ -d "$INSTALL_DIR/.git" ]]; then
    log "Updating existing checkout at $INSTALL_DIR"
    git -C "$INSTALL_DIR" fetch --depth 1 origin "$REF"
    git -C "$INSTALL_DIR" checkout --quiet "$REF"
    git -C "$INSTALL_DIR" reset --hard --quiet "origin/$REF"
else
    log "Cloning ThreadHop ($REF) to $INSTALL_DIR"
    mkdir -p "$(dirname "$INSTALL_DIR")"
    git clone --depth 1 --branch "$REF" "$REPO_URL" "$INSTALL_DIR"
fi

mkdir -p "$BIN_DIR"
ln -sfn "$INSTALL_DIR/threadhop" "$BIN_DIR/threadhop"
log "Linked $BIN_DIR/threadhop -> $INSTALL_DIR/threadhop"

if ! echo ":$PATH:" | grep -q ":$BIN_DIR:"; then
    cat >&2 <<EOF

ThreadHop is installed, but $BIN_DIR is not on your PATH.
Add this line to your ~/.zshrc:

    export PATH="$BIN_DIR:\$PATH"

Then reload:  source ~/.zshrc

EOF
else
    log "Done. Run:  threadhop --version"
fi
