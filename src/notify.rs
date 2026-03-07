use crate::config::NtfyConfig;
use crate::policy::{PolicyEvent, PolicyEventKind};
use crate::timeutil;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{Client, Request, StatusCode};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send_event(&self, event: &PolicyEvent) -> Result<()>;
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
    async fn send_event(&self, event: &PolicyEvent) -> Result<()> {
        let mut tags = self.config.tags.clone();
        let (status_text, extra_tag) = localized_status_and_tag(event.kind.clone());

        tags.push(extra_tag.to_string());

        let title = format!(
            "{} [{}] {}",
            self.config.title_prefix, event.gpu_name, status_text
        );

        let body = format!(
            "GPU序号={} 核心利用率={:.2}% 显存占用率={:.2}% 原因={} 时间={}",
            event.gpu_index,
            event.gpu_util_percent,
            event.memory_util_percent,
            localize_reason(event.reason.as_str()),
            timeutil::format_utc8(&event.at)
        );

        let priority = match event.kind {
            PolicyEventKind::Alert => self.config.priority,
            PolicyEventKind::Recovery => self.config.priority.saturating_sub(1).max(1),
        };

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

fn localized_status_and_tag(kind: PolicyEventKind) -> (&'static str, &'static str) {
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
        let (alert_status, _) = localized_status_and_tag(PolicyEventKind::Alert);
        let (recovery_status, _) = localized_status_and_tag(PolicyEventKind::Recovery);
        assert_eq!(alert_status, "空闲");
        assert_eq!(recovery_status, "繁忙恢复");
    }

    #[test]
    fn reason_text_is_localized_to_chinese() {
        assert_eq!(localize_reason("idle_detected"), "检测到 GPU 空闲");
        assert_eq!(localize_reason("idle_still_detected"), "GPU 持续空闲");
        assert_eq!(localize_reason("busy_detected"), "GPU 已恢复繁忙");
    }
}
