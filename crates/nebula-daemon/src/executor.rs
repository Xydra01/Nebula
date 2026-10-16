//! The autonomous single executor loop (issue #32).
//!
//! Alternates model chat calls and schema-constrained tool calls until the goal is
//! achieved, a limit is reached, or the task is cancelled. Runs inside the task's
//! isolated git worktree and Windows Job Object.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use nebula_model::types::{ChatOutput, ChatRequest, StreamItem};
use nebula_model::{ModelBackend, ModelError};
use nebula_proto::{
    ChatId, ChatMessage, ChatToken, Event, Role, StepId, TaskCreateParams, TaskDone, TaskId,
    TaskStatus, TaskStep, TraceId,
};
use nebula_resources::GB;
use nebula_sandbox::job::MemoryScope;
use nebula_sandbox::task_job::{CpuRateLimit, JobLimits, MemoryLimit, TaskJob};
use nebula_tools::{CURRENT_TASK_JOB, CURRENT_WORKTREE};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::Shared;
use crate::circuit_breaker::CircuitBreakerTracker;

const DEFAULT_STEP_MAX_TOKENS: u32 = 4096;

/// Runs one autonomous executor task until completion, limits, or cancellation.
pub(crate) async fn run_task(
    shared: Arc<Shared>,
    task_id: TaskId,
    trace_id: TraceId,
    params: TaskCreateParams,
    worktree_root: PathBuf,
    token: CancellationToken,
) {
    let wt_task_id = nebula_sandbox::worktree::TaskId::new(task_id.to_string());

    // 1. Build the task's Job Object
    let job_limits = build_job_limits(&shared);
    let task_job = match TaskJob::new(task_id.to_string(), job_limits) {
        Ok(job) => Arc::new(job),
        Err(e) => {
            tracing::error!(event = "task.job_create_failed", error = %e);
            let _ = shared.worktrees.finalize(&wt_task_id).await;
            let summary = format!("could not create task job: {e}");
            shared.notifications.notify_task_done(
                &task_id,
                &TaskStatus::Failed,
                &summary,
                &trace_id,
            );
            let done_ev = Event::TaskDone(TaskDone {
                task_id,
                status: TaskStatus::Failed,
                summary,
            });
            shared.emit_event(done_ev);
            return;
        }
    };

    // Register job handle in executor_tasks for global stop termination
    if let Some(state) = shared
        .executor_tasks
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get_mut(&task_id)
    {
        state.task_job = Some(Arc::clone(&task_job));
    }

    // 2. Ensure model profile is loaded
    let profile = params.profile.as_deref().or_else(|| {
        let p = shared.config.executor.default_profile.as_str();
        if p.is_empty() { None } else { Some(p) }
    });
    if let Err(e) = crate::server::ensure_profile(&shared, profile, &token).await {
        tracing::error!(event = "task.model_profile_failed", error = %e.message);
        let _ = shared.worktrees.finalize(&wt_task_id).await;
        let summary = format!("model unavailable: {}", e.message);
        shared
            .notifications
            .notify_task_done(&task_id, &TaskStatus::Failed, &summary, &trace_id);
        let done_ev = Event::TaskDone(TaskDone {
            task_id,
            status: TaskStatus::Failed,
            summary,
        });
        shared.emit_event(done_ev);
        return;
    }

    // 3. Resolve limits
    let max_tool_calls = params
        .max_tool_calls
        .unwrap_or(shared.config.executor.max_tool_calls);
    let max_tokens = params
        .max_tokens
        .unwrap_or(shared.config.executor.max_tokens);
    let wall_time_secs = params
        .wall_time_secs
        .unwrap_or(shared.config.executor.wall_time_secs);

    // 4. Seed conversation
    let mut messages = vec![
        ChatMessage {
            role: Role::System,
            content: "You are Nebula, an autonomous local AI coding assistant. You are working in an isolated git worktree for a task. \
Your working directory is the root of the worktree. \
Inspect files, run commands and tests, edit code, and verify your changes. \
Use the available tools according to their schema. \
When the task is complete, finish with a clear final summary without requesting further tools.".into(),
        },
        ChatMessage {
            role: Role::User,
            content: params.goal.clone(),
        },
    ];

    let loop_future = execute_loop(
        LoopContext {
            shared: &shared,
            task_id,
            trace_id,
            worktree_root: &worktree_root,
            task_job: &task_job,
            max_tool_calls,
            max_tokens,
            token: &token,
        },
        &mut messages,
    );

    let (status, summary) =
        match tokio::time::timeout(Duration::from_secs(wall_time_secs), loop_future).await {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(event = "task.timeout", wall_time_secs = wall_time_secs);
                (
                    TaskStatus::LimitReached,
                    format!("wall-clock timeout reached ({wall_time_secs}s)"),
                )
            }
        };

    // 5. Cleanup and finalization
    drop(task_job);
    let finalized_branch = shared.worktrees.finalize(&wt_task_id).await;

    let should_create_pr = params.create_pr.unwrap_or(shared.config.github.enabled);
    let mut final_summary = summary;
    if status == TaskStatus::Completed
        && should_create_pr
        && let Ok(branch_name) = finalized_branch
    {
        match crate::github::handle_task_completion(crate::github::TaskCompletionParams {
            repo_root: &worktree_root,
            branch: &branch_name,
            goal: &params.goal,
            summary: &final_summary,
            trace_id,
            test_results: None,
            config: &shared.config.github,
            token_override: None,
            api_base_url_override: None,
            git_remote_url_override: None,
            extra_coauthor: None,
        })
        .await
        {
            Ok(Some(pr_url)) => {
                final_summary = format!("{final_summary}\n\nPull Request: {pr_url}");
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(event = "github.flow_failed", error = %e);
                final_summary = format!("{final_summary}\n\n(GitHub PR creation failed: {e})");
            }
        }
    }

    tracing::info!(event = "task.done", status = ?status, summary = %final_summary);
    shared
        .notifications
        .notify_task_done(&task_id, &status, &final_summary, &trace_id);
    let done_ev = Event::TaskDone(TaskDone {
        task_id,
        status,
        summary: final_summary,
    });
    shared.emit_event(done_ev);
}

fn build_job_limits(shared: &Shared) -> JobLimits {
    let mut limits = JobLimits::default();
    if shared.config.tasks.memory_limit_mib > 0 {
        let bytes = shared.config.tasks.memory_limit_mib * 1024 * 1024;
        let scope = match shared.config.tasks.memory_limit_scope {
            nebula_config::MemoryScopeConfig::Process => MemoryScope::PerProcess,
            nebula_config::MemoryScopeConfig::Job => MemoryScope::WholeJob,
        };
        limits.memory = Some(MemoryLimit { bytes, scope });
    }
    if shared.config.tasks.cpu_rate_percent > 0 {
        limits.cpu = Some(CpuRateLimit {
            percent: shared.config.tasks.cpu_rate_percent,
        });
    }
    limits
}

struct LoopContext<'a> {
    shared: &'a Shared,
    task_id: TaskId,
    trace_id: TraceId,
    worktree_root: &'a Path,
    task_job: &'a Arc<TaskJob>,
    max_tool_calls: u32,
    max_tokens: u32,
    token: &'a CancellationToken,
}

fn normalize_mount_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('/', "\\");
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    s.to_uppercase()
}

fn mount_matches(path: &Path, mount: &str) -> bool {
    let p_str = normalize_mount_path(path);
    let m_str = mount.to_uppercase().replace('/', "\\");
    let m_trimmed = m_str.trim_end_matches('\\');
    p_str.starts_with(&m_str) || p_str.starts_with(m_trimmed)
}

fn check_disk_guard(shared: &Shared, worktree_root: &Path) -> Result<(), String> {
    let norm_worktree = normalize_mount_path(worktree_root);
    let drive_prefix = norm_worktree.chars().take(2).collect::<String>();

    let guard = shared.config.resources.volumes.iter().find(|g| {
        mount_matches(worktree_root, &g.mount) || g.mount.to_uppercase().starts_with(&drive_prefix)
    });

    let Some(guard) = guard else {
        return Ok(());
    };

    let free_bytes = if let Some(snap) = shared.latest_snapshot() {
        snap.disks
            .iter()
            .find(|d| mount_matches(Path::new(&d.mount), &guard.mount))
            .map(|d| d.free_bytes)
            .unwrap_or(u64::MAX)
    } else {
        let disks = sysinfo::Disks::new_with_refreshed_list();
        disks
            .iter()
            .find(|d| mount_matches(d.mount_point(), &guard.mount))
            .map(sysinfo::Disk::available_space)
            .unwrap_or(u64::MAX)
    };

    let warn_bytes = guard.warn_gb * GB;
    if free_bytes < warn_bytes {
        let free_gb = free_bytes / GB;
        tracing::warn!(
            event = "disk_guard.tripped",
            mount = %guard.mount,
            free_gb = free_gb,
            warn_gb = guard.warn_gb,
        );
        return Err(format!(
            "disk guard tripped: free space on {} is below budget ({} GB free < {} GB budget)",
            guard.mount, free_gb, guard.warn_gb
        ));
    }

    Ok(())
}

async fn execute_loop(
    ctx: LoopContext<'_>,
    messages: &mut Vec<ChatMessage>,
) -> (TaskStatus, String) {
    if let Err(reason) = check_disk_guard(ctx.shared, ctx.worktree_root) {
        return (TaskStatus::LimitReached, reason);
    }

    let mut tracker = CircuitBreakerTracker::new(ctx.shared.config.circuit_breakers);
    let mut step_index: u32 = 0;
    let mut tool_calls_executed: u32 = 0;
    let mut total_tokens_used: u32 = 0;

    loop {
        if ctx.token.is_cancelled() {
            return (TaskStatus::Cancelled, "task cancelled by client".into());
        }

        if let Err(e) = ctx.shared.game_mode.wait_if_active(ctx.token).await {
            return (TaskStatus::Cancelled, e);
        }

        // List tools and convert to OpenAI function schemas
        let descriptors = ctx.shared.tool_host.list_tools();
        let tools: Vec<Value> = descriptors
            .into_iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description.unwrap_or_default(),
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();

        let remaining_tokens = ctx.max_tokens.saturating_sub(total_tokens_used);
        if remaining_tokens == 0 {
            return (
                TaskStatus::LimitReached,
                format!("model token budget exhausted ({total_tokens_used} tokens used)"),
            );
        }

        let mut req = ChatRequest::new(messages.clone());
        req.tools = tools;
        req.max_tokens = DEFAULT_STEP_MAX_TOKENS.min(remaining_tokens);
        req.trace_id = Some(ctx.trace_id);

        let backend = match ctx.shared.chat.backend() {
            Ok(b) => b,
            Err(e) => return (TaskStatus::Failed, format!("model backend error: {e}")),
        };

        let mut stream = tokio::select! {
            () = ctx.token.cancelled() => return (TaskStatus::Cancelled, "task cancelled".into()),
            r = backend.chat(req) => match r {
                Ok(s) => s,
                Err(ModelError::Timeout) => return (TaskStatus::LimitReached, "model call timed out".into()),
                Err(e) => return (TaskStatus::Failed, format!("model call failed: {e}")),
            },
        };

        let mut output = ChatOutput::default();
        let chat_id = ChatId::from_ulid(ctx.task_id.ulid());
        loop {
            let next_item = tokio::select! {
                () = ctx.token.cancelled() => return (TaskStatus::Cancelled, "task cancelled".into()),
                item = stream.next() => item,
            };

            let Some(item_res) = next_item else {
                break;
            };

            let item = match item_res {
                Ok(it) => it,
                Err(ModelError::Timeout) => {
                    return (TaskStatus::LimitReached, "model stream timed out".into());
                }
                Err(e) => return (TaskStatus::Failed, format!("model stream failed: {e}")),
            };

            match item {
                StreamItem::Token { text, reasoning } => {
                    if reasoning {
                        output.reasoning.push_str(&text);
                    } else {
                        output.text.push_str(&text);
                    }
                    let token_ev = Event::ChatToken(ChatToken {
                        chat_id,
                        text: if reasoning {
                            String::new()
                        } else {
                            text.clone()
                        },
                        reasoning: if reasoning { text } else { String::new() },
                    });
                    ctx.shared.emit_event(token_ev);
                }
                StreamItem::ToolCall(c) => output.tool_calls.push(c),
                StreamItem::Usage(u) => output.usage = Some(u),
                StreamItem::Done(r) => {
                    output.stop_reason = Some(r);
                    break;
                }
            }
        }

        if let Some(usage) = &output.usage {
            let step_tokens = usage.prompt_n + usage.predicted_n;
            if let Err(trip) = tracker.check_step_tokens(step_tokens) {
                tracing::warn!(event = "circuit_breaker.tripped", reason = %trip);
                ctx.shared
                    .notifications
                    .notify_breaker_tripped(&trip.to_string(), Some(&ctx.trace_id));
                return (
                    TaskStatus::LimitReached,
                    format!("circuit breaker tripped: {trip}"),
                );
            }
            total_tokens_used += step_tokens;
            if total_tokens_used >= ctx.max_tokens {
                return (
                    TaskStatus::LimitReached,
                    format!(
                        "model token budget exhausted ({total_tokens_used} >= {})",
                        ctx.max_tokens
                    ),
                );
            }
        }

        // If the model finished without calling any tools, the task is complete.
        if output.tool_calls.is_empty() {
            let summary = if output.text.is_empty() {
                "Task completed".into()
            } else {
                output.text
            };
            return (TaskStatus::Completed, summary);
        }

        // Check if executing this turn's tool calls would exceed the tool call limit
        let num_calls = u32::try_from(output.tool_calls.len()).unwrap_or(u32::MAX);
        if tool_calls_executed + num_calls > ctx.max_tool_calls {
            return (
                TaskStatus::LimitReached,
                format!(
                    "maximum tool-call limit reached ({} calls)",
                    ctx.max_tool_calls
                ),
            );
        }

        // Emit step event
        let tool_names: Vec<String> = output.tool_calls.iter().map(|c| c.name.clone()).collect();
        let step_id = StepId::new();
        let step_ev = Event::TaskStep(TaskStep {
            task_id: ctx.task_id,
            step_id,
            step_index,
            tool_calls: tool_names.clone(),
        });
        ctx.shared.emit_event(step_ev);

        tracing::info!(
            event = "task.step",
            trace_id = %ctx.trace_id,
            task_id = %ctx.task_id,
            step_id = %step_id,
            step_index = step_index,
            tool_calls = ?tool_names,
        );
        step_index += 1;

        // Add assistant turn to conversation
        let assistant_turn = if output.text.is_empty() {
            format!("Calling tools: {}", tool_names.join(", "))
        } else {
            output.text
        };
        messages.push(ChatMessage {
            role: Role::Assistant,
            content: assistant_turn,
        });

        // Dispatch each tool call within worktree and job scopes
        for call in output.tool_calls {
            if ctx.token.is_cancelled() {
                return (TaskStatus::Cancelled, "task cancelled".into());
            }

            tool_calls_executed += 1;
            let args: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
            let trace_str = ctx.trace_id.to_string();

            let call_res = CURRENT_WORKTREE
                .scope(
                    Some(ctx.worktree_root.to_path_buf()),
                    CURRENT_TASK_JOB.scope(Some(Arc::clone(ctx.task_job)), async {
                        ctx.shared
                            .tool_host
                            .call(&call.name, args.clone(), Some(&trace_str))
                            .await
                    }),
                )
                .await;

            let (result_content, is_error) = match call_res {
                Ok(outcome) => {
                    let err = outcome.is_error;
                    let text = if err {
                        format!("[tool error] {}", outcome.content)
                    } else {
                        outcome.content.to_string()
                    };
                    (text, err)
                }
                Err(err) => (format!("[tool dispatch error: {err}]"), true),
            };

            if is_error
                && (result_content.contains("requires an approval")
                    || result_content.contains("lacking an approval"))
            {
                ctx.shared
                    .notifications
                    .notify_approval_needed(&call.name, "System", None, &trace_str);
                let app_needed = nebula_proto::ApprovalNeeded {
                    command: call.name.clone(),
                    tier: "System".into(),
                    rule_id: None,
                    trace_id: trace_str.clone(),
                    summary: result_content.clone(),
                };
                ctx.shared.emit_event(Event::ApprovalNeeded(app_needed));
            }

            if let Err(trip) = tracker.record_tool_call(&call.name, &args, is_error) {
                tracing::warn!(event = "circuit_breaker.tripped", reason = %trip);
                ctx.shared
                    .notifications
                    .notify_breaker_tripped(&trip.to_string(), Some(&ctx.trace_id));
                return (
                    TaskStatus::LimitReached,
                    format!("circuit breaker tripped: {trip}"),
                );
            }

            messages.push(ChatMessage {
                role: Role::Tool,
                content: result_content,
            });
        }
    }
}
