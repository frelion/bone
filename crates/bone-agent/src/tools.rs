use std::{collections::BTreeMap, sync::Arc};

use bone_provider::rig::{
    completion::ToolDefinition,
    tool::{
        IntoToolOutput, PortableDynamicTool, PortableTool, ToolExecutionError,
        ToolResult as ExecutionToolResult,
    },
};

use crate::{AgentConfigError, agent::START_ACTION_TOOL};

#[derive(Debug, Default)]
pub(crate) struct Tools {
    entries: BTreeMap<String, PortableDynamicTool>,
}

impl Tools {
    pub(crate) fn register<T>(&mut self, tool: T) -> Result<(), AgentConfigError>
    where
        T: PortableTool + 'static,
        T::Args: 'static,
        T::Output: 'static,
    {
        self.register_dynamic(erase(tool))
    }

    pub(crate) fn register_dynamic(
        &mut self,
        tool: PortableDynamicTool,
    ) -> Result<(), AgentConfigError> {
        let name = tool.name().to_owned();
        if name == START_ACTION_TOOL {
            return Err(AgentConfigError::ReservedTool);
        }
        if self.entries.contains_key(&name) {
            return Err(AgentConfigError::DuplicateTool(name));
        }
        self.entries.insert(name, tool);
        Ok(())
    }

    pub(crate) fn definitions(&self) -> Vec<ToolDefinition> {
        self.entries
            .values()
            .map(PortableDynamicTool::definition)
            .collect()
    }

    pub(crate) fn get(&self, name: &str) -> Option<PortableDynamicTool> {
        self.entries.get(name).cloned()
    }
}

pub(crate) fn missing_tool(name: &str) -> ExecutionToolResult {
    ExecutionToolResult::failed(ToolExecutionError::not_found(format!(
        "tool `{name}` is not registered"
    )))
}

fn erase<T>(tool: T) -> PortableDynamicTool
where
    T: PortableTool + 'static,
    T::Args: 'static,
    T::Output: 'static,
{
    let description = tool.description();
    let parameters = tool.parameters();
    let tool = Arc::new(tool);

    PortableDynamicTool::new(T::NAME, description, parameters, move |arguments| {
        let tool = Arc::clone(&tool);
        Box::pin(async move {
            let arguments = serde_json::from_value::<T::Args>(arguments).map_err(|error| {
                ToolExecutionError::invalid_args(format!(
                    "arguments for tool `{}` did not match its schema",
                    T::NAME
                ))
                .with_source(error)
            })?;
            let output = tool
                .call(arguments)
                .await
                .map_err(|error| tool.map_error(error))?;
            output.into_tool_output()
        })
    })
}
