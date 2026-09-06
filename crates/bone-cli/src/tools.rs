use std::{future::Future, pin::Pin, sync::Arc};

use bone_agent::{JobContext, JobOutcome, ToolEffect, ToolPort, ToolSpec};
use bone_tools::{Tool, ToolEnvironment};
use serde_json::Value;

use crate::model::cancelled;

/// The CLI's first tool set. Classification belongs to the adapter, never to
/// model-supplied arguments. Write tools need their own effect-aware adapter.
pub fn read_only_tools(environment: &ToolEnvironment) -> Vec<Arc<dyn ToolPort>> {
    vec![
        Arc::new(ReadOnlyTool::new(environment.read())),
        Arc::new(ReadOnlyTool::new(environment.glob())),
        Arc::new(ReadOnlyTool::new(environment.grep())),
    ]
}

struct ReadOnlyTool<T> {
    tool: Arc<T>,
}

impl<T> ReadOnlyTool<T> {
    fn new(tool: T) -> Self {
        Self {
            tool: Arc::new(tool),
        }
    }
}

impl<T: Tool + 'static> ToolPort for ReadOnlyTool<T> {
    fn specification(&self) -> ToolSpec {
        let definition = self.tool.definition();
        ToolSpec {
            name: definition.name().to_owned(),
            description: definition.description().to_owned(),
            parameters: definition.parameters().clone(),
            effect: ToolEffect::ReadOnly,
        }
    }

    fn run(
        &self,
        arguments: Value,
        mut context: JobContext,
    ) -> Pin<Box<dyn Future<Output = JobOutcome> + Send + 'static>> {
        let tool = Arc::clone(&self.tool);
        Box::pin(async move {
            let arguments = match serde_json::from_value::<T::Args>(arguments) {
                Ok(arguments) => arguments,
                Err(_) => {
                    return JobOutcome::failed("tool arguments do not match the declared schema");
                }
            };
            let output = tokio::select! {
                biased;
                _ = context.wait_for_cancellation() => return cancelled(),
                result = tool.call(arguments) => match result {
                    Ok(output) => output,
                    Err(error) => {
                        let failure = tool.map_error(error);
                        let message = failure.model_output().as_text()
                            .map(str::to_owned)
                            .or_else(|| failure.model_output().as_json().map(Value::to_string))
                            .unwrap_or_else(|| "tool execution failed".into());
                        return JobOutcome::failed(message);
                    }
                },
            };
            match serde_json::to_value(output) {
                Ok(value) => JobOutcome::artifact(value),
                Err(_) => JobOutcome::failed("tool output could not be serialized"),
            }
        })
    }
}
