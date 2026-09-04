use std::{collections::VecDeque, future::Future, pin::Pin, time::Duration};

use bone_llm::{
    FinishReason, InputItem, Model, Request, Response, ToolCall, ToolDefinition, ToolOutput,
};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use tokio::time::timeout;

use crate::{
    Action, ActionError, ToolFailure, ToolFailureKind, Turn,
    tools::{ToolOutcome, Tools, missing_tool},
};

const DEFAULT_MAX_TURNS: usize = 32;
const DEFAULT_MAX_TOOL_CALLS_PER_TURN: usize = 16;
const DEFAULT_MODEL_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(900);

#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) max_turns: usize,
    pub(crate) max_tool_calls_per_turn: usize,
    pub(crate) model_timeout: Duration,
    pub(crate) tool_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_turns: DEFAULT_MAX_TURNS,
            max_tool_calls_per_turn: DEFAULT_MAX_TOOL_CALLS_PER_TURN,
            model_timeout: DEFAULT_MODEL_TIMEOUT,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
        }
    }
}

struct FinishedTool {
    action: usize,
    turn: usize,
    tool: usize,
    outcome: ToolOutcome,
}

type RunningTool = Pin<Box<dyn Future<Output = FinishedTool> + Send + 'static>>;

pub(crate) async fn drive(
    model: &Model,
    instructions: Option<&str>,
    tools: &Tools,
    limits: Limits,
    mut actions: Vec<Action>,
) -> Vec<Action> {
    let mut ready = (0..actions.len()).collect::<VecDeque<_>>();
    let mut running = FuturesUnordered::<RunningTool>::new();

    loop {
        while let Some(action_index) = ready.pop_front() {
            if actions[action_index].turns().len() >= limits.max_turns {
                actions[action_index].fail(ActionError::TurnLimit {
                    limit: limits.max_turns,
                });
                continue;
            }

            let input = actions[action_index].model_input();
            let definitions = tools.definitions();
            let completion = complete(
                model,
                input,
                instructions,
                definitions,
                limits.model_timeout,
            );
            tokio::pin!(completion);

            let response = loop {
                if running.is_empty() {
                    break completion.await;
                }

                tokio::select! {
                    response = &mut completion => break response,
                    Some(finished) = running.next() => {
                        settle_tool(&mut actions, &mut ready, finished);
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

            let finish_reason = response.finish_reason().cloned();
            if response.items().is_empty() {
                actions[action_index].push_turn(Turn::new(response, Vec::new()));
                actions[action_index].fail(ActionError::Incomplete { finish_reason });
                continue;
            }

            let calls = response.tool_calls().cloned().collect::<Vec<_>>();

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

            if calls.is_empty() {
                let output = response.text();
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
        settle_tool(&mut actions, &mut ready, finished);
    }

    actions
}

async fn complete(
    model: &Model,
    input: Vec<InputItem>,
    instructions: Option<&str>,
    tools: Vec<ToolDefinition>,
    deadline: Duration,
) -> Result<Response, ActionError> {
    let mut request = Request::new(input).tools(tools);
    if let Some(instructions) = instructions.filter(|text| !text.trim().is_empty()) {
        request = request.instructions(instructions);
    }
    let completion = model.complete(request);
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
) -> RunningTool {
    let execution = tools.execute(call.name(), call.arguments().clone());
    Box::pin(async move {
        let outcome = match execution {
            Some(execution) => {
                let name = call.name().to_owned();
                match timeout(deadline, execution).await {
                    Ok(outcome) => outcome,
                    Err(_) => {
                        let feedback = format!("tool `{name}` timed out after {deadline:?}");
                        ToolOutcome::failed(ToolFailure::new(
                            ToolFailureKind::Timeout,
                            feedback.clone(),
                            ToolOutput::text(feedback),
                        ))
                    }
                }
            }
            None => missing_tool(call.name()),
        };
        FinishedTool {
            action,
            turn,
            tool,
            outcome,
        }
    })
}

fn poll_ready_tools(
    actions: &mut [Action],
    ready: &mut VecDeque<usize>,
    running: &mut FuturesUnordered<RunningTool>,
) {
    while let Some(Some(finished)) = running.next().now_or_never() {
        settle_tool(actions, ready, finished);
    }
}

fn settle_tool(actions: &mut [Action], ready: &mut VecDeque<usize>, finished: FinishedTool) {
    let action = finished.action;
    if actions[action].record_outcome(finished.turn, finished.tool, finished.outcome) {
        ready.push_back(action);
    }
}
