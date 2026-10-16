//! The autonomous single executor loop (issue #32).
//!
//! Alternates model chat calls and schema-constrained tool calls until the goal is
//! achieved, a limit is reached, or the task is cancelled. Runs inside the task's
//! isolated git worktree and Windows Job Object.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nebula_model::types::ChatRequest;
use nebula_model::{ModelBackend, ModelError};
use nebula_proto::{
    ChatMessage, Event, Notification, Role, StepId, TaskCreateParams, TaskDone, TaskId, TaskStatus,
    TaskStep, TraceId,
};
use nebula_sandbox::job::MemoryScope;
use nebula_sandbox::task_job::{CpuRateLimit, JobLimits, MemoryLimit, TaskJob};
use nebula_tools::{CURRENT_TASK_JOB, CURRENT_WORKTREE};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::Shared;

type Outbox = mpsc::Sender<nebula_proto::Message>;

const DEFAULT_STEP_MAX_TOKENS: u32 = 4096;

fn note(event: Event) -> nebula_proto::Message {
    nebula_proto::Message::Notification(Notification::new(event))
}

/// Runs one autonomous executor task until completion, limits, or cancellation.
pub(crate) async fn run_task(
    shared: Arc<Shared>,
    task_id: TaskId,
    trace_id: TraceId,
    params: TaskCreateParams,
    worktree_root: PathBuf,
    tx: Outbox,
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
            let _ = tx
                .send(note(Event::TaskDone(TaskDone {
                    task_id,
                    status: TaskStatus::Failed,
                    summary: format!("could not create task job: {e}"),
                })))
                .await;
            return;
        }
    };

    // 2. Ensure model profile is loaded
    let profile = params.profile.as_deref().or_else(|| {
        let p = shared.config.executor.default_profile.as_str();
        if p.is_empty() { None } else { Some(p) }
    });
    if let Err(e) = crate::server::ensure_profile(&shared, profile, &token).await {
        tracing::error!(event = "task.model_profile_failed", error = %e.message);
        let _ = shared.worktrees.finalize(&wt_task_id).await;
        let _ = tx
            .send(note(Event::TaskDone(TaskDone {
                task_id,
                status: TaskStatus::Failed,
                summary: format!("model unavailable: {}", e.message),
            })))
            .await;
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
            content: params.goal,
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
            tx: &tx,
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
    let _ = shared.worktrees.finalize(&wt_task_id).await;

    tracing::info!(event = "task.done", status = ?status, summary = %summary);
    let _ = tx
        .send(note(Event::TaskDone(TaskDone {
            task_id,
            status,
            summary,
        })))
        .await;
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
    tx: &'a Outbox,
    token: &'a CancellationToken,
}

async fn execute_loop(
    ctx: LoopContext<'_>,
    messages: &mut Vec<ChatMessage>,
) -> (TaskStatus, String) {
    let mut step_index: u32 = 0;
    let mut tool_calls_executed: u32 = 0;
    let mut total_tokens_used: u32 = 0;

    loop {
        if ctx.token.is_cancelled() {
            return (TaskStatus::Cancelled, "task cancelled by client".into());
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

        let stream = tokio::select! {
            () = ctx.token.cancelled() => return (TaskStatus::Cancelled, "task cancelled".into()),
            r = backend.chat(req) => match r {
                Ok(s) => s,
                Err(ModelError::Timeout) => return (TaskStatus::LimitReached, "model call timed out".into()),
                Err(e) => return (TaskStatus::Failed, format!("model call failed: {e}")),
            },
        };

        let output = tokio::select! {
            () = ctx.token.cancelled() => return (TaskStatus::Cancelled, "task cancelled".into()),
            r = stream.collect() => match r {
                Ok(out) => out,
                Err(ModelError::Timeout) => return (TaskStatus::LimitReached, "model stream timed out".into()),
                Err(e) => return (TaskStatus::Failed, format!("model stream failed: {e}")),
            },
        };

        if let Some(usage) = &output.usage {
            total_tokens_used += usage.prompt_n + usage.predicted_n;
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
        let _ = ctx
            .tx
            .send(note(Event::TaskStep(TaskStep {
                task_id: ctx.task_id,
                step_id,
                step_index,
                tool_calls: tool_names.clone(),
            })))
            .await;

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
                            .call(&call.name, args, Some(&trace_str))
                            .await
                    }),
                )
                .await;

            let result_content = match call_res {
                Ok(outcome) => {
                    if outcome.is_error {
                        format!("[tool error] {}", outcome.content)
                    } else {
                        outcome.content.to_string()
                    }
                }
                Err(err) => {
                    format!("[tool dispatch error: {err}]")
                }
            };

            messages.push(ChatMessage {
                role: Role::Tool,
                content: result_content,
            });
        }
    }
}
