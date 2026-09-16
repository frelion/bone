use std::sync::Arc;

use bone_core::{CallContext, PortFuture, ToolEffect, ToolOutcome, ToolPort, ToolSpec};
use serde_json::Value;

use crate::tools::{Tool, ToolEnvironment};

/// Read-only tool adapters. Classification belongs to the adapter, never
/// to model-supplied arguments. Write tools use an effect-aware application adapter.
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

    fn run(&self, arguments: Value, _context: CallContext) -> PortFuture<ToolOutcome> {
        let tool = Arc::clone(&self.tool);
        Box::pin(async move {
            let arguments = match serde_json::from_value::<T::Args>(arguments) {
                Ok(arguments) => arguments,
                Err(_) => {
                    return ToolOutcome::failed("tool arguments do not match the declared schema");
                }
            };
            let output = match tool.call(arguments).await {
                Ok(output) => output,
                Err(error) => {
                    let failure = tool.map_error(error);
                    let message = failure
                        .model_output()
                        .as_text()
                        .map(str::to_owned)
                        .or_else(|| failure.model_output().as_json().map(Value::to_string))
                        .unwrap_or_else(|| "tool execution failed".into());
                    return ToolOutcome::failed(message);
                }
            };
            match serde_json::to_value(output) {
                Ok(value) => ToolOutcome::value(value),
                Err(_) => ToolOutcome::failed("tool output could not be serialized"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_adapters_preserve_tool_effects() {
        let workspace = tempfile::tempdir().unwrap();
        let environment = ToolEnvironment::new(workspace.path()).unwrap();
        let tools = read_only_tools(&environment);
        let specifications = tools
            .iter()
            .map(|tool| tool.specification())
            .collect::<Vec<_>>();

        assert_eq!(
            specifications
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["read", "glob", "grep"]
        );
        assert!(
            specifications
                .iter()
                .all(|tool| tool.effect == ToolEffect::ReadOnly)
        );
    }
}
