use std::{path::Path, sync::Arc};

use bone_llm::{Model, ModelOptions, Protocol, Request};
use bone_tools::{ToolEnvironment, ToolError, ToolLimits};

use crate::{Agent, AgentLimits, ModelAdapter, RuntimeError, read_only_tools};

/// A connected model with protocol-specific request defaults already checked.
#[derive(Clone, Debug)]
pub struct ConfiguredModel {
    model: Model,
    options: Option<ModelOptions>,
}

impl ConfiguredModel {
    pub fn new(model: Model, options: Option<ModelOptions>) -> Result<Self, ConfiguredModelError> {
        if let Some(options) = options.as_ref() {
            options.validate()?;
            if options.protocol() != model.protocol() {
                return Err(ConfiguredModelError::Protocol {
                    model: model.protocol(),
                    options: options.protocol(),
                });
            }
        }
        Ok(Self { model, options })
    }

    pub fn without_options(model: Model) -> Self {
        Self {
            model,
            options: None,
        }
    }

    pub(crate) fn model(&self) -> &Model {
        &self.model
    }

    pub(crate) fn apply_to(&self, request: Request) -> Result<Request, ConfiguredModelError> {
        match self.options.clone() {
            Some(options) => Ok(options.apply_to(request)?),
            None => Ok(request),
        }
    }
}

impl From<Model> for ConfiguredModel {
    fn from(model: Model) -> Self {
        Self::without_options(model)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConfiguredModelError {
    #[error(transparent)]
    Options(#[from] bone_llm::ModelRequestOptionsError),
    #[error("{options} options do not belong to a {model} model")]
    Protocol { model: Protocol, options: Protocol },
}

impl Agent {
    pub fn start(
        workspace: impl AsRef<Path>,
        coordinator: impl Into<ConfiguredModel>,
        worker: impl Into<ConfiguredModel>,
        limits: AgentLimits,
        tool_limits: ToolLimits,
    ) -> Result<Self, StartError> {
        let environment = ToolEnvironment::with_limits(workspace, tool_limits)?;
        let model = ModelAdapter::new(coordinator, worker);
        Ok(Self::with_ports(
            Arc::new(model),
            read_only_tools(&environment),
            limits,
        )?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Tools(#[from] ToolError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}
