use crate::config::NtfyConfig;
use crate::policy::{PolicyEvent, PolicyEventKind};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Client, Request, StatusCode};
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send_events(&self, events: &[PolicyEvent]) -> Result<()>;
    async fn send_text(&self, title: &str, body: &str, tags: &[String], priority: u8)
    -> Result<()>;
}

#[derive(Clone)]
pub struct NtfyNotifier {
    client: Client,
    config: NtfyConfig,
}

impl NtfyNotifier {
    pub fn new(config: NtfyConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .build()
            .context("failed to build reqwest client")?;

        Ok(Self { client, config })
    }

    #[cfg(test)]
    fn topic_url(&self) -> String {
        format!("{}/{}", self.config.server, self.config.topic)
    }

    fn build_publish_request(
        &self,
        title: &str,
        body: &str,
        tags: &[String],
        priority: u8,
    ) -> Result<Request> {
        build_publish_request(&self.client, &self.config, title, body, tags, priority)
    }

    async fn publish(&self, title: &str, body: &str, tags: &[String], priority: u8) -> Result<()> {
        let max_retries = self.config.max_retries.max(1);
        let mut backoff = Duration::from_millis(self.config.retry_initial_backoff_millis);
        let mut last_error: Option<anyhow::Error> = None;

        for attempt in 1..=max_retries {
            let request = self.build_publish_request(title, body, tags, priority)?;

            match self.client.execute(request).await {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) => {
                    let status = response.status();
                    let message = response.text().await.unwrap_or_default();
                    let error = build_http_error(status, &message, &self.config);

                    if !is_retryable_status(status) {
                        return Err(error);
                    }

                    last_error = Some(error);
                }
                Err(err) => {
                    last_error = Some(anyhow!("failed to send ntfy notification: {err}"));
                }
            }

            if attempt < max_retries {
                warn!(
                    attempt,
                    max_retries,
                    backoff_millis = backoff.as_millis(),
                    "ntfy notification attempt failed, retrying"
                );
                sleep(backoff).await;
                backoff = backoff.saturating_mul(2);
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("failed to publish ntfy notification")))
    }
}

#[async_trait]
impl Notifier for NtfyNotifier {
    async fn send_events(&self, events: &[PolicyEvent]) -> Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let (title, body, tags, priority) = format_events_notification(&self.config, events);
        self.publish(&title, &body, &tags, priority).await
    }

    async fn send_text(
        &self,
        title: &str,
        body: &str,
        tags: &[String],
        priority: u8,
    ) -> Result<()> {
        self.publish(title, body, tags, priority).await
    }
}

pub(crate) fn build_publish_request(
    client: &Client,
    config: &NtfyConfig,
    title: &str,
    body: &str,
    tags: &[String],
    priority: u8,
) -> Result<Request> {
    let mut headers = HeaderMap::new();

    headers.insert("X-Markdown", HeaderValue::from_static("yes"));
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/markdown"));

    headers.insert(
        "X-Title",
        HeaderValue::from_str(title).context("invalid title header for ntfy request")?,
    );

    headers.insert(
        "X-Priority",
        HeaderValue::from_str(&priority.to_string())
            .context("invalid priority header for ntfy request")?,
    );

    if !tags.is_empty() {
        headers.insert(
            "X-Tags",
            HeaderValue::from_str(&tags.join(","))
                .context("invalid tags header for ntfy request")?,
        );
    }

    if let Some(token) = config.token.as_ref().filter(|t| !t.trim().is_empty()) {
        let value = format!("Bearer {}", token.trim());
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&value)
                .context("invalid authorization header for ntfy request")?,
        );
    }

    let request = client
        .post(format!(
            "{}/{}",
            config.server.trim_end_matches('/'),
            config.topic
        ))
        .headers(headers)
        .body(body.to_string())
        .build()
        .context("failed to build ntfy request")?;

    Ok(request)
}

fn is_retryable_status(status: StatusCode) -> bool {
    status.is_server_error()
        || status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
}

fn build_http_error(status: StatusCode, message: &str, config: &NtfyConfig) -> anyhow::Error {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        let token_hint = if config
            .token
            .as_ref()
            .is_some_and(|token| !token.trim().is_empty())
        {
            "当前已配置 token，请确认该 token 对此 topic 具备 publish 权限；如果是 ntfy.sh 公共 topic，可尝试移除 token。"
        } else {
            "当前未配置 token；如果 topic 受保护，请配置 token 或 token_env。"
        };

        return anyhow!(
            "ntfy authentication failed with status {} (server={} topic={}): {}. {}",
            status,
            config.server,
            config.topic,
            message,
            token_hint
        );
    }

    anyhow!("ntfy request failed with status {}: {}", status, message)
}

fn format_events_notification(
    config: &NtfyConfig,
    events: &[PolicyEvent],
) -> (String, String, Vec<String>, u8) {
    let has_alert = events
        .iter()
        .any(|event| matches!(event.kind, PolicyEventKind::Alert));
    let has_recovery = events
        .iter()
        .any(|event| matches!(event.kind, PolicyEventKind::Recovery));

    let status_text = match (has_alert, has_recovery) {
        (true, false) => "空闲",
        (false, true) => "繁忙恢复",
        _ => "状态更新",
    };

    let title = if events.len() == 1 {
        format!(
            "{} [GPU{}] {}",
            config.title_prefix, events[0].gpu_index, status_text
        )
    } else {
        format!(
            "{} [{} GPUs] {}",
            config.title_prefix,
            events.len(),
            status_text
        )
    };

    let body = format_events_markdown_table(events);

    let mut tags = config.tags.clone();
    if has_alert {
        tags.push("idle".to_string());
    }
    if has_recovery {
        tags.push("busy".to_string());
    }

    let priority = if has_alert {
        config.priority
    } else {
        config.priority.saturating_sub(1).max(1)
    };

    (title, body, dedup_tags(tags), priority)
}

fn format_events_markdown_table(events: &[PolicyEvent]) -> String {
    let mut ordered: Vec<&PolicyEvent> = events.iter().collect();
    ordered.sort_by_key(|event| event.gpu_index);

    let headers = [
        "GPU序号",
        "GPU型号",
        "状态",
        "核心利用率",
        "已使用显存(GB)",
        "原因",
    ];

    let mut rows: Vec<[String; 6]> = Vec::with_capacity(ordered.len());
    for event in ordered {
        let (status_text, _) = localized_status_and_tag(&event.kind);
        rows.push([
            event.gpu_index.to_string(),
            escape_table_cell(&abbreviate_gpu_name(&event.gpu_name)),
            status_text.to_string(),
            format!("{:.2}%", event.gpu_util_percent),
            format!("{:.2}", bytes_to_gb(event.memory_used_bytes)),
            escape_table_cell(localize_reason(event.reason.as_str())),
        ]);
    }

    let mut widths = [
        headers[0].chars().count(),
        headers[1].chars().count(),
        headers[2].chars().count(),
        headers[3].chars().count(),
        headers[4].chars().count(),
        headers[5].chars().count(),
    ];
    for row in &rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(cell.chars().count());
        }
    }

    let pad = |cell: &str, width: usize| format!("{cell:<width$}", width = width);
    let header_line = headers
        .iter()
        .enumerate()
        .map(|(idx, cell)| pad(cell, widths[idx]))
        .collect::<Vec<_>>()
        .join(" | ");
    let separator_line = widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("-|-");

    let mut lines = vec![
        "### GPU 状态明细".to_string(),
        "```".to_string(),
        header_line,
        separator_line,
    ];
    for row in rows {
        lines.push(
            row.iter()
                .enumerate()
                .map(|(idx, cell)| pad(cell, widths[idx]))
                .collect::<Vec<_>>()
                .join(" | "),
        );
    }
    lines.push("```".to_string());

    lines.join("\n")
}

fn bytes_to_gb(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / 1024.0
}

fn dedup_tags(tags: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    tags.into_iter()
        .filter(|tag| seen.insert(tag.clone()))
        .collect()
}

fn escape_table_cell(value: &str) -> String {
    value.replace('|', "\\|")
}

fn abbreviate_gpu_name(name: &str) -> String {
    let trimmed = name.trim();
    let without_vendor = trimmed.strip_prefix("NVIDIA ").unwrap_or(trimmed);
    without_vendor
        .strip_prefix("GeForce ")
        .unwrap_or(without_vendor)
        .to_string()
}

fn localized_status_and_tag(kind: &PolicyEventKind) -> (&'static str, &'static str) {
    match kind {
        PolicyEventKind::Alert => ("空闲", "idle"),
        PolicyEventKind::Recovery => ("繁忙恢复", "busy"),
    }
}

fn localize_reason(reason: &str) -> &str {
    match reason {
        "idle_detected" => "检测到 GPU 空闲",
        "idle_still_detected" => "GPU 持续空闲",
        "busy_detected" => "GPU 已恢复繁忙",
        _ => reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NotificationPolicyConfig, TriggerMode};
    use crate::gpu::GpuSample;
    use crate::policy::PolicyEngine;
    use chrono::{TimeZone, Utc};

    fn test_config() -> NtfyConfig {
        NtfyConfig {
            server: "https://ntfy.example.com".to_string(),
            topic: "gpu-topic".to_string(),
            token: Some("secret_token".to_string()),
            token_env: None,
            title_prefix: "GPU".to_string(),
            priority: 4,
            tags: vec!["gpu".to_string()],
            timeout_seconds: 3,
            max_retries: 3,
            retry_initial_backoff_millis: 50,
        }
    }

    #[test]
    fn build_request_contains_expected_headers() {
        let client = Client::new();
        let config = test_config();
        let request = build_publish_request(
            &client,
            &config,
            "hello",
            "world",
            &["gpu".to_string(), "alert".to_string()],
            5,
        )
        .unwrap();

        assert_eq!(request.url().as_str(), "https://ntfy.example.com/gpu-topic");
        assert_eq!(
            request
                .headers()
                .get("X-Markdown")
                .unwrap()
                .to_str()
                .unwrap(),
            "yes"
        );
        assert_eq!(
            request
                .headers()
                .get(CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "text/markdown"
        );
        assert_eq!(
            request.headers().get("X-Title").unwrap().to_str().unwrap(),
            "hello"
        );
        assert_eq!(
            request
                .headers()
                .get("X-Priority")
                .unwrap()
                .to_str()
                .unwrap(),
            "5"
        );
        assert_eq!(
            request.headers().get("X-Tags").unwrap().to_str().unwrap(),
            "gpu,alert"
        );
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer secret_token"
        );
    }

    #[test]
    fn build_request_skips_optional_headers_when_empty() {
        let client = Client::new();
        let mut config = test_config();
        config.token = None;

        let request = build_publish_request(&client, &config, "hello", "world", &[], 3).unwrap();

        assert!(request.headers().get("X-Tags").is_none());
        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    #[test]
    fn notifier_topic_url() {
        let notifier = NtfyNotifier::new(test_config()).unwrap();
        assert_eq!(notifier.topic_url(), "https://ntfy.example.com/gpu-topic");
    }

    #[test]
    fn forbidden_is_not_retryable() {
        assert!(!is_retryable_status(StatusCode::FORBIDDEN));
        assert!(!is_retryable_status(StatusCode::BAD_REQUEST));
    }

    #[test]
    fn too_many_requests_is_retryable() {
        assert!(is_retryable_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable_status(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[test]
    fn auth_error_contains_actionable_hint() {
        let config = test_config();
        let err = build_http_error(StatusCode::FORBIDDEN, "forbidden", &config).to_string();
        assert!(err.contains("publish 权限"));
        assert!(err.contains("server=https://ntfy.example.com"));
        assert!(err.contains("topic=gpu-topic"));
    }

    #[test]
    fn event_status_text_is_chinese() {
        let (alert_status, _) = localized_status_and_tag(&PolicyEventKind::Alert);
        let (recovery_status, _) = localized_status_and_tag(&PolicyEventKind::Recovery);
        assert_eq!(alert_status, "空闲");
        assert_eq!(recovery_status, "繁忙恢复");
    }

    #[test]
    fn reason_text_is_localized_to_chinese() {
        assert_eq!(localize_reason("idle_detected"), "检测到 GPU 空闲");
        assert_eq!(localize_reason("idle_still_detected"), "GPU 持续空闲");
        assert_eq!(localize_reason("busy_detected"), "GPU 已恢复繁忙");
    }

    #[test]
    fn gpu_name_is_abbreviated_for_table_display() {
        assert_eq!(abbreviate_gpu_name("NVIDIA GeForce RTX 4090"), "RTX 4090");
        assert_eq!(
            abbreviate_gpu_name("NVIDIA A100-PCIE-40GB"),
            "A100-PCIE-40GB"
        );
        assert_eq!(
            abbreviate_gpu_name("Tesla V100-SXM2-16GB"),
            "Tesla V100-SXM2-16GB"
        );
    }

    fn build_policy_engine() -> PolicyEngine {
        PolicyEngine::new(NotificationPolicyConfig {
            gpu_util_percent: 20.0,
            memory_util_percent: 20.0,
            trigger_mode: TriggerMode::Both,
            trigger_after_consecutive_samples: 1,
            recovery_after_consecutive_samples: 1,
            repeat_idle_notifications: false,
            resend_cooldown_seconds: 3600,
            send_recovery: true,
            suppress_in_quiet_hours: true,
        })
    }

    fn build_idle_event(index: u32, uuid: &str) -> PolicyEvent {
        let mut engine = build_policy_engine();
        let sample = GpuSample {
            index,
            uuid: uuid.to_string(),
            name: "NVIDIA GeForce RTX 4090".to_string(),
            gpu_util_percent: 0.0,
            memory_used_bytes: 1_500_000_000,
            memory_total_bytes: 12_000_000_000,
        };

        engine
            .evaluate(&sample, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
            .expect("should emit alert event")
    }

    #[test]
    fn markdown_body_uses_table_format_and_memory_gb() {
        let event = build_idle_event(2, "GPU-2");
        let ntfy_cfg = test_config();
        let (title, body, tags, _priority) = format_events_notification(&ntfy_cfg, &[event]);

        assert!(title.contains("[GPU2]"));
        assert!(body.contains("### GPU 状态明细"));
        assert!(body.contains("```"));
        assert!(body.contains("GPU序号"));
        assert!(body.contains("GPU型号"));
        assert!(body.contains("核心利用率"));
        assert!(body.contains("已使用显存(GB)"));
        assert!(body.contains("2"));
        assert!(body.contains("RTX 4090"));
        assert!(body.contains("0.00%"));
        assert!(body.contains("检测到 GPU 空闲"));
        assert!(!body.contains("| ---: |"));
        assert!(!body.contains("时间"));
        assert!(!body.contains("+08:00"));
        assert_eq!(tags, vec!["gpu".to_string(), "idle".to_string()]);
    }

    #[test]
    fn multiple_events_are_merged_and_tags_deduped() {
        let event5 = build_idle_event(5, "GPU-5");
        let event7 = build_idle_event(7, "GPU-7");
        let mut ntfy_cfg = test_config();
        ntfy_cfg.tags = vec!["idle".to_string(), "gpu".to_string()];

        let (title, body, tags, _priority) =
            format_events_notification(&ntfy_cfg, &[event7, event5]);

        assert!(title.contains("[2 GPUs]"));
        assert!(body.contains("```"));
        assert!(body.contains("GPU序号"));
        assert!(body.contains("5"));
        assert!(body.contains("7"));
        assert_eq!(body.matches("RTX 4090").count(), 2);
        assert!(!body.contains("时间"));
        assert_eq!(tags, vec!["idle".to_string(), "gpu".to_string()]);
    }
}
