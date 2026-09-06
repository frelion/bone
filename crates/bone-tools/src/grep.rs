use std::{
    fs::File,
    io::{self, Read},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use crate::{Tool, ToolFailure};
use bone_llm::ToolDefinition;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ToolEnvironment, ToolError, ToolLimits,
    search_walk::{
        CancellableReader, CancellationGuard, SearchWalk, SearchWalkEvent, push_bounded_warning,
        reject_vcs_root,
    },
    workspace::{ResolvedPath, path_to_slashes},
};

const REGEX_SIZE_LIMIT: usize = 10 * 1024 * 1024;
const REGEX_DFA_SIZE_LIMIT: usize = 10 * 1024 * 1024;
const MAX_FILTER_GLOB_BYTES: usize = 4 * 1024;

/// Arguments accepted by [`GrepTool`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GrepArgs {
    /// Rust regular expression, or a literal string when `literal` is true.
    pub pattern: String,
    /// Workspace-relative file or directory. Defaults to the workspace root.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional glob matched against paths relative to `path`.
    #[serde(default)]
    pub glob: Option<String>,
    /// Interpret `pattern` as a literal string instead of a regular expression.
    #[serde(default)]
    pub literal: bool,
    /// Match ASCII and Unicode letters without regard to case.
    #[serde(default)]
    pub case_insensitive: bool,
    /// Disable default dotfile filtering while still excluding VCS metadata.
    /// An ignore whitelist may include a hidden path even when this is false.
    #[serde(default)]
    pub include_hidden: bool,
    /// Maximum matching lines to return, bounded by the environment hard limit.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Number of leading and trailing context lines to return.
    #[serde(default)]
    pub context: Option<usize>,
}

/// One matching or contextual source line returned by [`GrepTool`].
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GrepMatch {
    /// Workspace-relative path using `/` separators.
    pub path: String,
    /// One-based source line number.
    pub line_number: u64,
    /// Matching source line without its line terminator.
    pub text: String,
    /// `match`, `before_context`, or `after_context`.
    pub kind: String,
    /// True when this individual line was shortened.
    pub line_truncated: bool,
}

/// Bounded, sorted search results returned by [`GrepTool`].
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GrepOutput {
    /// Matching and contextual lines sorted by path and line. When truncated,
    /// the selected subset can depend on filesystem enumeration order.
    pub matches: Vec<GrepMatch>,
    /// Number of entries in `matches` whose kind is `match`.
    pub match_count: usize,
    /// True when a result, traversal, output, or warning limit was reached.
    pub truncated: bool,
    /// Number of filesystem entries inspected, including directories.
    pub scanned_entries: usize,
    /// Number of regular files passed to the searcher.
    pub searched_files: usize,
    /// Sum of file sizes admitted to the searcher.
    pub searched_bytes: u64,
    /// Files rejected after NUL-based binary detection.
    pub binary_files_skipped: usize,
    /// Files skipped because they exceed the configured search size limit.
    pub oversized_files_skipped: usize,
    /// Recoverable traversal, file, binary, and ignore-file warnings.
    pub warnings: Vec<String>,
}

/// Search workspace text files using ripgrep's streaming Rust libraries.
#[derive(Clone, Debug)]
pub struct GrepTool {
    environment: ToolEnvironment,
}

impl GrepTool {
    pub(crate) fn new(environment: ToolEnvironment) -> Self {
        Self { environment }
    }
}

impl Tool for GrepTool {
    type Args = GrepArgs;
    type Output = GrepOutput;
    type Error = ToolError;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "grep",
            format!(
                "Search text files inside the workspace with a Rust regular expression or literal string. The search respects bounded workspace-local ignore files at or below the selected search root (rules above an explicitly selected root are not inherited), skips binary and oversized files, never enters VCS metadata, and returns at most {} sorted matching lines. If truncated, the selected subset can depend on filesystem enumeration order.",
                self.environment.limits.max_grep_matches,
            ),
            json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "minLength": 1,
                        "description": format!("Rust regex syntax, or literal text when literal is true; at most {} UTF-8 bytes", self.environment.limits.max_grep_pattern_bytes)
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory relative to the workspace, or an absolute path inside it; defaults to ."
                    },
                    "glob": {
                        "type": "string",
                        "minLength": 1,
                        "description": format!("Optional file glob relative to path, using / separators; at most {MAX_FILTER_GLOB_BYTES} UTF-8 bytes")
                    },
                    "literal": {
                        "type": "boolean",
                        "default": false,
                        "description": "Treat pattern as literal text"
                    },
                    "case_insensitive": {
                        "type": "boolean",
                        "default": false,
                        "description": "Enable Unicode-aware case-insensitive matching"
                    },
                    "include_hidden": {
                        "type": "boolean",
                        "default": false,
                        "description": "Disable default dotfile filtering; ignore whitelists can still include hidden paths, while VCS metadata is always excluded"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": self.environment.limits.max_grep_matches,
                        "description": "Maximum matching lines to return; context lines do not count"
                    },
                    "context": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": 10,
                        "default": 0,
                        "description": "Number of lines before and after each match"
                    }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        )
    }

    fn map_error(&self, error: Self::Error) -> ToolFailure {
        error.into_tool_failure()
    }

    async fn call(&self, arguments: Self::Args) -> Result<Self::Output, Self::Error> {
        validate_arguments(&arguments, &self.environment.limits)?;
        let result_limit = arguments
            .limit
            .unwrap_or(self.environment.limits.max_grep_matches);
        let context = arguments.context.unwrap_or(0);
        let requested_path = arguments.path.as_deref().unwrap_or(".");
        let resolved = self
            .environment
            .workspace
            .resolve_existing(requested_path)
            .await?;
        reject_vcs_root(&resolved)?;

        let workspace_root = self.environment.workspace.root().to_path_buf();
        let limits = self.environment.limits.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut cancellation_guard = CancellationGuard::new(cancelled.clone());
        let joined = tokio::task::spawn_blocking(move || {
            run_grep(
                &workspace_root,
                resolved,
                arguments,
                result_limit,
                context,
                &limits,
                &cancelled,
            )
        })
        .await;
        cancellation_guard.disarm();
        joined.map_err(|error| ToolError::Task(error.to_string()))?
    }
}

fn validate_arguments(arguments: &GrepArgs, limits: &ToolLimits) -> Result<(), ToolError> {
    if arguments.pattern.is_empty() {
        return Err(ToolError::InvalidArgs(
            "grep pattern must not be empty".to_owned(),
        ));
    }
    if arguments.pattern.len() > limits.max_grep_pattern_bytes {
        return Err(ToolError::InvalidArgs(format!(
            "grep pattern exceeds {} UTF-8 bytes",
            limits.max_grep_pattern_bytes
        )));
    }
    if let Some(glob) = &arguments.glob {
        if glob.is_empty() {
            return Err(ToolError::InvalidArgs(
                "grep glob must not be empty".to_owned(),
            ));
        }
        if glob.len() > MAX_FILTER_GLOB_BYTES {
            return Err(ToolError::InvalidArgs(format!(
                "grep glob exceeds {MAX_FILTER_GLOB_BYTES} UTF-8 bytes"
            )));
        }
    }
    if let Some(limit) = arguments.limit
        && (limit == 0 || limit > limits.max_grep_matches)
    {
        return Err(ToolError::InvalidArgs(format!(
            "limit must be between 1 and {}",
            limits.max_grep_matches
        )));
    }
    if arguments.context.is_some_and(|context| context > 10) {
        return Err(ToolError::InvalidArgs(
            "context must be between 0 and 10".to_owned(),
        ));
    }
    Ok(())
}

fn run_grep(
    workspace_root: &Path,
    resolved: ResolvedPath,
    arguments: GrepArgs,
    result_limit: usize,
    context: usize,
    limits: &ToolLimits,
    cancelled: &AtomicBool,
) -> Result<GrepOutput, ToolError> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(arguments.case_insensitive)
        .fixed_strings(arguments.literal)
        .multi_line(true)
        .crlf(true)
        .line_terminator(Some(b'\n'))
        .ban_byte(Some(0))
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build(&arguments.pattern)
        .map_err(|error| ToolError::InvalidRegex(error.to_string()))?;
    let glob = build_filter_glob(arguments.glob.as_deref())?;

    // grep-searcher's line buffer needs one spare byte to probe EOF when a
    // maximum-sized file has no trailing line terminator.
    let heap_limit = usize::try_from(limits.max_search_file_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(1);

    let mut output = GrepOutput {
        matches: Vec::new(),
        match_count: 0,
        truncated: false,
        scanned_entries: 0,
        searched_files: 0,
        searched_bytes: 0,
        binary_files_skipped: 0,
        oversized_files_skipped: 0,
        warnings: Vec::new(),
    };
    let mut output_bytes = 0usize;

    let mut walk = SearchWalk::new(
        &resolved.absolute,
        workspace_root,
        arguments.include_hidden,
        limits,
        cancelled,
    );
    while let Some(event) = walk.next_event() {
        let path = match event {
            SearchWalkEvent::Warning(warning) => {
                if !push_bounded_warning(
                    &mut output.warnings,
                    &mut output_bytes,
                    limits.max_output_bytes,
                    warning,
                    workspace_root,
                ) {
                    output.truncated = true;
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
        let filter_candidate = if relative_to_search.as_os_str().is_empty() {
            Path::new(path.file_name().unwrap_or_default())
        } else {
            relative_to_search
        };
        if glob
            .as_ref()
            .is_some_and(|matcher| !matcher.is_match(filter_candidate))
        {
            continue;
        }

        let relative_to_workspace =
            path.strip_prefix(workspace_root)
                .map_err(|_| ToolError::OutsideWorkspace {
                    path: resolved.display.clone(),
                })?;
        let display_path = path_to_slashes(relative_to_workspace);

        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) => {
                if !push_bounded_warning(
                    &mut output.warnings,
                    &mut output_bytes,
                    limits.max_output_bytes,
                    format!("could not open {display_path}: {:?}", error.kind()),
                    workspace_root,
                ) {
                    output.truncated = true;
                }
                continue;
            }
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                if !push_bounded_warning(
                    &mut output.warnings,
                    &mut output_bytes,
                    limits.max_output_bytes,
                    format!("could not inspect {display_path}: {:?}", error.kind()),
                    workspace_root,
                ) {
                    output.truncated = true;
                }
                continue;
            }
        };
        if !metadata.is_file() {
            if !push_bounded_warning(
                &mut output.warnings,
                &mut output_bytes,
                limits.max_output_bytes,
                format!("skipped non-regular file {display_path}"),
                workspace_root,
            ) {
                output.truncated = true;
            }
            continue;
        }
        if metadata.len() > limits.max_search_file_bytes {
            output.oversized_files_skipped += 1;
            if !push_bounded_warning(
                &mut output.warnings,
                &mut output_bytes,
                limits.max_output_bytes,
                format!(
                    "skipped oversized file {display_path} ({} bytes; limit {})",
                    metadata.len(),
                    limits.max_search_file_bytes
                ),
                workspace_root,
            ) {
                output.truncated = true;
            }
            continue;
        }

        let next_searched_bytes = output.searched_bytes.checked_add(metadata.len());
        if next_searched_bytes.is_none_or(|bytes| bytes > limits.max_search_total_bytes) {
            output.truncated = true;
            let _ = push_bounded_warning(
                &mut output.warnings,
                &mut output_bytes,
                limits.max_output_bytes,
                format!(
                    "stopped before {display_path}: cumulative search size would exceed {} bytes",
                    limits.max_search_total_bytes
                ),
                workspace_root,
            );
            break;
        }
        let next_searched_bytes = next_searched_bytes.expect("checked above");

        output.searched_files += 1;
        output.searched_bytes = next_searched_bytes;
        let rows_before_file = output.matches.len();
        let match_count_before_file = output.match_count;
        let output_bytes_before_file = output_bytes;
        let mut searcher = SearcherBuilder::new()
            .line_number(true)
            .heap_limit(Some(heap_limit))
            .before_context(context)
            .after_context(context)
            .binary_detection(BinaryDetection::quit(0))
            .build();
        let (search_result, limit_hit, binary_offset) = {
            let mut sink = CollectSink {
                path: &display_path,
                matches: &mut output.matches,
                match_count: &mut output.match_count,
                output_bytes: &mut output_bytes,
                max_matches: result_limit,
                max_output_bytes: limits.max_output_bytes,
                max_line_chars: limits.max_grep_line_chars,
                limit_hit: false,
                binary_offset: None,
                pending_before: Vec::new(),
                collecting_after: false,
                cancelled,
            };
            let reader = CancellableReader::new(file, cancelled).take(metadata.len());
            let search_result = searcher.search_reader(&matcher, reader, &mut sink);
            (search_result, sink.limit_hit, sink.binary_offset)
        };

        let binary_detected = binary_offset.is_some();
        if let Some(offset) = binary_offset {
            output.matches.truncate(rows_before_file);
            output.match_count = match_count_before_file;
            output_bytes = output_bytes_before_file;
            output.binary_files_skipped += 1;
            if !push_bounded_warning(
                &mut output.warnings,
                &mut output_bytes,
                limits.max_output_bytes,
                format!("skipped binary file {display_path} (NUL at byte {offset})"),
                workspace_root,
            ) {
                output.truncated = true;
            }
        }
        if !binary_detected
            && output.matches[rows_before_file..]
                .iter()
                .any(|line| line.line_truncated)
        {
            output.truncated = true;
        }
        if let Err(error) = search_result
            && !push_bounded_warning(
                &mut output.warnings,
                &mut output_bytes,
                limits.max_output_bytes,
                format!("could not search {display_path}: {:?}", error.kind()),
                workspace_root,
            )
        {
            output.truncated = true;
        }
        if limit_hit && !binary_detected {
            output.truncated = true;
            break;
        }
        if output.match_count == result_limit {
            output.truncated = true;
            break;
        }
        if cancelled.load(Ordering::Relaxed) {
            output.truncated = true;
            break;
        }
    }

    output.scanned_entries = walk.scanned_entries();
    output.truncated |= walk.truncated();
    output.matches.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.line_number.cmp(&right.line_number))
            .then_with(|| match_kind_rank(&left.kind).cmp(&match_kind_rank(&right.kind)))
    });
    output.warnings.sort();

    Ok(output)
}

fn build_filter_glob(pattern: Option<&str>) -> Result<Option<GlobSet>, ToolError> {
    let Some(pattern) = pattern else {
        return Ok(None);
    };
    let glob = GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .map_err(|error| ToolError::InvalidGlob(error.to_string()))?;
    GlobSetBuilder::new()
        .add(glob)
        .build()
        .map(Some)
        .map_err(|error| ToolError::InvalidGlob(error.to_string()))
}

struct CollectSink<'a> {
    path: &'a str,
    matches: &'a mut Vec<GrepMatch>,
    match_count: &'a mut usize,
    output_bytes: &'a mut usize,
    max_matches: usize,
    max_output_bytes: usize,
    max_line_chars: usize,
    limit_hit: bool,
    binary_offset: Option<u64>,
    pending_before: Vec<PendingRow>,
    collecting_after: bool,
    cancelled: &'a AtomicBool,
}

struct PendingRow {
    line_number: u64,
    text: String,
    kind: &'static str,
    line_truncated: bool,
}

impl Sink for CollectSink<'_> {
    type Error = io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        matched: &SinkMatch<'_>,
    ) -> Result<bool, Self::Error> {
        if self.cancelled.load(Ordering::Relaxed) {
            self.pending_before.clear();
            self.collecting_after = false;
            return Ok(false);
        }
        if self.limit_hit {
            self.pending_before.clear();
            self.collecting_after = false;
            return Ok(true);
        }
        if *self.match_count == self.max_matches {
            self.limit_hit = true;
            self.pending_before.clear();
            self.collecting_after = false;
            return Ok(true);
        }

        let matched_row = PendingRow::new(
            matched.line_number().unwrap_or(0),
            matched.bytes(),
            "match",
            self.max_line_chars,
        );
        if self.commit_match_group(matched_row) {
            *self.match_count += 1;
            self.collecting_after = true;
        } else {
            self.limit_hit = true;
            self.collecting_after = false;
        }
        Ok(true)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        context: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        if self.cancelled.load(Ordering::Relaxed) {
            self.pending_before.clear();
            self.collecting_after = false;
            return Ok(false);
        }
        if self.limit_hit {
            return Ok(true);
        }
        match context.kind() {
            SinkContextKind::Before => {
                self.pending_before.push(PendingRow::new(
                    context.line_number().unwrap_or(0),
                    context.bytes(),
                    "before_context",
                    self.max_line_chars,
                ));
            }
            SinkContextKind::After if self.collecting_after => {
                let row = PendingRow::new(
                    context.line_number().unwrap_or(0),
                    context.bytes(),
                    "after_context",
                    self.max_line_chars,
                );
                if !self.commit_row(row) {
                    self.limit_hit = true;
                    self.collecting_after = false;
                }
            }
            SinkContextKind::After | SinkContextKind::Other => {}
        }
        Ok(true)
    }

    fn binary_data(
        &mut self,
        _searcher: &Searcher,
        binary_byte_offset: u64,
    ) -> Result<bool, Self::Error> {
        self.binary_offset.get_or_insert(binary_byte_offset);
        Ok(!self.cancelled.load(Ordering::Relaxed))
    }
}

impl CollectSink<'_> {
    fn commit_match_group(&mut self, mut matched: PendingRow) -> bool {
        let group_cost =
            self.rows_cost(self.pending_before.iter().chain(std::iter::once(&matched)));
        let remaining = self.max_output_bytes.saturating_sub(*self.output_bytes);
        if group_cost <= remaining {
            for row in std::mem::take(&mut self.pending_before) {
                self.commit_row_unchecked(row);
            }
            self.commit_row_unchecked(matched);
            return true;
        }

        // Context is optional; when the complete group does not fit, prefer a
        // matching line over emitting context with no corresponding match.
        self.limit_hit = true;
        self.pending_before.clear();
        let overhead = self.row_overhead(&matched, !self.matches.is_empty());
        let remaining = self.max_output_bytes.saturating_sub(*self.output_bytes);
        if remaining <= overhead {
            return false;
        }
        let text_budget = remaining - overhead;
        if matched.text.len() > text_budget {
            matched
                .text
                .truncate(utf8_boundary(&matched.text, text_budget));
            matched.line_truncated = true;
        }
        self.commit_row_unchecked(matched);
        true
    }

    fn commit_row(&mut self, row: PendingRow) -> bool {
        let cost = self.row_cost(&row, !self.matches.is_empty());
        if self.output_bytes.saturating_add(cost) > self.max_output_bytes {
            return false;
        }
        self.commit_row_unchecked(row);
        true
    }

    fn commit_row_unchecked(&mut self, row: PendingRow) {
        let cost = self.row_cost(&row, !self.matches.is_empty());
        *self.output_bytes += cost;
        self.matches.push(GrepMatch {
            path: self.path.to_owned(),
            line_number: row.line_number,
            text: row.text,
            kind: row.kind.to_owned(),
            line_truncated: row.line_truncated,
        });
    }

    fn rows_cost<'a>(&self, rows: impl Iterator<Item = &'a PendingRow>) -> usize {
        let mut has_previous = !self.matches.is_empty();
        rows.fold(0usize, |total, row| {
            let cost = self.row_cost(row, has_previous);
            has_previous = true;
            total.saturating_add(cost)
        })
    }

    fn row_cost(&self, row: &PendingRow, has_previous: bool) -> usize {
        self.row_overhead(row, has_previous)
            .saturating_add(row.text.len())
    }

    fn row_overhead(&self, row: &PendingRow, has_previous: bool) -> usize {
        self.path.len()
            + decimal_digits(row.line_number)
            + usize::from(has_previous)
            + row.kind.len()
            + 3
    }
}

impl PendingRow {
    fn new(line_number: u64, bytes: &[u8], kind: &'static str, max_chars: usize) -> Self {
        let (text, line_truncated) = bounded_line(bytes, max_chars);
        Self {
            line_number,
            text,
            kind,
            line_truncated,
        }
    }
}

fn bounded_line(bytes: &[u8], max_chars: usize) -> (String, bool) {
    let bytes = strip_line_terminator(bytes);
    let max_input_bytes = max_chars.saturating_mul(4);
    let inspected_len = bytes.len().min(max_input_bytes);
    let inspected = &bytes[..inspected_len];
    let lossy = String::from_utf8_lossy(inspected);
    let mut chars = lossy.chars();
    let text: String = chars.by_ref().take(max_chars).collect();
    let truncated = bytes.len() > inspected_len || chars.next().is_some();
    (text, truncated)
}

fn strip_line_terminator(mut bytes: &[u8]) -> &[u8] {
    if bytes.ends_with(b"\n") {
        bytes = &bytes[..bytes.len() - 1];
    }
    if bytes.ends_with(b"\r") {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn utf8_boundary(value: &str, max_bytes: usize) -> usize {
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn decimal_digits(mut value: u64) -> usize {
    let mut digits = 1;
    while value >= 10 {
        value /= 10;
        digits += 1;
    }
    digits
}

fn match_kind_rank(kind: &str) -> u8 {
    match kind {
        "before_context" => 0,
        "match" => 1,
        "after_context" => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::Tool;

    use super::*;

    fn fixture() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".git")).unwrap();
        fs::create_dir_all(temp.path().join("src")).unwrap();
        fs::write(temp.path().join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(temp.path().join("a.rs"), "zero\nneedle one\n").unwrap();
        fs::write(temp.path().join("src/b.rs"), "needle two\ntail\n").unwrap();
        fs::write(temp.path().join("ignored.rs"), "needle ignored\n").unwrap();
        fs::write(temp.path().join(".hidden.rs"), "needle hidden\n").unwrap();
        fs::write(temp.path().join(".git/private.rs"), "needle metadata\n").unwrap();
        fs::write(
            temp.path().join("binary.dat"),
            b"needle before NUL\n\0needle after NUL\n",
        )
        .unwrap();
        temp
    }

    #[tokio::test]
    async fn returns_sorted_lines_and_reports_binary_files() {
        let temp = fixture();
        let output = ToolEnvironment::new(temp.path())
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "needle".to_owned(),
                path: None,
                glob: Some("**/*.rs".to_owned()),
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: None,
            })
            .await
            .unwrap();

        assert_eq!(
            output.matches,
            [
                GrepMatch {
                    path: "a.rs".to_owned(),
                    line_number: 2,
                    text: "needle one".to_owned(),
                    kind: "match".to_owned(),
                    line_truncated: false,
                },
                GrepMatch {
                    path: "src/b.rs".to_owned(),
                    line_number: 1,
                    text: "needle two".to_owned(),
                    kind: "match".to_owned(),
                    line_truncated: false,
                }
            ]
        );
        assert_eq!(output.match_count, 2);
        assert_eq!(output.binary_files_skipped, 0);
        assert!(!output.truncated);
    }

    #[tokio::test]
    async fn detects_binary_files_without_a_glob_filter() {
        let temp = fixture();
        let output = ToolEnvironment::new(temp.path())
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "needle".to_owned(),
                path: Some("binary.dat".to_owned()),
                glob: None,
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: None,
            })
            .await
            .unwrap();

        assert!(output.matches.is_empty());
        assert_eq!(output.binary_files_skipped, 1);
        assert!(output.warnings[0].contains("binary file binary.dat"));
    }

    #[tokio::test]
    async fn literal_mode_does_not_interpret_regex_metacharacters() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("values.txt"), "needle.*\nneedle123\n").unwrap();
        let tool = ToolEnvironment::new(temp.path()).unwrap().grep();
        let output = tool
            .call(GrepArgs {
                pattern: "needle.*".to_owned(),
                path: None,
                glob: None,
                literal: true,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: None,
            })
            .await
            .unwrap();

        assert_eq!(output.matches.len(), 1);
        assert_eq!(output.matches[0].text, "needle.*");
    }

    #[tokio::test]
    async fn matches_anchors_on_lf_and_crlf_lines() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("lines.txt"), b"needle\nneedle\r\n").unwrap();
        let output = ToolEnvironment::new(temp.path())
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "^needle$".to_owned(),
                path: Some("lines.txt".to_owned()),
                glob: None,
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: None,
            })
            .await
            .unwrap();

        assert_eq!(output.match_count, 2);
        assert_eq!(
            output
                .matches
                .iter()
                .map(|matched| matched.text.as_str())
                .collect::<Vec<_>>(),
            ["needle", "needle"]
        );
    }

    #[tokio::test]
    async fn searches_maximum_sized_file_without_trailing_newline() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("exact.txt"), b"abc").unwrap();
        let limits = ToolLimits {
            max_search_file_bytes: 3,
            max_search_total_bytes: 3,
            ..ToolLimits::default()
        };
        let output = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "a".to_owned(),
                path: Some("exact.txt".to_owned()),
                glob: None,
                literal: true,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: None,
            })
            .await
            .unwrap();

        assert_eq!(output.match_count, 1);
        assert_eq!(output.matches[0].text, "abc");
        assert!(output.warnings.is_empty());
    }

    #[tokio::test]
    async fn reports_top_level_truncation_when_a_line_is_shortened() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("sample.txt"), "needle and more\n").unwrap();
        let limits = ToolLimits {
            max_grep_line_chars: 6,
            ..ToolLimits::default()
        };
        let output = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "needle".to_owned(),
                path: None,
                glob: None,
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: None,
            })
            .await
            .unwrap();

        assert_eq!(output.matches[0].text, "needle");
        assert!(output.matches[0].line_truncated);
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn enforces_global_match_and_pattern_limits() {
        let temp = fixture();
        let limits = ToolLimits {
            max_grep_pattern_bytes: 6,
            ..ToolLimits::default()
        };
        let tool = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .grep();

        let capped = tool
            .call(GrepArgs {
                pattern: "needle".to_owned(),
                path: None,
                glob: Some("**/*.rs".to_owned()),
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: Some(1),
                context: None,
            })
            .await
            .unwrap();
        assert_eq!(capped.matches.len(), 1);
        assert_eq!(capped.match_count, 1);
        assert!(capped.truncated);

        let oversized_pattern = tool
            .call(GrepArgs {
                pattern: "needles".to_owned(),
                path: None,
                glob: None,
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: None,
            })
            .await;
        assert!(matches!(oversized_pattern, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn skips_files_over_the_configured_size() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("large.txt"), "needle").unwrap();
        let limits = ToolLimits {
            max_search_file_bytes: 3,
            ..ToolLimits::default()
        };
        let output = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "needle".to_owned(),
                path: None,
                glob: None,
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: None,
            })
            .await
            .unwrap();

        assert!(output.matches.is_empty());
        assert_eq!(output.oversized_files_skipped, 1);
    }

    #[tokio::test]
    async fn returns_structured_context_without_counting_it_as_matches() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(
            temp.path().join("sample.txt"),
            "before\nneedle\nafter\nfar away\n",
        )
        .unwrap();
        let output = ToolEnvironment::new(temp.path())
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "needle".to_owned(),
                path: Some("sample.txt".to_owned()),
                glob: None,
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: Some(1),
            })
            .await
            .unwrap();

        assert_eq!(output.match_count, 1);
        assert_eq!(
            output
                .matches
                .iter()
                .map(|line| (line.line_number, line.kind.as_str(), line.text.as_str()))
                .collect::<Vec<_>>(),
            [
                (1, "before_context", "before"),
                (2, "match", "needle"),
                (3, "after_context", "after"),
            ]
        );
    }

    #[tokio::test]
    async fn scans_past_match_limit_and_rolls_back_a_late_binary_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut binary = b"needle early\nneedle over limit\n".to_vec();
        for _ in 0..40_000 {
            binary.extend_from_slice(b"padding\n");
        }
        binary.extend_from_slice(b"\0late binary marker\n");
        fs::write(temp.path().join("a-binary.dat"), binary).unwrap();

        let tool = ToolEnvironment::new(temp.path()).unwrap().grep();
        let arguments = GrepArgs {
            pattern: "needle".to_owned(),
            path: None,
            glob: None,
            literal: false,
            case_insensitive: false,
            include_hidden: false,
            limit: Some(1),
            context: None,
        };
        // With only the binary file, the NUL must be discovered even after
        // reaching the match limit, and earlier matches must be rolled back.
        let binary_only = tool.call(arguments.clone()).await.unwrap();
        assert_eq!(binary_only.binary_files_skipped, 1);
        assert_eq!(binary_only.match_count, 0);
        assert!(binary_only.matches.is_empty());

        // Permit both files to be visited in either filesystem order. Binary
        // matches must not consume the quota for the valid text file.
        fs::write(temp.path().join("z-text.txt"), "needle text\n").unwrap();
        let output = tool
            .call(GrepArgs {
                limit: Some(2),
                ..arguments
            })
            .await
            .unwrap();

        assert_eq!(output.binary_files_skipped, 1);
        assert_eq!(output.match_count, 1);
        assert_eq!(output.matches.len(), 1);
        assert_eq!(output.matches[0].path, "z-text.txt");
        assert_eq!(output.matches[0].kind, "match");
    }

    #[tokio::test]
    async fn output_budget_never_leaves_context_without_a_match() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("s"), "b\nneedle-long\na\n").unwrap();

        let partial_limits = ToolLimits {
            max_output_bytes: 20,
            ..ToolLimits::default()
        };
        let partial = ToolEnvironment::with_limits(temp.path(), partial_limits)
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "needle".to_owned(),
                path: Some("s".to_owned()),
                glob: None,
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: Some(1),
            })
            .await
            .unwrap();

        assert_eq!(partial.match_count, 1);
        assert_eq!(partial.matches.len(), 1);
        assert_eq!(partial.matches[0].kind, "match");
        assert!(partial.matches[0].line_truncated);
        assert!(partial.truncated);

        let empty_limits = ToolLimits {
            max_output_bytes: 9,
            ..ToolLimits::default()
        };
        let empty = ToolEnvironment::with_limits(temp.path(), empty_limits)
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "needle".to_owned(),
                path: Some("s".to_owned()),
                glob: None,
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: Some(1),
            })
            .await
            .unwrap();

        assert_eq!(empty.match_count, 0);
        assert!(empty.matches.is_empty());
        assert!(empty.truncated);
    }

    #[tokio::test]
    async fn stops_before_exceeding_the_total_search_byte_limit() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("a.txt"), "needle\n").unwrap();
        fs::write(temp.path().join("b.txt"), "needle\n").unwrap();
        let limits = ToolLimits {
            max_search_total_bytes: 7,
            ..ToolLimits::default()
        };
        let output = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .grep()
            .call(GrepArgs {
                pattern: "needle".to_owned(),
                path: None,
                glob: None,
                literal: false,
                case_insensitive: false,
                include_hidden: false,
                limit: None,
                context: None,
            })
            .await
            .unwrap();

        assert_eq!(output.searched_files, 1);
        assert_eq!(output.searched_bytes, 7);
        assert_eq!(output.match_count, 1);
        assert!(matches!(output.matches[0].path.as_str(), "a.txt" | "b.txt"));
        assert!(output.truncated);
    }
}
