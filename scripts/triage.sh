#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_PATH="${CONFIG_PATH:-$ROOT_DIR/config.toml}"
ENV_PATH="${ENV_PATH:-/etc/gpu-usage-ntfy.env}"
BINARY_PATH="${BINARY_PATH:-$ROOT_DIR/target/release/gpu-usage-ntfy}"
UNIT_NAME="${UNIT_NAME:-gpu-usage-ntfy}"
SYSTEMCTL_BIN="${SYSTEMCTL_BIN:-systemctl}"
FAILS=0
WARNS=0
CONFIG_HAS_TOKEN=no
CONFIG_USES_TOKEN_ENV=no

pass() { echo "[PASS] $*"; }
fail() { echo "[FAIL] $*"; FAILS=$((FAILS + 1)); }
warn() { echo "[WARN] $*"; WARNS=$((WARNS + 1)); }
info() { echo "[INFO] $*"; }
section() { echo; echo "=== $* ==="; }

file_mtime_epoch() {
  local path="$1"
  [[ -e "$path" ]] || return 1
  stat -c '%Y' "$path"
}

service_start_epoch() {
  local started_at
  started_at="$("$SYSTEMCTL_BIN" show "$UNIT_NAME" -p ExecMainStartTimestamp --value 2>/dev/null || true)"
  [[ -n "$started_at" && "$started_at" != "n/a" ]] || return 1
  date -d "$started_at" +%s 2>/dev/null
}

service_start_timestamp_raw() {
  local started_at
  started_at="$("$SYSTEMCTL_BIN" show "$UNIT_NAME" -p ExecMainStartTimestamp --value 2>/dev/null || true)"
  [[ -n "$started_at" && "$started_at" != "n/a" ]] || return 1
  printf '%s' "$started_at"
}

check_restart_required() {
  local path="$1"
  local label="$2"
  local service_epoch="$3"
  local file_epoch

  file_epoch="$(file_mtime_epoch "$path" || true)"
  if [[ -z "$file_epoch" ]]; then
    return 0
  fi

  if (( file_epoch > service_epoch )); then
    fail "$label is newer than the running service; restart $UNIT_NAME to apply it"
  else
    pass "running service is at least as new as $label"
  fi
}

read_env_file_token() {
  [[ -f "$ENV_PATH" ]] || return 1
  local line
  line="$(sed -n 's/^NTFY_TOKEN=//p' "$ENV_PATH" | head -n1)"
  [[ -n "$line" ]] || return 1

  if [[ "$line" == '"'*'"' && ${#line} -ge 2 ]]; then
    line="${line:1:${#line}-2}"
  fi

  local output=""
  local i
  local current_char
  for ((i = 0; i < ${#line}; i++)); do
    current_char="${line:$i:1}"
    if [[ "$current_char" == '\' && $((i + 1)) -lt ${#line} ]]; then
      ((i++))
      output+="${line:$i:1}"
    else
      output+="$current_char"
    fi
  done

  printf '%s' "$output"
}

check_running_process_matches_service() {
  local main_pid="$1"
  local -a proc_argv=()
  local config_seen=no

  mapfile -d '' -t proc_argv < "/proc/$main_pid/cmdline"

  if [[ "${proc_argv[0]:-}" == "$BINARY_PATH" ]]; then
    pass "running process uses expected binary path"
  else
    fail "running process binary path does not match $BINARY_PATH"
  fi

  for ((i = 0; i < ${#proc_argv[@]} - 1; i++)); do
    if [[ "${proc_argv[$i]}" == "--config" && "${proc_argv[$((i + 1))]}" == "$CONFIG_PATH" ]]; then
      config_seen=yes
      break
    fi
  done

  if [[ "$config_seen" == yes ]]; then
    pass "running process uses expected config path"
  else
    fail "running process does not appear to use --config $CONFIG_PATH"
  fi
}

section "config.toml"
if [[ -f "$CONFIG_PATH" ]]; then
  pass "config file exists: $CONFIG_PATH"
else
  fail "missing config file: $CONFIG_PATH"
fi

if [[ -f "$CONFIG_PATH" ]]; then
  if grep -Eq '^[[:space:]]*token[[:space:]]*=' "$CONFIG_PATH"; then
    CONFIG_HAS_TOKEN=yes
    pass 'config.toml contains an ntfy token (accepted for local ignored config)'
  fi

  if grep -Eq '^[[:space:]]*token_env[[:space:]]*=[[:space:]]*"NTFY_TOKEN"' "$CONFIG_PATH"; then
    CONFIG_USES_TOKEN_ENV=yes
    pass 'config uses token_env = "NTFY_TOKEN"'
  elif grep -Eq '^[[:space:]]*token_env[[:space:]]*=' "$CONFIG_PATH"; then
    warn 'config uses token_env with a nonstandard variable name'
  fi

  if [[ "$CONFIG_HAS_TOKEN" == no && "$CONFIG_USES_TOKEN_ENV" == no ]]; then
    warn 'config.toml has no ntfy auth configured; this is fine for public topics only'
  fi

  config_mode="$(stat -c '%a' "$CONFIG_PATH" 2>/dev/null || echo unknown)"
  if [[ "$config_mode" == "600" ]]; then
    pass "config.toml permissions are 600"
  else
    warn "config.toml permissions are $config_mode (600 recommended when secrets are present)"
  fi

  info "config summary:"
  grep -nE 'server|topic|token_env|allow_insecure_http' "$CONFIG_PATH" || true
  if [[ "$CONFIG_HAS_TOKEN" == yes ]]; then
    echo 'token_in_config=present_redacted'
  else
    echo 'token_in_config=absent'
  fi
fi

section "$ENV_PATH"
if [[ "$CONFIG_HAS_TOKEN" == yes ]]; then
  if [[ -e "$ENV_PATH" ]]; then
    pass "env file exists (optional because config.toml already contains token): $ENV_PATH"
    env_mode="$(stat -c '%a' "$ENV_PATH" 2>/dev/null || echo unknown)"
    env_owner="$(stat -c '%U:%G' "$ENV_PATH" 2>/dev/null || echo unknown)"
    if [[ "$env_mode" == "600" ]]; then
      pass "env file permissions are 600"
    else
      warn "env file permissions are $env_mode (600 recommended)"
    fi
    if [[ "$env_owner" == "root:root" ]]; then
      pass "env file ownership is root:root"
    else
      warn "env file ownership is $env_owner (root:root recommended)"
    fi
  else
    pass 'env file not required because config.toml already contains token'
  fi
else
  if [[ "$CONFIG_USES_TOKEN_ENV" == yes ]]; then
    if [[ -e "$ENV_PATH" ]]; then
      pass "env file exists: $ENV_PATH"
      env_mode="$(stat -c '%a' "$ENV_PATH" 2>/dev/null || echo unknown)"
      env_owner="$(stat -c '%U:%G' "$ENV_PATH" 2>/dev/null || echo unknown)"

      if [[ "$env_mode" == "600" ]]; then
        pass "env file permissions are 600"
      else
        fail "env file permissions are $env_mode (expected 600)"
      fi

      if [[ "$env_owner" == "root:root" ]]; then
        pass "env file ownership is root:root"
      else
        warn "env file ownership is $env_owner (root:root recommended)"
      fi

      if [[ $EUID -eq 0 ]]; then
        static_env_token="$(read_env_file_token || true)"
        if grep -Eq '^NTFY_TOKEN="?replace_with_rotated_token"?$' "$ENV_PATH"; then
          fail "NTFY_TOKEN is still the placeholder value in $ENV_PATH"
        elif [[ -n "$static_env_token" ]]; then
          pass "NTFY_TOKEN is set in $ENV_PATH"
        else
          fail "NTFY_TOKEN is missing or empty in $ENV_PATH"
        fi
      else
        warn "run with sudo to verify the contents of $ENV_PATH"
      fi
    else
      fail "missing env file: $ENV_PATH"
    fi
  else
    pass 'env file not required because config.toml is not using token_env'
  fi
fi

section "systemd unit"
if "$SYSTEMCTL_BIN" cat "$UNIT_NAME" >/dev/null 2>&1; then
  pass "systemd unit is installed: $UNIT_NAME"
else
  fail "systemd unit is not installed: $UNIT_NAME"
fi

active_state="$("$SYSTEMCTL_BIN" is-active "$UNIT_NAME" 2>/dev/null || true)"
case "$active_state" in
  active)
    pass "service is active (running)"
    ;;
  activating)
    warn "service is still activating"
    ;;
  *)
    fail "service is not active (state: ${active_state:-unknown})"
    ;;
esac

enabled_state="$("$SYSTEMCTL_BIN" is-enabled "$UNIT_NAME" 2>/dev/null || true)"
case "$enabled_state" in
  enabled)
    pass "service is enabled"
    ;;
  *)
    warn "service is not enabled (state: ${enabled_state:-unknown})"
    ;;
esac

if "$SYSTEMCTL_BIN" cat "$UNIT_NAME" 2>/dev/null | grep -F -- "$BINARY_PATH --config $CONFIG_PATH" >/dev/null; then
  pass "systemd unit points at this checkout"
else
  warn "systemd ExecStart does not appear to point at this checkout"
fi

if [[ "$active_state" == "active" ]]; then
  service_epoch="$(service_start_epoch || true)"
  if [[ -n "${service_epoch:-}" ]]; then
    pass "service start timestamp is available for restart checks"
    check_restart_required "$BINARY_PATH" "release binary" "$service_epoch"
    fragment_path="$("$SYSTEMCTL_BIN" show "$UNIT_NAME" -p FragmentPath --value 2>/dev/null || true)"
    if [[ -n "$fragment_path" && "$fragment_path" != "n/a" ]]; then
      check_restart_required "$fragment_path" "installed unit file" "$service_epoch"
    fi
    if [[ "$CONFIG_USES_TOKEN_ENV" == yes ]]; then
      env_freshness_fallback_needed=yes
    fi
  else
    warn "could not determine service start time; restart freshness checks skipped"
  fi

  main_pid="$("$SYSTEMCTL_BIN" show "$UNIT_NAME" -p MainPID --value 2>/dev/null || true)"
  if [[ -n "$main_pid" && "$main_pid" != "0" && -r "/proc/$main_pid/cmdline" ]]; then
    check_running_process_matches_service "$main_pid"
  else
    warn "could not read /proc/<pid>/cmdline to verify the live process command line"
  fi

  if [[ "$CONFIG_USES_TOKEN_ENV" == yes && $EUID -eq 0 ]]; then
    if [[ -n "$main_pid" && "$main_pid" != "0" && -r "/proc/$main_pid/environ" ]]; then
      env_token="$(read_env_file_token || true)"
      proc_token="$(tr '\0' '\n' < "/proc/$main_pid/environ" | sed -n 's/^NTFY_TOKEN=//p' | head -n1)"

      if [[ -z "$proc_token" ]]; then
        fail "running service is missing NTFY_TOKEN in its environment"
      elif [[ "$env_token" == "$proc_token" ]]; then
        pass "running service environment matches $ENV_PATH"
        env_freshness_fallback_needed=no
      else
        fail "running service is using a different NTFY_TOKEN than $ENV_PATH; restart is required"
      fi
    else
      warn "could not read /proc/<pid>/environ to compare the live NTFY_TOKEN"
    fi
  elif [[ "$CONFIG_USES_TOKEN_ENV" == yes ]]; then
    warn "run with sudo to compare the live NTFY_TOKEN against $ENV_PATH"
  fi

  if [[ "${env_freshness_fallback_needed:-no}" == yes ]]; then
    warn "falling back to env file mtime because the live NTFY_TOKEN could not be compared"
    if [[ -n "${service_epoch:-}" ]]; then
      check_restart_required "$ENV_PATH" "$ENV_PATH" "$service_epoch"
    fi
  fi
fi

section "recent logs"
logs_since=()
if [[ "$active_state" == "active" ]]; then
  current_start_raw="$(service_start_timestamp_raw || true)"
  if [[ -n "$current_start_raw" ]]; then
    logs_since=(--since "$current_start_raw")
    info "only checking logs since current service start: $current_start_raw"
  fi
fi

logs="$(journalctl -u "$UNIT_NAME" "${logs_since[@]}" -n 120 --no-pager 2>/dev/null || true)"
if [[ -z "$logs" ]]; then
  warn "no recent journal logs found for $UNIT_NAME"
else
  bad_regex='authentication failed|failed to load config|failed to parse TOML config|failed to initialize NVML|invalid ntfy\.server|ntfy\.server must use https:// unless ntfy\.allow_insecure_http = true|ntfy\.token_env only supports|missing env var for ntfy token|poll cycle failed|failed to reload config, keeping previous in-memory config'
  good_regex='GPU monitor started|gpu sample|ntfy notification sent|suppressed by quiet hours|no GPU devices found by NVML'

  if printf '%s
' "$logs" | grep -Eiq "$bad_regex"; then
    fail "recent logs contain known error markers"
    printf '%s
' "$logs" | grep -Ein "$bad_regex" | tail -n 20 || true
  else
    pass "recent logs do not contain known fatal error markers"
  fi

  if printf '%s
' "$logs" | grep -Eq "$good_regex"; then
    pass "recent logs contain healthy activity markers"
    printf '%s
' "$logs" | grep -En "$good_regex" | tail -n 20 || true
  else
    warn "recent logs do not show healthy activity markers yet"
  fi
fi

section "summary"
if (( FAILS > 0 )); then
  echo "TRIAGE RESULT: FAIL ($FAILS fail, $WARNS warn)"
  exit 1
elif (( WARNS > 0 )); then
  echo "TRIAGE RESULT: PASS WITH WARNINGS ($WARNS warn)"
else
  echo "TRIAGE RESULT: PASS"
fi
