use crate::{Tool, ToolFailure};
use bone_llm::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};

use crate::{ToolEnvironment, ToolError};

/// Arguments accepted by [`ReadTool`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReadArgs {
    /// Workspace-relative path, or an absolute path inside the workspace.
    pub path: String,
    /// First line to return, using one-based indexing.
    #[serde(default)]
    pub offset: Option<usize>,
    /// Maximum number of lines to return.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Structured, paginated text returned by [`ReadTool`].
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ReadOutput {
    pub path: String,
    pub start_line: usize,
    pub end_line: Option<usize>,
    pub content: String,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    pub line_truncated: bool,
}

/// Read a UTF-8 text file with line numbers and bounded output.
#[derive(Clone, Debug)]
pub struct ReadTool {
    environment: ToolEnvironment,
}

impl ReadTool {
    pub(crate) fn new(environment: ToolEnvironment) -> Self {
        Self { environment }
    }
}

impl Tool for ReadTool {
    type Args = ReadArgs;
    type Output = ReadOutput;
    type Error = ToolError;

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "read",
            format!(
                "Read a UTF-8 text file inside the workspace with line numbers. Files are limited to {} bytes and output to {} lines or {} bytes; use offset and limit to continue. A truncated long-line tail is omitted.",
                self.environment.limits.max_read_file_bytes,
                self.environment.limits.max_read_lines,
                self.environment.limits.max_output_bytes
            ),
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path relative to the workspace, or an absolute path inside it"
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "First line to return, using one-based indexing"
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": self.environment.limits.max_read_lines,
                        "description": "Maximum number of lines to return"
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn map_error(&self, error: Self::Error) -> ToolFailure {
        error.into_tool_failure()
    }

    async fn call(&self, arguments: Self::Args) -> Result<Self::Output, Self::Error> {
        let offset = arguments.offset.unwrap_or(1);
        if offset == 0 {
            return Err(ToolError::InvalidArgs(
                "offset must be greater than zero".to_owned(),
            ));
        }
        let limit = arguments
            .limit
            .unwrap_or(self.environment.limits.max_read_lines);
        if limit == 0 || limit > self.environment.limits.max_read_lines {
            return Err(ToolError::InvalidArgs(format!(
                "limit must be between 1 and {}",
                self.environment.limits.max_read_lines
            )));
        }

        let resolved = self
            .environment
            .workspace
            .resolve_existing(&arguments.path)
            .await?;
        let metadata = tokio::fs::metadata(&resolved.absolute)
            .await
            .map_err(|error| ToolError::io_display("inspect file", &resolved.display, error))?;
        if !metadata.is_file() {
            return Err(ToolError::InvalidArgs(format!(
                "path is not a file: {}",
                resolved.display
            )));
        }
        if metadata.len() > self.environment.limits.max_read_file_bytes {
            return Err(ToolError::InvalidArgs(format!(
                "{} is {} bytes; maximum readable file size is {} bytes",
                resolved.display,
                metadata.len(),
                self.environment.limits.max_read_file_bytes
            )));
        }
        let file = tokio::fs::File::open(&resolved.absolute)
            .await
            .map_err(|error| ToolError::io_display("open file", &resolved.display, error))?;
        let mut reader = BufReader::new(file);
        let mut line_number = 0usize;
        let mut scan_remaining = self.environment.limits.max_read_file_bytes;

        while line_number + 1 < offset {
            let line = read_line_capped(&mut reader, 0, &mut scan_remaining)
                .await
                .map_err(|error| ToolError::io_display("read file", &resolved.display, error))?;
            if line.is_none() {
                return Err(ToolError::InvalidArgs(format!(
                    "offset {offset} is beyond the end of {}",
                    resolved.display
                )));
            }
            line_number += 1;
        }

        let mut rendered = Vec::new();
        let mut output_bytes = 0usize;
        let mut next_offset = None;
        let mut line_truncated = false;
        let mut last_rendered_line = None;

        while rendered.len() < limit {
            let next_line_number = line_number + 1;
            let prefix = format!("{next_line_number:>6}\t");
            let separator_bytes = usize::from(!rendered.is_empty());
            if output_bytes + separator_bytes + prefix.len()
                > self.environment.limits.max_output_bytes
            {
                return Err(ToolError::InvalidArgs(format!(
                    "max_output_bytes ({}) is too small to render the line-number prefix for line {next_line_number}",
                    self.environment.limits.max_output_bytes
                )));
            }
            let available = self
                .environment
                .limits
                .max_output_bytes
                .saturating_sub(output_bytes + separator_bytes + prefix.len());
            let Some(line) = read_line_capped(
                &mut reader,
                available.saturating_add(2),
                &mut scan_remaining,
            )
            .await
            .map_err(|error| ToolError::io_display("read file", &resolved.display, error))?
            else {
                if rendered.is_empty() && offset > 1 {
                    return Err(ToolError::InvalidArgs(format!(
                        "offset {offset} is beyond the end of {}",
                        resolved.display
                    )));
                }
                break;
            };
            line_number = next_line_number;
            let bytes = strip_line_terminator(&line.bytes);
            let text = decode_utf8_prefix(bytes, line.capped).map_err(|error| {
                ToolError::io_display("decode UTF-8 file", &resolved.display, error)
            })?;

            if line.capped || text.len() > available {
                let partial = utf8_prefix(text, available);
                rendered.push(format!("{prefix}{partial}"));
                last_rendered_line = Some(line_number);
                line_truncated = true;
                if !reader
                    .fill_buf()
                    .await
                    .map_err(|error| ToolError::io_display("read file", &resolved.display, error))?
                    .is_empty()
                {
                    next_offset = Some(line_number + 1);
                }
                break;
            }

            rendered.push(format!("{prefix}{text}"));
            output_bytes += separator_bytes + prefix.len() + text.len();
            last_rendered_line = Some(line_number);
        }

        if next_offset.is_none()
            && rendered.len() == limit
            && !reader
                .fill_buf()
                .await
                .map_err(|error| ToolError::io_display("read file", &resolved.display, error))?
                .is_empty()
        {
            next_offset = Some(line_number + 1);
        }

        Ok(ReadOutput {
            path: resolved.display,
            start_line: offset,
            end_line: last_rendered_line,
            content: rendered.join("\n"),
            truncated: next_offset.is_some() || line_truncated,
            next_offset,
            line_truncated,
        })
    }
}

#[derive(Debug)]
struct CappedLine {
    bytes: Vec<u8>,
    capped: bool,
}

/// Consume one complete line while retaining at most `capacity` bytes.
async fn read_line_capped<R>(
    reader: &mut R,
    capacity: usize,
    scan_remaining: &mut u64,
) -> io::Result<Option<CappedLine>>
where
    R: AsyncBufRead + Unpin,
{
    let mut bytes = Vec::with_capacity(capacity.min(8 * 1024));
    let mut capped = false;
    let mut saw_input = false;
    let mut utf8_tail = Vec::with_capacity(3);

    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            if !utf8_tail.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "file ended with an incomplete UTF-8 sequence",
                ));
            }
            return if saw_input {
                Ok(Some(CappedLine { bytes, capped }))
            } else {
                Ok(None)
            };
        }
        saw_input = true;

        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buffer.len(), |index| index + 1);
        validate_utf8_chunk(&mut utf8_tail, &buffer[..consumed])?;
        let consumed_u64 = u64::try_from(consumed).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "read scan byte count overflowed",
            )
        })?;
        if consumed_u64 > *scan_remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "read scan exceeded max_read_file_bytes",
            ));
        }
        *scan_remaining -= consumed_u64;
        let remaining = capacity.saturating_sub(bytes.len());
        let retained = remaining.min(consumed);
        bytes.extend_from_slice(&buffer[..retained]);
        capped |= retained < consumed;
        reader.consume(consumed);

        if newline.is_some() {
            debug_assert!(utf8_tail.is_empty());
            return Ok(Some(CappedLine { bytes, capped }));
        }
    }
}

fn validate_utf8_chunk(tail: &mut Vec<u8>, bytes: &[u8]) -> io::Result<()> {
    let mut combined = Vec::with_capacity(tail.len() + bytes.len());
    combined.extend_from_slice(tail);
    combined.extend_from_slice(bytes);
    match std::str::from_utf8(&combined) {
        Ok(_) => {
            tail.clear();
            Ok(())
        }
        Err(error) if error.error_len().is_none() => {
            tail.clear();
            tail.extend_from_slice(&combined[error.valid_up_to()..]);
            Ok(())
        }
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    }
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

fn decode_utf8_prefix(bytes: &[u8], capped: bool) -> io::Result<&str> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(error) if capped && error.error_len().is_none() => {
            Ok(std::str::from_utf8(&bytes[..error.valid_up_to()])
                .expect("valid_up_to always identifies a UTF-8 prefix"))
        }
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    }
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

#[cfg(test)]
mod tests {
    use crate::Tool;

    use super::*;

    #[tokio::test]
    async fn reads_numbered_pages_and_reports_continuation() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("sample.txt"), "one\ntwo\nthree\n")
            .await
            .unwrap();
        let tool = ToolEnvironment::new(temp.path()).unwrap().read();

        let output = tool
            .call(ReadArgs {
                path: "sample.txt".to_owned(),
                offset: Some(2),
                limit: Some(1),
            })
            .await
            .unwrap();

        assert_eq!(output.content, "     2\ttwo");
        assert_eq!(output.next_offset, Some(3));
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn rejects_paths_outside_the_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let parent_file = tempfile::NamedTempFile::new().unwrap();
        tokio::fs::write(parent_file.path(), "secret")
            .await
            .unwrap();
        let tool = ToolEnvironment::new(temp.path()).unwrap().read();

        let result = tool
            .call(ReadArgs {
                path: parent_file.path().display().to_string(),
                offset: None,
                limit: None,
            })
            .await;

        assert!(matches!(result, Err(ToolError::OutsideWorkspace { .. })));
    }

    #[tokio::test]
    async fn missing_relative_paths_do_not_expose_the_workspace_root() {
        let temp = tempfile::tempdir().unwrap();
        let tool = ToolEnvironment::new(temp.path()).unwrap().read();

        let error = tool
            .call(ReadArgs {
                path: "missing.txt".to_owned(),
                offset: None,
                limit: None,
            })
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("missing.txt"));
        assert!(!error.contains(&temp.path().display().to_string()));
    }

    #[tokio::test]
    async fn bounds_a_single_very_long_line() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(
            temp.path().join("long.txt"),
            format!("{}\nnext\n", "x".repeat(10_000)),
        )
        .await
        .unwrap();
        let limits = crate::ToolLimits {
            max_output_bytes: 64,
            ..crate::ToolLimits::default()
        };
        let tool = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .read();

        let output = tool
            .call(ReadArgs {
                path: "long.txt".to_owned(),
                offset: None,
                limit: None,
            })
            .await
            .unwrap();

        assert!(output.line_truncated);
        assert_eq!(output.next_offset, Some(2));
        assert!(output.content.len() <= 64);
    }

    #[tokio::test]
    async fn a_truncated_final_line_does_not_advertise_a_missing_page() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("long.txt"), "x".repeat(10_000))
            .await
            .unwrap();
        let limits = crate::ToolLimits {
            max_output_bytes: 64,
            ..crate::ToolLimits::default()
        };
        let output = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .read()
            .call(ReadArgs {
                path: "long.txt".to_owned(),
                offset: None,
                limit: None,
            })
            .await
            .unwrap();

        assert!(output.line_truncated);
        assert!(output.truncated);
        assert_eq!(output.next_offset, None);
    }

    #[tokio::test]
    async fn rejects_offsets_beyond_the_last_line() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("sample.txt"), "one\ntwo\n")
            .await
            .unwrap();
        let tool = ToolEnvironment::new(temp.path()).unwrap().read();

        let result = tool
            .call(ReadArgs {
                path: "sample.txt".to_owned(),
                offset: Some(3),
                limit: None,
            })
            .await;

        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn rejects_files_over_the_read_limit() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("large.txt"), "too large")
            .await
            .unwrap();
        let limits = crate::ToolLimits {
            max_read_file_bytes: 3,
            ..crate::ToolLimits::default()
        };
        let tool = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .read();

        let result = tool
            .call(ReadArgs {
                path: "large.txt".to_owned(),
                offset: None,
                limit: None,
            })
            .await;

        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn errors_instead_of_returning_a_non_advancing_cursor() {
        let temp = tempfile::tempdir().unwrap();
        tokio::fs::write(temp.path().join("sample.txt"), "one\n")
            .await
            .unwrap();
        let limits = crate::ToolLimits {
            max_output_bytes: 1,
            ..crate::ToolLimits::default()
        };
        let tool = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .read();

        let result = tool
            .call(ReadArgs {
                path: "sample.txt".to_owned(),
                offset: None,
                limit: None,
            })
            .await;

        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn rejects_invalid_utf8_hidden_beyond_a_truncated_prefix() {
        let temp = tempfile::tempdir().unwrap();
        let mut content = vec![b'x'; 1_000];
        content.push(0xff);
        content.push(b'\n');
        tokio::fs::write(temp.path().join("invalid.txt"), content)
            .await
            .unwrap();
        let limits = crate::ToolLimits {
            max_output_bytes: 64,
            ..crate::ToolLimits::default()
        };
        let tool = ToolEnvironment::with_limits(temp.path(), limits)
            .unwrap()
            .read();

        let result = tool
            .call(ReadArgs {
                path: "invalid.txt".to_owned(),
                offset: None,
                limit: None,
            })
            .await;

        assert!(matches!(result, Err(ToolError::Io { .. })));
    }
}
