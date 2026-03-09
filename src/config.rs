use crate::timeutil;
use anyhow::{Context, Result, bail};
use chrono::Timelike;
use reqwest::Url;
use serde::Deserialize;
use std::env;
use std::fmt::{Display, Formatter};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
#[cfg(unix)]
use tracing::warn;

const SUPPORTED_NTFY_TOKEN_ENV: &str = "NTFY_TOKEN";

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppConfig {
    pub monitor: MonitorConfig,
    pub ntfy: NtfyConfig,
    pub quiet_hours: Vec<QuietWindow>,
    pub policy: NotificationPolicyConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            monitor: MonitorConfig::default(),
            ntfy: NtfyConfig::default(),
            quiet_hours: Vec::new(),
            policy: NotificationPolicyConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file: {}", path.display()))?;

        let mut config: Self = toml::from_str(&raw)
            .with_context(|| format!("failed to parse TOML config: {}", path.display()))?;

        config.resolve_from_env()?;
        config.validate()?;
        config.warn_if_insecure_ntfy_auth_permissions(path);
        Ok(config)
    }

    fn resolve_from_env(&mut self) -> Result<()> {
        self.ntfy.server = self.ntfy.server.trim_end_matches('/').to_string();

        if let Some(env_name) = self.ntfy.token_env.clone() {
            if env_name != SUPPORTED_NTFY_TOKEN_ENV {
                bail!("ntfy.token_env only supports {}", SUPPORTED_NTFY_TOKEN_ENV);
            }

            let token = env::var(&env_name)
                .with_context(|| format!("missing env var for ntfy token: {}", env_name))?;
            self.ntfy.token = Some(token);
        } else if let Some(token) = self.ntfy.token.clone()
            && let Some(env_name) = parse_env_ref(&token)
        {
            if env_name != SUPPORTED_NTFY_TOKEN_ENV {
                bail!(
                    "ntfy.token env reference only supports ${{{}}}",
                    SUPPORTED_NTFY_TOKEN_ENV
                );
            }

            let resolved = env::var(env_name)
                .with_context(|| format!("missing env var referenced by token: {}", env_name))?;
            self.ntfy.token = Some(resolved);
        }

        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.monitor.interval_seconds == 0 {
            bail!("monitor.interval_seconds must be > 0");
        }

        self.validate_ntfy_server()?;

        if let Some(token_env) = self.ntfy.token_env.as_deref()
            && token_env != SUPPORTED_NTFY_TOKEN_ENV
        {
            bail!("ntfy.token_env only supports {}", SUPPORTED_NTFY_TOKEN_ENV);
        }

        if self.ntfy.topic.trim().is_empty() {
            bail!("ntfy.topic cannot be empty");
        }

        if !(1..=5).contains(&self.ntfy.priority) {
            bail!("ntfy.priority must be in [1, 5]");
        }

        if self.ntfy.max_retries == 0 {
            bail!("ntfy.max_retries must be > 0");
        }

        if self.ntfy.retry_initial_backoff_millis == 0 {
            bail!("ntfy.retry_initial_backoff_millis must be > 0");
        }

        let policy = &self.policy;

        if !(0.0..=100.0).contains(&policy.gpu_util_percent) {
            bail!("policy.gpu_util_percent must be in [0, 100]");
        }

        if !(0.0..=100.0).contains(&policy.memory_util_percent) {
            bail!("policy.memory_util_percent must be in [0, 100]");
        }

        if policy.trigger_after_consecutive_samples == 0 {
            bail!("policy.trigger_after_consecutive_samples must be > 0");
        }

        if policy.recovery_after_consecutive_samples == 0 {
            bail!("policy.recovery_after_consecutive_samples must be > 0");
        }

        Ok(())
    }

    pub fn now_in_quiet_hours(&self) -> bool {
        if self.quiet_hours.is_empty() {
            return false;
        }

        let now = chrono::Utc::now()
            .with_timezone(&timeutil::utc8_offset())
            .time();
        self.quiet_hours.iter().any(|q| q.contains_time(now))
    }

    fn validate_ntfy_server(&self) -> Result<()> {
        let url = Url::parse(&self.ntfy.server)
            .with_context(|| format!("invalid ntfy.server URL: {}", self.ntfy.server))?;

        if url.host_str().is_none() {
            bail!("ntfy.server must include a host");
        }

        match url.scheme() {
            "https" => Ok(()),
            "http" if self.ntfy.allow_insecure_http => Ok(()),
            "http" => bail!("ntfy.server must use https:// unless ntfy.allow_insecure_http = true"),
            _ => bail!("ntfy.server must use http:// or https://"),
        }
    }

    #[cfg(unix)]
    fn warn_if_insecure_ntfy_auth_permissions(&self, path: &Path) {
        if !self.ntfy.uses_auth() {
            return;
        }

        let Ok(metadata) = fs::metadata(path) else {
            return;
        };

        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            warn!(
                config_path = %path.display(),
                mode = format!("{:03o}", mode),
                "config file with ntfy auth is accessible by group/others; consider chmod 600"
            );
        }
    }

    #[cfg(not(unix))]
    fn warn_if_insecure_ntfy_auth_permissions(&self, _path: &Path) {}
}

fn parse_env_ref(input: &str) -> Option<&str> {
    if input.starts_with("${") && input.ends_with('}') && input.len() > 3 {
        Some(&input[2..input.len() - 1])
    } else {
        None
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct MonitorConfig {
    pub interval_seconds: u64,
    pub send_startup_notification: bool,
    pub sample_log: bool,
}

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            interval_seconds: 10,
            send_startup_notification: true,
            sample_log: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct NtfyConfig {
    pub server: String,
    pub topic: String,
    pub token: Option<String>,
    pub token_env: Option<String>,
    pub allow_insecure_http: bool,
    pub title_prefix: String,
    pub priority: u8,
    pub tags: Vec<String>,
    pub timeout_seconds: u64,
    pub max_retries: u32,
    pub retry_initial_backoff_millis: u64,
}

impl NtfyConfig {
    fn uses_auth(&self) -> bool {
        self.token
            .as_ref()
            .is_some_and(|token| !token.trim().is_empty())
            || self.token_env.is_some()
    }
}

impl Default for NtfyConfig {
    fn default() -> Self {
        Self {
            server: "https://ntfy.sh".to_string(),
            topic: "gpu-usage-alerts".to_string(),
            token: None,
            token_env: None,
            allow_insecure_http: false,
            title_prefix: "GPU Monitor".to_string(),
            priority: 4,
            tags: vec!["gpu".to_string(), "monitor".to_string()],
            timeout_seconds: 10,
            max_retries: 3,
            retry_initial_backoff_millis: 500,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct NotificationPolicyConfig {
    pub gpu_util_percent: f64,
    pub memory_util_percent: f64,
    pub trigger_mode: TriggerMode,
    pub trigger_after_consecutive_samples: u32,
    pub recovery_after_consecutive_samples: u32,
    pub repeat_idle_notifications: bool,
    pub resend_cooldown_seconds: u64,
    pub send_recovery: bool,
    pub suppress_in_quiet_hours: bool,
}

impl Default for NotificationPolicyConfig {
    fn default() -> Self {
        Self {
            gpu_util_percent: 20.0,
            memory_util_percent: 20.0,
            trigger_mode: TriggerMode::Both,
            trigger_after_consecutive_samples: 3,
            recovery_after_consecutive_samples: 2,
            repeat_idle_notifications: false,
            resend_cooldown_seconds: 3600,
            send_recovery: true,
            suppress_in_quiet_hours: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum TriggerMode {
    Any,
    Both,
}

impl Display for TriggerMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            TriggerMode::Any => write!(f, "any"),
            TriggerMode::Both => write!(f, "both"),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq)]
pub struct QuietWindow {
    pub start: ClockTime,
    pub end: ClockTime,
}

impl QuietWindow {
    pub fn contains_time(&self, time: chrono::NaiveTime) -> bool {
        self.contains_minutes((time.hour() * 60 + time.minute()) as u16)
    }

    pub fn contains_minutes(&self, minute_of_day: u16) -> bool {
        let start = self.start.minutes;
        let end = self.end.minutes;

        if start == end {
            return true;
        }

        if start < end {
            minute_of_day >= start && minute_of_day < end
        } else {
            minute_of_day >= start || minute_of_day < end
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct ClockTime {
    minutes: u16,
}

impl ClockTime {
    #[cfg(test)]
    pub fn as_hhmm(&self) -> String {
        format!("{:02}:{:02}", self.minutes / 60, self.minutes % 60)
    }

    #[cfg(test)]
    pub(crate) fn from_hhmm_for_test(hour: u16, minute: u16) -> Self {
        assert!(hour <= 23);
        assert!(minute <= 59);
        Self {
            minutes: hour * 60 + minute,
        }
    }
}

impl<'de> Deserialize<'de> for ClockTime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        parse_clock_time(&raw).map_err(serde::de::Error::custom)
    }
}

fn parse_clock_time(raw: &str) -> Result<ClockTime> {
    let (hour, minute) = raw
        .split_once(':')
        .with_context(|| format!("invalid clock time '{}', expected HH:MM", raw))?;

    let hour: u16 = hour
        .parse()
        .with_context(|| format!("invalid hour in clock time '{}': must be integer", raw))?;

    let minute: u16 = minute
        .parse()
        .with_context(|| format!("invalid minute in clock time '{}': must be integer", raw))?;

    if hour > 23 || minute > 59 {
        bail!("invalid clock time '{}': out of range", raw);
    }

    Ok(ClockTime {
        minutes: hour * 60 + minute,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn quiet_hours_cross_day() {
        let q = QuietWindow {
            start: parse_clock_time("22:00").unwrap(),
            end: parse_clock_time("08:00").unwrap(),
        };

        assert!(q.contains_minutes(23 * 60));
        assert!(q.contains_minutes(7 * 60 + 59));
        assert!(!q.contains_minutes(8 * 60 + 1));
    }

    #[test]
    fn quiet_hours_same_day() {
        let q = QuietWindow {
            start: parse_clock_time("09:00").unwrap(),
            end: parse_clock_time("18:00").unwrap(),
        };

        assert!(q.contains_minutes(10 * 60));
        assert!(!q.contains_minutes(19 * 60));
    }

    #[test]
    fn env_ref_parse() {
        assert_eq!(parse_env_ref("${TOKEN}"), Some("TOKEN"));
        assert_eq!(parse_env_ref("TOKEN"), None);
    }

    #[test]
    fn clock_time_display() {
        let t = parse_clock_time("07:05").unwrap();
        assert_eq!(t.as_hhmm(), "07:05");
    }

    #[test]
    fn parse_minimal_config_applies_defaults() {
        let raw = r#"
[ntfy]
topic = "my-topic"
"#;

        let mut cfg: AppConfig = toml::from_str(raw).unwrap();
        cfg.resolve_from_env().unwrap();
        cfg.validate().unwrap();

        assert_eq!(cfg.monitor.interval_seconds, 10);
        assert_eq!(cfg.ntfy.server, "https://ntfy.sh");
        assert_eq!(cfg.policy.trigger_mode, TriggerMode::Both);
        assert_eq!(cfg.policy.gpu_util_percent, 20.0);
        assert_eq!(cfg.policy.memory_util_percent, 20.0);
        assert!(!cfg.policy.repeat_idle_notifications);
        assert!(!cfg.ntfy.allow_insecure_http);
    }

    #[test]
    fn rejects_unsupported_token_env_name() {
        let raw = r#"
[ntfy]
topic = "my-topic"
token_env = "AWS_SECRET_ACCESS_KEY"
"#;

        let mut cfg: AppConfig = toml::from_str(raw).unwrap();
        let err = cfg.resolve_from_env().unwrap_err().to_string();
        assert!(err.contains("ntfy.token_env only supports NTFY_TOKEN"));
    }

    #[test]
    fn rejects_unsupported_token_env_reference() {
        let raw = r#"
[ntfy]
topic = "my-topic"
token = "${AWS_SECRET_ACCESS_KEY}"
"#;

        let mut cfg: AppConfig = toml::from_str(raw).unwrap();
        let err = cfg.resolve_from_env().unwrap_err().to_string();
        assert!(err.contains("ntfy.token env reference only supports ${NTFY_TOKEN}"));
    }

    #[test]
    fn rejects_http_server_without_explicit_opt_in() {
        let raw = r#"
[ntfy]
server = "http://ntfy.internal"
topic = "my-topic"
"#;

        let mut cfg: AppConfig = toml::from_str(raw).unwrap();
        cfg.resolve_from_env().unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(
            err.contains("ntfy.server must use https:// unless ntfy.allow_insecure_http = true")
        );
    }

    #[test]
    fn allows_http_server_when_explicitly_enabled() {
        let raw = r#"
[ntfy]
server = "http://ntfy.internal"
topic = "my-topic"
allow_insecure_http = true
"#;

        let mut cfg: AppConfig = toml::from_str(raw).unwrap();
        cfg.resolve_from_env().unwrap();
        cfg.validate().unwrap();
    }
}
