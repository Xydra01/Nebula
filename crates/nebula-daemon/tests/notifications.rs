#![allow(clippy::unwrap_used, clippy::panic, missing_docs)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path as AxPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use nebula_config::NotificationsConfig;
use nebula_daemon::notifications::{NotificationError, NotificationService, is_tailscale_or_local};
use nebula_proto::{CheckStatus, TaskId, TaskStatus, TraceId};
use nebula_telemetry::Redactor;

#[derive(Clone, Debug)]
struct CapturedNotification {
    topic: String,
    title: Option<String>,
    priority: Option<String>,
    tags: Option<String>,
    click: Option<String>,
    auth: Option<String>,
    body: String,
}

#[derive(Clone, Default)]
struct MockNtfyState {
    captured: Arc<Mutex<Vec<CapturedNotification>>>,
}

#[test]
fn test_is_tailscale_or_local_validation() {
    // Loopback
    assert!(is_tailscale_or_local("http://127.0.0.1:8080").is_ok());
    assert!(is_tailscale_or_local("http://localhost:8080/topic").is_ok());
    assert!(is_tailscale_or_local("http://[::1]:8080").is_ok());
    assert!(is_tailscale_or_local("http://host.docker.internal:8080").is_ok());

    // Tailscale IPv4 range (100.64.0.0/10: 100.64.0.0 - 100.127.255.255)
    assert!(is_tailscale_or_local("http://100.64.0.1:8080").is_ok());
    assert!(is_tailscale_or_local("http://100.100.100.100:8080/topic").is_ok());
    assert!(is_tailscale_or_local("http://100.127.255.254:8080").is_ok());

    // Tailscale IPv6 ULA range (fd7a:115c:a1e0::/48)
    assert!(is_tailscale_or_local("http://[fd7a:115c:a1e0::1]:8080").is_ok());
    assert!(is_tailscale_or_local("http://[fd7a:115c:a1e0:1234::5678]:8080").is_ok());

    // MagicDNS hostnames
    assert!(is_tailscale_or_local("https://my-node.ts.net/alerts").is_ok());
    assert!(is_tailscale_or_local("http://machine.tailscale.ts.net:8080").is_ok());

    // Public / external endpoints rejected
    assert!(matches!(
        is_tailscale_or_local("https://ntfy.sh/alerts"),
        Err(NotificationError::TailscaleViolation(_))
    ));
    assert!(matches!(
        is_tailscale_or_local("http://1.1.1.1/alerts"),
        Err(NotificationError::TailscaleViolation(_))
    ));
    assert!(matches!(
        is_tailscale_or_local("http://8.8.8.8:8080"),
        Err(NotificationError::TailscaleViolation(_))
    ));
    assert!(matches!(
        is_tailscale_or_local("https://example.com/alerts"),
        Err(NotificationError::TailscaleViolation(_))
    ));
    // Adjacent to Tailscale IPv4 but outside /10 range
    assert!(matches!(
        is_tailscale_or_local("http://100.63.255.255/alerts"),
        Err(NotificationError::TailscaleViolation(_))
    ));
    assert!(matches!(
        is_tailscale_or_local("http://100.128.0.1/alerts"),
        Err(NotificationError::TailscaleViolation(_))
    ));

    // Invalid URLs
    assert!(matches!(
        is_tailscale_or_local("not-a-valid-url"),
        Err(NotificationError::InvalidUrl(_))
    ));
}

#[tokio::test]
async fn test_ntfy_delivery_headers_and_deep_link() {
    let state = MockNtfyState::default();

    let app = Router::new()
        .route("/v1/health", get(|| async { (StatusCode::OK, "healthy") }))
        .route(
            "/{topic}",
            post(
                |AxPath(topic): AxPath<String>,
                 headers: HeaderMap,
                 State(st): State<MockNtfyState>,
                 body: String| async move {
                    let title = headers
                        .get("Title")
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                    let priority = headers
                        .get("Priority")
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                    let tags = headers
                        .get("Tags")
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                    let click = headers
                        .get("Click")
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);
                    let auth = headers
                        .get("Authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(ToOwned::to_owned);

                    st.captured.lock().unwrap().push(CapturedNotification {
                        topic,
                        title,
                        priority,
                        tags,
                        click,
                        auth,
                        body,
                    });
                    StatusCode::OK
                },
            ),
        )
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = NotificationsConfig {
        enabled: true,
        server_url: format!("http://127.0.0.1:{port}"),
        topic: "nebula-alerts".into(),
        token_target: "nebula/ntfy_token".into(),
        tailscale_only: true,
        timeout_secs: 5,
        approval_url_base: "https://nebula.internal/approve".into(),
        doctor_alert_interval_mins: 5,
    };

    let redactor = Arc::new(Redactor::new());
    redactor.register("SECRET_SUPER_PW");
    let service =
        NotificationService::with_overrides(config, redactor, Some("test_token_456".into()), None);

    let trace_id = TraceId::new();

    // 1. Approval needed
    service.notify_approval_needed(
        "git push origin main",
        "tier2",
        Some("rule.git.push"),
        &trace_id.to_string(),
    );

    // 2. Task completed
    let task_id = "01J9ZQ3V5W8X0Y2Z4A6B8C0D2E".parse::<TaskId>().unwrap();
    service.notify_task_done(
        &task_id,
        &TaskStatus::Completed,
        "Task completed successfully",
        &trace_id,
    );

    // 3. Task failed
    service.notify_task_done(&task_id, &TaskStatus::Failed, "Compiler error", &trace_id);

    // 4. Circuit breaker tripped
    service.notify_breaker_tripped("Tool failed 3 times in a row", Some(&trace_id));

    // 5. Test notification (synchronous)
    service
        .notify_test(Some("Manual test notification"))
        .await
        .unwrap();

    // Wait a brief moment for background tasks to complete
    tokio::time::sleep(Duration::from_millis(100)).await;

    let captured = state.captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 5);

    // Verify 1: Approval needed
    let approval = &captured[0];
    assert_eq!(approval.topic, "nebula-alerts");
    assert_eq!(approval.auth.as_deref(), Some("Bearer test_token_456"));
    assert_eq!(approval.priority.as_deref(), Some("high"));
    assert!(
        approval
            .title
            .as_deref()
            .unwrap()
            .contains("Approval Required")
    );
    assert!(approval.tags.as_deref().unwrap().contains("warning"));
    assert_eq!(
        approval.click.as_deref(),
        Some(format!("https://nebula.internal/approve?trace_id={trace_id}").as_str())
    );
    assert!(
        approval
            .body
            .contains("Command 'git push origin main' (tier2)")
    );
    assert!(approval.body.contains("rule 'rule.git.push'"));

    // Verify 2: Task completed
    let completed = &captured[1];
    assert_eq!(completed.priority.as_deref(), Some("default"));
    assert!(completed.title.as_deref().unwrap().contains("Completed"));
    assert!(
        completed
            .tags
            .as_deref()
            .unwrap()
            .contains("white_check_mark")
    );

    // Verify 3: Task failed
    let failed = &captured[2];
    assert_eq!(failed.priority.as_deref(), Some("high"));
    assert!(failed.title.as_deref().unwrap().contains("Failed"));
    assert!(failed.tags.as_deref().unwrap().contains("x"));

    // Verify 4: Circuit breaker tripped
    let breaker = &captured[3];
    assert_eq!(breaker.priority.as_deref(), Some("urgent"));
    assert!(
        breaker
            .title
            .as_deref()
            .unwrap()
            .contains("Circuit Breaker Tripped")
    );
    assert!(breaker.tags.as_deref().unwrap().contains("rotating_light"));

    // Verify 5: Test notification
    let test_notif = &captured[4];
    assert_eq!(test_notif.priority.as_deref(), Some("default"));
    assert_eq!(test_notif.body, "Manual test notification");
}

#[tokio::test]
async fn test_secret_redaction() {
    let state = MockNtfyState::default();

    let app =
        Router::new()
            .route(
                "/{topic}",
                post(
                    |AxPath(topic): AxPath<String>,
                     State(st): State<MockNtfyState>,
                     body: String| async move {
                        st.captured.lock().unwrap().push(CapturedNotification {
                            topic,
                            title: None,
                            priority: None,
                            tags: None,
                            click: None,
                            auth: None,
                            body,
                        });
                        StatusCode::OK
                    },
                ),
            )
            .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = NotificationsConfig {
        enabled: true,
        server_url: format!("http://127.0.0.1:{port}"),
        topic: "nebula-alerts".into(),
        token_target: "nebula/ntfy_token".into(),
        tailscale_only: true,
        timeout_secs: 5,
        approval_url_base: String::new(),
        doctor_alert_interval_mins: 5,
    };

    let redactor = Arc::new(Redactor::new());
    redactor.register("SECRET_SUPER_PW");
    let service = NotificationService::with_overrides(config, redactor, None, None);

    service
        .notify_test(Some("Contains sensitive token SECRET_SUPER_PW inside"))
        .await
        .unwrap();

    let captured = state.captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
    assert!(!captured[0].body.contains("SECRET_SUPER_PW"));
    assert!(captured[0].body.contains("[REDACTED]"));
}

#[tokio::test]
async fn test_tailscale_enforcement_in_service() {
    let config = NotificationsConfig {
        enabled: true,
        server_url: "https://ntfy.sh".into(),
        topic: "alerts".into(),
        token_target: "nebula/ntfy_token".into(),
        tailscale_only: true,
        timeout_secs: 5,
        approval_url_base: String::new(),
        doctor_alert_interval_mins: 5,
    };

    let redactor = Arc::new(Redactor::new());
    let service = NotificationService::with_overrides(config, redactor, None, None);

    let err = service.notify_test(None).await.unwrap_err();
    assert!(matches!(err, NotificationError::TailscaleViolation(_)));
}

#[tokio::test]
async fn test_doctor_fail_debouncing() {
    let state = MockNtfyState::default();

    let app =
        Router::new()
            .route(
                "/{topic}",
                post(
                    |AxPath(topic): AxPath<String>,
                     State(st): State<MockNtfyState>,
                     body: String| async move {
                        st.captured.lock().unwrap().push(CapturedNotification {
                            topic,
                            title: None,
                            priority: None,
                            tags: None,
                            click: None,
                            auth: None,
                            body,
                        });
                        StatusCode::OK
                    },
                ),
            )
            .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = NotificationsConfig {
        enabled: true,
        server_url: format!("http://127.0.0.1:{port}"),
        topic: "nebula-alerts".into(),
        token_target: "nebula/ntfy_token".into(),
        tailscale_only: true,
        timeout_secs: 5,
        approval_url_base: String::new(),
        doctor_alert_interval_mins: 60, // 60 min cooldown
    };

    let redactor = Arc::new(Redactor::new());
    let service = NotificationService::with_overrides(config, redactor, None, None);

    // First call -> alert sent
    service.notify_doctor_fail("backup.google_drive", "token expired 2 hours ago");

    // Wait a brief moment
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Immediate second call -> debounced / skipped
    service.notify_doctor_fail("backup.google_drive", "token expired 2 hours ago");

    tokio::time::sleep(Duration::from_millis(50)).await;

    let captured = state.captured.lock().unwrap().clone();
    assert_eq!(captured.len(), 1);
}

#[tokio::test]
async fn test_doctor_check_health() {
    let app = Router::new().route("/v1/health", get(|| async { (StatusCode::OK, "healthy") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let config = NotificationsConfig {
        enabled: true,
        server_url: format!("http://127.0.0.1:{port}"),
        topic: "alerts".into(),
        token_target: "nebula/ntfy_token".into(),
        tailscale_only: true,
        timeout_secs: 5,
        approval_url_base: String::new(),
        doctor_alert_interval_mins: 5,
    };

    let redactor = Arc::new(Redactor::new());
    let service = NotificationService::with_overrides(config, redactor, None, None);

    let check = service.check_health().await;
    assert_eq!(check.name, "notifications");
    assert_eq!(check.status, CheckStatus::Ok);
}
