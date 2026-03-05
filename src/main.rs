mod app;
mod config;
mod gpu;
mod notify;
mod policy;

use crate::app::MonitorApp;
use crate::config::AppConfig;
use crate::gpu::NvmlSampler;
use crate::notify::NtfyNotifier;
use anyhow::{Result, anyhow};
use clap::Parser;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "gpu-usage-ntfy",
    version,
    about = "持续监控 GPU 空闲状态（基于利用率/显存阈值）并通过 ntfy 发送通知"
)]
struct Args {
    /// 配置文件路径（TOML）
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    let args = Args::parse();
    let config = AppConfig::load(&args.config).map_err(|err| {
        anyhow!(
            "failed to load config from {}: {err}",
            args.config.display()
        )
    })?;

    let sampler = NvmlSampler::new()?;
    let notifier = NtfyNotifier::new(config.ntfy.clone())?;
    let mut app = MonitorApp::new(config, sampler, notifier);

    app.run().await
}

fn init_tracing() -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .try_init()
        .map_err(|err| anyhow!("failed to initialize tracing subscriber: {err}"))?;

    Ok(())
}
