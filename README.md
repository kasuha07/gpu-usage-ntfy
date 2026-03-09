# gpu-usage-ntfy

一个常驻的 Rust GPU 空闲监控工具：持续检测所有 NVIDIA GPU 的利用率与显存占用，在 GPU 进入“空闲”状态时通过 ntfy 推送通知，并在恢复繁忙时可选发送恢复通知。

## 功能特性
- 检测所有 NVIDIA GPU：核心利用率 + 显存占用
- 基于“空闲阈值”的通知策略：支持连续样本判定、恢复通知、重复提醒冷却
- 支持 quiet hours 免打扰时间段，且支持跨日
- 支持 ntfy 自定义服务器、topic、token
- 支持两种认证方式：
  - 本地 `config.toml` 明文 token
  - `token_env = "NTFY_TOKEN"` 环境变量注入
- 默认要求 ntfy 使用 `https://`；只有显式开启 `allow_insecure_http = true` 才允许 `http://`
- 发送失败自动重试（指数退避）
- 支持配置文件热重载
- 日志时间、quiet hours 判定、通知时间统一使用 UTC+8

## 运行依赖
- Linux
- NVIDIA 驱动
- NVML 动态库（通常随驱动提供）
- Rust / Cargo（本项目当前通过 Cargo 运行和构建）

## 快速开始
### 1) 准备配置文件
```bash
cp config.example.toml config.toml
```

### 2) 配置 ntfy 认证（任选其一）

#### 方式 A：直接写入本地 `config.toml`
适合单机、自用、且 `config.toml` 不会提交到仓库的场景。

```toml
[ntfy]
server = "https://ntfy.sh"
topic = "gpu-usage-alerts"
token = "tk_xxx"
```

#### 方式 B：通过环境变量注入
更适合 tmux / systemd 等部署方式。

配置文件中写：
```toml
[ntfy]
server = "https://ntfy.sh"
topic = "gpu-usage-alerts"
token_env = "NTFY_TOKEN"
```

当前程序只接受 `NTFY_TOKEN` 这一环境变量名。然后在启动前导出：
```bash
export NTFY_TOKEN='tk_xxx'
```

#### 公共 topic 的特殊情况
如果你使用的是 `ntfy.sh` 的公共 topic，通常**不需要 token**。此时可以删除 `token` / `token_env`，避免因无效认证导致 `403 Forbidden`。

### 3) 收紧配置文件权限
如果 `config.toml` 中包含 token，建议限制为仅当前用户可读：
```bash
chmod 600 config.toml
```

### 4) 直接运行
默认读取仓库根目录下的 `config.toml`：
```bash
./scripts/run-monitor.sh
```

如需显式指定配置路径：
```bash
./scripts/run-monitor.sh /absolute/path/to/config.toml
```

### 5) 热更新配置
程序运行期间直接编辑并保存 `config.toml`，会在**下一次采样周期**自动重载。若新配置无效，会保留旧配置并在日志中输出告警。

## 配置说明
示例配置见：[`config.example.toml`](./config.example.toml)

### ntfy 相关关键项
- `server`：ntfy 服务器地址，默认 `https://ntfy.sh`
- `topic`：推送 topic
- `token`：直接写入配置文件的 token
- `token_env`：从环境变量读取 token，目前仅支持 `NTFY_TOKEN`
- `allow_insecure_http`：默认 `false`；如使用内网明文 HTTP，必须显式设为 `true`
- `priority`：消息优先级，范围 `1 ~ 5`
- `tags`：消息标签

### 判定策略关键项
- `gpu_util_percent`：GPU 核心利用率低于等于该值时视为“空闲”
- `memory_util_percent`：显存占用率低于等于该值时视为“空闲”
- `trigger_mode`：
  - `any`：任一指标满足即触发
  - `both`：两个指标都满足才触发
- `trigger_after_consecutive_samples`：连续满足空闲条件多少次后发送通知
- `recovery_after_consecutive_samples`：连续不满足空闲条件多少次后发送恢复通知
- `repeat_idle_notifications`：空闲期间是否重复提醒
- `resend_cooldown_seconds`：重复提醒冷却时间
- `send_recovery`：是否发送恢复通知
- `suppress_in_quiet_hours`：是否在 quiet hours 内抑制通知

## 启动方式

### 方式 1：前台直接运行
```bash
./scripts/run-monitor.sh
```

配合调试日志：
```bash
RUST_LOG=debug ./scripts/run-monitor.sh
```

### 方式 2：tmux 后台运行
如果使用 `token_env`，请先在**同一个 shell** 中导出 `NTFY_TOKEN`，再启动 tmux：
```bash
export NTFY_TOKEN='tk_xxx'
./scripts/tmux-start.sh
```

附加查看：
```bash
tmux -L gpu-usage-ntfy attach -t gpu-usage-ntfy
```

停止：
```bash
tmux -L gpu-usage-ntfy kill-server
```

> `scripts/tmux-start.sh` 会拒绝复用已存在的同名 tmux socket，避免旧环境变量残留导致注入不生效。

### 方式 3：systemd 常驻部署
适合长期运行。

#### 交互式安装并启动
```bash
sudo ./scripts/install-systemd.sh
```

安装脚本会交互式询问并自动写入常用配置，例如：
- `ntfy.server`
- `ntfy.topic`
- 认证方式（无 token / `token_env = "NTFY_TOKEN"` / 明文 token）
- `priority`
- 常用 `policy` 阈值与恢复策略
- quiet hours

脚本会自动完成这些动作：
- 按需构建 `target/release/gpu-usage-ntfy`
- 生成或更新仓库根目录下的 `config.toml`
- 在选择 `token_env` 模式时生成或更新 `/etc/gpu-usage-ntfy.env`
- 安装并启动 systemd service
- 在覆盖已有配置前自动创建 `*.bak.<timestamp>` 备份

> 推荐在 systemd 下优先选择 `token_env = "NTFY_TOKEN"`，这样 token 会写入 `/etc/gpu-usage-ntfy.env`，避免直接落盘到 `config.toml`。

#### 卸载
```bash
sudo ./scripts/uninstall-systemd.sh
```

### systemd 说明
- service 模板位于：`deploy/systemd/gpu-usage-ntfy.service`
- 默认环境文件：`/etc/gpu-usage-ntfy.env`
- 安装脚本会在安装时把 service 模板中的路径占位符替换为当前仓库、配置文件、环境文件与二进制路径
- 如果你后续移动仓库目录，需要重新运行 `sudo ./scripts/install-systemd.sh`

## 运维与排障

### 一键诊断
```bash
sudo ./scripts/triage.sh
```

会检查：
- `config.toml` 是否存在、权限是否合理
- 当前是 `token` 模式还是 `token_env = "NTFY_TOKEN"` 模式
- `/etc/gpu-usage-ntfy.env` 是否存在、权限是否正确、是否仍是占位 token
- systemd unit 是否已安装 / 启用 / 运行
- 最近日志中是否出现已知错误标记

### 一键修复（安全范围内）
```bash
sudo ./scripts/fix-common-issues.sh
```

该脚本会自动处理常见低风险问题，例如：
- 将 `config.toml` 权限收紧为 `600`
- 在 `token_env` 模式下自动补齐 `/etc/gpu-usage-ntfy.env` 模板
- 收紧 env 文件权限与 owner
- 安装/刷新 systemd unit
- 在认证条件满足时启动或重启服务

不会做的事：
- 不会强制删除 `config.toml` 里的本地 token
- 不会替你生成真实 token

### 采集调试信息
```bash
sudo ./scripts/collect-debug-bundle.sh | tee gpu-usage-ntfy-debug.txt
```

该脚本会收集一份适合粘贴分享的问题诊断报告，包括：
- systemd 状态
- 最近日志
- 配置摘要
- 文件权限信息
- `nvidia-smi` / NVML 可见性

脚本会尽量隐藏 `NTFY_TOKEN` 明文值，但在对外分享前仍建议人工检查输出。

## 常见问题

### 1. `403 Forbidden` / `authentication failed`
可能原因：
- 对公共 topic 错误地携带了无效 token
- token 没有对应 topic 的发布权限
- token 对应的服务器与 `server` 配置不一致

建议排查：
- 如果是 `ntfy.sh` 公共 topic，先移除 `token` / `token_env` 再试
- 如果是受保护 topic / 自建 ntfy，确认 token 与 server 配套，且具备发布权限

### 2. 启动时报 `missing env var for ntfy token: NTFY_TOKEN`
说明你启用了：
```toml
token_env = "NTFY_TOKEN"
```
但启动环境里没有提供该变量。

可选择：
- 在当前 shell 中先 `export NTFY_TOKEN=...`
- 或改为直接在 `config.toml` 中写 `token = "..."`
- 若走 systemd，则把 token 放进 `/etc/gpu-usage-ntfy.env`

### 3. `ntfy.server must use https:// unless ntfy.allow_insecure_http = true`
程序默认只允许 HTTPS。

如果你确实是在**受信任内网**里调试明文 HTTP，可在配置中显式开启：
```toml
[ntfy]
server = "http://ntfy.internal"
allow_insecure_http = true
```

### 4. systemd 已安装但服务没启动
建议依次执行：
```bash
sudo ./scripts/triage.sh
sudo ./scripts/fix-common-issues.sh
sudo journalctl -u gpu-usage-ntfy -n 100 --no-pager
```

### 5. 想确认程序是否真的看到了 GPU
可以执行：
```bash
nvidia-smi -L
```

若仍不确定，可采集调试信息：
```bash
sudo ./scripts/collect-debug-bundle.sh | tee gpu-usage-ntfy-debug.txt
```

## 相关文件
- `config.example.toml`：配置示例
- `scripts/run-monitor.sh`：前台运行入口
- `scripts/tmux-start.sh`：tmux 后台启动
- `scripts/install-systemd.sh`：交互式安装并启动 systemd 服务
- `scripts/uninstall-systemd.sh`：卸载 systemd 服务
- `scripts/triage.sh`：诊断当前部署状态
- `scripts/fix-common-issues.sh`：自动修复常见安全/部署问题
- `scripts/collect-debug-bundle.sh`：采集调试信息
- `deploy/systemd/gpu-usage-ntfy.service`：systemd unit 模板（安装时会注入当前仓库/配置/env/二进制路径）
- `deploy/systemd/gpu-usage-ntfy.env.example`：systemd 环境文件模板（token_env 模式使用）
