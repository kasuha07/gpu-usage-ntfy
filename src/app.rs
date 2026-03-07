use crate::config::{AppConfig, NtfyConfig};
use crate::gpu::GpuSampler;
use crate::notify::{Notifier, NtfyNotifier};
use crate::policy::{PolicyEngine, PolicyEvent, PolicyEventKind};
use anyhow::Result;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::select;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, warn};

pub trait NotifierFactory: Send + Sync {
    fn build(&self, config: NtfyConfig) -> Result<Arc<dyn Notifier>>;
}

#[derive(Default)]
pub struct NtfyNotifierFactory;

impl NotifierFactory for NtfyNotifierFactory {
    fn build(&self, config: NtfyConfig) -> Result<Arc<dyn Notifier>> {
        Ok(Arc::new(NtfyNotifier::new(config)?))
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ReloadOutcome {
    Unchanged,
    Reloaded { interval_changed: bool },
    Failed,
}

pub struct MonitorApp<S>
where
    S: GpuSampler,
{
    config_path: PathBuf,
    config: AppConfig,
    sampler: S,
    notifier: Arc<dyn Notifier>,
    notifier_factory: Arc<dyn NotifierFactory>,
    policy_engine: PolicyEngine,
}

impl<S> MonitorApp<S>
where
    S: GpuSampler,
{
    pub fn new(
        config_path: impl AsRef<Path>,
        config: AppConfig,
        sampler: S,
        notifier_factory: Arc<dyn NotifierFactory>,
    ) -> Result<Self> {
        let notifier = notifier_factory.build(config.ntfy.clone())?;
        let policy_engine = PolicyEngine::new(config.policy.clone());

        Ok(Self {
            config_path: config_path.as_ref().to_path_buf(),
            config,
            sampler,
            notifier,
            notifier_factory,
            policy_engine,
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        if self.config.monitor.send_startup_notification {
            self.send_startup_notification().await;
        }

        let mut current_interval = self.config.monitor.interval_seconds;
        let mut ticker = interval(Duration::from_secs(current_interval));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        info!(
            interval_seconds = self.config.monitor.interval_seconds,
            config_path = %self.config_path.display(),
            "GPU monitor started"
        );

        loop {
            select! {
                _ = tokio::signal::ctrl_c() => {
                    info!("received Ctrl+C signal, shutting down monitor");
                    self.send_shutdown_notification().await;
                    break;
                }
                _ = ticker.tick() => {
                    match self.try_reload_config() {
                        ReloadOutcome::Reloaded { interval_changed } if interval_changed => {
                            current_interval = self.config.monitor.interval_seconds;
                            ticker = interval(Duration::from_secs(current_interval));
                            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
                            info!(
                                interval_seconds = current_interval,
                                "monitor interval updated from reloaded config"
                            );
                        }
                        ReloadOutcome::Failed | ReloadOutcome::Unchanged | ReloadOutcome::Reloaded { .. } => {}
                    }

                    if let Err(err) = self.poll_once().await {
                        error!(error = ?err, "poll cycle failed");
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn poll_once(&mut self) -> Result<()> {
        let samples = self.sampler.sample_all()?;

        if samples.is_empty() {
            warn!("no GPU devices found by NVML");
            return Ok(());
        }

        for sample in samples {
            let memory_util_percent = sample.memory_util_percent();

            if self.config.monitor.sample_log {
                info!(
                    gpu_index = sample.index,
                    gpu_uuid = %sample.uuid,
                    gpu_name = %sample.name,
                    gpu_util_percent = format!("{:.2}", sample.gpu_util_percent),
                    memory_util_percent = format!("{:.2}", memory_util_percent),
                    memory_used_mib = bytes_to_mib(sample.memory_used_bytes),
                    memory_total_mib = bytes_to_mib(sample.memory_total_bytes),
                    "gpu sample"
                );
            }

            if let Some(event) = self.policy_engine.evaluate(&sample, Utc::now()) {
                if self.is_quiet_hours_suppressed() {
                    info!(
                        gpu_uuid = %event.gpu_uuid,
                        event_kind = ?event.kind,
                        "notification suppressed by quiet hours policy"
                    );
                    self.policy_engine.on_notification_not_sent(&event);
                    continue;
                }

                if let Err(err) = self.notifier.send_event(&event).await {
                    error!(
                        error = ?err,
                        gpu_uuid = %event.gpu_uuid,
                        event_kind = ?event.kind,
                        "failed to send ntfy notification"
                    );
                    self.policy_engine.on_notification_not_sent(&event);
                } else {
                    info!(
                        gpu_uuid = %event.gpu_uuid,
                        event_kind = ?event.kind,
                        "ntfy notification sent"
                    );
                }
            }
        }

        Ok(())
    }

    fn try_reload_config(&mut self) -> ReloadOutcome {
        let new_config = match AppConfig::load(&self.config_path) {
            Ok(cfg) => cfg,
            Err(err) => {
                warn!(
                    config_path = %self.config_path.display(),
                    error = ?err,
                    "failed to reload config, keeping previous in-memory config"
                );
                return ReloadOutcome::Failed;
            }
        };

        if new_config == self.config {
            return ReloadOutcome::Unchanged;
        }

        let interval_changed =
            new_config.monitor.interval_seconds != self.config.monitor.interval_seconds;
        let ntfy_changed = new_config.ntfy != self.config.ntfy;

        let mut new_notifier = None;
        if ntfy_changed {
            match self.notifier_factory.build(new_config.ntfy.clone()) {
                Ok(notifier) => new_notifier = Some(notifier),
                Err(err) => {
                    warn!(
                        config_path = %self.config_path.display(),
                        error = ?err,
                        "failed to rebuild notifier from reloaded config, keeping previous config"
                    );
                    return ReloadOutcome::Failed;
                }
            }
        }

        self.config = new_config;
        if let Some(notifier) = new_notifier {
            self.notifier = notifier;
        }
        self.policy_engine = PolicyEngine::new(self.config.policy.clone());

        info!(
            config_path = %self.config_path.display(),
            interval_seconds = self.config.monitor.interval_seconds,
            ntfy_changed,
            "config reloaded"
        );

        ReloadOutcome::Reloaded { interval_changed }
    }

    async fn send_startup_notification(&self) {
        let title = format!("{} [系统] 已启动", self.config.ntfy.title_prefix);
        let body = format!(
            "GPU 空闲监控已启动。采样间隔={}秒，触发模式={}，空闲阈值：核心利用率≤{:.2}%，显存占用率≤{:.2}%。",
            self.config.monitor.interval_seconds,
            self.config.policy.trigger_mode,
            self.config.policy.gpu_util_percent,
            self.config.policy.memory_util_percent
        );

        let mut tags = self.config.ntfy.tags.clone();
        tags.push("startup".to_string());

        if self.is_quiet_hours_suppressed() {
            info!("startup notification suppressed by quiet hours policy");
            return;
        }

        if let Err(err) = self
            .notifier
            .send_text(&title, &body, &tags, self.config.ntfy.priority)
            .await
        {
            warn!(error = ?err, "failed to send startup notification");
        }
    }

    async fn send_shutdown_notification(&self) {
        let title = format!("{} [系统] 已停止", self.config.ntfy.title_prefix);
        let body = "GPU 监控程序已正常停止。".to_string();

        let mut tags = self.config.ntfy.tags.clone();
        tags.push("shutdown".to_string());

        if self.is_quiet_hours_suppressed() {
            info!("shutdown notification suppressed by quiet hours policy");
            return;
        }

        if let Err(err) = self
            .notifier
            .send_text(&title, &body, &tags, self.config.ntfy.priority)
            .await
        {
            warn!(error = ?err, "failed to send shutdown notification");
        }
    }

    fn is_quiet_hours_suppressed(&self) -> bool {
        self.config.policy.suppress_in_quiet_hours && self.config.now_in_quiet_hours()
    }
}

fn bytes_to_mib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0
}

#[allow(dead_code)]
fn _event_kind_to_string(kind: &PolicyEventKind) -> &'static str {
    match kind {
        PolicyEventKind::Alert => "空闲",
        PolicyEventKind::Recovery => "繁忙",
    }
}

#[allow(dead_code)]
fn _event_to_log_message(event: &PolicyEvent) -> String {
    format!(
        "{} {} gpu_util={:.2} mem_util={:.2}",
        event.gpu_name,
        _event_kind_to_string(&event.kind),
        event.gpu_util_percent,
        event.memory_util_percent
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AppConfig, MonitorConfig, NotificationPolicyConfig, NtfyConfig, QuietWindow, TriggerMode,
    };
    use crate::gpu::{GpuSample, GpuSampler};
    use crate::notify::Notifier;
    use anyhow::{Result, anyhow};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::fs;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Clone)]
    struct SequenceSampler {
        cycles: Arc<Mutex<VecDeque<Vec<GpuSample>>>>,
    }

    impl SequenceSampler {
        fn new(cycles: Vec<Vec<GpuSample>>) -> Self {
            Self {
                cycles: Arc::new(Mutex::new(VecDeque::from(cycles))),
            }
        }
    }

    impl GpuSampler for SequenceSampler {
        fn sample_all(&self) -> Result<Vec<GpuSample>> {
            let mut cycles = self.cycles.lock().expect("sampler mutex poisoned");
            Ok(cycles.pop_front().unwrap_or_default())
        }
    }

    #[derive(Clone)]
    struct MockNotifier {
        fail_first_event: Arc<Mutex<bool>>,
        event_calls: Arc<Mutex<u32>>,
        text_messages: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl MockNotifier {
        fn new(fail_first_event: bool) -> Self {
            Self {
                fail_first_event: Arc::new(Mutex::new(fail_first_event)),
                event_calls: Arc::new(Mutex::new(0)),
                text_messages: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn event_calls(&self) -> u32 {
            *self.event_calls.lock().expect("event_calls mutex poisoned")
        }

        fn text_messages(&self) -> Vec<(String, String)> {
            self.text_messages
                .lock()
                .expect("text_messages mutex poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl Notifier for MockNotifier {
        async fn send_event(&self, _event: &PolicyEvent) -> Result<()> {
            let mut calls = self.event_calls.lock().expect("event_calls mutex poisoned");
            *calls += 1;

            let mut fail_first = self
                .fail_first_event
                .lock()
                .expect("fail_first_event mutex poisoned");

            if *fail_first {
                *fail_first = false;
                return Err(anyhow!("injected send failure"));
            }

            Ok(())
        }

        async fn send_text(
            &self,
            title: &str,
            body: &str,
            _tags: &[String],
            _priority: u8,
        ) -> Result<()> {
            self.text_messages
                .lock()
                .expect("text_messages mutex poisoned")
                .push((title.to_string(), body.to_string()));
            Ok(())
        }
    }

    #[derive(Clone)]
    struct StaticNotifierFactory {
        notifier: MockNotifier,
        build_calls: Arc<Mutex<u32>>,
        fail_build: Arc<Mutex<bool>>,
    }

    impl StaticNotifierFactory {
        fn new(notifier: MockNotifier) -> Self {
            Self {
                notifier,
                build_calls: Arc::new(Mutex::new(0)),
                fail_build: Arc::new(Mutex::new(false)),
            }
        }

        fn build_calls(&self) -> u32 {
            *self.build_calls.lock().expect("build_calls mutex poisoned")
        }

        fn set_fail_build(&self, fail: bool) {
            *self.fail_build.lock().expect("fail_build mutex poisoned") = fail;
        }
    }

    impl NotifierFactory for StaticNotifierFactory {
        fn build(&self, _config: NtfyConfig) -> Result<Arc<dyn Notifier>> {
            let mut calls = self.build_calls.lock().expect("build_calls mutex poisoned");
            *calls += 1;

            if *self.fail_build.lock().expect("fail_build mutex poisoned") {
                return Err(anyhow!("injected factory failure"));
            }

            Ok(Arc::new(self.notifier.clone()))
        }
    }

    fn sample_idle() -> GpuSample {
        GpuSample {
            index: 0,
            uuid: "GPU-TEST".to_string(),
            name: "Test GPU".to_string(),
            gpu_util_percent: 5.0,
            memory_used_bytes: 50,
            memory_total_bytes: 1000,
        }
    }

    fn base_config() -> AppConfig {
        AppConfig {
            monitor: MonitorConfig {
                interval_seconds: 10,
                send_startup_notification: false,
                sample_log: false,
            },
            ntfy: NtfyConfig::default(),
            quiet_hours: Vec::new(),
            policy: NotificationPolicyConfig {
                gpu_util_percent: 20.0,
                memory_util_percent: 20.0,
                trigger_mode: TriggerMode::Both,
                trigger_after_consecutive_samples: 1,
                recovery_after_consecutive_samples: 1,
                resend_cooldown_seconds: 600,
                send_recovery: true,
                suppress_in_quiet_hours: true,
            },
        }
    }

    fn write_temp_config(content: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("gpu-usage-ntfy-test-{nanos}.toml"));
        fs::write(&path, content).expect("failed to write temp config");
        path
    }

    #[tokio::test]
    async fn notification_failure_rolls_back_alert_state() {
        let config = base_config();
        let config_path = write_temp_config("[ntfy]\ntopic = \"gpu-topic\"\n");
        let sampler = SequenceSampler::new(vec![vec![sample_idle()], vec![sample_idle()]]);
        let notifier = MockNotifier::new(true);
        let factory = StaticNotifierFactory::new(notifier.clone());

        let mut app = MonitorApp::new(config_path, config, sampler, Arc::new(factory))
            .expect("app should construct");

        app.poll_once().await.expect("first poll should finish");
        assert_eq!(notifier.event_calls(), 1);
        assert_eq!(app.policy_engine.active_alerts(), 0);

        app.poll_once().await.expect("second poll should finish");
        assert_eq!(notifier.event_calls(), 2);
        assert_eq!(app.policy_engine.active_alerts(), 1);
    }

    #[tokio::test]
    async fn quiet_hours_suppression_does_not_commit_alert_state() {
        let mut config = base_config();
        config.quiet_hours = vec![QuietWindow {
            start: crate::config::ClockTime::from_hhmm_for_test(0, 0),
            end: crate::config::ClockTime::from_hhmm_for_test(0, 0),
        }];

        let config_path = write_temp_config("[ntfy]\ntopic = \"gpu-topic\"\n");
        let sampler = SequenceSampler::new(vec![vec![sample_idle()]]);
        let notifier = MockNotifier::new(false);
        let factory = StaticNotifierFactory::new(notifier.clone());
        let mut app = MonitorApp::new(config_path, config, sampler, Arc::new(factory))
            .expect("app should construct");

        app.poll_once().await.expect("poll should finish");
        assert_eq!(notifier.event_calls(), 0);
        assert_eq!(app.policy_engine.active_alerts(), 0);
    }

    #[tokio::test]
    async fn startup_notification_uses_chinese_content() {
        let mut config = base_config();
        config.monitor.send_startup_notification = true;
        config.ntfy.title_prefix = "GPU 监控".to_string();

        let config_path = write_temp_config("[ntfy]\ntopic = \"gpu-topic\"\n");
        let sampler = SequenceSampler::new(vec![]);
        let notifier = MockNotifier::new(false);
        let factory = StaticNotifierFactory::new(notifier.clone());
        let app = MonitorApp::new(config_path, config, sampler, Arc::new(factory))
            .expect("app should construct");

        app.send_startup_notification().await;

        let messages = notifier.text_messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].0.contains("[系统] 已启动"));
        assert!(messages[0].1.contains("GPU 空闲监控已启动"));
    }

    #[tokio::test]
    async fn shutdown_notification_uses_chinese_content() {
        let mut config = base_config();
        config.ntfy.title_prefix = "GPU 监控".to_string();

        let config_path = write_temp_config("[ntfy]\ntopic = \"gpu-topic\"\n");
        let sampler = SequenceSampler::new(vec![]);
        let notifier = MockNotifier::new(false);
        let factory = StaticNotifierFactory::new(notifier.clone());
        let app = MonitorApp::new(config_path, config, sampler, Arc::new(factory))
            .expect("app should construct");

        app.send_shutdown_notification().await;

        let messages = notifier.text_messages();
        assert_eq!(messages.len(), 1);
        assert!(messages[0].0.contains("[系统] 已停止"));
        assert!(messages[0].1.contains("GPU 监控程序已正常停止"));
    }

    #[test]
    fn reload_applies_new_config_and_rebuilds_notifier() {
        let initial_path = write_temp_config(
            r#"
[monitor]
interval_seconds = 10
send_startup_notification = false
sample_log = false

[ntfy]
server = "https://ntfy.sh"
topic = "topic-a"
title_prefix = "GPU Monitor"
priority = 4
tags = ["gpu"]
timeout_seconds = 10
max_retries = 3
retry_initial_backoff_millis = 500

[policy]
gpu_util_percent = 20.0
memory_util_percent = 20.0
trigger_mode = "both"
trigger_after_consecutive_samples = 1
recovery_after_consecutive_samples = 1
resend_cooldown_seconds = 600
send_recovery = true
suppress_in_quiet_hours = true
"#,
        );

        let config = AppConfig::load(&initial_path).expect("initial config should load");
        let sampler = SequenceSampler::new(vec![]);
        let notifier = MockNotifier::new(false);
        let factory = StaticNotifierFactory::new(notifier);

        let mut app = MonitorApp::new(&initial_path, config, sampler, Arc::new(factory.clone()))
            .expect("app should construct");

        assert_eq!(factory.build_calls(), 1);

        fs::write(
            &initial_path,
            r#"
[monitor]
interval_seconds = 3
send_startup_notification = false
sample_log = true

[ntfy]
server = "https://ntfy.sh"
topic = "topic-b"
title_prefix = "GPU Reloaded"
priority = 5
tags = ["gpu", "reload"]
timeout_seconds = 10
max_retries = 3
retry_initial_backoff_millis = 500

[policy]
gpu_util_percent = 30.0
memory_util_percent = 25.0
trigger_mode = "any"
trigger_after_consecutive_samples = 2
recovery_after_consecutive_samples = 2
resend_cooldown_seconds = 60
send_recovery = false
suppress_in_quiet_hours = false
"#,
        )
        .expect("should update config file");

        let outcome = app.try_reload_config();
        assert_eq!(
            outcome,
            ReloadOutcome::Reloaded {
                interval_changed: true
            }
        );
        assert_eq!(factory.build_calls(), 2);
        assert_eq!(app.config.monitor.interval_seconds, 3);
        assert!(app.config.monitor.sample_log);
        assert_eq!(app.config.ntfy.topic, "topic-b");
        assert_eq!(app.config.policy.gpu_util_percent, 30.0);
        assert_eq!(app.config.policy.trigger_mode, TriggerMode::Any);
    }

    #[test]
    fn reload_invalid_config_keeps_previous_config() {
        let config_path = write_temp_config(
            r#"
[ntfy]
topic = "topic-a"
"#,
        );

        let config = AppConfig::load(&config_path).expect("initial config should load");
        let sampler = SequenceSampler::new(vec![]);
        let notifier = MockNotifier::new(false);
        let factory = StaticNotifierFactory::new(notifier);

        let mut app = MonitorApp::new(
            &config_path,
            config.clone(),
            sampler,
            Arc::new(factory.clone()),
        )
        .expect("app should construct");

        assert_eq!(factory.build_calls(), 1);

        fs::write(
            &config_path,
            r#"
[monitor]
interval_seconds = 0

[ntfy]
topic = "topic-a"
"#,
        )
        .expect("should write invalid config");

        let outcome = app.try_reload_config();
        assert_eq!(outcome, ReloadOutcome::Failed);
        assert_eq!(factory.build_calls(), 1);
        assert_eq!(app.config, config);
    }

    #[test]
    fn reload_notifier_build_failure_keeps_previous_config() {
        let config_path = write_temp_config(
            r#"
[ntfy]
topic = "topic-a"
"#,
        );

        let config = AppConfig::load(&config_path).expect("initial config should load");
        let sampler = SequenceSampler::new(vec![]);
        let notifier = MockNotifier::new(false);
        let factory = StaticNotifierFactory::new(notifier);

        let mut app = MonitorApp::new(
            &config_path,
            config.clone(),
            sampler,
            Arc::new(factory.clone()),
        )
        .expect("app should construct");

        factory.set_fail_build(true);
        fs::write(
            &config_path,
            r#"
[ntfy]
topic = "topic-b"
"#,
        )
        .expect("should write updated config");

        let outcome = app.try_reload_config();
        assert_eq!(outcome, ReloadOutcome::Failed);
        assert_eq!(app.config, config);
    }
}
