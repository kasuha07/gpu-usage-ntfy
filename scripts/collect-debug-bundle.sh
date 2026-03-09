#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_PATH="$ROOT_DIR/config.toml"
ENV_PATH="/etc/gpu-usage-ntfy.env"
UNIT_NAME="gpu-usage-ntfy"

section() {
  echo
  echo "=== $* ==="
}

run_shell() {
  local cmd="$1"
  echo "+ $cmd"
  set +e
  bash -lc "$cmd" 2>&1
  local status=$?
  set -e
  echo "[exit: $status]"
}

shell_quote() {
  printf '%q' "$1"
}

print_env_status() {
  if [[ ! -e "$ENV_PATH" ]]; then
    echo "env_file_status=missing"
    return
  fi

  if [[ $EUID -ne 0 ]]; then
    echo "env_file_status=present_unreadable_without_root"
    return
  fi

  if grep -Eq '^NTFY_TOKEN=.+' "$ENV_PATH"; then
    if grep -Eq '^NTFY_TOKEN=replace_with_rotated_token$' "$ENV_PATH"; then
      echo 'NTFY_TOKEN=placeholder_present'
    else
      echo 'NTFY_TOKEN=present_nonempty_redacted'
    fi
  else
    echo 'NTFY_TOKEN=missing_or_empty'
  fi
}

echo "gpu-usage-ntfy debug bundle"
echo "generated_at=$(date -Is)"
echo "hostname=$(hostname 2>/dev/null || echo unknown)"
echo "user=$(id -un 2>/dev/null || echo unknown)"
echo "root_dir=$ROOT_DIR"
echo "note=secret_values_are_redacted_but_review_before_sharing"

ROOT_DIR_Q="$(shell_quote "$ROOT_DIR")"
CONFIG_PATH_Q="$(shell_quote "$CONFIG_PATH")"
ENV_PATH_Q="$(shell_quote "$ENV_PATH")"
UNIT_NAME_Q="$(shell_quote "$UNIT_NAME")"

section "system info"
run_shell 'uname -a'
run_shell 'command -v systemctl >/dev/null 2>&1 && systemctl --version | sed -n "1,3p" || echo systemctl_not_available'
run_shell 'command -v cargo >/dev/null 2>&1 && cargo --version || echo cargo_not_available'

section "repo state"
run_shell "cd $ROOT_DIR_Q && git rev-parse --short HEAD 2>/dev/null || echo git_commit_unknown"
run_shell "cd $ROOT_DIR_Q && git status --short"
run_shell "cd $ROOT_DIR_Q && ls -l target/release/gpu-usage-ntfy 2>/dev/null || echo release_binary_missing"

section "config.toml"
run_shell "if [[ -f $CONFIG_PATH_Q ]]; then stat -c '%a %U:%G %n' $CONFIG_PATH_Q; else echo config_missing; fi"
run_shell "if [[ -f $CONFIG_PATH_Q ]]; then grep -nE 'server|topic|token_env|allow_insecure_http' $CONFIG_PATH_Q; else true; fi"
run_shell "if [[ -f $CONFIG_PATH_Q ]]; then if grep -Eq '^[[:space:]]*token[[:space:]]*=' $CONFIG_PATH_Q; then echo plaintext_token_present=yes; else echo plaintext_token_present=no; fi; fi"

section "/etc/gpu-usage-ntfy.env"
run_shell "if [[ -e $ENV_PATH_Q ]]; then stat -c '%a %U:%G %n' $ENV_PATH_Q; else echo env_file_missing; fi"
print_env_status

section "systemd unit"
run_shell "if command -v systemctl >/dev/null 2>&1; then systemctl is-enabled $UNIT_NAME_Q; else echo systemctl_not_available; fi"
run_shell "if command -v systemctl >/dev/null 2>&1; then systemctl is-active $UNIT_NAME_Q; else echo systemctl_not_available; fi"
run_shell "if command -v systemctl >/dev/null 2>&1; then systemctl --no-pager --full status $UNIT_NAME_Q | sed -n '1,20p'; else echo systemctl_not_available; fi"
run_shell "if command -v systemctl >/dev/null 2>&1; then systemctl cat $UNIT_NAME_Q | grep -E '^(EnvironmentFile|ExecStart|WorkingDirectory)=' || true; else echo systemctl_not_available; fi"

section "recent journal logs"
run_shell "if command -v journalctl >/dev/null 2>&1; then journalctl -u $UNIT_NAME_Q -n 120 --no-pager; else echo journalctl_not_available; fi"

section "filtered log markers"
run_shell "if command -v journalctl >/dev/null 2>&1; then journalctl -u $UNIT_NAME_Q -n 200 --no-pager | grep -Ein 'GPU monitor started|gpu sample|ntfy notification sent|suppressed by quiet hours|no GPU devices found by NVML|error|failed|forbidden|unauthorized|missing env var|invalid|warn' || true; else echo journalctl_not_available; fi"

section "nvidia / nvml"
run_shell 'command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L || echo nvidia-smi_not_available_or_failed'
run_shell 'command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi --query-gpu=name,uuid --format=csv,noheader || echo nvidia_smi_query_unavailable'
run_shell 'command -v ldconfig >/dev/null 2>&1 && ldconfig -p | grep -i nvidia-ml || echo ldconfig_or_libnvidia_ml_unavailable'

section "trusted nvml paths from source"
run_shell "cd $ROOT_DIR_Q && grep -n '/.*libnvidia-ml\.so' src/gpu.rs || echo nvml_paths_not_found_in_source"

echo
echo '=== end debug bundle ==='
