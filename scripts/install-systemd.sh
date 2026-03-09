#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_TEMPLATE_PATH="${CONFIG_TEMPLATE_PATH:-$ROOT_DIR/config.example.toml}"
CONFIG_PATH="${CONFIG_PATH:-$ROOT_DIR/config.toml}"
ENV_PATH="${ENV_PATH:-/etc/gpu-usage-ntfy.env}"
ENV_EXAMPLE_PATH="${ENV_EXAMPLE_PATH:-$ROOT_DIR/deploy/systemd/gpu-usage-ntfy.env.example}"
SERVICE_TEMPLATE_PATH="${SERVICE_TEMPLATE_PATH:-$ROOT_DIR/deploy/systemd/gpu-usage-ntfy.service}"
SERVICE_DST="${SERVICE_DST:-/etc/systemd/system/gpu-usage-ntfy.service}"
BINARY_PATH="${BINARY_PATH:-$ROOT_DIR/target/release/gpu-usage-ntfy}"
SYSTEMCTL_BIN="${SYSTEMCTL_BIN:-systemctl}"
UNIT_NAME="${UNIT_NAME:-gpu-usage-ntfy}"
declare -a EXISTING_QUIET_WINDOWS=()
declare -a SELECTED_QUIET_WINDOWS=()

pass() { echo "[PASS] $*"; }
warn() { echo "[WARN] $*"; }
info() { echo "[INFO] $*"; }
section() { echo; echo "=== $* ==="; }

shell_quote() {
  printf '%q' "$1"
}

toml_escape_string() {
  local value="$1"
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  printf '%s' "$value"
}

env_escape_string() {
  local value="$1"
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  printf '%s' "$value"
}

trim() {
  local value="$1"
  value="${value#"${value%%[![:space:]]*}"}"
  value="${value%"${value##*[![:space:]]}"}"
  printf '%s' "$value"
}

lower() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

sed_escape_replacement() {
  printf '%s' "$1" | sed -e 's/[\\&|]/\\\\&/g'
}

backup_file() {
  local path="$1"
  [[ -e "$path" ]] || return 0

  local backup_path="${path}.bak.$(date +%Y%m%d-%H%M%S)"
  cp -a "$path" "$backup_path"
  info "已备份 $(basename "$path") -> $backup_path"
}

resolve_config_owner_group() {
  if [[ -f "$CONFIG_PATH" ]]; then
    CONFIG_OWNER="$(stat -c '%U' "$CONFIG_PATH")"
    CONFIG_GROUP="$(stat -c '%G' "$CONFIG_PATH")"
    return 0
  fi

  if [[ -n "${SUDO_USER:-}" && "${SUDO_USER}" != "root" ]]; then
    CONFIG_OWNER="$SUDO_USER"
    CONFIG_GROUP="$(id -gn "$SUDO_USER")"
    return 0
  fi

  CONFIG_OWNER="root"
  CONFIG_GROUP="root"
}

toml_get_raw() {
  local file="$1"
  local section="$2"
  local key="$3"

  [[ -f "$file" ]] || return 1

  awk -v section="$section" -v key="$key" '
    function trim(s) {
      sub(/^[[:space:]]+/, "", s)
      sub(/[[:space:]]+$/, "", s)
      return s
    }

    /^[[:space:]]*#/ { next }
    /^\[\[/ {
      if (in_section) {
        exit
      }
      next
    }
    /^\[/ {
      in_section = ($0 == "[" section "]")
      next
    }
    in_section && $0 ~ ("^[[:space:]]*" key "[[:space:]]*=") {
      line = $0
      sub(/^[^=]*=[[:space:]]*/, "", line)
      sub(/[[:space:]]+#.*$/, "", line)
      print trim(line)
      exit
    }
  ' "$file"
}

toml_unquote() {
  local value
  value="$(trim "$1")"
  if [[ "$value" == '"'*'"' && ${#value} -ge 2 ]]; then
    value="${value:1:${#value}-2}"
  fi
  printf '%s' "$value"
}

source_file_for_defaults() {
  if [[ -f "$CONFIG_PATH" ]]; then
    printf '%s' "$CONFIG_PATH"
  else
    printf '%s' "$CONFIG_TEMPLATE_PATH"
  fi
}

source_raw() {
  local section="$1"
  local key="$2"
  local fallback="$3"
  local raw

  raw="$(toml_get_raw "$SOURCE_CONFIG" "$section" "$key" || true)"
  if [[ -z "$raw" ]]; then
    printf '%s' "$fallback"
  else
    printf '%s' "$raw"
  fi
}

source_string() {
  local section="$1"
  local key="$2"
  local fallback="$3"
  local raw

  raw="$(source_raw "$section" "$key" "\"$fallback\"")"
  toml_unquote "$raw"
}

source_bool() {
  local section="$1"
  local key="$2"
  local fallback="$3"
  local raw

  raw="$(trim "$(source_raw "$section" "$key" "$fallback")")"
  if [[ "$raw" == "true" || "$raw" == "false" ]]; then
    printf '%s' "$raw"
  else
    printf '%s' "$fallback"
  fi
}

source_number() {
  local section="$1"
  local key="$2"
  local fallback="$3"
  local raw

  raw="$(trim "$(source_raw "$section" "$key" "$fallback")")"
  if [[ -z "$raw" ]]; then
    printf '%s' "$fallback"
  else
    printf '%s' "$raw"
  fi
}

extract_quiet_hours_windows() {
  local file="$1"
  [[ -f "$file" ]] || return 1

  awk '
    function trim(s) {
      sub(/^[[:space:]]+/, "", s)
      sub(/[[:space:]]+$/, "", s)
      return s
    }

    function strip_quotes(s) {
      gsub(/^"/, "", s)
      gsub(/"$/, "", s)
      return s
    }

    /^\[\[quiet_hours\]\]/ {
      if (in_block && start != "" && end != "") {
        print start "|" end
      }
      in_block = 1
      start = ""
      end = ""
      next
    }

    in_block && /^\[/ {
      if (start != "" && end != "") {
        print start "|" end
      }
      in_block = 0
      start = ""
      end = ""
      exit
    }

    in_block && /^[[:space:]]*start[[:space:]]*=/ {
      line = $0
      sub(/^[^=]*=[[:space:]]*/, "", line)
      sub(/[[:space:]]+#.*$/, "", line)
      start = strip_quotes(trim(line))
      next
    }

    in_block && /^[[:space:]]*end[[:space:]]*=/ {
      line = $0
      sub(/^[^=]*=[[:space:]]*/, "", line)
      sub(/[[:space:]]+#.*$/, "", line)
      end = strip_quotes(trim(line))
      next
    }

    END {
      if (in_block && start != "" && end != "") {
        print start "|" end
      }
    }
  ' "$file"
}

quiet_hours_inline_summary() {
  local summary=""
  local window

  for window in "$@"; do
    local start="${window%%|*}"
    local end="${window#*|}"
    if [[ -n "$summary" ]]; then
      summary+=", "
    fi
    summary+="$start -> $end"
  done

  printf '%s' "$summary"
}

configure_quiet_hours_interactively() {
  local default_start="$1"
  local default_end="$2"
  local add_more="false"
  local quiet_start
  local quiet_end

  SELECTED_QUIET_WINDOWS=()

  while true; do
    prompt_time_of_day quiet_start "quiet hours 开始时间" "$default_start"
    prompt_time_of_day quiet_end "quiet hours 结束时间" "$default_end"
    SELECTED_QUIET_WINDOWS+=("$quiet_start|$quiet_end")

    prompt_yes_no add_more "继续添加下一段 quiet hours？" false
    [[ "$add_more" == "true" ]] || break
    default_start="$quiet_start"
    default_end="$quiet_end"
  done
}

print_selected_quiet_hours_summary() {
  local window

  if (( ${#SELECTED_QUIET_WINDOWS[@]} == 0 )); then
    info 'quiet_hours: disabled'
    return 0
  fi

  info "quiet_hours (${#SELECTED_QUIET_WINDOWS[@]} 段):"
  for window in "${SELECTED_QUIET_WINDOWS[@]}"; do
    info "  - ${window%%|*} -> ${window#*|}"
  done
}

load_existing_env_token() {
  [[ -f "$ENV_PATH" ]] || return 0

  awk '
    BEGIN { FS = "=" }
    $1 == "NTFY_TOKEN" {
      sub(/^[^=]*=/, "", $0)
      gsub(/^"/, "", $0)
      gsub(/"$/, "", $0)
      print $0
      exit
    }
  ' "$ENV_PATH"
}

normalize_bool_input() {
  case "$(lower "$(trim "$1")")" in
    y|yes|true|1) printf 'true' ;;
    n|no|false|0) printf 'false' ;;
    *) return 1 ;;
  esac
}

prompt_with_default() {
  local __var_name="$1"
  local prompt_label="$2"
  local default_value="${3-}"
  local input_value

  if [[ -n "$default_value" ]]; then
    read -r -p "$prompt_label [$default_value]: " input_value || exit 1
    input_value="${input_value:-$default_value}"
  else
    read -r -p "$prompt_label: " input_value || exit 1
  fi

  input_value="$(trim "$input_value")"
  printf -v "$__var_name" '%s' "$input_value"
}

prompt_yes_no() {
  local __var_name="$1"
  local prompt_label="$2"
  local default_value="$3"
  local default_hint
  local input
  local normalized

  if [[ "$default_value" == "true" ]]; then
    default_hint="Y/n"
  else
    default_hint="y/N"
  fi

  while true; do
    read -r -p "$prompt_label [$default_hint]: " input || exit 1
    input="$(trim "$input")"
    if [[ -z "$input" ]]; then
      printf -v "$__var_name" '%s' "$default_value"
      return 0
    fi

    if normalized="$(normalize_bool_input "$input")"; then
      printf -v "$__var_name" '%s' "$normalized"
      return 0
    fi

    echo "请输入 y 或 n。"
  done
}

prompt_secret_token() {
  local __var_name="$1"
  local prompt_label="$2"
  local existing_value="${3-}"
  local value

  while true; do
    if [[ -n "$existing_value" ]]; then
      read -r -s -p "$prompt_label [留空保留现有值]: " value || exit 1
      echo
      value="${value:-$existing_value}"
    else
      read -r -s -p "$prompt_label: " value || exit 1
      echo
    fi

    if [[ -n "$value" ]]; then
      printf -v "$__var_name" '%s' "$value"
      return 0
    fi

    echo "token 不能为空。"
  done
}

prompt_int_range() {
  local __var_name="$1"
  local prompt_label="$2"
  local default_value="$3"
  local min_value="$4"
  local max_value="$5"
  local value

  while true; do
    prompt_with_default value "$prompt_label" "$default_value"
    if [[ "$value" =~ ^[0-9]+$ ]] && (( value >= min_value && value <= max_value )); then
      printf -v "$__var_name" '%s' "$value"
      return 0
    fi

    echo "请输入 $min_value 到 $max_value 之间的整数。"
  done
}

prompt_percent() {
  local __var_name="$1"
  local prompt_label="$2"
  local default_value="$3"
  local value

  while true; do
    prompt_with_default value "$prompt_label" "$default_value"
    if [[ "$value" =~ ^([0-9]+([.][0-9]+)?|[.][0-9]+)$ ]] \
      && awk -v v="$value" 'BEGIN { exit !(v >= 0 && v <= 100) }'; then
      printf -v "$__var_name" '%s' "$value"
      return 0
    fi

    echo "请输入 0 到 100 之间的数字。"
  done
}

prompt_time_of_day() {
  local __var_name="$1"
  local prompt_label="$2"
  local default_value="$3"
  local value
  local hour
  local minute

  while true; do
    prompt_with_default value "$prompt_label" "$default_value"
    if [[ "$value" =~ ^([0-9]{2}):([0-9]{2})$ ]]; then
      hour="${BASH_REMATCH[1]}"
      minute="${BASH_REMATCH[2]}"
      if (( 10#$hour <= 23 && 10#$minute <= 59 )); then
        printf -v "$__var_name" '%s' "$value"
        return 0
      fi
    fi

    echo "请输入合法时间，格式为 HH:MM（例如 22:00）。"
  done
}

prompt_auth_mode() {
  local default_choice="1"
  local choice

  case "$DEFAULT_AUTH_MODE" in
    none) default_choice="1" ;;
    env) default_choice="2" ;;
    config) default_choice="3" ;;
  esac

  while true; do
    echo "认证方式："
    echo "  1) 无 token（适合 ntfy.sh 公共 topic）"
    echo "  2) token_env = \"NTFY_TOKEN\"（推荐，token 写入 $ENV_PATH）"
    echo "  3) 直接写入 config.toml"
    read -r -p "请选择认证方式 [$default_choice]: " choice || exit 1
    choice="$(trim "${choice:-$default_choice}")"
    case "$choice" in
      1) AUTH_MODE="none"; return 0 ;;
      2) AUTH_MODE="env"; return 0 ;;
      3) AUTH_MODE="config"; return 0 ;;
      *) echo "请输入 1、2 或 3。" ;;
    esac
  done
}

prompt_trigger_mode() {
  local choice
  local default_choice="2"

  case "$DEFAULT_TRIGGER_MODE" in
    any) default_choice="1" ;;
    both) default_choice="2" ;;
  esac

  while true; do
    echo "触发模式："
    echo "  1) any  - GPU 利用率或显存利用率任一满足空闲阈值即触发"
    echo "  2) both - GPU 利用率和显存利用率都满足空闲阈值才触发"
    read -r -p "请选择触发模式 [$default_choice]: " choice || exit 1
    choice="$(trim "${choice:-$default_choice}")"
    case "$choice" in
      1) TRIGGER_MODE="any"; return 0 ;;
      2) TRIGGER_MODE="both"; return 0 ;;
      *) echo "请输入 1 或 2。" ;;
    esac
  done
}

detect_default_auth_mode() {
  if [[ ! -f "$CONFIG_PATH" ]]; then
    printf 'none'
    return 0
  fi

  local token_env_raw
  local token_raw
  local token_env_value
  local token_value

  token_env_raw="$(toml_get_raw "$CONFIG_PATH" ntfy token_env || true)"
  token_raw="$(toml_get_raw "$CONFIG_PATH" ntfy token || true)"
  token_env_value="$(toml_unquote "$token_env_raw")"
  token_value="$(toml_unquote "$token_raw")"

  if [[ "$token_env_value" == "NTFY_TOKEN" || "$token_value" == '${NTFY_TOKEN}' ]]; then
    printf 'env'
  elif [[ -n "$token_value" ]]; then
    printf 'config'
  else
    printf 'none'
  fi
}

render_service_file() {
  local output_path="$1"
  local escaped_root
  local escaped_config_path
  local escaped_env_path
  local escaped_binary_path
  local escaped_run_user
  local escaped_run_group

  escaped_root="$(sed_escape_replacement "$ROOT_DIR")"
  escaped_config_path="$(sed_escape_replacement "$CONFIG_PATH")"
  escaped_env_path="$(sed_escape_replacement "$ENV_PATH")"
  escaped_binary_path="$(sed_escape_replacement "$BINARY_PATH")"
  escaped_run_user="$(sed_escape_replacement "$CONFIG_OWNER")"
  escaped_run_group="$(sed_escape_replacement "$CONFIG_GROUP")"

  sed \
    -e "s|__ROOT_DIR__|$escaped_root|g" \
    -e "s|__CONFIG_PATH__|$escaped_config_path|g" \
    -e "s|__ENV_PATH__|$escaped_env_path|g" \
    -e "s|__BINARY_PATH__|$escaped_binary_path|g" \
    -e "s|__RUN_USER__|$escaped_run_user|g" \
    -e "s|__RUN_GROUP__|$escaped_run_group|g" \
    "$SERVICE_TEMPLATE_PATH" > "$output_path"
}

write_config_file() {
  local tmp_file
  tmp_file="$(mktemp)"

  cat > "$tmp_file" <<CONFIG_EOF
[monitor]
interval_seconds = $MONITOR_INTERVAL_SECONDS
send_startup_notification = $SEND_STARTUP_NOTIFICATION
sample_log = $SAMPLE_LOG

[ntfy]
server = "$(toml_escape_string "$NTFY_SERVER")"
topic = "$(toml_escape_string "$NTFY_TOPIC")"
CONFIG_EOF

  case "$AUTH_MODE" in
    env)
      echo 'token_env = "NTFY_TOKEN"' >> "$tmp_file"
      ;;
    config)
      echo "token = \"$(toml_escape_string "$AUTH_TOKEN")\"" >> "$tmp_file"
      ;;
  esac

  cat >> "$tmp_file" <<CONFIG_EOF
allow_insecure_http = $ALLOW_INSECURE_HTTP
title_prefix = "$(toml_escape_string "$TITLE_PREFIX")"
priority = $NTFY_PRIORITY
tags = $TAGS_RAW
timeout_seconds = $TIMEOUT_SECONDS
max_retries = $MAX_RETRIES
retry_initial_backoff_millis = $RETRY_INITIAL_BACKOFF_MILLIS
CONFIG_EOF

  if [[ "$ENABLE_QUIET_HOURS" == "true" ]]; then
    local window
    for window in "${SELECTED_QUIET_WINDOWS[@]}"; do
      local quiet_start="${window%%|*}"
      local quiet_end="${window#*|}"
      cat >> "$tmp_file" <<CONFIG_EOF

[[quiet_hours]]
start = "$(toml_escape_string "$quiet_start")"
end = "$(toml_escape_string "$quiet_end")"
CONFIG_EOF
    done
  fi

  cat >> "$tmp_file" <<CONFIG_EOF

[policy]
gpu_util_percent = $POLICY_GPU_UTIL_PERCENT
memory_util_percent = $POLICY_MEMORY_UTIL_PERCENT
trigger_mode = "$(toml_escape_string "$TRIGGER_MODE")"
trigger_after_consecutive_samples = $TRIGGER_AFTER_CONSECUTIVE_SAMPLES
recovery_after_consecutive_samples = $RECOVERY_AFTER_CONSECUTIVE_SAMPLES
repeat_idle_notifications = $REPEAT_IDLE_NOTIFICATIONS
resend_cooldown_seconds = $RESEND_COOLDOWN_SECONDS
send_recovery = $SEND_RECOVERY
suppress_in_quiet_hours = $SUPPRESS_IN_QUIET_HOURS
CONFIG_EOF

  if [[ -f "$CONFIG_PATH" ]]; then
    backup_file "$CONFIG_PATH"
  fi

  resolve_config_owner_group
  install -d -m 755 "$(dirname "$CONFIG_PATH")"
  install -o "$CONFIG_OWNER" -g "$CONFIG_GROUP" -m 600 "$tmp_file" "$CONFIG_PATH"
  rm -f "$tmp_file"
  pass "已写入 $CONFIG_PATH（owner: $CONFIG_OWNER:$CONFIG_GROUP）"
}

write_env_file_if_needed() {
  if [[ "$AUTH_MODE" != "env" ]]; then
    if [[ -f "$ENV_PATH" ]]; then
      info "保留已有 $ENV_PATH（当前配置不会使用 token_env）。"
    fi
    return 0
  fi

  local tmp_file
  tmp_file="$(mktemp)"

  cat > "$tmp_file" <<ENV_EOF
# Generated by scripts/install-systemd.sh
NTFY_TOKEN="$(env_escape_string "$AUTH_TOKEN")"
ENV_EOF

  if [[ -f "$ENV_PATH" ]]; then
    backup_file "$ENV_PATH"
  fi

  install -d -m 755 "$(dirname "$ENV_PATH")"
  install -o root -g root -m 600 "$tmp_file" "$ENV_PATH"
  rm -f "$tmp_file"
  pass "已写入 $ENV_PATH"
}

ensure_release_binary() {
  section "release binary"

  local needs_build=no

  if [[ ! -x "$BINARY_PATH" ]]; then
    needs_build=yes
    info 'release binary missing; building it now'
  elif find \
    "$ROOT_DIR/src" \
    "$ROOT_DIR/Cargo.toml" \
    "$ROOT_DIR/Cargo.lock" \
    -newer "$BINARY_PATH" \
    -print -quit | grep -q .; then
    needs_build=yes
    info 'source files are newer than the existing release binary; rebuilding'
  fi

  if [[ "$needs_build" == no ]]; then
    pass "release binary already up to date: $BINARY_PATH"
    return 0
  fi

  local build_user="${SUDO_USER:-}"
  local quoted_root_dir
  quoted_root_dir="$(shell_quote "$ROOT_DIR")"
  if [[ -n "$build_user" && "$build_user" != "root" ]]; then
    su - "$build_user" -c "cd $quoted_root_dir && cargo build --release"
  else
    cd "$ROOT_DIR"
    cargo build --release
  fi

  pass 'built release binary'
}

enable_and_restart_service() {
  "$SYSTEMCTL_BIN" enable "$UNIT_NAME" >/dev/null

  if "$SYSTEMCTL_BIN" is-active --quiet "$UNIT_NAME"; then
    "$SYSTEMCTL_BIN" restart "$UNIT_NAME"
    pass "已重启 $UNIT_NAME，使新的二进制/配置/env 生效"
  else
    "$SYSTEMCTL_BIN" start "$UNIT_NAME"
    pass "已启动 $UNIT_NAME"
  fi
}

install_and_start_service() {
  local rendered_service
  rendered_service="$(mktemp)"

  render_service_file "$rendered_service"

  if [[ -f "$SERVICE_DST" ]]; then
    backup_file "$SERVICE_DST"
  fi

  install -d -m 755 "$(dirname "$SERVICE_DST")"
  install -o root -g root -m 644 "$rendered_service" "$SERVICE_DST"
  rm -f "$rendered_service"

  "$SYSTEMCTL_BIN" daemon-reload
  enable_and_restart_service
  "$SYSTEMCTL_BIN" status --no-pager --lines=20 "$UNIT_NAME"
}

ensure_prerequisites() {
  if [[ $EUID -ne 0 ]]; then
    echo "please run as root: sudo ./scripts/install-systemd.sh" >&2
    exit 1
  fi

  if [[ ! -f "$CONFIG_TEMPLATE_PATH" ]]; then
    echo "missing config example: $CONFIG_TEMPLATE_PATH" >&2
    exit 1
  fi

  if [[ ! -f "$SERVICE_TEMPLATE_PATH" ]]; then
    echo "missing service template: $SERVICE_TEMPLATE_PATH" >&2
    exit 1
  fi

  if [[ ! -f "$ENV_EXAMPLE_PATH" ]]; then
    warn "missing env example: $ENV_EXAMPLE_PATH（不影响安装，但 token_env 模式将不再引用示例模板）"
  fi

  if ! command -v "$SYSTEMCTL_BIN" >/dev/null 2>&1; then
    echo "systemctl command not found: $SYSTEMCTL_BIN" >&2
    exit 1
  fi
}

collect_defaults() {
  SOURCE_CONFIG="$(source_file_for_defaults)"

  MONITOR_INTERVAL_SECONDS="$(source_number monitor interval_seconds 10)"
  SEND_STARTUP_NOTIFICATION="$(source_bool monitor send_startup_notification true)"
  SAMPLE_LOG="$(source_bool monitor sample_log true)"

  DEFAULT_SERVER="$(source_string ntfy server https://ntfy.sh)"
  DEFAULT_TOPIC="$(source_string ntfy topic gpu-usage-alerts)"
  DEFAULT_ALLOW_INSECURE_HTTP="$(source_bool ntfy allow_insecure_http false)"
  TITLE_PREFIX="$(source_string ntfy title_prefix 'GPU Monitor')"
  DEFAULT_PRIORITY="$(source_number ntfy priority 4)"
  TAGS_RAW="$(source_raw ntfy tags '["gpu", "monitor"]')"
  TIMEOUT_SECONDS="$(source_number ntfy timeout_seconds 10)"
  MAX_RETRIES="$(source_number ntfy max_retries 3)"
  RETRY_INITIAL_BACKOFF_MILLIS="$(source_number ntfy retry_initial_backoff_millis 500)"

  DEFAULT_POLICY_GPU_UTIL_PERCENT="$(source_number policy gpu_util_percent 20.0)"
  DEFAULT_POLICY_MEMORY_UTIL_PERCENT="$(source_number policy memory_util_percent 20.0)"
  DEFAULT_TRIGGER_MODE="$(source_string policy trigger_mode both)"
  DEFAULT_TRIGGER_AFTER_CONSECUTIVE_SAMPLES="$(source_number policy trigger_after_consecutive_samples 3)"
  DEFAULT_RECOVERY_AFTER_CONSECUTIVE_SAMPLES="$(source_number policy recovery_after_consecutive_samples 2)"
  DEFAULT_REPEAT_IDLE_NOTIFICATIONS="$(source_bool policy repeat_idle_notifications false)"
  DEFAULT_RESEND_COOLDOWN_SECONDS="$(source_number policy resend_cooldown_seconds 3600)"
  DEFAULT_SEND_RECOVERY="$(source_bool policy send_recovery true)"
  DEFAULT_SUPPRESS_IN_QUIET_HOURS="$(source_bool policy suppress_in_quiet_hours true)"

  mapfile -t EXISTING_QUIET_WINDOWS < <(extract_quiet_hours_windows "$SOURCE_CONFIG" || true)
  if (( ${#EXISTING_QUIET_WINDOWS[@]} > 0 )); then
    DEFAULT_ENABLE_QUIET_HOURS=true
    DEFAULT_QUIET_START="${EXISTING_QUIET_WINDOWS[0]%%|*}"
    DEFAULT_QUIET_END="${EXISTING_QUIET_WINDOWS[0]#*|}"
  else
    DEFAULT_ENABLE_QUIET_HOURS=false
    DEFAULT_QUIET_START='22:00'
    DEFAULT_QUIET_END='08:00'
  fi

  DEFAULT_AUTH_MODE="$(detect_default_auth_mode)"
  EXISTING_ENV_TOKEN="$(load_existing_env_token || true)"
  EXISTING_CONFIG_TOKEN=""
  if [[ -f "$CONFIG_PATH" ]]; then
    local existing_token_raw
    existing_token_raw="$(toml_get_raw "$CONFIG_PATH" ntfy token || true)"
    EXISTING_CONFIG_TOKEN="$(toml_unquote "$existing_token_raw")"
    if [[ "$EXISTING_CONFIG_TOKEN" == '${NTFY_TOKEN}' ]]; then
      EXISTING_CONFIG_TOKEN=""
    fi
  fi
}

collect_user_inputs() {
  section "interactive systemd install"
  info "将为当前仓库生成/更新："
  info "  - $CONFIG_PATH"
  info "  - $ENV_PATH（仅当你选择 token_env 模式时）"
  info "  - $SERVICE_DST"
  info "已有文件会自动备份为 *.bak.<timestamp>。"

  while true; do
    prompt_with_default NTFY_SERVER "ntfy server URL" "$DEFAULT_SERVER"
    [[ -n "$NTFY_SERVER" ]] && break
    echo "ntfy server URL 不能为空。"
  done

  while true; do
    if [[ "$NTFY_SERVER" == http://* ]]; then
      prompt_yes_no ALLOW_INSECURE_HTTP "检测到 http:// 地址，是否允许不安全 HTTP？" "$DEFAULT_ALLOW_INSECURE_HTTP"
      if [[ "$ALLOW_INSECURE_HTTP" == "true" ]]; then
        break
      fi
      echo "使用 http:// 时必须启用 allow_insecure_http，或者改成 https://。"
      continue
    fi

    ALLOW_INSECURE_HTTP=false
    break
  done

  while true; do
    prompt_with_default NTFY_TOPIC "ntfy topic" "$DEFAULT_TOPIC"
    [[ -n "$NTFY_TOPIC" ]] && break
    echo "ntfy topic 不能为空。"
  done

  prompt_auth_mode
  case "$AUTH_MODE" in
    none)
      AUTH_TOKEN=""
      ;;
    env)
      prompt_secret_token AUTH_TOKEN "NTFY token（将写入 $ENV_PATH）" "$EXISTING_ENV_TOKEN"
      ;;
    config)
      prompt_secret_token AUTH_TOKEN "NTFY token（将写入 $CONFIG_PATH）" "$EXISTING_CONFIG_TOKEN"
      ;;
  esac

  prompt_int_range NTFY_PRIORITY "ntfy priority（1-5）" "$DEFAULT_PRIORITY" 1 5
  prompt_percent POLICY_GPU_UTIL_PERCENT "GPU 利用率空闲阈值（%）" "$DEFAULT_POLICY_GPU_UTIL_PERCENT"
  prompt_percent POLICY_MEMORY_UTIL_PERCENT "显存利用率空闲阈值（%）" "$DEFAULT_POLICY_MEMORY_UTIL_PERCENT"
  prompt_trigger_mode
  prompt_int_range TRIGGER_AFTER_CONSECUTIVE_SAMPLES "连续多少次空闲样本后发送通知" "$DEFAULT_TRIGGER_AFTER_CONSECUTIVE_SAMPLES" 1 999999
  prompt_int_range RECOVERY_AFTER_CONSECUTIVE_SAMPLES "连续多少次恢复样本后发送恢复通知" "$DEFAULT_RECOVERY_AFTER_CONSECUTIVE_SAMPLES" 1 999999
  prompt_yes_no SEND_RECOVERY "发送恢复通知？" "$DEFAULT_SEND_RECOVERY"
  prompt_yes_no REPEAT_IDLE_NOTIFICATIONS "空闲期间重复提醒？" "$DEFAULT_REPEAT_IDLE_NOTIFICATIONS"
  if [[ "$REPEAT_IDLE_NOTIFICATIONS" == "true" ]]; then
    prompt_int_range RESEND_COOLDOWN_SECONDS "重复提醒冷却时间（秒）" "$DEFAULT_RESEND_COOLDOWN_SECONDS" 0 99999999
  else
    RESEND_COOLDOWN_SECONDS="$DEFAULT_RESEND_COOLDOWN_SECONDS"
  fi
  prompt_yes_no SUPPRESS_IN_QUIET_HOURS "在 quiet hours 内抑制通知？" "$DEFAULT_SUPPRESS_IN_QUIET_HOURS"
  prompt_yes_no ENABLE_QUIET_HOURS "启用 quiet hours？" "$DEFAULT_ENABLE_QUIET_HOURS"
  SELECTED_QUIET_WINDOWS=()
  if [[ "$ENABLE_QUIET_HOURS" == "true" ]]; then
    if (( ${#EXISTING_QUIET_WINDOWS[@]} > 0 )); then
      local keep_existing_quiet_hours
      local existing_quiet_hours_summary
      existing_quiet_hours_summary="$(quiet_hours_inline_summary "${EXISTING_QUIET_WINDOWS[@]}")"
      prompt_yes_no keep_existing_quiet_hours "保留现有 quiet hours（$existing_quiet_hours_summary）？" true
      if [[ "$keep_existing_quiet_hours" == "true" ]]; then
        SELECTED_QUIET_WINDOWS=("${EXISTING_QUIET_WINDOWS[@]}")
      else
        configure_quiet_hours_interactively "$DEFAULT_QUIET_START" "$DEFAULT_QUIET_END"
      fi
    else
      configure_quiet_hours_interactively "$DEFAULT_QUIET_START" "$DEFAULT_QUIET_END"
    fi
  fi

  section "配置摘要"
  info "ntfy.server: $NTFY_SERVER"
  info "ntfy.topic: $NTFY_TOPIC"
  case "$AUTH_MODE" in
    none) info 'auth mode: 无 token' ;;
    env) info "auth mode: token_env = NTFY_TOKEN（写入 $ENV_PATH）" ;;
    config) info "auth mode: 直接写入 $CONFIG_PATH" ;;
  esac
  info "priority: $NTFY_PRIORITY"
  info "policy.gpu_util_percent: $POLICY_GPU_UTIL_PERCENT"
  info "policy.memory_util_percent: $POLICY_MEMORY_UTIL_PERCENT"
  info "policy.trigger_mode: $TRIGGER_MODE"
  info "policy.trigger_after_consecutive_samples: $TRIGGER_AFTER_CONSECUTIVE_SAMPLES"
  info "policy.recovery_after_consecutive_samples: $RECOVERY_AFTER_CONSECUTIVE_SAMPLES"
  info "policy.repeat_idle_notifications: $REPEAT_IDLE_NOTIFICATIONS"
  if [[ "$REPEAT_IDLE_NOTIFICATIONS" == "true" ]]; then
    info "policy.resend_cooldown_seconds: $RESEND_COOLDOWN_SECONDS"
  fi
  info "policy.send_recovery: $SEND_RECOVERY"
  info "policy.suppress_in_quiet_hours: $SUPPRESS_IN_QUIET_HOURS"
  print_selected_quiet_hours_summary

  prompt_yes_no CONFIRM_INSTALL "确认写入配置并安装/启动 systemd 服务？" true
  if [[ "$CONFIRM_INSTALL" != "true" ]]; then
    echo '已取消，未写入任何文件。'
    exit 0
  fi
}

main() {
  ensure_prerequisites
  collect_defaults
  collect_user_inputs

  section "writing files"
  write_config_file
  write_env_file_if_needed

  section "installing service"
  ensure_release_binary
  install_and_start_service
}

main "$@"
