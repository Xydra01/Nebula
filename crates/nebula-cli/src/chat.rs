//! `nebula chat`: line-based, multi-turn; the history lives here, not in the daemon.

use std::io::Write;

use anyhow::Context as _;
use nebula_daemon::client::Client;
use nebula_proto::{
    ChatCancelParams, ChatId, ChatMessage, ChatStartParams, ChatStarted, Empty, Event, Method,
    ReasoningEffort, Role, StopReason, Usage, error_code,
};
use serde::Serialize;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use crate::{ChatArgs, Ctx, connect, emit_json, format};

/// One finished turn, as printed with `--json`.
#[derive(Debug, Serialize)]
struct Turn {
    chat_id: ChatId,
    text: String,
    reasoning: String,
    stop_reason: StopReason,
    usage: Usage,
}

enum Outcome {
    Done(Turn),
    Failed { code: i64, message: String },
}

pub(crate) async fn chat(
    ctx: &Ctx,
    args: &ChatArgs,
    input: impl AsyncBufRead + Unpin,
    out: &mut dyn Write,
) -> anyhow::Result<u8> {
    let schema = match &args.schema {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            Some(
                serde_json::from_str(&text)
                    .with_context(|| format!("{} is not JSON", path.display()))?,
            )
        }
        None => None,
    };
    let mut c = connect(ctx).await?;
    let mut history: Vec<ChatMessage> = Vec::new();
    let mut lines = input.lines();
    if ctx.interactive && !ctx.json {
        writeln!(
            out,
            "{}",
            ctx.style.dim("/reset clears the history, /exit quits")
        )?;
    }
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        if ctx.interactive && !ctx.json {
            write!(out, "{} ", ctx.style.bold("you>"))?;
            out.flush()?;
        }
        let line = tokio::select! {
            _ = &mut ctrl_c => break,
            line = lines.next_line() => line?,
        };
        let Some(line) = line else { break };
        match line.trim() {
            "" => continue,
            "/exit" | "/quit" => break,
            "/reset" => {
                history.clear();
                if !ctx.json {
                    writeln!(out, "{}", ctx.style.dim("(history cleared)"))?;
                }
                continue;
            }
            _ => {}
        }
        history.push(ChatMessage {
            role: Role::User,
            content: line,
        });
        let params = ChatStartParams {
            messages: history.clone(),
            profile: args.profile.clone(),
            response_schema: schema.clone(),
            max_tokens: args.max_tokens,
            reasoning: args.reasoning.map(ReasoningEffort::from),
        };
        let started: ChatStarted = c.call(Method::ChatStart(params)).await?;
        match stream_reply(ctx, &mut c, started.chat_id, out).await? {
            Outcome::Done(turn) => {
                if ctx.json {
                    emit_json(out, &turn)?;
                } else {
                    writeln!(
                        out,
                        "{}",
                        format::usage_line(&turn.usage, turn.stop_reason, ctx.style)
                    )?;
                }
                history.push(ChatMessage {
                    role: Role::Assistant,
                    content: turn.text,
                });
            }
            Outcome::Failed { code, message } => {
                history.pop();
                if ctx.json {
                    emit_json(
                        out,
                        &serde_json::json!({ "chat_id": started.chat_id, "error": { "code": code, "message": message } }),
                    )?;
                } else if code == error_code::CANCELLED {
                    writeln!(out, "{}", ctx.style.dim("(cancelled)"))?;
                } else {
                    writeln!(out, "{}", ctx.style.bad(&format!("error: {message}")))?;
                }
            }
        }
    }
    Ok(0)
}

async fn stream_reply(
    ctx: &Ctx,
    c: &mut Client,
    chat_id: ChatId,
    out: &mut dyn Write,
) -> anyhow::Result<Outcome> {
    let (mut text, mut reasoning) = (String::new(), String::new());
    let show = !ctx.json;
    let show_reasoning = show && ctx.interactive;
    let mut in_reasoning = false;
    let mut cancel_sent = false;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        let ev = tokio::select! {
            ev = c.next_event() => ev?,
            _ = &mut ctrl_c, if !cancel_sent => {
                cancel_sent = true;
                let _ = c.call::<Empty>(Method::ChatCancel(ChatCancelParams { chat_id })).await;
                continue;
            }
        };
        match ev {
            Event::ChatToken(t) if t.chat_id == chat_id => {
                if !t.reasoning.is_empty() {
                    if show_reasoning {
                        write!(out, "{}", ctx.style.dim(&t.reasoning))?;
                        in_reasoning = true;
                    }
                    reasoning.push_str(&t.reasoning);
                }
                if !t.text.is_empty() {
                    if show {
                        if in_reasoning {
                            writeln!(out, "\n")?;
                            in_reasoning = false;
                        }
                        write!(out, "{}", t.text)?;
                    }
                    text.push_str(&t.text);
                }
                out.flush()?;
            }
            Event::ChatDone(d) if d.chat_id == chat_id => {
                if show {
                    writeln!(out)?;
                }
                return Ok(Outcome::Done(Turn {
                    chat_id,
                    text,
                    reasoning,
                    stop_reason: d.stop_reason,
                    usage: d.usage,
                }));
            }
            Event::ChatError(e) if e.chat_id == chat_id => {
                if show && (!text.is_empty() || in_reasoning) {
                    writeln!(out)?;
                }
                return Ok(Outcome::Failed {
                    code: e.code,
                    message: e.message,
                });
            }
            _ => {}
        }
    }
}
