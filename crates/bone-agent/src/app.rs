//! Endpoint-injected Agent runtime construction.
//!
//! Storage, provider authentication, and settings resolution are deliberately
//! outside this module. The caller owns those concerns and supplies a complete
//! [`ResolvedAgentRuntimeConfig`] at the exact boundary where a new runtime is
//! created.

use std::{path::Path, sync::Arc};

use bone_llm::Endpoint;
use bone_tools::{ToolEnvironment, ToolError};

use crate::{
    AgentHandle, KernelConfig, ModelAdapter, ResolvedAgentRuntimeConfig, Runtime, RuntimeConfig,
    RuntimeError, read_only_tools,
};

/// Failures while constructing an Agent runtime from already-resolved values.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Tools(#[from] ToolError),
    #[error(transparent)]
    Model(#[from] bone_llm::ConfigError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
}

/// A product-injected model endpoint that can start independent Agent runtimes.
///
/// Authentication and provider connection are complete before construction.
/// Each call to [`AgentHost::start`] consumes one immutable resolved runtime
/// configuration and creates independent models, tools, Kernel, and Runtime.
#[derive(Clone)]
pub struct AgentHost {
    endpoint: Endpoint,
}

impl AgentHost {
    /// Construct an Agent host from an already-connected provider endpoint.
    pub fn new(endpoint: Endpoint) -> Self {
        Self { endpoint }
    }

    /// Start one runtime pinned to the supplied resolved configuration.
    ///
    /// The method performs no disk configuration reads and no provider login.
    /// Later settings changes therefore cannot mutate this runtime; callers
    /// resolve a fresh configuration and create a new runtime when they want
    /// changed model, tool, or deadline values to take effect.
    pub fn start(
        &self,
        workspace: impl AsRef<Path>,
        config: ResolvedAgentRuntimeConfig,
    ) -> Result<AgentHandle, StartError> {
        let environment = ToolEnvironment::with_limits(workspace, config.tool_limits().clone())?;
        let deadlines = config.deadlines();
        let model = ModelAdapter::new(
            self.endpoint.model(&config.coordinator().model)?,
            self.endpoint.model(&config.solver().model)?,
        )
        .with_efforts(config.coordinator().effort, config.solver().effort);
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
