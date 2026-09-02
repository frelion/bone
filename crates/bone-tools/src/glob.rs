use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use globset::{GlobBuilder, GlobSetBuilder};
use rig_core::tool::{PortableTool, ToolExecutionError};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ToolEnvironment, ToolError, ToolLimits,
    search_walk::{SearchWalk, SearchWalkEvent, is_vcs_name},
    workspace::{ResolvedPath, path_to_slashes},
};

const MAX_GLOB_PATTERN_BYTES: usize = 4 * 1024;

/// Arguments accepted by [`GlobTool`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GlobArgs {
    /// Glob matched against paths relative to `path`. Use `/` as the separator.
    pub pattern: String,
    /// Workspace-relative directory to search. Defaults to the workspace root.
    #[serde(default)]
    pub path: Option<String>,
    /// Disable default dotfile filtering while still excluding VCS metadata.
    /// An ignore whitelist may include a hidden path even when this is false.
    #[serde(default)]
    pub include_hidden: bool,
    /// Maximum paths to return, bounded by the environment hard limit.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Bounded, sorted paths returned by [`GlobTool`].
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GlobOutput {
    /// Sorted workspace-relative paths using `/` separators. When truncated,
    /// the selected subset can depend on filesystem enumeration order.
    pub paths: Vec<String>,
    /// True when a result, traversal, output, or warning limit was reached.
    pub truncated: bool,
    /// Number of filesystem entries inspected, including directories.
    pub scanned_entries: usize,
    /// Recoverable traversal and ignore-file errors.
    pub warnings: Vec<String>,
}

/// Find files by glob without invoking a shell or an external `glob` program.
#[derive(Clone, Debug)]
pub struct GlobTool {
    environment: ToolEnvironment,
}

impl GlobTool {
    pub fn new(environment: ToolEnvironment) -> Self {
        Self { environment }
    }
}

impl PortableTool for GlobTool {
    const NAME: &'static str = "glob";
    type Args = GlobArgs;
    type Output = GlobOutput;
    type Error = ToolError;

    fn description(&self) -> String {
        format!(
            "Find files inside the workspace using a root-relative glob. The search respects bounded workspace-local ignore files at or below the selected search root (rules above an explicitly selected root are not inherited), never enters VCS metadata, and returns at most {} sorted paths after scanning at most {} entries. If truncated, the selected subset can depend on filesystem enumeration order.",
            self.environment.limits.max_glob_results, self.environment.limits.max_walk_entries,
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "minLength": 1,
                    "description": format!("Glob relative to path, using / separators; * does not cross directories and ** does; at most {MAX_GLOB_PATTERN_BYTES} UTF-8 bytes")
                },
                "path": {
                    "type": "string",
                    "description": "Directory relative to the workspace, or an absolute directory inside it; defaults to ."
                },
                "include_hidden": {
                    "type": "boolean",
                    "default": false,
                    "description": "Disable default dotfile filtering; ignore whitelists can still include hidden paths, while VCS metadata is always excluded"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": self.environment.limits.max_glob_results,
                    "description": "Maximum paths to return"
                }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        error.into_execution_error()
    }

    async fn call(&self, arguments: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_pattern(&arguments.pattern)?;
        let result_limit = arguments
            .limit
            .unwrap_or(self.environment.limits.max_glob_results);
        if result_limit == 0 || result_limit > self.environment.limits.max_glob_results {
            return Err(ToolError::InvalidArgs(format!(
                "limit must be between 1 and {}",
                self.environment.limits.max_glob_results
            )));
        }
        let requested_path = arguments.path.as_deref().unwrap_or(".");
        let resolved = self
            .environment
            .workspace
            .resolve_existing(requested_path)
            .await?;
        let metadata = tokio::fs::metadata(&resolved.absolute)
            .await
            .map_err(|error| {
                ToolError::io_display("inspect glob root", &resolved.display, error)
            })?;
        if !metadata.is_dir() {
            return Err(ToolError::InvalidArgs(format!(
                "glob path is not a directory: {}",
                display_or_dot(&resolved.display)
            )));
        }
        reject_vcs_root(&resolved)?;

        let workspace_root = self.environment.workspace.root().to_path_buf();
        let limits = self.environment.limits.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut cancellation_guard = CancellationGuard::new(cancelled.clone());
        let joined = tokio::task::spawn_blocking(move || {
            run_glob(
                &workspace_root,
                resolved,
                &arguments.pattern,
                arguments.include_hidden,
                result_limit,
                &limits,
                &cancelled,
            )
        })
        .await;
        cancellation_guard.disarm();
        joined.map_err(|error| ToolError::Task(error.to_string()))?
    }
}

fn validate_pattern(pattern: &str) -> Result<(), ToolError> {
    if pattern.is_empty() {
        return Err(ToolError::InvalidArgs(
            "glob pattern must not be empty".to_owned(),
        ));
    }
    if pattern.len() > MAX_GLOB_PATTERN_BYTES {
        return Err(ToolError::InvalidArgs(format!(
            "glob pattern exceeds {MAX_GLOB_PATTERN_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

fn reject_vcs_root(resolved: &ResolvedPath) -> Result<(), ToolError> {
    if Path::new(&resolved.display)
        .components()
        .any(|component| is_vcs_name(component.as_os_str()))
    {
        return Err(ToolError::PermissionDenied {
            path: display_or_dot(&resolved.display).to_owned(),
        });
    }
    Ok(())
}

fn run_glob(
    workspace_root: &Path,
    resolved: ResolvedPath,
    pattern: &str,
    include_hidden: bool,
    result_limit: usize,
    limits: &ToolLimits,
    cancelled: &AtomicBool,
) -> Result<GlobOutput, ToolError> {
    let glob = GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .map_err(|error| ToolError::InvalidGlob(error.to_string()))?;
    let matcher = GlobSetBuilder::new()
        .add(glob)
        .build()
        .map_err(|error| ToolError::InvalidGlob(error.to_string()))?;

    let mut paths = Vec::new();
    let mut warnings = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated = false;

    let mut walk = SearchWalk::new(
        &resolved.absolute,
        workspace_root,
        include_hidden,
        limits,
        cancelled,
    );
    while let Some(event) = walk.next_event() {
        let path = match event {
            SearchWalkEvent::Warning(warning) => {
                if !push_bounded(
                    &mut warnings,
                    &mut output_bytes,
                    limits.max_output_bytes,
                    redact_workspace(warning, workspace_root),
                ) {
                    truncated = true;
                }
                continue;
            }
            SearchWalkEvent::File(path) => path,
        };
        let relative_to_search =
            path.strip_prefix(&resolved.absolute)
                .map_err(|_| ToolError::OutsideWorkspace {
                    path: resolved.display.clone(),
                })?;
        if !matcher.is_match(relative_to_search) {
            continue;
        }
        if paths.len() == result_limit {
            truncated = true;
            break;
        }

        let relative_to_workspace =
            path.strip_prefix(workspace_root)
                .map_err(|_| ToolError::OutsideWorkspace {
                    path: resolved.display.clone(),
                })?;
        let display = path_to_slashes(relative_to_workspace);
        if !push_bounded(
            &mut paths,
            &mut output_bytes,
            limits.max_output_bytes,
            display,
        ) {
            truncated = true;
            break;
        }
    }

    let scanned_entries = walk.scanned_entries();
    truncated |= walk.truncated();
    paths.sort();
    warnings.sort();

    Ok(GlobOutput {
        paths,
        truncated,
        scanned_entries,
        warnings,
    })
}

struct CancellationGuard {
    cancelled: Arc<AtomicBool>,
    armed: bool,
}

impl CancellationGuard {
    fn new(cancelled: Arc<AtomicBool>) -> Self {
        Self {
            cancelled,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

fn push_bounded(
    values: &mut Vec<String>,
    used_bytes: &mut usize,
    max_bytes: usize,
    value: String,
) -> bool {
    let cost = value.len() + usize::from(!values.is_empty());
    if used_bytes.saturating_add(cost) > max_bytes {
        return false;
    }
    *used_bytes += cost;
    values.push(value);
    true
}

fn redact_workspace(value: String, workspace_root: &Path) -> String {
    let root = workspace_root.display().to_string();
    if root.is_empty() || root == std::path::MAIN_SEPARATOR_STR {
        value
    } else {
        value.replace(&root, ".")
    }
}

fn display_or_dot(display: &str) -> &str {
    if display.is_empty() { "." } else { display }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rig_core::tool::PortableTool;

    use super::*;

    fn fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(temp.path().join("root.rs"), "root").unwrap();
        fs::write(temp.path().join("src/lib.rs"), "lib").unwrap();
        fs::write(temp.path().join("ignored.rs"), "ignored").unwrap();
        fs::write(temp.path().join(".hidden.rs"), "hidden").unwrap();
        fs::write(temp.path().join(".git/private.rs"), "metadata").unwrap();
        temp
    }

    #[tokio::test]
    async fn returns_sorted_workspace_paths_and_respects_ignores() {
        let temp = fixture();
        let output = ToolEnvironment::new(temp.path())
            .unwrap()
            .glob()
            .call(GlobArgs {
                pattern: "**/*.rs".to_owned(),
                path: None,
                include_hidden: false,
                limit: None,
            })
            .await
            .unwrap();

        assert_eq!(output.paths, ["root.rs", "src/lib.rs"]);
        assert!(!output.truncated);
        assert!(output.warnings.is_empty());
    }

    #[tokio::test]
    async fn includes_dotfiles_without_entering_git_metadata() {
        let temp = fixture();
        let output = ToolEnvironment::new(temp.path())
            .unwrap()
            .glob()
            .call(GlobArgs {
                pattern: "**/*.rs".to_owned(),
                path: None,
                include_hidden: true,
                limit: None,
            })
            .await
            .unwrap();

        assert_eq!(output.paths, [".hidden.rs", "root.rs", "src/lib.rs"]);
        assert!(!output.paths.iter().any(|path| path.starts_with(".git/")));
    }

    #[tokio::test]
    async fn enforces_result_and_output_limits() {
        let temp = fixture();
        let limits = ToolLimits {
            max_glob_results: 1,
            ..ToolLimits::default()
        };
        let output = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .glob()
            .call(GlobArgs {
                pattern: "**/*.rs".to_owned(),
                path: None,
                include_hidden: false,
                limit: None,
            })
            .await
            .unwrap();

        assert_eq!(output.paths.len(), 1);
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn walk_limit_is_hard_before_fetching_children() {
        let temp = fixture();
        let limits = ToolLimits {
            max_walk_entries: 1,
            ..ToolLimits::default()
        };
        let output = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .glob()
            .call(GlobArgs {
                pattern: "**/*.rs".to_owned(),
                path: None,
                include_hidden: true,
                limit: None,
            })
            .await
            .unwrap();

        assert!(output.paths.is_empty());
        assert_eq!(output.scanned_entries, 1);
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn rejects_invalid_patterns_and_vcs_roots() {
        let temp = fixture();
        let tool = ToolEnvironment::new(temp.path()).unwrap().glob();

        let invalid = tool
            .call(GlobArgs {
                pattern: "[".to_owned(),
                path: None,
                include_hidden: false,
                limit: None,
            })
            .await;
        assert!(matches!(invalid, Err(ToolError::InvalidGlob(_))));

        let vcs = tool
            .call(GlobArgs {
                pattern: "**/*".to_owned(),
                path: Some(".git".to_owned()),
                include_hidden: true,
                limit: None,
            })
            .await;
        assert!(matches!(vcs, Err(ToolError::PermissionDenied { .. })));
    }

    #[test]
    fn cancellation_guard_signals_only_while_armed() {
        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let _guard = CancellationGuard::new(cancelled.clone());
        }
        assert!(cancelled.load(Ordering::Relaxed));

        let cancelled = Arc::new(AtomicBool::new(false));
        let mut guard = CancellationGuard::new(cancelled.clone());
        guard.disarm();
        drop(guard);
        assert!(!cancelled.load(Ordering::Relaxed));
    }

    #[test]
    fn warning_paths_hide_the_workspace_prefix() {
        let root = Path::new("/private/workspace");
        assert_eq!(
            redact_workspace("walk error under /private/workspace/src".to_owned(), root),
            "walk error under ./src"
        );
    }
}
