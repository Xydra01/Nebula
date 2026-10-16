//! Push notifications over Tailscale via ntfy (issue #37, design Section 4.5).
//!
//! Delivers alerts for approval requests, task completion/failure, circuit-breaker trips,
//! and failing doctor checks to phone and laptop devices over Tailscale.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use nebula_config::NotificationsConfig;
use nebula_proto::{CheckStatus, DoctorCheck, TaskId, TaskStatus, TraceId};
use nebula_telemetry::Redactor;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use reqwest::{Client, StatusCode, Url};

/// Maximum notification body length before truncation.
const MAX_BODY_LEN: usize = 1024;

/// Notification errors.
#[derive(Debug, thiserror::Error)]
pub enum NotificationError {
    /// Push notifications are disabled in config.
    #[error("notifications are disabled")]
    Disabled,
    /// Destination URL is invalid.
    #[error("invalid notification URL: {0}")]
    InvalidUrl(String),
    /// Destination host is not a Tailscale or local address when `tailscale_only` is true.
    #[error("destination violates tailscale_only policy: {0}")]
    TailscaleViolation(String),
    /// HTTP transport failure.
    #[error("failed to send notification: {0}")]
    Http(String),
    /// Server returned an error HTTP response.
    #[error("ntfy server returned error status {code}: {body}")]
    ServerError {
        /// HTTP status code.
        code: u16,
        /// Response body text.
        body: String,
    },
}

/// Notification priority level for ntfy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationPriority {
    /// Lowest priority (1).
    Min = 1,
    /// Low priority (2).
    Low = 2,
    /// Default priority (3).
    Default = 3,
    /// High priority (4).
    High = 4,
    /// Urgent / max priority (5).
    Urgent = 5,
}

impl NotificationPriority {
    /// Converts priority to header string expected by ntfy.
    #[must_use]
    pub const fn as_header_str(&self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Low => "low",
            Self::Default => "default",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }
}

/// A structured notification to dispatch to ntfy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationPayload {
    /// Notification title.
    pub title: String,
    /// Delivery priority.
    pub priority: NotificationPriority,
    /// Emojis / tags for the notification.
    pub tags: Vec<String>,
    /// Optional deep link / click URL.
    pub click: Option<String>,
    /// Notification body text (secret-free and length-capped).
    pub body: String,
}

/// Validates whether a URL points to local loopback or a Tailscale address.
///
/// Supported Tailscale address formats:
/// - IPv4 loopback (`127.0.0.0/8`) and IPv6 loopback (`::1`)
/// - Hostname `localhost`
/// - Tailscale CGNAT IPv4 range: `100.64.0.0/10` (`100.64.0.0` ..= `100.127.255.255`)
/// - Tailscale ULA IPv6 range: `fd7a:115c:a1e0::/48`
/// - MagicDNS hostnames ending in `.ts.net`
///
/// # Errors
/// Returns [`NotificationError::InvalidUrl`] if the URL cannot be parsed or lacks a host.
/// Returns [`NotificationError::TailscaleViolation`] if destination is external/public.
pub fn is_tailscale_or_local(url: &str) -> Result<(), NotificationError> {
    let parsed = Url::parse(url).map_err(|e| NotificationError::InvalidUrl(e.to_string()))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| NotificationError::InvalidUrl("missing host in url".into()))?;

    // Hostname checks
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".ts.net")
        || host.eq_ignore_ascii_case("host.docker.internal")
    {
        return Ok(());
    }

    // IP address checks
    let clean_host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = clean_host.parse::<IpAddr>() {
        match ip {
            IpAddr::V4(v4) => {
                if v4.is_loopback() {
                    return Ok(());
                }
                let octets = v4.octets();
                // Tailscale IPv4 CGNAT range: 100.64.0.0/10 (100.64.0.0 ..= 100.127.255.255)
                if octets[0] == 100 && (64..=127).contains(&octets[1]) {
                    return Ok(());
                }
            }
            IpAddr::V6(v6) => {
                if v6.is_loopback() {
                    return Ok(());
                }
                let segs = v6.segments();
                // Tailscale IPv6 ULA range: fd7a:115c:a1e0::/48
                if segs[0] == 0xfd7a && segs[1] == 0x115c && segs[2] == 0xa1e0 {
                    return Ok(());
                }
            }
        }
    }

    Err(NotificationError::TailscaleViolation(format!(
        "host '{host}' is neither local loopback nor within Tailscale range (100.64.0.0/10, fd7a:115c:a1e0::/48, *.ts.net)"
    )))
}

/// Service that coordinates push notifications to ntfy.
#[derive(Clone)]
pub struct NotificationService {
    config: NotificationsConfig,
    client: Client,
    redactor: Arc<Redactor>,
    last_doctor_alert: Arc<Mutex<HashMap<String, Instant>>>,
    token_override: Option<String>,
    api_base_url_override: Option<String>,
}

impl NotificationService {
    /// Creates a new notification service from daemon configuration and redactor.
    #[must_use]
    pub fn new(config: NotificationsConfig, redactor: Arc<Redactor>) -> Self {
        let timeout = Duration::from_secs(config.timeout_secs.max(1));
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            config,
            client,
            redactor,
            last_doctor_alert: Arc::new(Mutex::new(HashMap::new())),
            token_override: None,
            api_base_url_override: None,
        }
    }

    /// Creates a service with overrides for testing.
    #[must_use]
    pub fn with_overrides(
        config: NotificationsConfig,
        redactor: Arc<Redactor>,
        token_override: Option<String>,
        api_base_url_override: Option<String>,
    ) -> Self {
        let timeout = Duration::from_secs(config.timeout_secs.max(1));
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            config,
            client,
            redactor,
            last_doctor_alert: Arc::new(Mutex::new(HashMap::new())),
            token_override,
            api_base_url_override,
        }
    }

    /// Whether notifications are currently enabled in config.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Gets active configuration.
    #[must_use]
    pub fn config(&self) -> &NotificationsConfig {
        &self.config
    }

    /// Sends a raw notification payload to the ntfy server.
    pub async fn send(&self, payload: &NotificationPayload) -> Result<(), NotificationError> {
        if !self.config.enabled {
            return Err(NotificationError::Disabled);
        }

        let base_url = self
            .api_base_url_override
            .as_deref()
            .unwrap_or(&self.config.server_url);

        if self.config.tailscale_only {
            is_tailscale_or_local(base_url)?;
        }

        let topic = &self.config.topic;
        let endpoint = format!(
            "{}/{}",
            base_url.trim_end_matches('/'),
            topic.trim_start_matches('/')
        );

        let mut headers = HeaderMap::new();
        if let Ok(val) = HeaderValue::from_str(&payload.title) {
            headers.insert("Title", val);
        }
        if let Ok(val) = HeaderValue::from_str(payload.priority.as_header_str()) {
            headers.insert("Priority", val);
        }
        if !payload.tags.is_empty() {
            let tags_str = payload.tags.join(",");
            if let Ok(val) = HeaderValue::from_str(&tags_str) {
                headers.insert("Tags", val);
            }
        }
        if let Some(click_url) = &payload.click
            && let Ok(val) = HeaderValue::from_str(click_url)
        {
            headers.insert("Click", val);
        }

        // Authentication token from Credential Manager or override
        let token = self.token_override.clone().or_else(|| {
            if self.config.token_target.is_empty() {
                None
            } else {
                crate::win::read_credential(&self.config.token_target)
            }
        });
        if let Some(tok) = token {
            let auth_header = format!("Bearer {tok}");
            if let Ok(val) = HeaderValue::from_str(&auth_header) {
                headers.insert(AUTHORIZATION, val);
            }
        }

        // Redact and cap body text
        let redacted = self.redactor.redact(&payload.body);
        let mut final_body = redacted.into_owned();
        if final_body.len() > MAX_BODY_LEN {
            final_body.truncate(MAX_BODY_LEN);
            final_body.push_str("... (truncated)");
        }

        let resp = self
            .client
            .post(&endpoint)
            .headers(headers)
            .body(final_body)
            .send()
            .await
            .map_err(|e| NotificationError::Http(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            tracing::error!(event = "notification.failed", status = status, body = %body);
            return Err(NotificationError::ServerError { code: status, body });
        }

        tracing::info!(event = "notification.sent", topic = %self.config.topic, title = %payload.title);
        Ok(())
    }

    /// Dispatches an approval-needed alert.
    pub fn notify_approval_needed(
        &self,
        command: &str,
        tier: &str,
        rule_id: Option<&str>,
        trace_id: &str,
    ) {
        if !self.config.enabled {
            return;
        }

        let click = if self.config.approval_url_base.is_empty() {
            None
        } else {
            Some(format!(
                "{}?trace_id={}",
                self.config.approval_url_base.trim_end_matches('/'),
                trace_id
            ))
        };

        let rule_desc = rule_id.unwrap_or("<unknown>");
        let payload = NotificationPayload {
            title: format!("Nebula: Approval Required [Tier {tier}]"),
            priority: NotificationPriority::High,
            tags: vec!["warning".into(), "lock".into()],
            click,
            body: format!(
                "Command '{command}' ({tier}) requires approval under rule '{rule_desc}'.\nTrace: {trace_id}"
            ),
        };

        let service = self.clone();
        tokio::spawn(async move {
            if let Err(e) = service.send(&payload).await {
                tracing::warn!(event = "notification.dispatch_failed", error = %e);
            }
        });
    }

    /// Dispatches a task done or failed alert.
    pub fn notify_task_done(
        &self,
        task_id: &TaskId,
        status: &TaskStatus,
        summary: &str,
        trace_id: &TraceId,
    ) {
        if !self.config.enabled {
            return;
        }

        let (title, priority, tags) = match status {
            TaskStatus::Completed => (
                "Nebula: Task Completed".to_string(),
                NotificationPriority::Default,
                vec!["white_check_mark".into(), "robot".into()],
            ),
            TaskStatus::Failed => (
                "Nebula: Task Failed".to_string(),
                NotificationPriority::High,
                vec!["x".into(), "warning".into()],
            ),
            TaskStatus::LimitReached => (
                "Nebula: Task Limit Reached".to_string(),
                NotificationPriority::High,
                vec!["hourglass".into(), "warning".into()],
            ),
            TaskStatus::Cancelled => (
                "Nebula: Task Cancelled".to_string(),
                NotificationPriority::Low,
                vec!["octagonal_sign".into()],
            ),
        };

        let payload = NotificationPayload {
            title,
            priority,
            tags,
            click: None,
            body: format!("{summary}\n\nTask: {task_id}\nTrace: {trace_id}"),
        };

        let service = self.clone();
        tokio::spawn(async move {
            if let Err(e) = service.send(&payload).await {
                tracing::warn!(event = "notification.dispatch_failed", error = %e);
            }
        });
    }

    /// Dispatches a circuit-breaker tripped alert.
    pub fn notify_breaker_tripped(&self, reason: &str, trace_id: Option<&TraceId>) {
        if !self.config.enabled {
            return;
        }

        let trace_str = trace_id
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| "none".into());
        let payload = NotificationPayload {
            title: "Nebula: Circuit Breaker Tripped".into(),
            priority: NotificationPriority::Urgent,
            tags: vec!["rotating_light".into(), "safety_pin".into()],
            click: None,
            body: format!("Circuit breaker tripped: {reason}\nTrace: {trace_str}"),
        };

        let service = self.clone();
        tokio::spawn(async move {
            if let Err(e) = service.send(&payload).await {
                tracing::warn!(event = "notification.dispatch_failed", error = %e);
            }
        });
    }

    /// Dispatches a doctor check failure alert with debouncing.
    pub fn notify_doctor_fail(&self, check_name: &str, detail: &str) {
        if !self.config.enabled {
            return;
        }

        // Debounce alert per check name
        let min_interval = Duration::from_secs(self.config.doctor_alert_interval_mins.max(1) * 60);
        let now = Instant::now();
        {
            let mut alerts = self
                .last_doctor_alert
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(last_time) = alerts.get(check_name)
                && now.duration_since(*last_time) < min_interval
            {
                return;
            }
            alerts.insert(check_name.to_string(), now);
        }

        let payload = NotificationPayload {
            title: format!("Nebula: Doctor Check FAILED ({check_name})"),
            priority: NotificationPriority::Urgent,
            tags: vec!["stethoscope".into(), "rotating_light".into()],
            click: None,
            body: format!("Doctor check '{check_name}' failed:\n{detail}"),
        };

        let service = self.clone();
        tokio::spawn(async move {
            if let Err(e) = service.send(&payload).await {
                tracing::warn!(event = "notification.dispatch_failed", error = %e);
            }
        });
    }

    /// Sends a test notification immediately (synchronous return of result).
    pub async fn notify_test(&self, message: Option<&str>) -> Result<(), NotificationError> {
        let body = message.unwrap_or("This is a test notification from Nebula.");
        let payload = NotificationPayload {
            title: "Nebula: Test Notification".into(),
            priority: NotificationPriority::Default,
            tags: vec!["bell".into()],
            click: None,
            body: body.to_string(),
        };
        self.send(&payload).await
    }

    /// Health check for `nebula doctor`.
    pub async fn check_health(&self) -> DoctorCheck {
        if !self.config.enabled {
            return DoctorCheck {
                name: "notifications".into(),
                status: CheckStatus::Ok,
                detail: "disabled in config".into(),
            };
        }

        let base_url = self
            .api_base_url_override
            .as_deref()
            .unwrap_or(&self.config.server_url);

        if self.config.tailscale_only
            && let Err(e) = is_tailscale_or_local(base_url)
        {
            return DoctorCheck {
                name: "notifications".into(),
                status: CheckStatus::Fail,
                detail: e.to_string(),
            };
        }

        // Check connectivity to the server
        let health_url = format!("{}/v1/health", base_url.trim_end_matches('/'));
        match self.client.get(&health_url).send().await {
            Ok(resp) => {
                if resp.status().is_success() || resp.status() == StatusCode::NOT_FOUND {
                    // ntfy root or /v1/health returns success
                    DoctorCheck {
                        name: "notifications".into(),
                        status: CheckStatus::Ok,
                        detail: format!(
                            "ntfy server reachable at {base_url} (topic: {})",
                            self.config.topic
                        ),
                    }
                } else {
                    DoctorCheck {
                        name: "notifications".into(),
                        status: CheckStatus::Warn,
                        detail: format!("ntfy server returned HTTP {}", resp.status()),
                    }
                }
            }
            Err(e) => DoctorCheck {
                name: "notifications".into(),
                status: CheckStatus::Warn,
                detail: format!("ntfy server unreachable at {base_url}: {e}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailscale_and_local_addresses_are_accepted() {
        assert!(is_tailscale_or_local("http://localhost:8080").is_ok());
        assert!(is_tailscale_or_local("http://127.0.0.1:8080/alerts").is_ok());
        assert!(is_tailscale_or_local("http://[::1]:8080").is_ok());
        assert!(is_tailscale_or_local("http://100.64.0.1:8080").is_ok());
        assert!(is_tailscale_or_local("http://100.127.255.255:8080").is_ok());
        assert!(is_tailscale_or_local("http://[fd7a:115c:a1e0::1]:8080").is_ok());
        assert!(is_tailscale_or_local("https://desktop.tailnet.ts.net:8080").is_ok());
    }

    #[test]
    fn public_internet_addresses_are_rejected() {
        assert!(is_tailscale_or_local("https://ntfy.sh/my-topic").is_err());
        assert!(is_tailscale_or_local("http://192.168.1.50:8080").is_err());
        assert!(is_tailscale_or_local("http://8.8.8.8:8080").is_err());
        assert!(is_tailscale_or_local("https://api.github.com").is_err());
    }

    #[test]
    fn disabled_service_refuses_send() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = NotificationsConfig::default();
            let redactor = Arc::new(Redactor::new());
            let svc = NotificationService::new(config, redactor);
            let payload = NotificationPayload {
                title: "Test".into(),
                priority: NotificationPriority::Default,
                tags: vec![],
                click: None,
                body: "Body".into(),
            };
            assert!(matches!(
                svc.send(&payload).await,
                Err(NotificationError::Disabled)
            ));
        });
    }
}
