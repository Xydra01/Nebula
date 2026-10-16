# Design: ntfy Push Notifications over Tailscale

## 1. Architecture

```
                          ┌───────────────────────────────────┐
                          │         nebula-daemon             │
                          │                                   │
┌──────────────────┐      │  ┌─────────────────────────────┐  │
│  Executor Loop   ├─────►│  │     NotificationService     │  │
│  (Task / Breaker)│      │  └──────────────┬──────────────┘  │
└──────────────────┘      │                 │                 │
┌──────────────────┐      │                 │                 │
│  Approval Refusal├─────►│                 │ HTTP POST       │
│  (Engine / Scope)│      │                 │ (reqwest)       │
└──────────────────┘      │                 ▼                 │
┌──────────────────┐      │  ┌─────────────────────────────┐  │
│  Doctor Monitor  ├─────►│  │    Tailscale Guard Validator│  │
│  (Fail / Expiry) │      │  └──────────────┬──────────────┘  │
└──────────────────┘      └─────────────────┼─────────────────┘
                                            │
                                            ▼ (Tailnet only: 100.64.0.0/10 or *.ts.net)
                               ┌─────────────────────────────┐
                               │  Self-Hosted ntfy Server    │
                               │  (in SearXNG WSL2 / Tailnet)│
                               └─────────────┬───────────────┘
                                             │
                                             ▼
                               ┌─────────────────────────────┐
                               │  Phone & Laptop ntfy Apps   │
                               └─────────────────────────────┘
```

## 2. Configuration (`config/default.toml` & `nebula-config`)

```toml
[notifications]
# Whether push notifications are enabled.
enabled = false
# ntfy server URL (e.g. "http://100.x.y.z:8080" or "http://127.0.0.1:8080").
server_url = "http://127.0.0.1:8080"
# ntfy topic name. Private topic on your tailnet-only ntfy server.
topic = "nebula-alerts"
# Credential Manager target containing an optional authentication token for ntfy.
token_target = "nebula/ntfy_token"
# Restricts destination server_url to Tailscale IPs (100.64.0.0/10, fd7a:115c:a1e0::/48),
# MagicDNS hostnames (*.ts.net), or local loopback. Rejects public internet servers.
tailscale_only = true
# Request timeout in seconds for publishing notifications.
timeout_secs = 5
# Base URL for the tailnet-only approval page (e.g. "http://ej-pc.tailnet:8080/approve").
approval_url_base = ""
# Minimum interval in minutes between periodic doctor health checks/alerts.
doctor_alert_interval_mins = 60
```

## 3. Tailscale Guard (`is_tailscale_or_local`)
- Parses destination host from `server_url`.
- Accepts:
  - IPv4 loopback `127.0.0.0/8` or IPv6 loopback `::1` or `localhost`.
  - Tailscale IPv4 CGNAT range: `100.64.0.0/10` (`100.64.0.0` - `100.127.255.255`).
  - Tailscale IPv6 ULA range: `fd7a:115c:a1e0::/48`.
  - Hostnames ending in `.ts.net`.
- Rejects any external or public hostname/IP when `tailscale_only = true`.

## 4. Notification Formatter & Headers
- **Approval Needed:**
  - `Title`: `Nebula: Approval Required [Tier {tier}]`
  - `Priority`: `high` (4)
  - `Tags`: `warning,lock`
  - `Click`: `{approval_url_base}?trace_id={trace_id}` (if set)
  - Body: `Command '{command}' requires approval under rule '{rule_id}'. Trace: {trace_id}`
- **Task Completed:**
  - `Title`: `Nebula: Task Completed`
  - `Priority`: `default` (3)
  - `Tags`: `white_check_mark,robot`
  - Body: `{summary}\nTrace: {trace_id}`
- **Task Failed / Limit Reached / Cancelled:**
  - `Title`: `Nebula: Task {status}`
  - `Priority`: `high` (4)
  - `Tags`: `x,warning`
  - Body: `{summary}\nTrace: {trace_id}`
- **Breaker Tripped:**
  - `Title`: `Nebula: Circuit Breaker Tripped`
  - `Priority`: `urgent` (5)
  - `Tags`: `rotating_light,safety_pin`
  - Body: `Circuit breaker tripped: {reason}\nTrace: {trace_id}`
- **Doctor Check FAILED:**
  - `Title`: `Nebula: Doctor Check FAILED ({check_name})`
  - `Priority`: `urgent` (5)
  - `Tags`: `stethoscope,rotating_light`
  - Body: `Doctor check '{check_name}' failed: {detail}`

## 5. Testing Strategy
- Unit tests for `is_tailscale_or_local` validation (accepting tailnet/loopback, rejecting public IPs/domains).
- In-process test server using `axum` verifying HTTP headers, POST body, tags, click links, and timeouts.
- Redaction verification ensuring registered secrets are masked and file contents are never attached.
- Mock notification client in daemon tests to avoid live HTTP requests during integration tests.
