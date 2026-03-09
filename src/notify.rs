use crate::config::NtfyConfig;
use crate::policy::{PolicyEvent, PolicyEventKind};
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::redirect::Policy as RedirectPolicy;
use reqwest::{Client, Request, StatusCode, Url};
use std::collections::HashSet;
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;
use unicode_width::UnicodeWidthStr;

#[allow(dead_code)]
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send_rows(&self, rows: &[NotificationRow]) -> Result<()>;

    async fn send_events(&self, events: &[PolicyEvent]) -> Result<()> {
        self.send_rows(&rows_from_events(events)).await
    }

    async fn send_text(&self, title: &str, body: &str, tags: &[String], priority: u8)
    -> Result<()>;
}

#[derive(Clone)]
pub struct NtfyNotifier {
    client: Client,
    config: NtfyConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotificationRow {
    pub gpu_index: u32,
    pub gpu_uuid: String,
    pub gpu_name: String,
    pub gpu_util_percent: f64,
    pub memory_used_bytes: u64,
    pub kind: PolicyEventKind,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPayload {
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub priority: u8,
    fingerprint: String,
}

impl NotificationPayload {
    #[cfg(test)]
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

impl NtfyNotifier {
    pub fn new(config: NtfyConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .redirect(RedirectPolicy::none())
            .build()
            .context("failed to build reqwest client")?;

        Ok(Self { client, config })
    }

    #[cfg(test)]
    fn topic_url(&self) -> String {
        build_topic_url(&self.config)
            .expect("topic URL should be valid")
            .to_string()
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
    async fn send_rows(&self, rows: &[NotificationRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }

        let payload = render_notification(&self.config, rows);
        self.publish(
            &payload.title,
            &payload.body,
            &payload.tags,
            payload.priority,
        )
        .await
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

pub fn row_from_event(event: &PolicyEvent) -> NotificationRow {
    NotificationRow {
        gpu_index: event.gpu_index,
        gpu_uuid: event.gpu_uuid.clone(),
        gpu_name: event.gpu_name.clone(),
        gpu_util_percent: event.gpu_util_percent,
        memory_used_bytes: event.memory_used_bytes,
        kind: event.kind.clone(),
        reason: event.reason.clone(),
    }
}

pub fn rows_from_events(events: &[PolicyEvent]) -> Vec<NotificationRow> {
    events.iter().map(row_from_event).collect()
}

#[cfg(test)]
pub fn payload_from_events(config: &NtfyConfig, events: &[PolicyEvent]) -> NotificationPayload {
    render_notification(config, &rows_from_events(events))
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
        header_value_from_utf8(title).context("invalid title header for ntfy request")?,
    );

    headers.insert(
        "X-Priority",
        HeaderValue::from_str(&priority.to_string())
            .context("invalid priority header for ntfy request")?,
    );

    if !tags.is_empty() {
        headers.insert(
            "X-Tags",
            header_value_from_utf8(&tags.join(","))
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

    let topic_url = build_topic_url(config)?;
    let request = client
        .post(topic_url)
        .headers(headers)
        .body(body.to_string())
        .build()
        .context("failed to build ntfy request")?;

    Ok(request)
}

fn build_topic_url(config: &NtfyConfig) -> Result<Url> {
    let mut url = Url::parse(config.server.trim_end_matches('/'))
        .context("invalid ntfy.server URL for request")?;
    url.path_segments_mut()
        .map_err(|_| anyhow!("ntfy.server URL cannot be a base for topic publishing"))?
        .push(config.topic.trim());
    Ok(url)
}

fn header_value_from_utf8(value: &str) -> Result<HeaderValue> {
    HeaderValue::from_bytes(value.as_bytes()).context("header contains invalid bytes")
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

pub fn render_notification(config: &NtfyConfig, rows: &[NotificationRow]) -> NotificationPayload {
    let has_alert = rows
        .iter()
        .any(|row| matches!(row.kind, PolicyEventKind::Alert));
    let has_recovery = rows
        .iter()
        .any(|row| matches!(row.kind, PolicyEventKind::Recovery));

    let status_text = match (has_alert, has_recovery) {
        (true, false) => "空闲",
        (false, true) => "繁忙恢复",
        _ => "状态更新",
    };

    let title = if rows.len() == 1 {
        format!(
            "{} [GPU{}] {}",
            config.title_prefix, rows[0].gpu_index, status_text
        )
    } else {
        format!(
            "{} [{} GPUs] {}",
            config.title_prefix,
            rows.len(),
            status_text
        )
    };

    let body = format_events_markdown_table(rows);

    let mut tags = config.tags.clone();
    if has_alert {
        tags.push("idle".to_string());
    }
    if has_recovery {
        tags.push("busy".to_string());
    }
    let tags = dedup_tags(tags);

    let priority = if has_alert {
        config.priority
    } else {
        config.priority.saturating_sub(1).max(1)
    };

    NotificationPayload {
        title,
        body,
        tags,
        priority,
        fingerprint: rows_fingerprint(rows),
    }
}

pub fn rows_fingerprint(rows: &[NotificationRow]) -> String {
    let mut signature_parts = rows
        .iter()
        .map(|row| {
            let kind = match row.kind {
                PolicyEventKind::Alert => "alert",
                PolicyEventKind::Recovery => "recovery",
            };
            format!("{}:{}", kind, row.gpu_uuid)
        })
        .collect::<Vec<_>>();
    signature_parts.sort();
    signature_parts.join("|")
}

fn format_events_markdown_table(rows: &[NotificationRow]) -> String {
    let mut ordered: Vec<&NotificationRow> = rows.iter().collect();
    ordered.sort_by_key(|row| row.gpu_index);

    let headers = [
        "GPU序号",
        "GPU型号",
        "状态",
        "核心利用率",
        "已使用显存(GB)",
        "原因",
    ];

    let mut table_rows: Vec<[String; 6]> = Vec::with_capacity(ordered.len());
    for row in ordered {
        let (status_text, _) = localized_status_and_tag(&row.kind);
        table_rows.push([
            row.gpu_index.to_string(),
            escape_table_cell(&abbreviate_gpu_name(&row.gpu_name)),
            status_text.to_string(),
            format!("{:.2}%", row.gpu_util_percent),
            format!("{:.2}", bytes_to_gb(row.memory_used_bytes)),
            escape_table_cell(localize_reason(row.reason.as_str())),
        ]);
    }

    let mut widths = [
        display_width(headers[0]),
        display_width(headers[1]),
        display_width(headers[2]),
        display_width(headers[3]),
        display_width(headers[4]),
        display_width(headers[5]),
    ];
    for row in &table_rows {
        for (idx, cell) in row.iter().enumerate() {
            widths[idx] = widths[idx].max(display_width(cell));
        }
    }

    let pad = |cell: &str, width: usize| pad_display_width(cell, width);
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
        .join("-|- ")
        .replace(" ", "");

    let mut lines = vec![
        "### GPU 状态明细".to_string(),
        "```".to_string(),
        header_line,
        separator_line,
    ];
    for row in table_rows {
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

fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn pad_display_width(value: &str, width: usize) -> String {
    let padding = width.saturating_sub(display_width(value));
    format!("{value}{}", " ".repeat(padding))
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
    use std::hint::black_box;
    use std::time::Instant;

    fn test_config() -> NtfyConfig {
        NtfyConfig {
            server: "https://ntfy.example.com".to_string(),
            topic: "gpu-topic".to_string(),
            token: Some("secret_token".to_string()),
            token_env: None,
            allow_insecure_http: false,
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
    fn build_request_allows_utf8_title_and_tags() {
        let client = Client::new();
        let config = test_config();
        let request = build_publish_request(
            &client,
            &config,
            "GPU 监控",
            "world",
            &["空闲".to_string(), "恢复".to_string()],
            5,
        )
        .unwrap();

        assert_eq!(
            request.headers().get("X-Title").unwrap().as_bytes(),
            "GPU 监控".as_bytes()
        );
        assert_eq!(
            request.headers().get("X-Tags").unwrap().as_bytes(),
            "空闲,恢复".as_bytes()
        );
    }

    #[test]
    fn build_request_encodes_topic_as_single_path_segment() {
        let client = Client::new();
        let mut config = test_config();
        config.server = "https://ntfy.example.com/base".to_string();
        config.topic = "gpu/topic?prod".to_string();

        let request = build_publish_request(&client, &config, "hello", "world", &[], 3).unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://ntfy.example.com/base/gpu%2Ftopic%3Fprod"
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

    fn split_table_line(line: &str) -> Vec<&str> {
        line.split(" | ").collect()
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
        let payload = payload_from_events(&ntfy_cfg, &[event]);

        assert!(payload.title.contains("[GPU2]"));
        assert!(payload.body.contains("### GPU 状态明细"));
        assert!(payload.body.contains("```"));
        assert!(payload.body.contains("GPU序号"));
        assert!(payload.body.contains("GPU型号"));
        assert!(payload.body.contains("核心利用率"));
        assert!(payload.body.contains("已使用显存(GB)"));
        assert!(payload.body.contains("2"));
        assert!(payload.body.contains("RTX 4090"));
        assert!(payload.body.contains("0.00%"));
        assert!(payload.body.contains("检测到 GPU 空闲"));
        assert!(!payload.body.contains("| ---: |"));
        assert!(!payload.body.contains("时间"));
        assert!(!payload.body.contains("+08:00"));
        assert_eq!(payload.tags, vec!["gpu".to_string(), "idle".to_string()]);
    }

    #[test]
    fn markdown_table_padding_uses_display_width_for_cjk_text() {
        let body = format_events_markdown_table(&[
            NotificationRow {
                gpu_index: 2,
                gpu_uuid: "GPU-2".to_string(),
                gpu_name: "NVIDIA GeForce RTX 4090".to_string(),
                gpu_util_percent: 0.0,
                memory_used_bytes: 7_320_000_000,
                kind: PolicyEventKind::Recovery,
                reason: "busy_detected".to_string(),
            },
            NotificationRow {
                gpu_index: 4,
                gpu_uuid: "GPU-4".to_string(),
                gpu_name: "NVIDIA GeForce RTX 4090".to_string(),
                gpu_util_percent: 0.0,
                memory_used_bytes: 620_000_000,
                kind: PolicyEventKind::Alert,
                reason: "idle_still_detected".to_string(),
            },
        ]);

        let code_block_lines = body
            .lines()
            .skip_while(|line| *line != "```")
            .skip(1)
            .take_while(|line| *line != "```")
            .collect::<Vec<_>>();

        assert!(code_block_lines.len() >= 4);

        let expected_widths = split_table_line(code_block_lines[0])
            .iter()
            .map(|cell| display_width(cell))
            .collect::<Vec<_>>();

        for line in code_block_lines.iter().skip(2) {
            let widths = split_table_line(line)
                .iter()
                .map(|cell| display_width(cell))
                .collect::<Vec<_>>();
            assert_eq!(widths, expected_widths, "line was: {line}");
        }
    }

    #[test]
    fn multiple_events_are_merged_and_tags_deduped() {
        let event5 = build_idle_event(5, "GPU-5");
        let event7 = build_idle_event(7, "GPU-7");
        let mut ntfy_cfg = test_config();
        ntfy_cfg.tags = vec!["idle".to_string(), "gpu".to_string()];

        let payload = payload_from_events(&ntfy_cfg, &[event7, event5]);

        assert!(payload.title.contains("[2 GPUs]"));
        assert!(payload.body.contains("```"));
        assert!(payload.body.contains("GPU序号"));
        assert!(payload.body.contains("5"));
        assert!(payload.body.contains("7"));
        assert_eq!(payload.body.matches("RTX 4090").count(), 2);
        assert_eq!(payload.tags, vec!["idle".to_string(), "gpu".to_string()]);
    }

    #[test]
    fn notification_payload_fingerprint_changes_with_rendered_summary() {
        let event5 = build_idle_event(5, "GPU-5");
        let event7 = build_idle_event(7, "GPU-7");
        let config = test_config();

        let payload1 = payload_from_events(&config, &[event5]);
        let payload2 = payload_from_events(&config, &[event7]);

        assert_ne!(payload1.fingerprint(), payload2.fingerprint());
    }

    #[test]
    #[ignore = "manual microbenchmark"]
    fn bench_rows_fingerprint_vs_full_render_path() {
        let rows = (0..64)
            .map(|index| NotificationRow {
                gpu_index: index,
                gpu_uuid: format!("GPU-{index}"),
                gpu_name: "NVIDIA GeForce RTX 4090".to_string(),
                gpu_util_percent: (index % 15) as f64,
                memory_used_bytes: 1_500_000_000 + (index as u64 * 1_000_000),
                kind: if index % 2 == 0 {
                    PolicyEventKind::Alert
                } else {
                    PolicyEventKind::Recovery
                },
                reason: if index % 2 == 0 {
                    "idle_still_detected".to_string()
                } else {
                    "busy_detected".to_string()
                },
            })
            .collect::<Vec<_>>();
        let config = test_config();
        let iterations = 5_000;

        let fingerprint_started_at = Instant::now();
        let mut fingerprint_len = 0usize;
        for _ in 0..iterations {
            fingerprint_len += rows_fingerprint(&rows).len();
        }
        let fingerprint_elapsed = fingerprint_started_at.elapsed();

        let render_started_at = Instant::now();
        let mut render_len = 0usize;
        for _ in 0..iterations {
            render_len += render_notification(&config, &rows).fingerprint().len();
        }
        let render_elapsed = render_started_at.elapsed();

        black_box(fingerprint_len + render_len);
        eprintln!(
            "rows_fingerprint: {:?}, render_notification(...).fingerprint(): {:?}",
            fingerprint_elapsed, render_elapsed
        );

        assert!(fingerprint_elapsed < render_elapsed);
    }
}
