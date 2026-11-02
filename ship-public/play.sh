#!/bin/sh
# Sekiro Together on Linux / Steam Deck: starts the game through me3 with the profile in
# this folder. Same as double-clicking sekiro-coop.me3, but works even when me3 is not on
# your PATH yet. Run it from a terminal in this folder:  sh play.sh
cd "$(dirname "$0")" || exit 1
ME3=$(command -v me3 || echo "$HOME/.local/bin/me3")
if [ ! -x "$ME3" ]; then
    echo "me3 not found. Install it first (one line, from the me3 docs):"
    echo "curl --proto '=https' --tlsv1.2 -sSfL https://github.com/garyttierney/me3/releases/latest/download/installer.sh | sh"
    exit 1
fi
if command -v pgrep >/dev/null 2>&1 && ! pgrep -x steam >/dev/null 2>&1; then
    echo "Steam doesn't seem to be running. If the game doesn't start, start Steam first and run this again."
fi
exec "$ME3" launch -p "$PWD/sekiro-coop.me3" "$@"
