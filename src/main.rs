mod app;
mod config;
mod gpu;
mod notify;
mod policy;
mod timeutil;

use crate::app::{MonitorApp, NtfyNotifierFactory};
use crate::config::AppConfig;
use crate::gpu::NvmlSampler;
use anyhow::{Result, anyhow};
use clap::Parser;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

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

#[tokio::main(flavor = "current_thread")]
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
    let notifier_factory = Arc::new(NtfyNotifierFactory);
    let mut app = MonitorApp::new(&args.config, config, sampler, notifier_factory)?;

    app.run().await
}

fn init_tracing() -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_timer(Utc8Timer)
        .with_target(false)
        .compact()
        .try_init()
        .map_err(|err| anyhow!("failed to initialize tracing subscriber: {err}"))?;

    Ok(())
}

struct Utc8Timer;

impl FormatTime for Utc8Timer {
    fn format_time(&self, w: &mut Writer<'_>) -> fmt::Result {
        let now = crate::timeutil::now_utc8_rfc3339_micros();
        write!(w, "{now}")
    }
}
