use crate::config::{NotificationPolicyConfig, TriggerMode};
use crate::gpu::GpuSample;
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum PolicyEventKind {
    Alert,
    Recovery,
}

#[derive(Debug, Clone)]
pub struct PolicyEvent {
    pub gpu_index: u32,
    pub gpu_uuid: String,
    pub gpu_name: String,
    pub gpu_util_percent: f64,
    pub memory_util_percent: f64,
    pub memory_used_bytes: u64,
    pub kind: PolicyEventKind,
    pub reason: String,
    state_mutation: StateMutation,
}

#[derive(Debug, Clone)]
enum StateMutation {
    ActivateAlert {
        previous_last_alert_sent_at: Option<DateTime<Utc>>,
    },
    RefreshAlert {
        previous_last_alert_sent_at: Option<DateTime<Utc>>,
    },
    ClearAlert,
}

#[derive(Debug, Clone, Default)]
struct GpuPolicyState {
    matching_count: u32,
    non_matching_count: u32,
    alert_active: bool,
    last_alert_sent_at: Option<DateTime<Utc>>,
}

pub struct PolicyEngine {
    config: NotificationPolicyConfig,
    states: HashMap<String, GpuPolicyState>,
}

impl PolicyEngine {
    pub fn new(config: NotificationPolicyConfig) -> Self {
        Self {
            config,
            states: HashMap::new(),
        }
    }

    pub fn evaluate(&mut self, sample: &GpuSample, now: DateTime<Utc>) -> Option<PolicyEvent> {
        let memory_util_percent = sample.memory_util_percent();
        let gpu_is_idle = sample.gpu_util_percent <= self.config.gpu_util_percent;
        let memory_is_idle = memory_util_percent <= self.config.memory_util_percent;

        let is_idle = match self.config.trigger_mode {
            TriggerMode::Any => gpu_is_idle || memory_is_idle,
            TriggerMode::Both => gpu_is_idle && memory_is_idle,
        };

        let state = self.states.entry(sample.uuid.clone()).or_default();

        if is_idle {
            state.matching_count = state.matching_count.saturating_add(1);
            state.non_matching_count = 0;

            if !state.alert_active {
                if state.matching_count >= self.config.trigger_after_consecutive_samples {
                    let previous_last_alert_sent_at = state.last_alert_sent_at;
                    state.alert_active = true;
                    state.last_alert_sent_at = Some(now);

                    return Some(PolicyEvent {
                        gpu_index: sample.index,
                        gpu_uuid: sample.uuid.clone(),
                        gpu_name: sample.name.clone(),
                        gpu_util_percent: sample.gpu_util_percent,
                        memory_util_percent,
                        memory_used_bytes: sample.memory_used_bytes,
                        kind: PolicyEventKind::Alert,
                        reason: "idle_detected".to_string(),
                        state_mutation: StateMutation::ActivateAlert {
                            previous_last_alert_sent_at,
                        },
                    });
                }

                return None;
            }

            if !self.config.repeat_idle_notifications {
                return None;
            }

            let should_resend = match state.last_alert_sent_at {
                None => true,
                Some(last_sent_at) => {
                    let cooldown = Duration::seconds(self.config.resend_cooldown_seconds as i64);
                    now.signed_duration_since(last_sent_at) >= cooldown
                }
            };

            if should_resend {
                let previous_last_alert_sent_at = state.last_alert_sent_at;
                state.last_alert_sent_at = Some(now);

                return Some(PolicyEvent {
                    gpu_index: sample.index,
                    gpu_uuid: sample.uuid.clone(),
                    gpu_name: sample.name.clone(),
                    gpu_util_percent: sample.gpu_util_percent,
                    memory_util_percent,
                    memory_used_bytes: sample.memory_used_bytes,
                    kind: PolicyEventKind::Alert,
                    reason: "idle_still_detected".to_string(),
                    state_mutation: StateMutation::RefreshAlert {
                        previous_last_alert_sent_at,
                    },
                });
            }

            return None;
        }

        state.matching_count = 0;

        if state.alert_active {
            state.non_matching_count = state.non_matching_count.saturating_add(1);

            if state.non_matching_count >= self.config.recovery_after_consecutive_samples {
                state.alert_active = false;
                state.non_matching_count = 0;

                if self.config.send_recovery {
                    return Some(PolicyEvent {
                        gpu_index: sample.index,
                        gpu_uuid: sample.uuid.clone(),
                        gpu_name: sample.name.clone(),
                        gpu_util_percent: sample.gpu_util_percent,
                        memory_util_percent,
                        memory_used_bytes: sample.memory_used_bytes,
                        kind: PolicyEventKind::Recovery,
                        reason: "busy_detected".to_string(),
                        state_mutation: StateMutation::ClearAlert,
                    });
                }
            }

            return None;
        }

        state.non_matching_count = 0;
        None
    }

    pub fn on_notification_not_sent(&mut self, event: &PolicyEvent) {
        let Some(state) = self.states.get_mut(&event.gpu_uuid) else {
            return;
        };

        match &event.state_mutation {
            StateMutation::ActivateAlert {
                previous_last_alert_sent_at,
            } => {
                state.alert_active = false;
                state.last_alert_sent_at = previous_last_alert_sent_at.clone();
                state.non_matching_count = 0;
                state.matching_count = self
                    .config
                    .trigger_after_consecutive_samples
                    .saturating_sub(1);
            }
            StateMutation::RefreshAlert {
                previous_last_alert_sent_at,
            } => {
                state.last_alert_sent_at = previous_last_alert_sent_at.clone();
            }
            StateMutation::ClearAlert => {
                state.alert_active = true;
                state.non_matching_count = self
                    .config
                    .recovery_after_consecutive_samples
                    .saturating_sub(1);
            }
        }
    }

    #[cfg(test)]
    pub fn active_alerts(&self) -> usize {
        self.states.values().filter(|s| s.alert_active).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn sample(gpu_util_percent: f64, memory_util_percent: f64) -> GpuSample {
        GpuSample {
            index: 0,
            uuid: "GPU-TEST".to_string(),
            name: "Test GPU".to_string(),
            gpu_util_percent,
            memory_used_bytes: (memory_util_percent * 10.0) as u64,
            memory_total_bytes: 1000,
        }
    }

    fn policy() -> NotificationPolicyConfig {
        NotificationPolicyConfig {
            gpu_util_percent: 20.0,
            memory_util_percent: 25.0,
            trigger_mode: TriggerMode::Both,
            trigger_after_consecutive_samples: 3,
            recovery_after_consecutive_samples: 2,
            repeat_idle_notifications: false,
            resend_cooldown_seconds: 120,
            send_recovery: true,
            suppress_in_quiet_hours: true,
        }
    }

    #[test]
    fn emits_alert_after_consecutive_trigger_count() {
        let mut engine = PolicyEngine::new(policy());
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        assert!(engine.evaluate(&sample(10.0, 20.0), t0).is_none());
        assert!(
            engine
                .evaluate(&sample(12.0, 15.0), t0 + Duration::seconds(10))
                .is_none()
        );

        let event = engine
            .evaluate(&sample(8.0, 10.0), t0 + Duration::seconds(20))
            .expect("expected alert event");

        assert_eq!(event.kind, PolicyEventKind::Alert);
        assert_eq!(event.reason, "idle_detected");
    }

    #[test]
    fn continuous_idle_does_not_resend_by_default() {
        let mut cfg = policy();
        cfg.trigger_after_consecutive_samples = 1;
        let mut engine = PolicyEngine::new(cfg);
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        assert!(engine.evaluate(&sample(10.0, 20.0), t0).is_some());
        assert!(
            engine
                .evaluate(&sample(12.0, 18.0), t0 + Duration::seconds(300))
                .is_none()
        );
    }

    #[test]
    fn respects_resend_cooldown_when_repeat_enabled() {
        let mut cfg = policy();
        cfg.trigger_after_consecutive_samples = 1;
        cfg.repeat_idle_notifications = true;
        cfg.resend_cooldown_seconds = 60;
        let mut engine = PolicyEngine::new(cfg);
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        assert!(engine.evaluate(&sample(10.0, 20.0), t0).is_some());
        assert!(
            engine
                .evaluate(&sample(12.0, 18.0), t0 + Duration::seconds(30))
                .is_none()
        );

        let event = engine
            .evaluate(&sample(9.0, 21.0), t0 + Duration::seconds(61))
            .expect("expected resend alert after cooldown");

        assert_eq!(event.kind, PolicyEventKind::Alert);
        assert_eq!(event.reason, "idle_still_detected");
    }

    #[test]
    fn emits_recovery_after_consecutive_non_idle_samples() {
        let mut cfg = policy();
        cfg.trigger_after_consecutive_samples = 1;
        cfg.recovery_after_consecutive_samples = 2;
        let mut engine = PolicyEngine::new(cfg);
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        assert!(engine.evaluate(&sample(10.0, 20.0), t0).is_some());

        assert!(
            engine
                .evaluate(&sample(80.0, 80.0), t0 + Duration::seconds(10))
                .is_none()
        );

        let recovery = engine
            .evaluate(&sample(70.0, 70.0), t0 + Duration::seconds(20))
            .expect("expected recovery event");

        assert_eq!(recovery.kind, PolicyEventKind::Recovery);
        assert_eq!(recovery.reason, "busy_detected");
        assert_eq!(engine.active_alerts(), 0);
    }

    #[test]
    fn trigger_mode_both_requires_both_thresholds() {
        let mut cfg = policy();
        cfg.trigger_mode = TriggerMode::Both;
        cfg.trigger_after_consecutive_samples = 1;
        let mut engine = PolicyEngine::new(cfg);
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        assert!(engine.evaluate(&sample(10.0, 90.0), now).is_none());
        assert!(engine.evaluate(&sample(10.0, 10.0), now).is_some());
    }

    #[test]
    fn rollback_alert_when_notification_not_sent() {
        let mut cfg = policy();
        cfg.trigger_after_consecutive_samples = 1;
        let mut engine = PolicyEngine::new(cfg);
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let event = engine
            .evaluate(&sample(10.0, 20.0), t0)
            .expect("expected alert");
        assert_eq!(engine.active_alerts(), 1);

        engine.on_notification_not_sent(&event);
        assert_eq!(engine.active_alerts(), 0);

        assert!(
            engine
                .evaluate(&sample(10.0, 20.0), t0 + Duration::seconds(10))
                .is_some()
        );
    }

    #[test]
    fn rollback_recovery_when_notification_not_sent() {
        let mut cfg = policy();
        cfg.trigger_after_consecutive_samples = 1;
        cfg.recovery_after_consecutive_samples = 1;
        let mut engine = PolicyEngine::new(cfg);
        let t0 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

        let _ = engine
            .evaluate(&sample(10.0, 20.0), t0)
            .expect("expected alert");
        assert_eq!(engine.active_alerts(), 1);

        let recovery = engine
            .evaluate(&sample(80.0, 80.0), t0 + Duration::seconds(10))
            .expect("expected recovery");
        assert_eq!(engine.active_alerts(), 0);

        engine.on_notification_not_sent(&recovery);
        assert_eq!(engine.active_alerts(), 1);

        assert!(
            engine
                .evaluate(&sample(80.0, 80.0), t0 + Duration::seconds(20))
                .is_some()
        );
    }
}
