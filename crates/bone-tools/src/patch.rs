use std::{
    collections::HashSet,
    io::{self, Write as _},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{Tool, ToolFailure};
use bone_llm::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tempfile::NamedTempFile;
use tokio::io::AsyncReadExt;

use crate::{ToolEnvironment, ToolError, workspace::ResolvedPath};

const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const ADD_FILE: &str = "*** Add File: ";
const DELETE_FILE: &str = "*** Delete File: ";
const UPDATE_FILE: &str = "*** Update File: ";
const MOVE_TO: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";

/// Arguments accepted by [`ApplyPatchTool`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApplyPatchArgs {
    /// A Codex-style `*** Begin Patch` document.
    pub patch: String,
}

/// One file mutation committed by [`ApplyPatchTool`].
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ApplyPatchChange {
    /// `add`, `update`, `delete`, or `move`.
    pub kind: String,
    /// Workspace-relative source or target path.
    pub path: String,
    /// Workspace-relative move destination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<String>,
    pub added_lines: usize,
    pub removed_lines: usize,
}

/// Structured summary of a successfully applied patch.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ApplyPatchOutput {
    pub summary: String,
    pub changes: Vec<ApplyPatchChange>,
}

/// Apply a Codex-style patch inside an immutable workspace boundary.
#[derive(Clone, Debug)]
pub struct ApplyPatchTool {
    environment: ToolEnvironment,
    #[cfg(test)]
    commit_hook: Option<CommitTestHook>,
}

impl ApplyPatchTool {
    pub(crate) fn new(environment: ToolEnvironment) -> Self {
        Self {
            environment,
            #[cfg(test)]
            commit_hook: None,
        }
    }
}

impl Tool for ApplyPatchTool {
    type Args = ApplyPatchArgs;
    type Output = ApplyPatchOutput;
    type Error = ToolError;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "apply_patch",
            format!(
                "Apply file changes inside the workspace. Grammar: wrap operations in `{BEGIN_PATCH}` and `{END_PATCH}`; `*** Add File: PATH` is followed by `+` lines; `*** Delete File: PATH` deletes a file; `*** Update File: PATH` uses `@@` hunks with context lines prefixed by a space, removals by `-`, and additions by `+`; an optional `*** Move to: PATH` must immediately follow its Update header. Paths must be workspace-relative. Limits: {} UTF-8 patch bytes, {} file operations, {} bytes per existing file, and {} combined existing-file bytes.",
                self.environment.limits.max_patch_bytes,
                self.environment.limits.max_patch_files,
                self.environment.limits.max_patch_file_bytes,
                self.environment.limits.max_patch_total_bytes,
            ),
            json!({
                "type": "object",
                "properties": {
                    "patch": {
                        "type": "string",
                        "description": format!(
                            "Codex-style patch document beginning with `*** Begin Patch` and ending with `*** End Patch`; limited to {} UTF-8 bytes",
                            self.environment.limits.max_patch_bytes
                        )
                    }
                },
                "required": ["patch"],
                "additionalProperties": false
            }),
        )
    }

    fn map_error(&self, error: Self::Error) -> ToolFailure {
        error.into_tool_failure()
    }

    async fn call(&self, arguments: Self::Args) -> Result<Self::Output, Self::Error> {
        if arguments.patch.len() > self.environment.limits.max_patch_bytes {
            return Err(ToolError::InvalidArgs(format!(
                "patch is {} bytes; maximum is {} bytes",
                arguments.patch.len(),
                self.environment.limits.max_patch_bytes
            )));
        }

        let operations = parse_patch(&arguments.patch)?;
        if operations.len() > self.environment.limits.max_patch_files {
            return Err(ToolError::InvalidArgs(format!(
                "patch contains {} file operations; maximum is {}",
                operations.len(),
                self.environment.limits.max_patch_files
            )));
        }
        let plan = build_plan(&self.environment, operations).await?;
        let environment = self.environment.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut cancellation_guard = PatchTransactionCancellationGuard::new(cancelled.clone());
        let control = CommitControl {
            cancelled,
            #[cfg(test)]
            hook: self.commit_hook.clone(),
        };

        // Dropping a JoinHandle detaches rather than aborts its task. Consequently,
        // dropping this tool call only sets the cancellation flag: the transaction
        // keeps ownership of its stages and rollback journal until it either commits
        // or restores all mutations it has made.
        let transaction =
            tokio::spawn(async move { commit_plan(&environment, plan, control).await });
        let joined = transaction.await;
        cancellation_guard.disarm();
        match joined {
            Ok(result) => result,
            Err(error) => Err(ToolError::Task(format!(
                "apply_patch transaction task failed: {error}"
            ))),
        }
    }
}

struct PatchTransactionCancellationGuard {
    cancelled: Arc<AtomicBool>,
    armed: bool,
}

impl PatchTransactionCancellationGuard {
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

impl Drop for PatchTransactionCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone)]
struct CommitControl {
    cancelled: Arc<AtomicBool>,
    #[cfg(test)]
    hook: Option<CommitTestHook>,
}

impl CommitControl {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

#[cfg(test)]
#[derive(Clone, Debug)]
struct CommitTestHook {
    pause_after_actions: usize,
    reached: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PatchOperation {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<UpdateChunk>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct UpdateChunk {
    context: Option<String>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    context_indices: Vec<(usize, usize)>,
    end_of_file: bool,
}

impl UpdateChunk {
    fn push_context(&mut self, line: String) {
        self.context_indices
            .push((self.old_lines.len(), self.new_lines.len()));
        self.old_lines.push(line.clone());
        self.new_lines.push(line);
    }

    fn has_lines(&self) -> bool {
        !self.old_lines.is_empty() || !self.new_lines.is_empty()
    }

    fn added_lines(&self) -> usize {
        self.new_lines
            .len()
            .saturating_sub(self.context_indices.len())
    }

    fn removed_lines(&self) -> usize {
        self.old_lines
            .len()
            .saturating_sub(self.context_indices.len())
    }
}

fn parse_patch(input: &str) -> Result<Vec<PatchOperation>, ToolError> {
    let input = input.trim_matches(['\r', '\n']);
    if input.is_empty() {
        return Err(patch_error("patch must not be empty"));
    }

    let lines = input
        .lines()
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>();
    if lines.first().map(|line| line.trim()) != Some(BEGIN_PATCH) {
        return Err(patch_error(&format!(
            "the first line must be `{BEGIN_PATCH}`"
        )));
    }
    if lines.last().map(|line| line.trim()) != Some(END_PATCH) {
        return Err(patch_error(&format!("the last line must be `{END_PATCH}`")));
    }

    let mut operations = Vec::new();
    let mut index = 1usize;
    let end = lines.len() - 1;

    while index < end {
        let line = lines[index];
        let line_number = index + 1;

        if let Some(raw_path) = line.strip_prefix(ADD_FILE) {
            let path = parse_path(raw_path, line_number)?;
            index += 1;
            let mut content = String::new();
            let mut added_any = false;
            while index < end && !is_operation_header(lines[index]) {
                let source = lines[index];
                let Some(added) = source.strip_prefix('+') else {
                    return Err(patch_line_error(
                        index + 1,
                        "every Add File content line must start with `+`",
                    ));
                };
                content.push_str(added);
                content.push('\n');
                added_any = true;
                index += 1;
            }
            if !added_any {
                return Err(patch_line_error(
                    line_number,
                    "Add File must contain at least one `+` line",
                ));
            }
            operations.push(PatchOperation::Add { path, content });
            continue;
        }

        if let Some(raw_path) = line.strip_prefix(DELETE_FILE) {
            operations.push(PatchOperation::Delete {
                path: parse_path(raw_path, line_number)?,
            });
            index += 1;
            continue;
        }

        if let Some(raw_path) = line.strip_prefix(UPDATE_FILE) {
            let path = parse_path(raw_path, line_number)?;
            index += 1;

            let move_to = if index < end {
                if let Some(raw_move) = lines[index].strip_prefix(MOVE_TO) {
                    let move_path = parse_path(raw_move, index + 1)?;
                    index += 1;
                    Some(move_path)
                } else {
                    None
                }
            } else {
                None
            };

            let mut chunks = Vec::<UpdateChunk>::new();
            while index < end && !is_operation_header(lines[index]) {
                let source = lines[index];
                let source_line = index + 1;

                if source == "@@" || source.starts_with("@@ ") {
                    if chunks.last().is_some_and(|chunk| !chunk.has_lines()) {
                        return Err(patch_line_error(
                            source_line,
                            "the previous update chunk contains no lines",
                        ));
                    }
                    chunks.push(UpdateChunk {
                        context: source.strip_prefix("@@ ").map(ToOwned::to_owned),
                        ..UpdateChunk::default()
                    });
                    index += 1;
                    continue;
                }

                if source == END_OF_FILE {
                    let Some(chunk) = chunks.last_mut() else {
                        return Err(patch_line_error(
                            source_line,
                            "End of File must follow an update chunk",
                        ));
                    };
                    if !chunk.has_lines() {
                        return Err(patch_line_error(
                            source_line,
                            "End of File must follow update lines",
                        ));
                    }
                    chunk.end_of_file = true;
                    index += 1;
                    continue;
                }

                if source.starts_with(MOVE_TO) {
                    return Err(patch_line_error(
                        source_line,
                        "Move to must appear immediately after Update File",
                    ));
                }

                if chunks.is_empty() {
                    chunks.push(UpdateChunk::default());
                }
                let Some(chunk) = chunks.last_mut() else {
                    return Err(patch_line_error(
                        source_line,
                        "failed to initialize update chunk",
                    ));
                };
                if chunk.end_of_file {
                    return Err(patch_line_error(
                        source_line,
                        "End of File must be the last line in its update chunk",
                    ));
                }

                if source.is_empty() {
                    chunk.push_context(String::new());
                } else if let Some(context) = source.strip_prefix(' ') {
                    chunk.push_context(context.to_owned());
                } else if let Some(added) = source.strip_prefix('+') {
                    chunk.new_lines.push(added.to_owned());
                } else if let Some(removed) = source.strip_prefix('-') {
                    chunk.old_lines.push(removed.to_owned());
                } else {
                    return Err(patch_line_error(
                        source_line,
                        "update lines must start with ` `, `+`, or `-`",
                    ));
                }
                index += 1;
            }

            if chunks.is_empty() || chunks.iter().any(|chunk| !chunk.has_lines()) {
                return Err(patch_line_error(
                    line_number,
                    "Update File must contain at least one non-empty chunk",
                ));
            }
            if move_to.is_none()
                && chunks
                    .iter()
                    .all(|chunk| chunk.added_lines() == 0 && chunk.removed_lines() == 0)
            {
                return Err(patch_line_error(
                    line_number,
                    "Update File does not change any lines",
                ));
            }
            operations.push(PatchOperation::Update {
                path,
                move_to,
                chunks,
            });
            continue;
        }

        return Err(patch_line_error(
            line_number,
            "expected Add File, Update File, or Delete File header",
        ));
    }

    if operations.is_empty() {
        return Err(patch_error("patch contains no file operations"));
    }
    Ok(operations)
}

fn parse_path(raw: &str, line: usize) -> Result<String, ToolError> {
    let path = raw.trim();
    if path.is_empty() {
        return Err(patch_line_error(line, "file path must not be empty"));
    }
    Ok(path.to_owned())
}

fn is_operation_header(line: &str) -> bool {
    line.starts_with(ADD_FILE) || line.starts_with(DELETE_FILE) || line.starts_with(UPDATE_FILE)
}

fn patch_error(message: &str) -> ToolError {
    ToolError::Patch(message.to_owned())
}

fn patch_line_error(line: usize, message: &str) -> ToolError {
    patch_error(&format!("line {line}: {message}"))
}

#[derive(Debug)]
enum PlannedAction {
    Add {
        target: ResolvedPath,
        content: String,
        added_lines: usize,
    },
    Delete {
        target: ResolvedPath,
        expected: String,
        permissions: std::fs::Permissions,
        removed_lines: usize,
    },
    Update {
        source: ResolvedPath,
        destination: Option<ResolvedPath>,
        expected: String,
        content: String,
        permissions: std::fs::Permissions,
        added_lines: usize,
        removed_lines: usize,
    },
}

impl PlannedAction {
    fn paths(&self) -> Vec<&ResolvedPath> {
        match self {
            Self::Add { target, .. } | Self::Delete { target, .. } => vec![target],
            Self::Update {
                source,
                destination,
                ..
            } => destination
                .as_ref()
                .map_or_else(|| vec![source], |destination| vec![source, destination]),
        }
    }
}

async fn build_plan(
    environment: &ToolEnvironment,
    operations: Vec<PatchOperation>,
) -> Result<Vec<PlannedAction>, ToolError> {
    let mut plan = Vec::with_capacity(operations.len());
    let mut claimed = HashSet::<PathBuf>::new();
    let mut snapshot_bytes = 0u64;

    for operation in operations {
        match operation {
            PatchOperation::Add { path, content } => {
                let target = environment.workspace.resolve_patch_path(&path).await?;
                claim_path(&mut claimed, &target)?;
                require_missing(&target, "add file").await?;
                require_parent_directory(&target).await?;
                let added_lines = content.lines().count();
                plan.push(PlannedAction::Add {
                    target,
                    content,
                    added_lines,
                });
            }
            PatchOperation::Delete { path } => {
                let target = environment.workspace.resolve_patch_path(&path).await?;
                claim_path(&mut claimed, &target)?;
                let snapshot = read_planned_snapshot(
                    environment,
                    &target,
                    "read file to delete",
                    &mut snapshot_bytes,
                )
                .await?;
                let removed_lines = logical_line_count(&snapshot.content);
                plan.push(PlannedAction::Delete {
                    target,
                    expected: snapshot.content,
                    permissions: snapshot.permissions,
                    removed_lines,
                });
            }
            PatchOperation::Update {
                path,
                move_to,
                chunks,
            } => {
                let source = environment.workspace.resolve_patch_path(&path).await?;
                claim_path(&mut claimed, &source)?;
                let snapshot = read_planned_snapshot(
                    environment,
                    &source,
                    "read file to update",
                    &mut snapshot_bytes,
                )
                .await?;
                let content = apply_update_chunks(&snapshot.content, &source.display, &chunks)?;
                let added_lines = chunks.iter().map(UpdateChunk::added_lines).sum();
                let removed_lines = chunks.iter().map(UpdateChunk::removed_lines).sum();

                let destination = if let Some(move_path) = move_to {
                    let destination = environment.workspace.resolve_patch_path(&move_path).await?;
                    claim_path(&mut claimed, &destination)?;
                    require_missing(&destination, "move file").await?;
                    require_parent_directory(&destination).await?;
                    Some(destination)
                } else {
                    None
                };

                plan.push(PlannedAction::Update {
                    source,
                    destination,
                    expected: snapshot.content,
                    content,
                    permissions: snapshot.permissions,
                    added_lines,
                    removed_lines,
                });
            }
        }
    }

    Ok(plan)
}

fn claim_path(claimed: &mut HashSet<PathBuf>, path: &ResolvedPath) -> Result<(), ToolError> {
    if claimed.iter().any(|existing| {
        existing == &path.absolute
            || existing.starts_with(&path.absolute)
            || path.absolute.starts_with(existing)
    }) {
        return Err(patch_error(&format!(
            "multiple operations target conflicting path `{}`",
            path.display
        )));
    }
    claimed.insert(path.absolute.clone());
    Ok(())
}

async fn require_missing(path: &ResolvedPath, operation: &str) -> Result<(), ToolError> {
    match tokio::fs::symlink_metadata(&path.absolute).await {
        Ok(_) => Err(patch_error(&format!(
            "cannot {operation}: `{}` already exists",
            path.display
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ToolError::io_display(
            operation_static(operation),
            &path.display,
            error,
        )),
    }
}

fn operation_static(operation: &str) -> &'static str {
    match operation {
        "add file" => "add file",
        "move file" => "move file",
        _ => "inspect patch target",
    }
}

async fn require_parent_directory(path: &ResolvedPath) -> Result<(), ToolError> {
    let Some(mut parent) = path.absolute.parent() else {
        return Err(patch_error(&format!(
            "path `{}` has no parent directory",
            path.display
        )));
    };
    loop {
        match tokio::fs::symlink_metadata(parent).await {
            Ok(metadata) if metadata.is_dir() => return Ok(()),
            Ok(_) => {
                return Err(patch_error(&format!(
                    "parent of `{}` is not a directory",
                    path.display
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                parent = parent.parent().ok_or_else(|| {
                    patch_error(&format!("could not resolve parent of `{}`", path.display))
                })?;
            }
            Err(error) => {
                return Err(ToolError::io_display(
                    "inspect parent directory",
                    &path.display,
                    error,
                ));
            }
        }
    }
}

struct FileSnapshot {
    content: String,
    permissions: std::fs::Permissions,
}

#[derive(Clone, Copy)]
struct SnapshotBudget {
    remaining: u64,
    total_limit: u64,
}

async fn read_planned_snapshot(
    environment: &ToolEnvironment,
    path: &ResolvedPath,
    operation: &'static str,
    used_bytes: &mut u64,
) -> Result<FileSnapshot, ToolError> {
    let total_limit = environment.limits.max_patch_total_bytes;
    let budget = SnapshotBudget {
        remaining: total_limit.saturating_sub(*used_bytes),
        total_limit,
    };
    let snapshot = read_regular_text(
        path,
        operation,
        environment.limits.max_patch_file_bytes,
        Some(budget),
    )
    .await?;
    let file_bytes = u64::try_from(snapshot.content.len())
        .map_err(|_| patch_error("patch snapshot size cannot be represented"))?;
    *used_bytes = used_bytes.checked_add(file_bytes).ok_or_else(|| {
        patch_error(&format!(
            "existing-file snapshots exceed the maximum combined size of {total_limit} bytes"
        ))
    })?;
    Ok(snapshot)
}

async fn read_regular_text(
    path: &ResolvedPath,
    operation: &'static str,
    max_bytes: u64,
    snapshot_budget: Option<SnapshotBudget>,
) -> Result<FileSnapshot, ToolError> {
    let file = tokio::fs::File::open(&path.absolute)
        .await
        .map_err(|error| ToolError::io_display(operation, &path.display, error))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|error| ToolError::io_display(operation, &path.display, error))?;
    if !metadata.is_file() {
        return Err(patch_error(&format!(
            "`{}` is not a regular file",
            path.display
        )));
    }
    if metadata.len() > max_bytes {
        return Err(patch_error(&format!(
            "`{}` is {} bytes; maximum patchable file size is {max_bytes} bytes",
            path.display,
            metadata.len()
        )));
    }
    if let Some(budget) = snapshot_budget
        && metadata.len() > budget.remaining
    {
        return Err(patch_error(&format!(
            "`{}` would exceed the maximum combined existing-file snapshot size of {} bytes",
            path.display, budget.total_limit
        )));
    }

    // The handle metadata check is only an optimization and a clearer error.
    // `take(max + 1)` is the hard bound if the file grows after metadata was
    // observed, so a concurrent append cannot make patch planning read forever.
    let effective_limit = snapshot_budget
        .map(|budget| max_bytes.min(budget.remaining))
        .unwrap_or(max_bytes);
    let mut bytes = Vec::new();
    file.take(effective_limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| ToolError::io_display(operation, &path.display, error))?;
    let actual_bytes = u64::try_from(bytes.len())
        .map_err(|_| patch_error("patch snapshot size cannot be represented"))?;
    if actual_bytes > max_bytes {
        return Err(patch_error(&format!(
            "`{}` grew beyond the maximum patchable file size of {max_bytes} bytes while being read",
            path.display
        )));
    }
    if let Some(budget) = snapshot_budget
        && actual_bytes > budget.remaining
    {
        return Err(patch_error(&format!(
            "`{}` would exceed the maximum combined existing-file snapshot size of {} bytes",
            path.display, budget.total_limit
        )));
    }
    let content = String::from_utf8(bytes).map_err(|error| {
        ToolError::io_display(
            operation,
            &path.display,
            io::Error::new(io::ErrorKind::InvalidData, error),
        )
    })?;
    Ok(FileSnapshot {
        content,
        permissions: metadata.permissions(),
    })
}

#[derive(Clone, Copy, Debug)]
enum MatchMode {
    Exact,
    TrimEnd,
    Trim,
}

fn find_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    end_of_file: bool,
) -> Result<Option<usize>, MatchMode> {
    if pattern.is_empty() {
        return Ok(Some(if end_of_file { lines.len() } else { start }));
    }
    if pattern.len() > lines.len() || start > lines.len().saturating_sub(pattern.len()) {
        return Ok(None);
    }

    let last = lines.len() - pattern.len();
    let range_start = if end_of_file { last } else { start };
    let range_end = last;
    for mode in [MatchMode::Exact, MatchMode::TrimEnd, MatchMode::Trim] {
        let mut found = None;
        for index in range_start..=range_end {
            if sequence_matches(&lines[index..index + pattern.len()], pattern, mode) {
                if found.is_some() {
                    return Err(mode);
                }
                found = Some(index);
            }
        }
        if found.is_some() {
            return Ok(found);
        }
    }
    Ok(None)
}

impl MatchMode {
    fn label(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::TrimEnd => "rstrip",
            Self::Trim => "trim",
        }
    }
}

fn sequence_matches(lines: &[String], pattern: &[String], mode: MatchMode) -> bool {
    lines
        .iter()
        .zip(pattern)
        .all(|(actual, expected)| match mode {
            MatchMode::Exact => actual == expected,
            MatchMode::TrimEnd => actual.trim_end() == expected.trim_end(),
            MatchMode::Trim => actual.trim() == expected.trim(),
        })
}

fn apply_update_chunks(
    original: &str,
    display_path: &str,
    chunks: &[UpdateChunk],
) -> Result<String, ToolError> {
    let mut text = TextFile::parse(original);
    let mut cursor = 0usize;

    for chunk in chunks {
        if let Some(context) = &chunk.context {
            let context_index =
                find_sequence(&text.lines, std::slice::from_ref(context), cursor, false)
                    .map_err(|mode| {
                        patch_error(&format!(
                            "context `{context}` has multiple {} matches in `{display_path}`",
                            mode.label()
                        ))
                    })?
                    .ok_or_else(|| {
                        patch_error(&format!(
                            "failed to find context `{context}` in `{display_path}`"
                        ))
                    })?;
            cursor = context_index + 1;
        }

        let start = if chunk.old_lines.is_empty() {
            if chunk.end_of_file || chunk.context.is_none() {
                text.lines.len()
            } else {
                cursor
            }
        } else {
            find_sequence(&text.lines, &chunk.old_lines, cursor, chunk.end_of_file)
                .map_err(|mode| {
                    patch_error(&format!(
                        "expected lines have multiple {} matches in `{display_path}`:\n{}",
                        mode.label(),
                        chunk.old_lines.join("\n")
                    ))
                })?
                .ok_or_else(|| {
                    patch_error(&format!(
                        "failed to find expected lines in `{display_path}`:\n{}",
                        chunk.old_lines.join("\n")
                    ))
                })?
        };

        let mut replacement = chunk.new_lines.clone();
        for &(old_index, new_index) in &chunk.context_indices {
            if let (Some(original_context), Some(replacement_context)) = (
                text.lines.get(start + old_index),
                replacement.get_mut(new_index),
            ) {
                *replacement_context = original_context.clone();
            }
        }

        let end = start + chunk.old_lines.len();
        text.lines.splice(start..end, replacement.iter().cloned());
        cursor = start + replacement.len();
    }

    Ok(text.render())
}

struct TextFile {
    lines: Vec<String>,
    newline: &'static str,
    trailing_newline: bool,
}

impl TextFile {
    fn parse(content: &str) -> Self {
        let newline = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        let trailing_newline = content.ends_with('\n') || content.ends_with('\r');
        let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
        let mut lines = normalized
            .split('\n')
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        if trailing_newline && lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if content.is_empty() {
            lines.clear();
        }
        Self {
            lines,
            newline,
            trailing_newline,
        }
    }

    fn render(self) -> String {
        let mut content = self.lines.join(self.newline);
        if self.trailing_newline && !self.lines.is_empty() {
            content.push_str(self.newline);
        }
        content
    }
}

async fn commit_plan(
    environment: &ToolEnvironment,
    plan: Vec<PlannedAction>,
    control: CommitControl,
) -> Result<ApplyPatchOutput, ToolError> {
    // Validate before staging so ordinary validation failures have no filesystem
    // effects. Every replacement and every rollback copy is then prepared in the
    // destination directory before the first committed mutation.
    validate_plan(environment, &plan).await?;
    if control.is_cancelled() {
        return Err(transaction_cancelled_error());
    }
    let staged = stage_plan(environment, &plan).await?;
    if control.is_cancelled() {
        return Err(transaction_cancelled_error());
    }

    // Staging can take time and can create missing parent directories. Resolve and
    // validate once more immediately before commit so those changes cannot turn a
    // stale plan into a partially applied patch.
    validate_plan(environment, &plan).await?;
    if control.is_cancelled() {
        return Err(transaction_cancelled_error());
    }
    commit_staged(environment, plan, staged, &control).await
}

enum StagedFiles {
    Add {
        replacement: NamedTempFile,
    },
    Delete {
        backup: NamedTempFile,
    },
    Update {
        replacement: NamedTempFile,
        backup: NamedTempFile,
    },
}

enum RollbackEntry {
    Added {
        target: ResolvedPath,
    },
    Deleted {
        target: ResolvedPath,
        backup: NamedTempFile,
    },
    Updated {
        target: ResolvedPath,
        backup: NamedTempFile,
    },
    Moved {
        source: ResolvedPath,
        destination: ResolvedPath,
        backup: NamedTempFile,
        source_removed: bool,
    },
}

async fn validate_plan(
    environment: &ToolEnvironment,
    plan: &[PlannedAction],
) -> Result<(), ToolError> {
    for action in plan {
        validate_action(environment, action).await?;
    }
    Ok(())
}

async fn validate_action(
    environment: &ToolEnvironment,
    action: &PlannedAction,
) -> Result<(), ToolError> {
    for path in action.paths() {
        require_same_resolution(environment, path).await?;
    }
    revalidate_action(environment, action).await
}

async fn require_same_resolution(
    environment: &ToolEnvironment,
    path: &ResolvedPath,
) -> Result<(), ToolError> {
    let resolved = environment
        .workspace
        .resolve_patch_path(&path.display)
        .await?;
    if resolved.absolute == path.absolute {
        Ok(())
    } else {
        Err(ToolError::OutsideWorkspace {
            path: path.display.clone(),
        })
    }
}

async fn stage_plan(
    environment: &ToolEnvironment,
    plan: &[PlannedAction],
) -> Result<Vec<StagedFiles>, ToolError> {
    let mut staged = Vec::with_capacity(plan.len());
    for action in plan {
        let files = match action {
            PlannedAction::Add {
                target, content, ..
            } => {
                create_parent_directories(environment, target).await?;
                StagedFiles::Add {
                    replacement: stage_content(
                        target,
                        content.as_bytes(),
                        None,
                        StagePurpose::Replacement,
                    )?,
                }
            }
            PlannedAction::Delete {
                target,
                expected,
                permissions,
                ..
            } => StagedFiles::Delete {
                backup: stage_content(
                    target,
                    expected.as_bytes(),
                    Some(permissions),
                    StagePurpose::Recovery,
                )?,
            },
            PlannedAction::Update {
                source,
                destination,
                expected,
                content,
                permissions,
                ..
            } => {
                let replacement_target = if let Some(destination) = destination {
                    create_parent_directories(environment, destination).await?;
                    destination
                } else {
                    source
                };
                let replacement = stage_content(
                    replacement_target,
                    content.as_bytes(),
                    Some(permissions),
                    StagePurpose::Replacement,
                )?;
                let backup = stage_content(
                    source,
                    expected.as_bytes(),
                    Some(permissions),
                    StagePurpose::Recovery,
                )?;
                StagedFiles::Update {
                    replacement,
                    backup,
                }
            }
        };
        staged.push(files);
    }
    Ok(staged)
}

fn stage_content(
    target: &ResolvedPath,
    content: &[u8],
    permissions: Option<&std::fs::Permissions>,
    purpose: StagePurpose,
) -> Result<NamedTempFile, ToolError> {
    let parent = target.absolute.parent().ok_or_else(|| {
        patch_error(&format!(
            "path `{}` has no parent directory",
            target.display
        ))
    })?;
    let mut staged = create_stage_file(parent, permissions.is_none(), purpose.prefix())
        .map_err(|error| ToolError::io_display("stage patch file", &target.display, error))?;
    staged.write_all(content).map_err(|error| {
        ToolError::io_display("write staged patch file", &target.display, error)
    })?;
    staged.flush().map_err(|error| {
        ToolError::io_display("flush staged patch file", &target.display, error)
    })?;
    if let Some(permissions) = permissions {
        staged
            .as_file()
            .set_permissions(permissions.clone())
            .map_err(|error| {
                ToolError::io_display("set staged file permissions", &target.display, error)
            })?;
    }
    Ok(staged)
}

#[derive(Clone, Copy)]
enum StagePurpose {
    Replacement,
    Recovery,
}

impl StagePurpose {
    fn prefix(self) -> &'static str {
        match self {
            Self::Replacement => ".bone-patch-stage-",
            Self::Recovery => ".bone-patch-recovery-",
        }
    }
}

#[cfg(unix)]
fn create_stage_file(
    parent: &std::path::Path,
    use_new_file_permissions: bool,
    prefix: &str,
) -> io::Result<NamedTempFile> {
    use std::os::unix::fs::PermissionsExt;

    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix);
    if use_new_file_permissions {
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    builder.tempfile_in(parent)
}

#[cfg(not(unix))]
fn create_stage_file(
    parent: &std::path::Path,
    _use_new_file_permissions: bool,
    prefix: &str,
) -> io::Result<NamedTempFile> {
    tempfile::Builder::new().prefix(prefix).tempfile_in(parent)
}

async fn commit_staged(
    environment: &ToolEnvironment,
    plan: Vec<PlannedAction>,
    staged: Vec<StagedFiles>,
    control: &CommitControl,
) -> Result<ApplyPatchOutput, ToolError> {
    let mut changes = Vec::with_capacity(plan.len());
    let mut journal = Vec::with_capacity(plan.len());

    if control.is_cancelled() {
        return Err(transaction_cancelled_error());
    }

    for (action, staged) in plan.into_iter().zip(staged) {
        match (action, staged) {
            (
                PlannedAction::Add {
                    target,
                    added_lines,
                    ..
                },
                StagedFiles::Add { replacement },
            ) => {
                let change = ApplyPatchChange {
                    kind: "add".to_owned(),
                    path: target.display.clone(),
                    moved_to: None,
                    added_lines,
                    removed_lines: 0,
                };
                if let Err(error) = persist_noclobber(replacement, &target, "add file") {
                    return Err(error_after_rollback(environment, error, journal).await);
                }
                journal.push(RollbackEntry::Added { target });
                changes.push(change);
            }
            (
                PlannedAction::Delete {
                    target,
                    removed_lines,
                    ..
                },
                StagedFiles::Delete { backup },
            ) => {
                let change = ApplyPatchChange {
                    kind: "delete".to_owned(),
                    path: target.display.clone(),
                    moved_to: None,
                    added_lines: 0,
                    removed_lines,
                };
                if let Err(error) = tokio::fs::remove_file(&target.absolute).await {
                    let error = ToolError::io_display("delete file", &target.display, error);
                    return Err(error_after_rollback(environment, error, journal).await);
                }
                journal.push(RollbackEntry::Deleted { target, backup });
                changes.push(change);
            }
            (
                PlannedAction::Update {
                    source,
                    destination: None,
                    added_lines,
                    removed_lines,
                    ..
                },
                StagedFiles::Update {
                    replacement,
                    backup,
                },
            ) => {
                let change = ApplyPatchChange {
                    kind: "update".to_owned(),
                    path: source.display.clone(),
                    moved_to: None,
                    added_lines,
                    removed_lines,
                };
                if let Err(error) = persist_replace(replacement, &source, "update file") {
                    return Err(error_after_rollback(environment, error, journal).await);
                }
                journal.push(RollbackEntry::Updated {
                    target: source,
                    backup,
                });
                changes.push(change);
            }
            (
                PlannedAction::Update {
                    source,
                    destination: Some(destination),
                    added_lines,
                    removed_lines,
                    ..
                },
                StagedFiles::Update {
                    replacement,
                    backup,
                },
            ) => {
                let change = ApplyPatchChange {
                    kind: "move".to_owned(),
                    path: source.display.clone(),
                    moved_to: Some(destination.display.clone()),
                    added_lines,
                    removed_lines,
                };
                if let Err(error) =
                    persist_noclobber(replacement, &destination, "create moved file")
                {
                    return Err(error_after_rollback(environment, error, journal).await);
                }

                let source_absolute = source.absolute.clone();
                let source_display = source.display.clone();
                journal.push(RollbackEntry::Moved {
                    source,
                    destination,
                    backup,
                    source_removed: false,
                });
                if let Err(error) = tokio::fs::remove_file(&source_absolute).await {
                    let error =
                        ToolError::io_display("remove moved source", &source_display, error);
                    return Err(error_after_rollback(environment, error, journal).await);
                }
                if let Some(RollbackEntry::Moved { source_removed, .. }) = journal.last_mut() {
                    *source_removed = true;
                }
                changes.push(change);
            }
            _ => {
                let error = patch_error("internal staged patch plan mismatch");
                return Err(error_after_rollback(environment, error, journal).await);
            }
        }

        #[cfg(test)]
        pause_at_commit_hook(control, changes.len()).await;
        if control.is_cancelled() {
            return Err(
                error_after_rollback(environment, transaction_cancelled_error(), journal).await,
            );
        }
    }

    let mut adds = 0usize;
    let mut updates = 0usize;
    let mut deletes = 0usize;
    let mut moves = 0usize;
    for change in &changes {
        match change.kind.as_str() {
            "add" => adds += 1,
            "update" => updates += 1,
            "delete" => deletes += 1,
            "move" => moves += 1,
            _ => {}
        }
    }
    let summary = format!(
        "Applied {} file change(s): {adds} added, {updates} updated, {deletes} deleted, {moves} moved.",
        changes.len()
    );
    Ok(ApplyPatchOutput { summary, changes })
}

fn transaction_cancelled_error() -> ToolError {
    ToolError::Task("apply_patch transaction cancelled".to_owned())
}

#[cfg(test)]
async fn pause_at_commit_hook(control: &CommitControl, committed_actions: usize) {
    let Some(hook) = &control.hook else {
        return;
    };
    if committed_actions != hook.pause_after_actions {
        return;
    }

    hook.reached.store(true, Ordering::Release);
    while !hook.release.load(Ordering::Acquire) {
        tokio::task::yield_now().await;
    }
}

fn persist_replace(
    staged: NamedTempFile,
    target: &ResolvedPath,
    operation: &'static str,
) -> Result<(), ToolError> {
    staged
        .persist(&target.absolute)
        .map(drop)
        .map_err(|error| ToolError::io_display(operation, &target.display, error.error))
}

fn persist_noclobber(
    staged: NamedTempFile,
    target: &ResolvedPath,
    operation: &'static str,
) -> Result<(), ToolError> {
    // On tempfile's hard-link fallback, failure to unlink the old staging name
    // may leave that hidden link behind, but the API still returns success. The
    // published target is therefore journaled normally; cleaning an unreported
    // staging-link residue is outside this in-memory transaction's guarantees.
    staged
        .persist_noclobber(&target.absolute)
        .map(drop)
        .map_err(|error| ToolError::io_display(operation, &target.display, error.error))
}

async fn error_after_rollback(
    environment: &ToolEnvironment,
    original: ToolError,
    journal: Vec<RollbackEntry>,
) -> ToolError {
    let failures = rollback_journal(environment, journal).await;
    if failures.is_empty() {
        original
    } else {
        let mut paths = Vec::new();
        for failure in &failures {
            if !paths.iter().any(|path| path == &failure.path) {
                paths.push(failure.path.clone());
            }
        }
        let details = failures
            .iter()
            .map(|failure| format!("{}: {}", failure.path, failure.detail))
            .collect::<Vec<_>>()
            .join("; ");
        ToolError::Io {
            operation: "apply patch rollback",
            path: paths.join(", "),
            // ToolError maps an `Other` I/O error to sanitized model feedback;
            // the original commit failure and recovery-file locations remain
            // available only through the operator-facing source chain.
            source: io::Error::other(format!(
                "{original}; rollback incomplete; unrecovered paths: {details}"
            )),
        }
    }
}

struct RollbackFailure {
    path: String,
    detail: String,
}

impl RollbackFailure {
    fn new(path: &ResolvedPath, error: impl std::fmt::Display) -> Self {
        Self {
            path: path.display.clone(),
            detail: error.to_string(),
        }
    }
}

async fn rollback_journal(
    environment: &ToolEnvironment,
    mut journal: Vec<RollbackEntry>,
) -> Vec<RollbackFailure> {
    let mut failures = Vec::new();
    while let Some(entry) = journal.pop() {
        match entry {
            RollbackEntry::Added { target } => {
                if let Err(error) = rollback_remove(environment, &target, "remove added file").await
                {
                    failures.push(RollbackFailure::new(&target, error));
                }
            }
            RollbackEntry::Deleted { target, backup }
            | RollbackEntry::Updated { target, backup } => {
                if let Err(failure) = rollback_restore(environment, &target, backup).await {
                    failures.push(failure);
                }
            }
            RollbackEntry::Moved {
                source,
                destination,
                backup,
                source_removed,
            } => {
                if let Err(error) =
                    rollback_remove(environment, &destination, "remove moved destination").await
                {
                    failures.push(RollbackFailure::new(&destination, error));
                }
                if source_removed
                    && let Err(failure) = rollback_restore(environment, &source, backup).await
                {
                    failures.push(failure);
                }
            }
        }
    }
    failures
}

async fn rollback_remove(
    environment: &ToolEnvironment,
    target: &ResolvedPath,
    operation: &'static str,
) -> Result<(), ToolError> {
    require_same_resolution(environment, target).await?;
    match tokio::fs::remove_file(&target.absolute).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ToolError::io_display(operation, &target.display, error)),
    }
}

async fn rollback_restore(
    environment: &ToolEnvironment,
    target: &ResolvedPath,
    backup: NamedTempFile,
) -> Result<(), RollbackFailure> {
    if let Err(error) = require_same_resolution(environment, target).await {
        return Err(retain_failed_backup(environment, target, backup, error));
    }
    match backup.persist(&target.absolute) {
        Ok(file) => {
            drop(file);
            Ok(())
        }
        Err(error) => {
            let restore_error =
                ToolError::io_display("restore file during rollback", &target.display, error.error);
            Err(retain_failed_backup(
                environment,
                target,
                error.file,
                restore_error,
            ))
        }
    }
}

fn retain_failed_backup(
    environment: &ToolEnvironment,
    target: &ResolvedPath,
    mut backup: NamedTempFile,
    restore_error: ToolError,
) -> RollbackFailure {
    let recovery_path = environment.workspace.display(backup.path());

    // Keep is preferable, especially on Windows where it clears temporary-file
    // attributes. Disable cleanup first so even a keep failure leaves the only
    // recovery copy available to an operator.
    backup.disable_cleanup(true);
    let keep_error = match backup.keep() {
        Ok((file, _)) => {
            drop(file);
            None
        }
        Err(mut error) => {
            let message = error.error.to_string();
            error.file.disable_cleanup(true);
            drop(error.file);
            Some(message)
        }
    };

    let detail = if let Some(keep_error) = keep_error {
        format!(
            "{restore_error}; recovery backup retained at `{recovery_path}` (marking it permanent also failed: {keep_error})"
        )
    } else {
        format!("{restore_error}; recovery backup retained at `{recovery_path}`")
    };
    RollbackFailure {
        path: target.display.clone(),
        detail,
    }
}

async fn revalidate_action(
    environment: &ToolEnvironment,
    action: &PlannedAction,
) -> Result<(), ToolError> {
    match action {
        PlannedAction::Add { target, .. } => require_missing(target, "add file").await,
        PlannedAction::Delete {
            target, expected, ..
        } => require_unchanged(target, expected, environment.limits.max_patch_file_bytes).await,
        PlannedAction::Update {
            source,
            destination,
            expected,
            ..
        } => {
            require_unchanged(source, expected, environment.limits.max_patch_file_bytes).await?;
            if let Some(destination) = destination {
                require_missing(destination, "move file").await?;
            }
            Ok(())
        }
    }
}

async fn require_unchanged(
    path: &ResolvedPath,
    expected: &str,
    max_bytes: u64,
) -> Result<(), ToolError> {
    let current = read_regular_text(path, "revalidate patch source", max_bytes, None).await?;
    if current.content == expected {
        Ok(())
    } else {
        Err(patch_error(&format!(
            "`{}` changed while the patch was being prepared; read it again and retry",
            path.display
        )))
    }
}

async fn create_parent_directories(
    environment: &ToolEnvironment,
    path: &ResolvedPath,
) -> Result<(), ToolError> {
    let parent = path
        .absolute
        .parent()
        .ok_or_else(|| patch_error(&format!("path `{}` has no parent directory", path.display)))?;
    tokio::fs::create_dir_all(parent).await.map_err(|error| {
        ToolError::io_display("create parent directories", &path.display, error)
    })?;

    // Do not trust a path merely because create_dir_all succeeded: a competing
    // process could have introduced a symlink in a missing component.
    let resolved = environment
        .workspace
        .resolve_patch_path(&path.display)
        .await?;
    if resolved.absolute != path.absolute {
        return Err(ToolError::OutsideWorkspace {
            path: path.display.clone(),
        });
    }
    Ok(())
}

fn logical_line_count(content: &str) -> usize {
    if content.is_empty() {
        0
    } else {
        content.lines().count()
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::Tool;

    use super::*;
    use crate::ToolLimits;

    fn tool(root: &Path) -> ApplyPatchTool {
        ToolEnvironment::new(root).unwrap().apply_patch()
    }

    fn commit_control() -> CommitControl {
        CommitControl {
            cancelled: Arc::new(AtomicBool::new(false)),
            hook: None,
        }
    }

    async fn apply(tool: &ApplyPatchTool, patch: &str) -> ApplyPatchOutput {
        tool.call(ApplyPatchArgs {
            patch: patch.to_owned(),
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn adds_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let output = apply(
            &tool(temp.path()),
            "*** Begin Patch\n*** Add File: src/new.txt\n+hello\n+world\n*** End Patch",
        )
        .await;

        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("src/new.txt"))
                .await
                .unwrap(),
            "hello\nworld\n"
        );
        assert_eq!(output.changes[0].kind, "add");
        assert_eq!(output.changes[0].path, "src/new.txt");
    }

    #[tokio::test]
    async fn updates_a_file_with_context() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("sample.txt"), "one\nold\nthree\n")
            .await
            .unwrap();

        let output = apply(
            &tool(temp.path()),
            "*** Begin Patch\n*** Update File: sample.txt\n@@\n one\n-old\n+new\n three\n*** End Patch",
        )
        .await;

        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("sample.txt"))
                .await
                .unwrap(),
            "one\nnew\nthree\n"
        );
        assert_eq!(output.changes[0].kind, "update");
        assert_eq!(output.changes[0].added_lines, 1);
        assert_eq!(output.changes[0].removed_lines, 1);
    }

    #[tokio::test]
    async fn deletes_a_file() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("obsolete.txt"), "gone\n")
            .await
            .unwrap();

        let output = apply(
            &tool(temp.path()),
            "*** Begin Patch\n*** Delete File: obsolete.txt\n*** End Patch",
        )
        .await;

        assert!(!temp.path().join("obsolete.txt").exists());
        assert_eq!(output.changes[0].kind, "delete");
    }

    #[tokio::test]
    async fn moves_and_updates_a_file() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("old.txt"), "before\n")
            .await
            .unwrap();

        let output = apply(
            &tool(temp.path()),
            "*** Begin Patch\n*** Update File: old.txt\n*** Move to: nested/new.txt\n@@\n-before\n+after\n*** End Patch",
        )
        .await;

        assert!(!temp.path().join("old.txt").exists());
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("nested/new.txt"))
                .await
                .unwrap(),
            "after\n"
        );
        assert_eq!(output.changes[0].kind, "move");
        assert_eq!(
            output.changes[0].moved_to.as_deref(),
            Some("nested/new.txt")
        );
    }

    #[tokio::test]
    async fn validation_failure_does_not_apply_earlier_operations() {
        let temp = tempfile::tempdir().unwrap();
        let result = tool(temp.path())
            .call(ApplyPatchArgs {
                patch: "*** Begin Patch\n*** Add File: created.txt\n+created\n*** Update File: missing.txt\n@@\n-old\n+new\n*** End Patch"
                    .to_owned(),
            })
            .await;

        assert!(result.is_err());
        assert!(!temp.path().join("created.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn staging_failure_does_not_commit_an_earlier_add() {
        use std::os::unix::fs::PermissionsExt;

        // Root can create files in a read-only directory, so this permission
        // regression is meaningful only for an unprivileged process.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let locked = temp.path().join("locked");
        tokio::fs::create_dir(&locked).await.unwrap();
        tokio::fs::write(locked.join("existing.txt"), "old\n")
            .await
            .unwrap();
        tokio::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555))
            .await
            .unwrap();

        let result = tool(temp.path())
            .call(ApplyPatchArgs {
                patch: "*** Begin Patch\n*** Add File: added.txt\n+added\n*** Update File: locked/existing.txt\n@@\n-old\n+new\n*** End Patch"
                    .to_owned(),
            })
            .await;

        tokio::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755))
            .await
            .unwrap();
        assert!(result.is_err());
        assert!(!temp.path().join("added.txt").exists());
        assert_eq!(
            tokio::fs::read_to_string(locked.join("existing.txt"))
                .await
                .unwrap(),
            "old\n"
        );
    }

    #[tokio::test]
    async fn later_commit_failure_rolls_back_all_prior_action_kinds() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("updated.txt"), "update-old\n")
            .await
            .unwrap();
        tokio::fs::write(temp.path().join("deleted.txt"), "delete-old\n")
            .await
            .unwrap();
        tokio::fs::write(temp.path().join("move-source.txt"), "move-old\n")
            .await
            .unwrap();
        tokio::fs::write(temp.path().join("fail.txt"), "fail-old\n")
            .await
            .unwrap();

        let environment = ToolEnvironment::new(temp.path()).unwrap();
        let operations = parse_patch(
            "*** Begin Patch\n*** Add File: added.txt\n+added\n*** Update File: updated.txt\n@@\n-update-old\n+update-new\n*** Delete File: deleted.txt\n*** Update File: move-source.txt\n*** Move to: move-target.txt\n@@\n-move-old\n+move-new\n*** Update File: fail.txt\n@@\n-fail-old\n+fail-new\n*** End Patch",
        )
        .unwrap();
        let plan = build_plan(&environment, operations).await.unwrap();
        validate_plan(&environment, &plan).await.unwrap();
        let staged = stage_plan(&environment, &plan).await.unwrap();
        validate_plan(&environment, &plan).await.unwrap();

        // Simulate a filesystem race after final validation: replacing a file
        // with a directory makes the final atomic rename fail only after every
        // earlier action has committed.
        tokio::fs::remove_file(temp.path().join("fail.txt"))
            .await
            .unwrap();
        tokio::fs::create_dir(temp.path().join("fail.txt"))
            .await
            .unwrap();

        let control = commit_control();
        let result = commit_staged(&environment, plan, staged, &control).await;
        assert!(result.is_err());
        assert!(!temp.path().join("added.txt").exists());
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("updated.txt"))
                .await
                .unwrap(),
            "update-old\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("deleted.txt"))
                .await
                .unwrap(),
            "delete-old\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("move-source.txt"))
                .await
                .unwrap(),
            "move-old\n"
        );
        assert!(!temp.path().join("move-target.txt").exists());
        assert!(temp.path().join("fail.txt").is_dir());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_atomic_replace_does_not_truncate_the_source() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("source.txt"), "original\n")
            .await
            .unwrap();
        let environment = ToolEnvironment::new(temp.path()).unwrap();
        let operations = parse_patch(
            "*** Begin Patch\n*** Update File: source.txt\n@@\n-original\n+replacement\n*** End Patch",
        )
        .unwrap();
        let plan = build_plan(&environment, operations).await.unwrap();
        let staged = stage_plan(&environment, &plan).await.unwrap();
        let replacement_path = match &staged[0] {
            StagedFiles::Update { replacement, .. } => replacement.path().to_owned(),
            _ => panic!("expected staged update"),
        };
        tokio::fs::remove_file(replacement_path).await.unwrap();

        let control = commit_control();
        let result = commit_staged(&environment, plan, staged, &control).await;
        assert!(result.is_err());
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("source.txt"))
                .await
                .unwrap(),
            "original\n"
        );
    }

    #[tokio::test]
    async fn rollback_failure_lists_the_unrecovered_workspace_path() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(temp.path().join("cannot-remove"))
            .await
            .unwrap();
        let environment = ToolEnvironment::new(temp.path()).unwrap();
        let target = environment
            .workspace
            .resolve_patch_path("cannot-remove")
            .await
            .unwrap();

        let error = error_after_rollback(
            &environment,
            patch_error("forced commit failure"),
            vec![RollbackEntry::Added { target }],
        )
        .await;

        assert!(matches!(
            &error,
            ToolError::Io { path, .. } if path == "cannot-remove"
        ));
        assert!(
            error
                .to_string()
                .contains("unrecovered paths: cannot-remove")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_the_call_rolls_back_a_detached_transaction() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("existing.txt"), "old\n")
            .await
            .unwrap();

        let reached = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let mut patch_tool = tool(temp.path());
        patch_tool.commit_hook = Some(CommitTestHook {
            pause_after_actions: 1,
            reached: reached.clone(),
            release: release.clone(),
        });
        let call = tokio::spawn(async move {
            patch_tool
                .call(ApplyPatchArgs {
                    patch: "*** Begin Patch\n*** Add File: added.txt\n+added\n*** Update File: existing.txt\n@@\n-old\n+new\n*** End Patch"
                        .to_owned(),
                })
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !reached.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(temp.path().join("added.txt").exists());

        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        release.store(true, Ordering::Release);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while temp.path().join("added.txt").exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("existing.txt"))
                .await
                .unwrap(),
            "old\n"
        );
    }

    #[tokio::test]
    async fn failed_restore_retains_a_recovery_file_without_model_path_leakage() {
        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("original.txt");
        tokio::fs::write(&original, "recover me\n").await.unwrap();
        let environment = ToolEnvironment::new(temp.path()).unwrap();
        let target = environment
            .workspace
            .resolve_patch_path("original.txt")
            .await
            .unwrap();
        let permissions = tokio::fs::metadata(&original).await.unwrap().permissions();
        let backup = stage_content(
            &target,
            b"recover me\n",
            Some(&permissions),
            StagePurpose::Recovery,
        )
        .unwrap();

        tokio::fs::remove_file(&original).await.unwrap();
        tokio::fs::create_dir(&original).await.unwrap();
        let error = error_after_rollback(
            &environment,
            patch_error("forced commit failure"),
            vec![RollbackEntry::Deleted { target, backup }],
        )
        .await;

        let operator_message = error.to_string();
        assert!(operator_message.contains(".bone-patch-recovery-"));
        let failure = error.into_tool_failure();
        assert_eq!(
            failure.model_output(),
            &bone_llm::ToolOutput::text("apply patch rollback failed for original.txt")
        );

        let mut entries = tokio::fs::read_dir(temp.path()).await.unwrap();
        let mut recovered = None;
        while let Some(entry) = entries.next_entry().await.unwrap() {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".bone-patch-recovery-")
            {
                recovered = Some(entry.path());
                break;
            }
        }
        let recovered = recovered.expect("recovery backup should remain in the workspace");
        assert_eq!(
            tokio::fs::read_to_string(recovered).await.unwrap(),
            "recover me\n"
        );
    }

    #[tokio::test]
    async fn rejects_workspace_escape_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        let outside = temp
            .path()
            .parent()
            .unwrap()
            .join("bone-tools-patch-escape.txt");
        let result = tool(temp.path())
            .call(ApplyPatchArgs {
                patch: "*** Begin Patch\n*** Add File: ../bone-tools-patch-escape.txt\n+secret\n*** End Patch"
                    .to_owned(),
            })
            .await;

        assert!(matches!(result, Err(ToolError::OutsideWorkspace { .. })));
        assert!(!outside.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_symlink_escape_before_writing() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), temp.path().join("linked")).unwrap();

        let result = tool(temp.path())
            .call(ApplyPatchArgs {
                patch: "*** Begin Patch\n*** Add File: linked/escape.txt\n+secret\n*** End Patch"
                    .to_owned(),
            })
            .await;

        assert!(matches!(result, Err(ToolError::PermissionDenied { .. })));
        assert!(!outside.path().join("escape.txt").exists());
    }

    #[tokio::test]
    async fn rejects_ambiguous_fuzzy_matches_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ambiguous.txt");
        let original = "target   \nother\ntarget\t\n";
        tokio::fs::write(&path, original).await.unwrap();

        let result = tool(temp.path())
            .call(ApplyPatchArgs {
                patch: "*** Begin Patch\n*** Update File: ambiguous.txt\n@@\n-target\n+changed\n*** End Patch"
                    .to_owned(),
            })
            .await;

        assert!(
            matches!(result, Err(ToolError::Patch(message)) if message.contains("multiple rstrip matches"))
        );
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn rejects_files_over_the_patch_planning_limit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("large.txt");
        tokio::fs::write(&path, "large\n").await.unwrap();
        let limits = ToolLimits {
            max_patch_file_bytes: 3,
            ..ToolLimits::default()
        };
        let tool = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .apply_patch();

        let result = tool
            .call(ApplyPatchArgs {
                patch:
                    "*** Begin Patch\n*** Update File: large.txt\n@@\n-large\n+small\n*** End Patch"
                        .to_owned(),
            })
            .await;

        assert!(
            matches!(result, Err(ToolError::Patch(message)) if message.contains("maximum patchable file size"))
        );
        assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), "large\n");
    }

    #[tokio::test]
    async fn rejects_too_many_file_operations_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        let limits = ToolLimits {
            max_patch_files: 1,
            ..ToolLimits::default()
        };
        let patch_tool = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .apply_patch();

        let result = patch_tool
            .call(ApplyPatchArgs {
                patch: "*** Begin Patch\n*** Add File: one.txt\n+one\n*** Add File: two.txt\n+two\n*** End Patch"
                    .to_owned(),
            })
            .await;

        assert!(
            matches!(result, Err(ToolError::InvalidArgs(message)) if message.contains("maximum is 1"))
        );
        assert!(!temp.path().join("one.txt").exists());
        assert!(!temp.path().join("two.txt").exists());
    }

    #[tokio::test]
    async fn rejects_excessive_combined_snapshot_bytes_before_writing() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("one.txt"), "one\n")
            .await
            .unwrap();
        tokio::fs::write(temp.path().join("two.txt"), "two\n")
            .await
            .unwrap();
        let limits = ToolLimits {
            max_patch_file_bytes: 10,
            max_patch_total_bytes: 7,
            ..ToolLimits::default()
        };
        let patch_tool = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .apply_patch();

        let result = patch_tool
            .call(ApplyPatchArgs {
                patch: "*** Begin Patch\n*** Delete File: one.txt\n*** Delete File: two.txt\n*** End Patch"
                    .to_owned(),
            })
            .await;

        assert!(
            matches!(result, Err(ToolError::Patch(message)) if message.contains("maximum combined existing-file snapshot size of 7 bytes"))
        );
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("one.txt"))
                .await
                .unwrap(),
            "one\n"
        );
        assert_eq!(
            tokio::fs::read_to_string(temp.path().join("two.txt"))
                .await
                .unwrap(),
            "two\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn move_preserves_source_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("script.sh");
        tokio::fs::write(&source, "old\n").await.unwrap();
        tokio::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o751))
            .await
            .unwrap();

        apply(
            &tool(temp.path()),
            "*** Begin Patch\n*** Update File: script.sh\n*** Move to: bin/script.sh\n@@\n-old\n+new\n*** End Patch",
        )
        .await;

        let mode = tokio::fs::metadata(temp.path().join("bin/script.sh"))
            .await
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o751);
    }

    #[test]
    fn arguments_reject_unknown_fields() {
        let value = json!({
            "patch": "*** Begin Patch\n*** Delete File: old.txt\n*** End Patch",
            "cwd": "/tmp"
        });
        assert!(serde_json::from_value::<ApplyPatchArgs>(value).is_err());
    }

    #[test]
    fn schema_describes_the_byte_limit_without_a_character_max_length() {
        let temp = tempfile::tempdir().unwrap();
        let definition = tool(temp.path()).definition();
        let parameters = definition.parameters();

        assert!(parameters.pointer("/properties/patch/maxLength").is_none());
        assert!(
            parameters["properties"]["patch"]["description"]
                .as_str()
                .unwrap()
                .contains("UTF-8 bytes")
        );
    }
}
