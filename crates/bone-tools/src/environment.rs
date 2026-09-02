use std::path::Path;

use crate::{
    ApplyPatchTool, BashTool, GlobTool, GrepTool, ReadTool, ToolError, ToolLimits,
    workspace::Workspace,
};

/// Immutable dependencies captured by every local built-in tool.
#[derive(Clone, Debug)]
pub struct ToolEnvironment {
    pub(crate) workspace: Workspace,
    pub(crate) limits: ToolLimits,
}

impl ToolEnvironment {
    /// Bind tools to a workspace using conservative default limits.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ToolError> {
        Self::with_limits(root, ToolLimits::default())
    }

    /// Bind tools to a workspace using caller-supplied hard limits.
    pub fn with_limits(root: impl AsRef<Path>, limits: ToolLimits) -> Result<Self, ToolError> {
        limits.validate()?;
        Ok(Self {
            workspace: Workspace::new(root)?,
            limits,
        })
    }

    /// Canonical workspace root.
    pub fn workspace_root(&self) -> &Path {
        self.workspace.root()
    }

    /// Effective hard limits.
    pub fn limits(&self) -> &ToolLimits {
        &self.limits
    }

    pub fn read(&self) -> ReadTool {
        ReadTool::new(self.clone())
    }

    pub fn glob(&self) -> GlobTool {
        GlobTool::new(self.clone())
    }

    pub fn grep(&self) -> GrepTool {
        GrepTool::new(self.clone())
    }

    pub fn apply_patch(&self) -> ApplyPatchTool {
        ApplyPatchTool::new(self.clone())
    }

    pub fn bash(&self) -> BashTool {
        BashTool::new(self.clone())
    }
}
