use crate::config::AppConfig;
use crate::gpu::GpuSampler;
use crate::notify::Notifier;
use crate::policy::{PolicyEngine, PolicyEvent, PolicyEventKind};
use anyhow::Result;
use chrono::Utc;
use std::time::Duration;
use tokio::select;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{error, info, warn};

pub struct MonitorApp<S, N>
where
    S: GpuSampler,
    N: Notifier,
{
    config: AppConfig,
    sampler: S,
    notifier: N,
    policy_engine: PolicyEngine,
}

impl<S, N> MonitorApp<S, N>
where
    S: GpuSampler,
    N: Notifier,
{
    pub fn new(config: AppConfig, sampler: S, notifier: N) -> Self {
        let policy_engine = PolicyEngine::new(config.policy.clone());

        Self {
            config,
            sampler,
            notifier,
            policy_engine,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        if self.config.monitor.send_startup_notification {
            self.send_startup_notification().await;
        }

        let mut ticker = interval(Duration::from_secs(self.config.monitor.interval_seconds));
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        info!(
            interval_seconds = self.config.monitor.interval_seconds,
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

    async fn send_startup_notification(&self) {
        let title = format!("{} [SYSTEM] STARTED", self.config.ntfy.title_prefix);
        let body = format!(
            "gpu idle monitor started. interval={}s trigger_mode={} idle_gpu_max={:.2}% idle_mem_max={:.2}%",
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
        let title = format!("{} [SYSTEM] STOPPED", self.config.ntfy.title_prefix);
        let body = "gpu monitor stopped gracefully".to_string();

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
        PolicyEventKind::Alert => "idle",
        PolicyEventKind::Recovery => "busy",
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
    use std::sync::{Arc, Mutex};

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
    }

    impl MockNotifier {
        fn new(fail_first_event: bool) -> Self {
            Self {
                fail_first_event: Arc::new(Mutex::new(fail_first_event)),
                event_calls: Arc::new(Mutex::new(0)),
            }
        }

        fn event_calls(&self) -> u32 {
            *self.event_calls.lock().expect("event_calls mutex poisoned")
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
            _title: &str,
            _body: &str,
            _tags: &[String],
            _priority: u8,
        ) -> Result<()> {
            Ok(())
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

    #[tokio::test]
    async fn notification_failure_rolls_back_alert_state() {
        let config = base_config();
        let sampler = SequenceSampler::new(vec![vec![sample_idle()], vec![sample_idle()]]);
        let notifier = MockNotifier::new(true);

        let mut app = MonitorApp::new(config, sampler, notifier.clone());

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

        let sampler = SequenceSampler::new(vec![vec![sample_idle()]]);
        let notifier = MockNotifier::new(false);
        let mut app = MonitorApp::new(config, sampler, notifier.clone());

        app.poll_once().await.expect("poll should finish");
        assert_eq!(notifier.event_calls(), 0);
        assert_eq!(app.policy_engine.active_alerts(), 0);
    }
}
