#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SESSION_NAME="${1:-gpu-usage-ntfy}"
CONFIG_PATH="${2:-$ROOT_DIR/config.toml}"
TMUX_SOCKET="${TMUX_SOCKET:-gpu-usage-ntfy}"

shell_quote() {
  printf '%q' "$1"
}

if ! command -v tmux >/dev/null 2>&1; then
  echo "tmux is not installed" >&2
  exit 1
fi

if tmux -L "$TMUX_SOCKET" list-sessions >/dev/null 2>&1; then
  echo "tmux server already exists on socket '$TMUX_SOCKET'" >&2
  echo "to guarantee fresh environment injection, stop it first:" >&2
  echo "  tmux -L $TMUX_SOCKET kill-server" >&2
  exit 1
fi

printf -v CMD 'cd %s && exec ./scripts/run-monitor.sh %s' \
  "$(shell_quote "$ROOT_DIR")" \
  "$(shell_quote "$CONFIG_PATH")"
tmux -L "$TMUX_SOCKET" new-session -d -s "$SESSION_NAME" "$CMD"

echo "started session '$SESSION_NAME' on tmux socket '$TMUX_SOCKET'"
echo "attach with: tmux -L $TMUX_SOCKET attach -t $SESSION_NAME"
echo "stop with:   tmux -L $TMUX_SOCKET kill-server"
