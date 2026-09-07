//! Model-injected Agent runtime construction.
//!
//! Storage, settings resolution, provider connection, credentials, and
//! protocol-specific request defaults are deliberately outside this module.
//! The caller supplies the two already-configured role models and one complete
//! [`ResolvedAgentRuntimeConfig`] at the boundary where a runtime is created.

use std::{path::Path, sync::Arc};

use bone_llm::{Model, ModelOptions, Protocol, Request};
use bone_tools::{ToolEnvironment, ToolError};

use crate::{
    AgentHandle, KernelConfig, ModelAdapter, ResolvedAgentRuntimeConfig, Runtime, RuntimeConfig,
    RuntimeError, read_only_tools,
};

/// One model selected by the product together with its protocol-specific
/// request defaults.
///
/// Construction verifies that options belong to the selected model's wire
/// protocol. This keeps a Responses-only setting such as reasoning effort from
/// becoming a no-op when a product selects an Anthropic or Chat Completions
/// model.
#[derive(Clone, Debug)]
pub struct ConfiguredModel {
    model: Model,
    options: Option<ModelOptions>,
}

impl ConfiguredModel {
    /// Pair a selected model with its optional protocol-specific defaults.
    pub fn new(model: Model, options: Option<ModelOptions>) -> Result<Self, ConfiguredModelError> {
        if let Some(options) = options.as_ref() {
            options.validate().map_err(ConfiguredModelError::Options)?;
            if options.protocol() != model.protocol() {
                return Err(ConfiguredModelError::UnsupportedOptions {
                    options_protocol: options.protocol(),
                    model_protocol: model.protocol(),
                });
            }
        }
        Ok(Self { model, options })
    }

    /// Use provider defaults for every protocol-specific request field.
    pub fn without_options(model: Model) -> Self {
        Self {
            model,
            options: None,
        }
    }

    /// The selected protocol-backed model.
    pub fn model(&self) -> &Model {
        &self.model
    }

    /// The validated protocol-specific request defaults, when configured.
    pub fn options(&self) -> Option<&ModelOptions> {
        self.options.as_ref()
    }

    pub(crate) fn apply_to(&self, request: Request) -> Result<Request, ConfiguredModelError> {
        match self.options.clone() {
            Some(options) => options
                .apply_to(request)
                .map_err(ConfiguredModelError::Options),
            None => Ok(request),
        }
    }
}

impl From<Model> for ConfiguredModel {
    fn from(model: Model) -> Self {
        Self::without_options(model)
    }
}

/// A local mismatch between a selected model and its request defaults.
#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub enum ConfiguredModelError {
    #[error(transparent)]
    Options(#[from] bone_llm::ModelRequestOptionsError),
    #[error("{options_protocol} model options cannot be used with {model_protocol} model")]
    UnsupportedOptions {
        options_protocol: Protocol,
        model_protocol: Protocol,
    },
}

/// The two role-specific models used by one Agent runtime.
///
/// Models are constructed by the product before they reach the Agent. This
/// permits the coordinator and solver to use independent endpoints, protocols,
/// credentials, and model identifiers without teaching the Agent about any of
/// those product concerns.
#[derive(Clone, Debug)]
pub struct AgentModels {
    coordinator: ConfiguredModel,
    solver: ConfiguredModel,
}

impl AgentModels {
    /// Pair the model used for interruption review with the model used for
    /// regular task work.
    pub fn new(coordinator: ConfiguredModel, solver: ConfiguredModel) -> Self {
        Self {
            coordinator,
            solver,
        }
    }

    /// The model that classifies input received while the solver is busy.
    pub fn coordinator(&self) -> &ConfiguredModel {
        &self.coordinator
    }

    /// The model that performs task work.
    pub fn solver(&self) -> &ConfiguredModel {
        &self.solver
    }
}

/// Failures while constructing an Agent runtime from already-resolved values.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Tools(#[from] ToolError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

/// A product-injected pair of role models that can start independent Agent
/// runtimes.
///
/// Authentication and provider connection are complete before construction.
/// Each call to [`AgentHost::start`] consumes one immutable resolved runtime
/// configuration and creates independent tools, Kernel, and Runtime.
#[derive(Clone, Debug)]
pub struct AgentHost {
    models: AgentModels,
}

impl AgentHost {
    /// Construct an Agent host from already-configured coordinator and solver
    /// models.
    pub fn new(models: AgentModels) -> Self {
        Self { models }
    }

    /// Start one runtime pinned to the supplied resolved configuration.
    ///
    /// The method performs no disk configuration reads, provider login, model
    /// selection, or protocol-option selection. Later settings changes cannot
    /// mutate this runtime; callers construct a fresh host and resolve a fresh
    /// configuration when they want changed values to take effect.
    pub fn start(
        &self,
        workspace: impl AsRef<Path>,
        config: ResolvedAgentRuntimeConfig,
    ) -> Result<AgentHandle, StartError> {
        let environment = ToolEnvironment::with_limits(workspace, config.tool_limits().clone())?;
        let deadlines = config.deadlines();
        let model = ModelAdapter::new(self.models.coordinator.clone(), self.models.solver.clone());
        Ok(Runtime::spawn(
            Arc::new(model),
            read_only_tools(&environment),
            KernelConfig {
                soft_deadline: deadlines.soft_deadline(),
                review_timeout: deadlines.review_timeout(),
                work_timeout: deadlines.work_timeout(),
            },
            RuntimeConfig {
                shutdown_grace_period: deadlines.shutdown_grace_period(),
            },
        )?)
    }
}
