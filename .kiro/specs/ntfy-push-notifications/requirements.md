# Requirements: ntfy Push Notifications over Tailscale

## 1. Overview
Deliver phone and laptop push notifications for approval requests, task outcomes, circuit-breaker trips, and failing doctor checks through a self-hosted or tailnet-only ntfy server (Issue #37, design Section 4.5).

## 2. Functional Requirements
- **FR1 (Notification Events):** Dispatch push notifications for key lifecycle events:
  - **Approval Needed:** Triggered when an above-threshold command lacks an authorizing grant (or worktree delete limit exceeded). Contains command name, tier, rule ID, and trace ID.
  - **Task Done / Failed:** Triggered when an autonomous executor task finishes with status `Completed`, `Failed`, `LimitReached`, or `Cancelled`.
  - **Breaker Tripped:** Triggered when any circuit breaker trips (`RepeatedToolFailure`, `RepeatedFileEdits`, `RunawayTokens`).
  - **Doctor Failed:** Triggered when a doctor check turns `Fail` (including weekly Google Drive backup token expiration).
- **FR2 (Tailscale Confinement):**
  - Reachable strictly over Tailscale or local loopback when `tailscale_only = true` (the default).
  - Validates that `server_url` resolves to Tailscale CGNAT (`100.64.0.0/10`), IPv6 (`fd7a:115c:a1e0::/48`), MagicDNS (`*.ts.net`), or local loopback (`127.0.0.1`, `::1`, `localhost`).
  - Refuses public internet endpoints (e.g. `ntfy.sh`) when `tailscale_only` is true to prevent leaking notifications onto public topics.
- **FR3 (Privacy & Secret-Free Payloads):**
  - Notifications must contain NO secrets, file contents, or argument values; only human-readable summaries and trace/task IDs.
  - Payloads are passed through secret redaction (`nebula-telemetry`) and length-capped.
- **FR4 (Delivery & Latency):**
  - Asynchronous background dispatch with a 5-second HTTP request timeout. Notifications must arrive on the phone within 10 seconds.
- **FR5 (Open Question 32 & Action Links):**
  - When `approval_url_base` is configured, attach deep links (`Click` header or `view` action) to approval notifications so the user can open the tailnet-only approval page from their phone or laptop.
- **FR6 (Doctor & Health Integration):**
  - Add a `"notifications"` doctor check verifying ntfy configuration and connectivity when enabled.
  - Periodic background doctor check monitor to detect and alert on newly failing checks.
- **FR7 (CLI & Wire Control):**
  - CLI `nebula notify status` and `nebula notify test [--message <msg>]`.
  - Wire methods `Method::NotifyStatus` and `Method::NotifyTest`.
