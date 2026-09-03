use std::{
    collections::{HashSet, VecDeque},
    time::Duration,
};

use bone_provider::rig::{
    completion::{AssistantContent, CompletionModel, CompletionResponse, FinishReason},
    message::ToolCall,
    tool::{ToolExecutionError, ToolResult as ExecutionToolResult},
    wasm_compat::{WasmBoxedFuture, timeout},
};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};

use crate::{
    Action, ActionError, ActionState, Turn,
    agent::Limits,
    tools::{Tools, missing_tool},
};

struct FinishedTool {
    action: usize,
    turn: usize,
    tool: usize,
    result: ExecutionToolResult,
}

pub(crate) async fn drive<M>(
    model: &M,
    instructions: Option<&str>,
    tools: &Tools,
    limits: Limits,
    mut actions: Vec<Action>,
) -> Vec<Action>
where
    M: CompletionModel + Clone,
{
    let mut ready = (0..actions.len()).collect::<VecDeque<_>>();
    let mut running = FuturesUnordered::<WasmBoxedFuture<'static, FinishedTool>>::new();

    loop {
        while let Some(action_index) = ready.pop_front() {
            if actions[action_index].state() != ActionState::Ready {
                continue;
            }
            if actions[action_index].turns().len() >= limits.max_turns {
                actions[action_index].fail(ActionError::TurnLimit {
                    limit: limits.max_turns,
                });
                continue;
            }

            let messages = actions[action_index].messages(instructions);
            let definitions = tools.definitions();
            let completion = complete(model, messages, definitions, limits.model_timeout);
            tokio::pin!(completion);

            let response = loop {
                if running.is_empty() {
                    break completion.await;
                }

                tokio::select! {
                    response = &mut completion => break response,
                    Some(finished) = running.next() => {
                        let finished_action = finished.action;
                        if record_result(&mut actions, finished) {
                            ready.push_back(finished_action);
                        }
                    }
                }
            };

            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    actions[action_index].fail(error);
                    continue;
                }
            };

            let finish_reason = response.finish_reason();
            if response.choice.is_empty() {
                actions[action_index].push_turn(Turn::new(response, Vec::new()));
                actions[action_index].fail(ActionError::Incomplete { finish_reason });
                continue;
            }

            let calls = response
                .choice
                .iter()
                .filter_map(|content| match content {
                    AssistantContent::ToolCall(call) => Some(call.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();

            if finish_reason
                .as_ref()
                .is_some_and(FinishReason::truncated_output)
            {
                // A truncated tool call may be syntactically valid but still
                // incomplete. Preserve it, but never execute its side effect.
                actions[action_index].push_turn(Turn::skipped(
                    response,
                    calls,
                    "model response was truncated; tool was not executed",
                ));
                actions[action_index].fail(ActionError::Incomplete { finish_reason });
                continue;
            }

            if calls.len() > limits.max_tool_calls_per_turn {
                let requested = calls.len();
                actions[action_index].push_turn(Turn::skipped(
                    response,
                    calls,
                    "tool batch exceeded the per-turn limit; tool was not executed",
                ));
                actions[action_index].fail(ActionError::ToolCallLimit {
                    requested,
                    limit: limits.max_tool_calls_per_turn,
                });
                continue;
            }

            if !unique_call_ids(actions[action_index].tool_calls().chain(calls.iter())) {
                actions[action_index].push_turn(Turn::skipped(
                    response,
                    calls,
                    "tool-call identifiers were duplicated; tools were not executed",
                ));
                actions[action_index].fail(ActionError::DuplicateToolCall);
                continue;
            }

            if calls.is_empty() {
                let output = final_text(&response);
                actions[action_index].push_turn(Turn::new(response, calls));
                match output {
                    Some(output) => actions[action_index].complete(output),
                    None => {
                        actions[action_index].fail(ActionError::Incomplete { finish_reason });
                    }
                }
                continue;
            }

            let turn_index = actions[action_index].push_turn(Turn::new(response, calls.clone()));
            for (tool_index, call) in calls.into_iter().enumerate() {
                running.push(start_tool(
                    action_index,
                    turn_index,
                    tool_index,
                    call,
                    tools,
                    limits.tool_timeout,
                ));
            }
            poll_ready_tools(&mut actions, &mut ready, &mut running);
        }

        let Some(finished) = running.next().await else {
            break;
        };
        let action_index = finished.action;
        if record_result(&mut actions, finished) {
            ready.push_back(action_index);
        }
    }

    actions
}

async fn complete<M>(
    model: &M,
    mut messages: Vec<bone_provider::rig::message::Message>,
    tools: Vec<bone_provider::rig::completion::ToolDefinition>,
    deadline: Duration,
) -> Result<CompletionResponse, ActionError>
where
    M: CompletionModel + Clone,
{
    let prompt = messages
        .pop()
        .expect("an action transcript always contains its intent");
    let completion = model
        .completion_request(prompt)
        .messages(messages)
        .tools(tools)
        .send();
    match timeout(deadline, completion).await {
        Ok(response) => response.map_err(ActionError::Model),
        Err(_) => Err(ActionError::ModelTimeout { timeout: deadline }),
    }
}

fn start_tool(
    action: usize,
    turn: usize,
    tool: usize,
    call: ToolCall,
    tools: &Tools,
    deadline: Duration,
) -> WasmBoxedFuture<'static, FinishedTool> {
    let registered = tools.get(&call.function.name);
    Box::pin(async move {
        let result = match registered {
            Some(registered) => {
                let name = call.function.name.clone();
                match timeout(deadline, registered.execute(call.function.arguments)).await {
                    Ok(Ok(output)) => ExecutionToolResult::success(output),
                    Ok(Err(error)) => ExecutionToolResult::failed(error),
                    Err(_) => ExecutionToolResult::failed(ToolExecutionError::timeout(format!(
                        "tool `{name}` timed out after {deadline:?}"
                    ))),
                }
            }
            None => missing_tool(&call.function.name),
        };
        FinishedTool {
            action,
            turn,
            tool,
            result,
        }
    })
}

pub(crate) fn unique_call_ids<'a>(calls: impl IntoIterator<Item = &'a ToolCall>) -> bool {
    let mut ids = HashSet::new();
    let mut provider_ids = HashSet::new();
    let mut provider_item_ids = HashSet::new();

    for call in calls {
        if !ids.insert(call.id.as_str()) {
            return false;
        }
        let Some(provider) = &call.provider else {
            continue;
        };
        if !provider_ids.insert(provider.call_id.as_str()) {
            return false;
        }
        if let Some(item_id) = provider.item_id.as_deref()
            && !provider_item_ids.insert(item_id)
        {
            return false;
        }
    }
    true
}

fn poll_ready_tools(
    actions: &mut [Action],
    ready: &mut VecDeque<usize>,
    running: &mut FuturesUnordered<WasmBoxedFuture<'static, FinishedTool>>,
) {
    while let Some(Some(finished)) = running.next().now_or_never() {
        let action_index = finished.action;
        if record_result(actions, finished) {
            ready.push_back(action_index);
        }
    }
}

fn record_result(actions: &mut [Action], finished: FinishedTool) -> bool {
    actions[finished.action].record_result(finished.turn, finished.tool, finished.result)
}

pub(crate) fn final_text(response: &CompletionResponse) -> Option<String> {
    let parts = response
        .choice
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) if !text.text.trim().is_empty() => {
                Some(text.text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}
