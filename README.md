# gpu-usage-ntfy

Rust 常驻工具：持续检测所有 NVIDIA GPU 的空闲状态（低利用率 + 低显存占用），并通过 ntfy 推送通知。

## 功能
- 检测所有 GPU：核心利用率 + 显存占用
- 基于“空闲阈值”策略通知：连续样本 / 冷却时间 / 恢复通知
- 支持免打扰时间段（quiet hours，含跨日）
- 支持 ntfy 自定义服务器与 token（含环境变量注入）
- 发送失败自动重试（指数退避）

## 依赖
- Linux + NVIDIA 驱动
- NVML 动态库（通常随驱动提供）

## 使用
1. 复制配置
   ```bash
   cp config.example.toml config.toml
   ```
2. 配置 token（可选）
   ```bash
   export NTFY_TOKEN='tk_xxx'
   ```
3. 运行
   ```bash
   cargo run --release -- --config config.toml
   ```

## 日志
默认输出 info 级别日志，可用 `RUST_LOG` 调整，例如：
```bash
RUST_LOG=debug cargo run -- --config config.toml
```
