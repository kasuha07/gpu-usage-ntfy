# gpu-usage-ntfy

一个面向 Linux + NVIDIA GPU 环境的 Rust 常驻监控工具：通过 NVML 持续采集 GPU 利用率与显存占用，并基于可配置阈值推断 GPU 是否处于“空闲”状态；当满足策略条件时通过 ntfy 发送通知，并在 GPU 恢复繁忙时按策略发送恢复通知。

## 适用场景

- 你在共享服务器、工作站或家用机器上等待 GPU 空出来
- 你希望用 ntfy 在手机或桌面端接收 GPU 空闲提醒
- 你需要一个可长期运行、支持热重载配置的轻量守护进程

## 核心特性

- **多卡监控**：遍历当前可见的全部 NVIDIA GPU
- **双指标判定**：基于核心利用率与显存占用率阈值推断空闲状态
- **可配置策略**：支持 `any` / `both` 触发模式、连续样本判定、恢复通知、重复提醒冷却
- **quiet hours 免打扰**：支持多个时间窗，且支持跨日
- **ntfy 推送**：支持自定义服务器、topic、priority、tags、token / `token_env`
- **安全默认值**：默认要求 ntfy 使用 `https://`；仅在显式开启 `allow_insecure_http = true` 时允许明文 HTTP
- **可靠发送**：对请求错误和可重试 HTTP 状态码自动重试，采用指数退避
- **热重载配置**：运行中修改 `config.toml`，下一次采样周期自动重载；若新配置无效，则继续使用旧配置
- **运维脚本齐全**：包含前台运行、tmux 后台运行、systemd 安装、诊断、常见问题修复与调试信息采集脚本

## 运行要求

- Linux
- NVIDIA 驱动
- NVML 动态库（通常随驱动提供）
- Rust / Cargo

> 当前实现依赖 NVML，因此仅适用于 NVIDIA GPU 环境。

> 另外，当前程序不会从任意系统搜索路径动态查找 NVML；它会尝试一组内置的可信绝对路径。如果你的驱动把 `libnvidia-ml.so` 安装在非标准位置，请优先使用调试脚本确认可见性。

## 快速开始

### 1. 克隆并进入项目目录

```bash
git clone https://github.com/kasuha07/gpu-usage-ntfy.git
cd gpu-usage-ntfy
```

### 2. 准备配置文件

```bash
cp config.example.toml config.toml
```

`config.toml` 默认已被 `.gitignore` 忽略，适合保留本地私有配置。

### 3. 配置 ntfy

> **推荐做法**：优先使用 `token_env = "NTFY_TOKEN"` 或 `token = "${NTFY_TOKEN}"`。这两种方式更适合公开仓库、长期部署和共享机器环境。

#### 方案 A（推荐）：通过环境变量注入 token

更适合 tmux / systemd 等长期运行方式。

```toml
[ntfy]
server = "https://ntfy.sh"
topic = "gpu-usage-alerts"
token_env = "NTFY_TOKEN"
```

然后在启动前导出变量：

```bash
export NTFY_TOKEN='tk_xxx'
```

> 当前程序仅支持 `NTFY_TOKEN` 这一个环境变量名。

#### 方案 B：在 `token` 字段中引用环境变量

如果你更偏向保留单一配置入口，也可以写成：

```toml
[ntfy]
server = "https://ntfy.sh"
topic = "gpu-usage-alerts"
token = "${NTFY_TOKEN}"
```

该语法当前同样只支持 `NTFY_TOKEN`。

#### 方案 C（仅限本地单机）：直接写入 token

仅适合本机自用、短期调试、且确认 `config.toml` 不会离开本机的场景；**不建议**在公开仓库、共享机器或长期维护环境中使用。

```toml
[ntfy]
server = "https://ntfy.sh"
topic = "gpu-usage-alerts"
token = "tk_xxx"
```

#### 公共 topic 的情况

如果你使用的是 `ntfy.sh` 公共 topic，通常不需要 token。此时可删除 `token` / `token_env`，避免因无效认证导致 `403 Forbidden`。

### 4. 运行程序

#### 直接通过脚本运行

```bash
./scripts/run-monitor.sh
```

如需显式指定配置文件路径：

```bash
./scripts/run-monitor.sh /absolute/path/to/config.toml
```

#### 直接通过 Cargo 运行

```bash
cargo run --release -- --config ./config.toml
```

### 5. 可选：收紧配置文件权限

如果 `config.toml` 中包含 token，建议限制为仅当前用户可读：

```bash
chmod 600 config.toml
```

## 配置说明

完整示例见 [`config.example.toml`](./config.example.toml)。

### `[monitor]`

| 字段 | 说明 |
| --- | --- |
| `interval_seconds` | 采样周期（秒） |
| `send_startup_notification` | 启动时是否发送通知；程序收到 `Ctrl+C` 退出时仍会发送退出通知 |
| `sample_log` | 是否输出每次采样日志 |

### `[ntfy]`

| 字段 | 说明 |
| --- | --- |
| `server` | ntfy 服务器地址，默认 `https://ntfy.sh` |
| `topic` | 推送目标 topic |
| `token` | 直接写入配置文件的 token；也支持写成 `${NTFY_TOKEN}` 引用环境变量 |
| `token_env` | 从环境变量读取 token，目前仅支持 `NTFY_TOKEN` |
| `allow_insecure_http` | 默认为 `false`；如使用内网明文 HTTP，必须显式开启 |
| `title_prefix` | 通知标题前缀 |
| `priority` | 消息优先级，范围 `1 ~ 5` |
| `tags` | ntfy 标签列表 |
| `timeout_seconds` | 单次请求超时 |
| `max_retries` | 最大发送尝试次数（含首次请求） |
| `retry_initial_backoff_millis` | 首次重试退避时间，后续指数退避 |

### `[[quiet_hours]]`

可配置多个免打扰时间段，支持跨日，例如：

```toml
[[quiet_hours]]
start = "22:00"
end = "08:00"
```

### `[policy]`

| 字段 | 说明 |
| --- | --- |
| `gpu_util_percent` | GPU 利用率低于等于该值时视为满足空闲条件 |
| `memory_util_percent` | 显存占用率低于等于该值时视为满足空闲条件 |
| `trigger_mode` | `any` 表示任一指标满足即可；`both` 表示两项都满足 |
| `trigger_after_consecutive_samples` | 连续满足多少次后发送空闲通知 |
| `recovery_after_consecutive_samples` | 连续不满足多少次后发送恢复通知 |
| `repeat_idle_notifications` | 空闲期间是否重复提醒 |
| `resend_cooldown_seconds` | 重复提醒冷却时间（秒） |
| `send_recovery` | GPU 恢复繁忙时是否发送恢复通知 |
| `suppress_in_quiet_hours` | quiet hours 内是否抑制通知 |

## 运行方式

### 前台运行

```bash
./scripts/run-monitor.sh
```

如果需要更多日志：

```bash
RUST_LOG=debug ./scripts/run-monitor.sh
```

### tmux 后台运行

如果使用 `token_env`，请先在同一个 shell 中导出 `NTFY_TOKEN`：

```bash
export NTFY_TOKEN='tk_xxx'
./scripts/tmux-start.sh
```

附加到会话：

```bash
tmux -L gpu-usage-ntfy attach -t gpu-usage-ntfy
```

停止 tmux 后台进程：

```bash
tmux -L gpu-usage-ntfy kill-server
```

> `scripts/tmux-start.sh` 会拒绝复用已存在的同名 tmux socket，以避免旧环境变量残留。

### systemd 常驻部署

> 在运行任何 `sudo ./scripts/*.sh` 之前，请先检查脚本内容，并尽量基于你信任的 tag / commit 执行。

交互式安装并启动：

```bash
sudo ./scripts/install-systemd.sh
```

安装脚本会根据交互输入完成以下动作：

- 按需构建 `target/release/gpu-usage-ntfy`
- 生成或更新仓库根目录下的 `config.toml`
- 在选择 `token_env` 模式时生成或更新 `/etc/gpu-usage-ntfy.env`
- 安装并启动 systemd service
- 覆盖已有配置前自动创建 `*.bak.<timestamp>` 备份

卸载：

```bash
sudo ./scripts/uninstall-systemd.sh
```

systemd 相关文件：

- `deploy/systemd/gpu-usage-ntfy.service`
- `deploy/systemd/gpu-usage-ntfy.env.example`

> 当前仓库的 systemd 部署方式是“从源码仓库就地运行 / 构建”的模式，不是预打包发行版安装。

## 运维与排障

> 诊断、修复和调试脚本可能需要 root 权限；在公开仓库场景下，建议先审阅脚本内容，再以 `sudo` 执行。

### 诊断当前部署状态

```bash
sudo ./scripts/triage.sh
```

### 自动修复常见低风险问题

```bash
sudo ./scripts/fix-common-issues.sh
```

该脚本会处理例如以下问题：

- `config.toml` 权限收紧为 `600`
- 在 `token_env` 模式下补齐 `/etc/gpu-usage-ntfy.env` 模板
- 收紧 env 文件权限与 owner
- 安装或刷新 systemd unit
- 在认证条件满足时启动或重启服务

### 采集调试信息

```bash
sudo ./scripts/collect-debug-bundle.sh | tee gpu-usage-ntfy-debug.txt
```

该脚本会汇总 systemd 状态、最近日志、配置摘要、文件权限以及 `nvidia-smi` / NVML 可见性信息，便于排查问题。

## 常见问题

### 1. `403 Forbidden` / `authentication failed`

常见原因：

- 对公共 topic 携带了无效 token
- token 没有对应 topic 的发布权限
- token 对应的服务器与 `server` 配置不一致

建议优先确认：

- 如果是 `ntfy.sh` 公共 topic，先移除 `token` / `token_env` 再测试
- 如果是受保护 topic / 自建 ntfy，确认 token 与 server 配套且具备 publish 权限

### 2. `missing env var for ntfy token: NTFY_TOKEN`

说明配置中启用了：

```toml
token_env = "NTFY_TOKEN"
```

但当前启动环境没有提供该变量。可选择：

- 先执行 `export NTFY_TOKEN=...`
- 如果是 `ntfy.sh` 公共 topic，可直接移除 `token` / `token_env`
- 仅在明确接受本地明文凭据风险时，再考虑在 `config.toml` 中写入 `token = "..."`
- 若使用 systemd，则把 token 放入 `/etc/gpu-usage-ntfy.env`

### 3. `ntfy.server must use https:// unless ntfy.allow_insecure_http = true`

程序默认只允许 HTTPS。如果你明确处于受信任内网环境，可显式开启：

```toml
[ntfy]
server = "http://ntfy.internal"
allow_insecure_http = true
```

### 4. 想确认程序是否真的看到了 GPU

先执行：

```bash
nvidia-smi -L
```

如果仍不确定，可进一步采集调试信息：

```bash
sudo ./scripts/collect-debug-bundle.sh | tee gpu-usage-ntfy-debug.txt
```

## CLI 参考

```bash
cargo run --release -- --config /path/to/config.toml
```

编译完成后，也可以直接运行仓库内二进制：

```bash
./target/release/gpu-usage-ntfy --config /path/to/config.toml
```

当前主要参数：

- `-c, --config <CONFIG>`：指定 TOML 配置文件路径

## 时间与时区说明

程序当前固定使用 **UTC+8** 处理以下时间相关逻辑：

- 日志时间戳
- `quiet_hours` 判定

如果你的部署环境不在 UTC+8，需要在配置策略上自行考虑这一点。

## 仓库结构

```text
.
├── config.example.toml
├── deploy/systemd/
├── scripts/
└── src/
```

关键文件说明：

- `src/main.rs`：CLI 入口
- `src/app.rs`：主循环、热重载、通知触发协调
- `src/gpu.rs`：NVML 初始化与 GPU 采样
- `src/policy.rs`：空闲 / 恢复策略引擎
- `src/notify.rs`：ntfy 请求构建、重试与通知渲染
- `scripts/run-monitor.sh`：前台运行入口
- `scripts/tmux-start.sh`：tmux 后台启动
- `scripts/install-systemd.sh`：交互式安装 systemd 服务
- `scripts/triage.sh`：诊断部署状态
- `scripts/fix-common-issues.sh`：自动修复常见问题
- `scripts/collect-debug-bundle.sh`：采集调试信息

## 开发校验

```bash
cargo test
cargo build --release
cargo run --release -- --help
```

如果你准备将仓库公开，建议在提交前再次确认：

- `config.toml` 未被纳入版本控制
- 不存在真实 token、密钥或环境文件内容被提交
- 不要直接打包整个工作目录对外分享；优先通过 Git 提交历史发布代码
- `README.md` 中的运行方式与你当前维护方式一致
