//! The autonomous single executor loop (issue #32).
//!
//! Alternates model chat calls and schema-constrained tool calls until the goal is
//! achieved, a limit is reached, or the task is cancelled. Runs inside the task's
//! isolated git worktree and Windows Job Object.

use std::collections::HashSet;
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
use tracing::Instrument;

use crate::Shared;
use crate::circuit_breaker::CircuitBreakerTracker;
use crate::language::ProvisionContext;

const DEFAULT_STEP_MAX_TOKENS: u32 = 16384;

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

    // 4. Resolve Language Module (explicit or auto-detected)
    let lang_module = if let Some(ref lang_id) = params.language {
        match shared.languages.get(lang_id) {
            Some(m) => Some(m),
            None => {
                let summary = format!("unknown language module: {lang_id}");
                tracing::error!(event = "task.language_not_found", language = %lang_id);
                let _ = shared.worktrees.finalize(&wt_task_id).await;
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
        }
    } else {
        let detected = shared.languages.detect(&worktree_root).await;
        if let Some(ref m) = detected {
            tracing::info!(
                event = "language.detected",
                language = %m.id(),
                repo = %worktree_root.display()
            );
        }
        detected
    };

    // 5. Provision Hermetic Environment
    if let Some(ref module) = lang_module {
        let prov_ctx = ProvisionContext {
            task_id: &task_id,
            worktree_root: &worktree_root,
            task_job: Some(&task_job),
            language_config: params.language_config.as_ref(),
            trace_id: &trace_id,
        };
        let prov_started = std::time::Instant::now();
        if let Err(e) = module.provision(&prov_ctx).await {
            tracing::error!(event = "language.provision_failed", error = %e);
            let _ = shared.worktrees.finalize(&wt_task_id).await;
            let summary = format!("language module provisioning failed: {e}");
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
        tracing::info!(
            event = "language.provisioned",
            language = %module.id(),
            duration_ms = prov_started.elapsed().as_millis()
        );
    }

    // 6. Check tool collisions and prepare language tools
    let language_tools: Vec<Arc<dyn nebula_tools::BuiltinTool>> =
        lang_module.as_ref().map(|m| m.tools()).unwrap_or_default();

    let existing_tool_names: HashSet<String> = shared
        .tool_host
        .list_tools()
        .into_iter()
        .map(|t| t.name)
        .collect();

    for lt in &language_tools {
        if existing_tool_names.contains(lt.name()) {
            let summary = format!(
                "tool name collision: language module tool '{}' collides with existing tool",
                lt.name()
            );
            tracing::error!(event = "task.tool_collision", tool = %lt.name());
            let _ = shared.worktrees.finalize(&wt_task_id).await;
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
    }

    // 7. Seed conversation
    let mut system_prompt = String::from(
        "You are Nebula, an autonomous local AI coding assistant working in an isolated git worktree. \
Your working directory is the root of the worktree. \
Inspect files, run commands and tests, edit code, and verify your changes autonomously. \
CRITICAL: You MUST execute tool calls on every step to make progress. Do NOT output conversational plans, status updates, or thoughts without tool calls. If you need to inspect files or run commands, invoke the tools directly. \
Only provide a final plain-text response without tool calls when the entire task has been fully implemented, verified, and completed.",
    );

    if let Some(ref module) = lang_module {
        let prov_ctx = ProvisionContext {
            task_id: &task_id,
            worktree_root: &worktree_root,
            task_job: Some(&task_job),
            language_config: params.language_config.as_ref(),
            trace_id: &trace_id,
        };
        let guidelines = module.prompt_guidelines(&prov_ctx);
        if !guidelines.is_empty() {
            system_prompt.push_str("\n\n--- Language Guidelines (");
            system_prompt.push_str(module.id());
            system_prompt.push_str(") ---\n");
            for g in guidelines {
                system_prompt.push_str("- ");
                system_prompt.push_str(&g);
                system_prompt.push('\n');
            }
        }
    }

    let mut messages = vec![
        ChatMessage {
            role: Role::System,
            content: system_prompt,
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
            language_tools: &language_tools,
            language_id: lang_module.as_ref().map(|m| m.id()),
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

    // 8. Cleanup and finalization
    drop(task_job);

    if status == TaskStatus::Completed
        && let Some(ref module) = lang_module
    {
        let prov_ctx = ProvisionContext {
            task_id: &task_id,
            worktree_root: &worktree_root,
            task_job: None,
            language_config: params.language_config.as_ref(),
            trace_id: &trace_id,
        };
        match module.finalize(&prov_ctx).await {
            Ok(report) => {
                tracing::info!(
                    event = "language.finalized",
                    language = %module.id(),
                    success = report.success,
                    summary = %report.summary,
                    artifacts_count = report.artifacts.len()
                );
            }
            Err(e) => {
                tracing::warn!(event = "language.finalize_failed", error = %e);
            }
        }
    }

    if status == TaskStatus::Completed || status == TaskStatus::LimitReached {
        commit_worktree_if_dirty(&worktree_root, &params.goal, &task_id).await;
    }

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
    language_tools: &'a [Arc<dyn nebula_tools::BuiltinTool>],
    language_id: Option<&'a str>,
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

    let block_gb = guard.block_gb.unwrap_or(guard.warn_gb);
    let block_bytes = block_gb * GB;
    if free_bytes < block_bytes {
        let free_gb = free_bytes / GB;
        tracing::warn!(
            event = "disk_guard.tripped",
            mount = %guard.mount,
            free_gb = free_gb,
            block_gb = block_gb,
        );
        return Err(format!(
            "disk guard tripped: free space on {} is below budget ({} GB free < {} GB budget)",
            guard.mount, free_gb, block_gb
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
    let mut nudge_count: u32 = 0;

    loop {
        if ctx.token.is_cancelled() {
            return (TaskStatus::Cancelled, "task cancelled by client".into());
        }

        if let Err(e) = ctx.shared.game_mode.wait_if_active(ctx.token).await {
            return (TaskStatus::Cancelled, e);
        }

        // List tools and convert to OpenAI function schemas
        let mut descriptors = ctx.shared.tool_host.list_tools();
        for lt in ctx.language_tools {
            descriptors.push(nebula_tools::ToolDescriptor {
                server: format!("language_{}", ctx.language_id.unwrap_or("module")),
                name: lt.name().to_string(),
                description: lt.description().map(ToOwned::to_owned),
                input_schema: lt.input_schema(),
            });
        }
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

        // If the model finished without calling any tools, check if it was conversational planning, pseudo-tool text, or completion.
        if output.tool_calls.is_empty() {
            let lower = output.text.to_lowercase();
            let is_calling_tools = lower.starts_with("calling tools")
                || lower.starts_with("calling tool")
                || lower.starts_with("invoking tool")
                || lower.starts_with("executing tool");
            let is_still_planning = lower.contains("let me ")
                || lower.contains("i will ")
                || lower.contains("i'll ")
                || lower.contains("i need to ")
                || lower.starts_with("let's ")
                || lower.contains("next, ");
            let is_empty = output.text.trim().is_empty();

            if (is_still_planning || is_calling_tools || is_empty) && nudge_count < 5 {
                nudge_count += 1;
                tracing::info!(
                    event = "task.nudge",
                    text = %output.text,
                    is_calling_tools = is_calling_tools,
                    is_still_planning = is_still_planning,
                    is_empty = is_empty
                );
                messages.push(ChatMessage {
                    role: Role::Assistant,
                    content: output.text,
                });
                let prompt = if is_calling_tools {
                    "To invoke a tool, you must emit a structured tool call. If all necessary actions are complete, provide a final plain-text response detailing what was created and verified."
                } else if is_empty {
                    "Please provide a final summary of what was accomplished, or invoke the appropriate tools to continue the task."
                } else {
                    "Please proceed with executing the task using the available tools now."
                };
                messages.push(ChatMessage {
                    role: Role::User,
                    content: prompt.into(),
                });
                continue;
            }

            if (is_still_planning || is_calling_tools) && nudge_count >= 5 {
                tracing::warn!(event = "task.nudge_limit_exhausted", text = %output.text);
                return (
                    TaskStatus::LimitReached,
                    format!(
                        "task execution stopped during unfinished planning: {}",
                        output.text.trim()
                    ),
                );
            }

            let summary = if is_calling_tools {
                format!("Task completed after executing {tool_calls_executed} tool call(s).")
            } else if output.text.trim().is_empty() {
                "Task completed".into()
            } else {
                output.text
            };
            return (TaskStatus::Completed, summary);
        }

        // Tools were invoked: reset consecutive planning nudge count
        nudge_count = 0;

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

        // Add assistant turn to conversation without synthetic text poisoning
        messages.push(ChatMessage {
            role: Role::Assistant,
            content: output.text,
        });

        // Dispatch each tool call within worktree and job scopes
        for call in output.tool_calls {
            if ctx.token.is_cancelled() {
                return (TaskStatus::Cancelled, "task cancelled".into());
            }

            tool_calls_executed += 1;
            let (args, parse_error) = match serde_json::from_str::<Value>(&call.arguments) {
                Ok(v) => (v, None),
                Err(e) => {
                    let is_truncated = output.stop_reason == Some(nebula_proto::StopReason::Length);
                    let detail = if is_truncated {
                        "tool arguments were truncated (exceeded step token limit). Please break work into smaller writes or shorter files"
                    } else {
                        "tool arguments were not valid JSON"
                    };
                    (
                        json!({ "raw": call.arguments }),
                        Some(format!("{detail}: {e}")),
                    )
                }
            };
            let trace_str = ctx.trace_id.to_string();

            let (mut result_content, mut is_error) = if let Some(err_msg) = parse_error {
                (format!("[tool error] {err_msg}"), true)
            } else {
                let lang_tool = ctx
                    .language_tools
                    .iter()
                    .find(|t| t.name() == call.name)
                    .cloned();

                let call_res = CURRENT_WORKTREE
                    .scope(
                        Some(ctx.worktree_root.to_path_buf()),
                        CURRENT_TASK_JOB.scope(
                            Some(Arc::clone(ctx.task_job)),
                            async {
                                if let Some(ref lt) = lang_tool {
                                    ctx.shared
                                        .tool_host
                                        .call_builtin(lt, args.clone(), Some(&trace_str))
                                        .instrument(tracing::info_span!("task.lang_tool", trace_id = %trace_str))
                                        .await
                                } else {
                                    ctx.shared
                                        .tool_host
                                        .call(&call.name, args.clone(), Some(&trace_str))
                                        .instrument(tracing::info_span!("task", trace_id = %trace_str))
                                        .await
                                }
                            },
                        ),
                    )
                    .await;

                match call_res {
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
                }
            };

            if is_error
                && (result_content.contains("requires an approval")
                    || result_content.contains("lacking an approval"))
            {
                let cmd_name = if call.name == "shell.run" {
                    args.get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&call.name)
                        .to_string()
                } else {
                    call.name.clone()
                };

                ctx.shared
                    .notifications
                    .notify_approval_needed(&cmd_name, "System", None, &trace_str);
                let app_needed = nebula_proto::ApprovalNeeded {
                    command: cmd_name.clone(),
                    tier: "System".into(),
                    rule_id: None,
                    trace_id: trace_str.clone(),
                    summary: result_content.clone(),
                };
                ctx.shared.emit_event(Event::ApprovalNeeded(app_needed));

                // Pause execution and wait for user's approval response (via TUI or ntfy)
                let mut resp_rx = ctx.shared.approval_responses.subscribe();
                let wait_for_approval = async {
                    loop {
                        match resp_rx.recv().await {
                            Ok(resp)
                                if resp.trace_id == trace_str
                                    || resp.command == cmd_name
                                    || nebula_sandbox::rules::normalize_command(&resp.command)
                                        == nebula_sandbox::rules::normalize_command(&cmd_name) =>
                            {
                                return Ok(resp.approved);
                            }
                            Ok(_) => continue,
                            Err(_) => return Err("approval notification channel closed"),
                        }
                    }
                };

                let approval_decision = tokio::select! {
                    () = ctx.token.cancelled() => {
                        return (TaskStatus::Cancelled, "task cancelled while waiting for approval".into());
                    }
                    () = tokio::time::sleep(Duration::from_secs(300)) => {
                        Err("approval request timed out after 5 minutes")
                    }
                    decision = wait_for_approval => decision,
                };

                match approval_decision {
                    Ok(true) => {
                        tracing::info!(event = "task.approval_granted", tool = %call.name, command = %cmd_name);
                        let tier = nebula_sandbox::tier::Tier::System;
                        let normalized_cmd = nebula_sandbox::rules::normalize_command(&cmd_name);

                        // Ensure approval grants are available in ctx.shared.approvals
                        let grant_id1 =
                            nebula_sandbox::approval::GrantId::new(ulid::Ulid::generate().0);
                        let scope1 = nebula_sandbox::approval::ApprovalScope {
                            command: normalized_cmd.clone(),
                            tier,
                            trace_id: "task".into(),
                        };
                        ctx.shared
                            .approvals
                            .insert(nebula_sandbox::approval::Approval::grant(grant_id1, scope1));

                        let grant_id2 =
                            nebula_sandbox::approval::GrantId::new(ulid::Ulid::generate().0);
                        let scope2 = nebula_sandbox::approval::ApprovalScope {
                            command: normalized_cmd,
                            tier,
                            trace_id: trace_str.clone(),
                        };
                        ctx.shared
                            .approvals
                            .insert(nebula_sandbox::approval::Approval::grant(grant_id2, scope2));

                        let grant_id3 =
                            nebula_sandbox::approval::GrantId::new(ulid::Ulid::generate().0);
                        let scope3 = nebula_sandbox::approval::ApprovalScope {
                            command: cmd_name.clone(),
                            tier,
                            trace_id: "task".into(),
                        };
                        ctx.shared
                            .approvals
                            .insert(nebula_sandbox::approval::Approval::grant(grant_id3, scope3));

                        let retry_res = CURRENT_WORKTREE
                            .scope(
                                Some(ctx.worktree_root.to_path_buf()),
                                CURRENT_TASK_JOB.scope(
                                    Some(Arc::clone(ctx.task_job)),
                                    ctx.shared
                                        .tool_host
                                        .call(&call.name, args.clone(), Some(&trace_str))
                                        .instrument(
                                            tracing::info_span!("task", trace_id = %trace_str),
                                        ),
                                ),
                            )
                            .await;

                        match retry_res {
                            Ok(outcome) => {
                                is_error = outcome.is_error;
                                result_content = if is_error {
                                    format!("[tool error] {}", outcome.content)
                                } else {
                                    outcome.content.to_string()
                                };
                            }
                            Err(err) => {
                                is_error = true;
                                result_content = format!("[tool dispatch error: {err}]");
                            }
                        }
                    }
                    Ok(false) => {
                        tracing::info!(event = "task.approval_denied", tool = %call.name, command = %cmd_name);
                        result_content =
                            format!("[tool error] command '{cmd_name}' was denied by user");
                        is_error = true;
                    }
                    Err(reason) => {
                        tracing::warn!(event = "task.approval_timeout", reason = %reason);
                        result_content = format!("[tool error] {reason}");
                        is_error = true;
                    }
                }
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

            let outcome_summary = if is_error {
                let first_line = result_content.lines().next().unwrap_or(&result_content);
                format!("  └─> [ERROR] {first_line}\n")
            } else {
                let first_line = result_content.lines().next().unwrap_or("");
                if first_line.is_empty() {
                    "  └─> [OK]\n".to_string()
                } else {
                    format!("  └─> [OK] {first_line}\n")
                }
            };
            ctx.shared.emit_event(Event::ChatToken(ChatToken {
                chat_id,
                text: outcome_summary,
                reasoning: String::new(),
            }));

            let tool_output = format!("[tool result: {}]\n{}", call.name, result_content);
            messages.push(ChatMessage {
                role: Role::Tool,
                content: tool_output,
            });
        }
    }
}

async fn commit_worktree_if_dirty(worktree_root: &Path, goal: &str, task_id: &TaskId) {
    use std::process::Stdio;
    use tokio::process::Command;

    // 1. Check if git status has uncommitted or untracked changes
    let mut status_cmd = Command::new("git");
    status_cmd
        .args(["status", "--porcelain"])
        .current_dir(worktree_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let is_dirty = match status_cmd.output().await {
        Ok(out) => !out.stdout.is_empty(),
        Err(e) => {
            tracing::warn!(event = "task.git_status_failed", error = %e);
            false
        }
    };

    if !is_dirty {
        return;
    }

    // 2. Stage all changes
    let mut add_cmd = Command::new("git");
    add_cmd
        .args(["add", "-A"])
        .current_dir(worktree_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    if let Err(e) = add_cmd.output().await {
        tracing::warn!(event = "task.git_add_failed", error = %e);
        return;
    }

    // 3. Commit staged changes with conventional commit format
    let commit_msg = format!(
        "feat: {goal}\n\nAutomated commit for task {task_id}.\n\nCo-authored-by: Nebula <nebula@local>"
    );
    let mut commit_cmd = Command::new("git");
    commit_cmd
        .args([
            "-c",
            "user.name=Nebula",
            "-c",
            "user.email=nebula@local",
            "commit",
            "-m",
            &commit_msg,
        ])
        .current_dir(worktree_root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    match commit_cmd.output().await {
        Ok(out) if out.status.success() => {
            tracing::info!(event = "task.worktree_committed", task_id = %task_id);
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            tracing::warn!(event = "task.git_commit_failed", stderr = %stderr);
        }
        Err(e) => {
            tracing::warn!(event = "task.git_commit_spawn_failed", error = %e);
        }
    }
}
