//! The agent turn loop: assistant turns, tool-call batches,
//! steering/follow-up/continuation message polling, and stop-hook
//! evaluation.

use std::sync::Arc;

use crate::abort::AbortSignal;
use crate::stream::StreamFn;
use crate::types::{
    AgentContext, AgentEvent, AgentMessage, AgentTool, AssistantContent, Message,
    ShouldStopAfterTurnContext, StopReason, ToolCall, ToolResultContent, ToolResultMessage,
    UserContent, UserMessage,
};

use super::abort::{
    poll_messages_unless_aborted, race_with_abort, settle_post_turn, PostTurnResult,
};
use super::response::stream_assistant_response;
use super::tools::execute_tool_calls;
use super::{AgentEventSink, AgentLoopConfig};

/// Sent once when a mid-task turn ends with only reasoning: some models (seen
/// on Qwen3.8 under exact sampling) announce an action in the thinking block
/// and then stop, and resending the identical prompt reproduces the stall.
const REASONING_ONLY_STALL_NUDGE: &str = "Continue: make the tool call you planned.";

// Direct port of the TS turn loop.
#[allow(clippy::too_many_lines)]
pub(crate) async fn run_loop(
    current_context: &mut AgentContext,
    new_messages: &mut Vec<AgentMessage>,
    config: &AgentLoopConfig,
    signal: Option<&AbortSignal>,
    emit: &AgentEventSink,
    stream_fn: Option<&StreamFn>,
) -> anyhow::Result<()> {
    let mut first_turn = true;
    let mut last_turn: Option<ShouldStopAfterTurnContext> = None;
    let mut stall_nudged = false;
    let mut error_memo_nudged = false;
    let mut pseudo_memo_nudged = false;
    let mut pending_messages =
        poll_messages_unless_aborted(config.get_steering_messages.as_ref(), signal).await?;

    macro_rules! should_stop_before_turn {
        () => {
            !first_turn
                && config
                    .should_stop_before_turn
                    .as_ref()
                    .map(|hook| hook())
                    .unwrap_or(false)
        };
    }

    loop {
        crate::abort::throw_if_aborted_signal(signal)?;
        let mut has_more_tool_calls = true;

        while has_more_tool_calls || !pending_messages.is_empty() {
            crate::abort::throw_if_aborted_signal(signal)?;
            if first_turn {
                first_turn = false;
            } else {
                emit(AgentEvent::TurnStart).await?;
            }

            if !pending_messages.is_empty() {
                for message in pending_messages.drain(..) {
                    emit(AgentEvent::MessageStart {
                        message: message.clone(),
                    })
                    .await?;
                    emit(AgentEvent::MessageEnd {
                        message: message.clone(),
                    })
                    .await?;
                    current_context.messages.push(message.clone());
                    new_messages.push(message);
                }
            }

            let follows_tool_result = matches!(
                current_context.messages.last(),
                Some(AgentMessage::Standard(Message::ToolResult(_)))
            );
            let message =
                stream_assistant_response(current_context, config, signal, emit, stream_fn).await?;
            new_messages.push(AgentMessage::from(message.clone()));

            if message.stop_reason == StopReason::Error
                || message.stop_reason == StopReason::Aborted
            {
                emit(AgentEvent::TurnEnd {
                    message: AgentMessage::from(message.clone()),
                    tool_results: Vec::new(),
                })
                .await?;
                emit(AgentEvent::AgentEnd {
                    aborted: message.stop_reason == StopReason::Aborted,
                    messages: new_messages.clone(),
                })
                .await?;
                return Ok(());
            }

            let tool_calls = message
                .tool_calls()
                .into_iter()
                .cloned()
                .collect::<Vec<ToolCall>>();

            // No visible text means the model neither answered nor asked the
            // user anything; the turn only ends a run because it lacks a tool call.
            let reasoning_only_stall = follows_tool_result
                && message.stop_reason == StopReason::Stop
                && tool_calls.is_empty()
                && !message.content.iter().any(|content| match content {
                    AssistantContent::Text(text) => !text.text.trim().is_empty(),
                    AssistantContent::Thinking(_) | AssistantContent::ToolCall(_) => false,
                })
                && message.content.iter().any(|content| match content {
                    AssistantContent::Thinking(thinking) => !thinking.thinking.trim().is_empty(),
                    AssistantContent::Text(_) | AssistantContent::ToolCall(_) => false,
                });

            let mut tool_results: Vec<ToolResultMessage> = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                let executed_tool_batch =
                    execute_tool_calls(current_context, &message, config, signal, emit).await?;
                tool_results.extend(executed_tool_batch.messages);
                has_more_tool_calls = !executed_tool_batch.terminate;

                for result in &tool_results {
                    current_context
                        .messages
                        .push(AgentMessage::from(result.clone()));
                    new_messages.push(AgentMessage::from(result.clone()));
                }
            }

            emit(AgentEvent::TurnEnd {
                message: AgentMessage::from(message.clone()),
                tool_results: tool_results.clone(),
            })
            .await?;
            if signal.is_some_and(AbortSignal::is_aborted) {
                emit(AgentEvent::AgentEnd {
                    aborted: true,
                    messages: new_messages.clone(),
                })
                .await?;
                return Ok(());
            }
            last_turn = Some(ShouldStopAfterTurnContext {
                message: message.clone(),
                tool_results: tool_results.clone(),
                context: clone_context(current_context),
                new_messages: new_messages.clone(),
            });

            let should_stop_result = settle_post_turn(
                race_with_abort(
                    async {
                        match config.should_stop_after_turn.as_ref() {
                            Some(hook) => hook(last_turn.clone().unwrap()).await,
                            None => Ok(false),
                        }
                    },
                    signal,
                ),
                signal,
            )
            .await?;
            match should_stop_result {
                PostTurnResult::Aborted | PostTurnResult::Completed(true) => {
                    emit(AgentEvent::AgentEnd {
                        aborted: matches!(should_stop_result, PostTurnResult::Aborted),
                        messages: new_messages.clone(),
                    })
                    .await?;
                    return Ok(());
                }
                PostTurnResult::Completed(false) => {}
            }
            if should_stop_before_turn!() {
                emit(AgentEvent::AgentEnd {
                    aborted: false,
                    messages: new_messages.clone(),
                })
                .await?;
                return Ok(());
            }

            let steering_messages_result = settle_post_turn(
                poll_messages_unless_aborted(config.get_steering_messages.as_ref(), signal),
                signal,
            )
            .await?;
            match steering_messages_result {
                PostTurnResult::Aborted => {
                    emit(AgentEvent::AgentEnd {
                        aborted: true,
                        messages: new_messages.clone(),
                    })
                    .await?;
                    return Ok(());
                }
                PostTurnResult::Completed(messages) => {
                    pending_messages = messages;
                    let failed_tool_calls =
                        tool_results.iter().filter(|result| result.is_error).count();
                    if failed_tool_calls > 0 {
                        stall_nudged = false;
                        pseudo_memo_nudged = false;
                        if pending_messages.is_empty() && !error_memo_nudged {
                            error_memo_nudged = true;
                            let total_tool_calls = tool_results.len();
                            let available_tools = format_available_tools(&current_context.tools);
                            let mut failures = Vec::new();
                            for result in tool_results.iter().filter(|result| result.is_error) {
                                let detail = result
                                    .content
                                    .iter()
                                    .filter_map(|content| match content {
                                        ToolResultContent::Text(text) => Some(text.text.as_str()),
                                        ToolResultContent::Image(_) => None,
                                    })
                                    .collect::<Vec<_>>()
                                    .join(" ");
                                let first_line =
                                    detail.lines().next().unwrap_or("unknown error").trim();
                                let first_line: String = first_line.chars().take(300).collect();
                                failures
                                    .push(format!("- '{}' failed: {first_line}", result.tool_name));
                            }
                            let failures = failures.join("\n");
                            pending_messages.push(AgentMessage::user(format!(
                                "Memo: {failed_tool_calls} of {total_tool_calls} tool call(s) failed.\n{failures}\nAvailable tools: {available_tools}. Fix the failing call and retry; do not repeat the identical failing call."
                            )));
                        }
                    } else if tool_calls.is_empty()
                        && message.content.iter().any(|content| match content {
                            AssistantContent::Text(text) => {
                                let lower = text.text.to_ascii_lowercase();
                                let has_tag = lower.contains("<tool_call")
                                    || lower.contains("<tool-call")
                                    || lower.contains("<function_call")
                                    || lower.contains("<invoke");
                                let has_call_shape = lower.contains("\"name\"")
                                    || lower.contains("\"args\"")
                                    || lower.contains("\"arguments\"")
                                    || lower.contains("<name>")
                                    || lower.contains("name=");
                                has_tag && has_call_shape
                            }
                            AssistantContent::Thinking(_) | AssistantContent::ToolCall(_) => false,
                        })
                    {
                        stall_nudged = false;
                        error_memo_nudged = false;
                        if pending_messages.is_empty() && !pseudo_memo_nudged {
                            pseudo_memo_nudged = true;
                            let available_tools = format_available_tools(&current_context.tools);
                            pending_messages.push(AgentMessage::user(format!(
                                "Memo: your previous reply wrote a tool call as plain text instead of invoking a tool, so nothing ran.\nDo not output XML tags or JSON tool calls (or \"args\") in assistant text.\nInvoke a registered tool directly through the native function-calling interface.\nAvailable tools: {available_tools}."
                            )));
                        }
                    } else if !reasoning_only_stall {
                        stall_nudged = false;
                        error_memo_nudged = false;
                        pseudo_memo_nudged = false;
                    } else if pending_messages.is_empty() && !stall_nudged {
                        stall_nudged = true;
                        pending_messages.push(AgentMessage::from(UserMessage {
                            content: UserContent::Text(REASONING_ONLY_STALL_NUDGE.to_string()),
                            timestamp: crate::now_ms(),
                        }));
                    }
                    // Steering drained by this poll owns the turn boundary;
                    // stop only when it was empty.
                    if pending_messages.is_empty() && should_stop_before_turn!() {
                        emit(AgentEvent::AgentEnd {
                            aborted: false,
                            messages: new_messages.clone(),
                        })
                        .await?;
                        return Ok(());
                    }
                }
            }
        }

        if should_stop_before_turn!() {
            break;
        }
        let follow_up_messages_result = settle_post_turn(
            poll_messages_unless_aborted(config.get_follow_up_messages.as_ref(), signal),
            signal,
        )
        .await?;
        let follow_up_messages = match follow_up_messages_result {
            PostTurnResult::Aborted => {
                emit(AgentEvent::AgentEnd {
                    aborted: true,
                    messages: new_messages.clone(),
                })
                .await?;
                return Ok(());
            }
            PostTurnResult::Completed(messages) => messages,
        };
        if !follow_up_messages.is_empty() {
            pending_messages = follow_up_messages;
            continue;
        }

        if should_stop_before_turn!() {
            break;
        }
        let continuation_messages_result = match last_turn.clone() {
            Some(context) => {
                let continuation_op: crate::BoxFut<'static, anyhow::Result<Vec<AgentMessage>>> =
                    match config.get_continuation_messages.as_ref() {
                        Some(hook) => hook(context, signal.cloned().unwrap_or_default()),
                        None => Box::pin(async { Ok(Vec::new()) }),
                    };
                settle_post_turn(race_with_abort(continuation_op, signal), signal).await?
            }
            None => PostTurnResult::Completed(Vec::new()),
        };
        let continuation_messages = match continuation_messages_result {
            PostTurnResult::Aborted => {
                emit(AgentEvent::AgentEnd {
                    aborted: true,
                    messages: new_messages.clone(),
                })
                .await?;
                return Ok(());
            }
            PostTurnResult::Completed(messages) => messages,
        };
        if !continuation_messages.is_empty() {
            pending_messages = continuation_messages;
            continue;
        }

        break;
    }

    emit(AgentEvent::AgentEnd {
        aborted: false,
        messages: new_messages.clone(),
    })
    .await?;
    Ok(())
}

use super::clone_context;

fn format_available_tools(tools: &[Arc<dyn AgentTool>]) -> String {
    if tools.is_empty() {
        return "none".to_string();
    }
    tools
        .iter()
        .map(|tool| {
            let schema = tool.parameters();
            let required_params: Vec<&str> = schema
                .get("required")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();
            let params = if !required_params.is_empty() {
                required_params.join(", ")
            } else if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
                props
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            } else {
                String::new()
            };
            if params.is_empty() {
                tool.name().to_string()
            } else {
                format!("{}({params})", tool.name())
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
