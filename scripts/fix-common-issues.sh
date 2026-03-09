#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_PATH="${CONFIG_PATH:-$ROOT_DIR/config.toml}"
ENV_PATH="${ENV_PATH:-/etc/gpu-usage-ntfy.env}"
ENV_EXAMPLE_PATH="${ENV_EXAMPLE_PATH:-$ROOT_DIR/deploy/systemd/gpu-usage-ntfy.env.example}"
SERVICE_SRC="${SERVICE_SRC:-$ROOT_DIR/deploy/systemd/gpu-usage-ntfy.service}"
SERVICE_DST="${SERVICE_DST:-/etc/systemd/system/gpu-usage-ntfy.service}"
BINARY_PATH="${BINARY_PATH:-$ROOT_DIR/target/release/gpu-usage-ntfy}"
TRIAGE_SCRIPT="${TRIAGE_SCRIPT:-$ROOT_DIR/scripts/triage.sh}"
UNIT_NAME="${UNIT_NAME:-gpu-usage-ntfy}"
SYSTEMCTL_BIN="${SYSTEMCTL_BIN:-systemctl}"
CHANGED=0
MANUAL_STEPS=0
CONFIG_HAS_TOKEN=no
CONFIG_USES_TOKEN_ENV=no

pass() { echo "[PASS] $*"; }
warn() { echo "[WARN] $*"; }
info() { echo "[INFO] $*"; }
section() { echo; echo "=== $* ==="; }

sed_escape_replacement() {
  printf '%s' "$1" | sed -e 's/[\\&|]/\\\\&/g'
}

render_service_file() {
  local output_path="$1"
  local escaped_root
  local escaped_config_path
  local escaped_env_path
  local escaped_binary_path

  escaped_root="$(sed_escape_replacement "$ROOT_DIR")"
  escaped_config_path="$(sed_escape_replacement "$CONFIG_PATH")"
  escaped_env_path="$(sed_escape_replacement "$ENV_PATH")"
  escaped_binary_path="$(sed_escape_replacement "$BINARY_PATH")"

  sed \
    -e "s|__ROOT_DIR__|$escaped_root|g" \
    -e "s|__CONFIG_PATH__|$escaped_config_path|g" \
    -e "s|__ENV_PATH__|$escaped_env_path|g" \
    -e "s|__BINARY_PATH__|$escaped_binary_path|g" \
    "$SERVICE_SRC" > "$output_path"
}

if [[ $EUID -ne 0 ]]; then
  echo "please run as root: sudo ./scripts/fix-common-issues.sh" >&2
  exit 1
fi

refresh_auth_mode() {
  if grep -Eq '^[[:space:]]*token[[:space:]]*=' "$CONFIG_PATH"; then
    CONFIG_HAS_TOKEN=yes
  else
    CONFIG_HAS_TOKEN=no
  fi

  if grep -Eq '^[[:space:]]*token_env[[:space:]]*=[[:space:]]*"NTFY_TOKEN"' "$CONFIG_PATH"; then
    CONFIG_USES_TOKEN_ENV=yes
  else
    CONFIG_USES_TOKEN_ENV=no
  fi
}

fix_config_toml() {
  section "config.toml fixes"

  if [[ ! -f "$CONFIG_PATH" ]]; then
    echo "missing config file: $CONFIG_PATH" >&2
    exit 1
  fi

  refresh_auth_mode

  if [[ "$CONFIG_HAS_TOKEN" == yes ]]; then
    pass 'config.toml contains a plaintext token; leaving it as-is per local config policy'
  elif [[ "$CONFIG_USES_TOKEN_ENV" == yes ]]; then
    pass 'config.toml already uses token_env = "NTFY_TOKEN"'
  else
    warn 'config.toml has no ntfy auth configured; this may still be fine for public topics'
  fi

  chmod 600 "$CONFIG_PATH"
  pass 'set config.toml permissions to 600'
}

fix_env_file() {
  section "/etc/gpu-usage-ntfy.env fixes"

  if [[ "$CONFIG_HAS_TOKEN" == yes ]]; then
    if [[ -f "$ENV_PATH" ]]; then
      chown root:root "$ENV_PATH"
      chmod 600 "$ENV_PATH"
      pass 'env file exists; kept it optional and tightened permissions to 600'
    else
      pass 'env file not required because config.toml already contains token'
    fi
    return 0
  fi

  if [[ "$CONFIG_USES_TOKEN_ENV" != yes ]]; then
    pass 'env file not required because config.toml is not using token_env'
    return 0
  fi

  if [[ ! -f "$ENV_PATH" ]]; then
    install -d -m 755 "$(dirname "$ENV_PATH")"
    install -o root -g root -m 600 "$ENV_EXAMPLE_PATH" "$ENV_PATH"
    CHANGED=1
    MANUAL_STEPS=1
    warn "created $ENV_PATH from template; you still need to set a real NTFY_TOKEN"
  else
    pass "$ENV_PATH already exists"
  fi

  chown root:root "$ENV_PATH"
  chmod 600 "$ENV_PATH"
  pass 'set env file ownership to root:root and permissions to 600'

  if grep -Eq '^NTFY_TOKEN=.+' "$ENV_PATH" && ! grep -Eq '^NTFY_TOKEN=replace_with_rotated_token$' "$ENV_PATH"; then
    pass 'env file contains a non-placeholder NTFY_TOKEN value'
    return 0
  fi

  MANUAL_STEPS=1
  warn 'env file still needs a real NTFY_TOKEN value before token_env mode can work correctly'
  return 1
}

ensure_release_binary() {
  section "release binary"

  if [[ -x "$BINARY_PATH" ]]; then
    pass "release binary already exists: $BINARY_PATH"
    return 0
  fi

  info 'release binary missing; building it now'
  local build_user="${SUDO_USER:-}"
  if [[ -n "$build_user" && "$build_user" != "root" ]]; then
    su - "$build_user" -c "cd '$ROOT_DIR' && cargo build --release"
  else
    cd "$ROOT_DIR"
    cargo build --release
  fi
  CHANGED=1
  pass 'built release binary'
}

install_or_refresh_unit() {
  section "systemd unit fixes"

  if ! command -v "$SYSTEMCTL_BIN" >/dev/null 2>&1; then
    echo "systemctl is not available on this machine: $SYSTEMCTL_BIN" >&2
    exit 1
  fi

  local rendered_service
  rendered_service="$(mktemp)"
  render_service_file "$rendered_service"
  install -d -m 755 "$(dirname "$SERVICE_DST")"
  install -o root -g root -m 644 "$rendered_service" "$SERVICE_DST"
  rm -f "$rendered_service"
  "$SYSTEMCTL_BIN" daemon-reload
  CHANGED=1
  pass 'installed/refreshed the systemd unit file and reloaded systemd'
}

start_or_restart_service_if_ready() {
  local env_ready="$1"
  section "service action"

  if [[ "$CONFIG_HAS_TOKEN" == yes || "$env_ready" == yes || "$CONFIG_USES_TOKEN_ENV" == no ]]; then
    "$SYSTEMCTL_BIN" enable --now "$UNIT_NAME"
    pass 'enabled and started gpu-usage-ntfy.service'
  else
    warn "skipping service start/restart until $ENV_PATH contains a real NTFY_TOKEN"
  fi
}

run_triage() {
  section "post-fix triage"
  if [[ -x "$TRIAGE_SCRIPT" ]]; then
    "$TRIAGE_SCRIPT" || true
  else
    warn 'triage script not found; skipping post-fix triage'
  fi
}

fix_config_toml
fix_env_file && ENV_READY=yes || ENV_READY=no
ensure_release_binary
install_or_refresh_unit
start_or_restart_service_if_ready "$ENV_READY"
run_triage

section "summary"
if (( CHANGED )); then
  info 'applied one or more safe automatic fixes'
else
  info 'no file changes were needed'
fi

if (( MANUAL_STEPS )); then
  warn 'manual action may still be required only if you are using token_env mode'
  exit 2
fi

pass 'common safe fixes completed'
