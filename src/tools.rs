use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rig_core::completion::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const OUTPUT_LIMIT: usize = 32 * 1024;
const DEFAULT_READ_LIMIT: usize = 8 * 1024;
const FILE_LIMIT: u64 = 16 * 1024 * 1024;
const DEFAULT_SHELL_TIMEOUT_SECONDS: u64 = 60;
const MAX_SHELL_TIMEOUT_SECONDS: u64 = 3600;

pub fn definitions(single_job: bool, read_only: bool) -> Vec<ToolDefinition> {
    let mut tools = vec![
        definition(
            "read_file",
            "Read a bounded page of a workspace UTF-8 file. Default page is 8192 bytes; an explicit limit can request up to 32768 bytes. Follow next_offset with another read_file call until the needed code is available; a page is not the complete file. Returns the entire file's SHA-256 for safe replacement. offset and limit are bytes.",
            json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"default":DEFAULT_READ_LIMIT,"maximum":OUTPUT_LIMIT}}),
            &["path"],
        ),
        definition(
            "search_files",
            "Search exact nonempty text in a workspace UTF-8 file or directory tree. Default limit 50, maximum 200. Results are ordered by relative path then UTF-8 byte offset; follow next_cursor for remaining matches. Symlinks, .git/target/node_modules/.bone directories, binary/non-UTF8 and files over 16 MiB are skipped. Snippets contain at most 1024 characters; queries longer than that may have their tail omitted. Cursor verifies request, exclusions, source hash and position; concurrent tree mutation is not snapshot-isolated.",
            json!({"query":{"type":"string","minLength":1},"path":{"type":"string","default":"."},"limit":{"type":"integer","minimum":1,"maximum":200,"default":50},"cursor":{"type":"string"}}),
            &["query"],
        ),
        definition(
            "list_files",
            "List files in a workspace directory (no recursive traversal).",
            json!({"path":{"type":"string"}}),
            &[],
        ),
        definition(
            "job_inspect",
            "Recover original session evidence after summaries. With users_only=true, page all session user inputs using before_id (an input ID) and limit (input count). With event_id, read the event's readable body, including summary and model response text; raw=true returns the original Event JSON instead. offset and limit are characters in event mode; use next_offset to continue. With job_id, page that job's original records using before_id and limit (record count). Results report truncation and next_before_id for record pagination. Omit users_only, job_id and event_id for the job catalog.",
            json!({"job_id":{"type":"string"},"event_id":{"type":"string"},"users_only":{"type":"boolean"},"raw":{"type":"boolean"},"before_id":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1}}),
            &[],
        ),
        definition(
            "ask_user",
            "Ask the user for missing information and suspend this job. Use alone in a tool-call batch.",
            json!({"question":{"type":"string"}}),
            &["question"],
        ),
        definition(
            "input_resolve",
            "Explicitly settle selected QUEUED inputs owned by your current job after considering their original requests. completed means their work is actually done; superseded means a newer instruction replaced that request, not that its work succeeded. Give a concrete nonempty reason and optional existing audit evidence event IDs. When the ACTIVE request incorporates earlier work, resolve those queued inputs only after their requirements are fulfilled or explicitly superseded. Leave independent work queued. Cannot resolve ACTIVE/foreign inputs, unknown writes, unfinished tool batches or live delegated assignments. Does not execute or cancel external work.",
            json!({"resolutions":{"type":"array","minItems":1,"items":{"type":"object","properties":{"input_id":{"type":"string"},"outcome":{"type":"string","enum":["completed","superseded"]},"reason":{"type":"string","minLength":1},"evidence_event_ids":{"type":"array","items":{"type":"string"}}},"required":["input_id","outcome","reason"],"additionalProperties":false}}}),
            &["resolutions"],
        ),
        definition(
            "pause_work",
            "Pause all work in this session when the user asks to stop. Use alone.",
            json!({"reason":{"type":"string"}}),
            &["reason"],
        ),
    ];
    if !read_only {
        tools.push(definition("edit_file", "Apply exact text edits to an existing workspace UTF-8 file (at most 16 MiB) using its whole-file expected_sha256. Each old_text must occur exactly once in the original source, including overlapping occurrences; edit spans cannot overlap. All edits are validated before replacement and do not cascade. Untouched bytes and permissions are preserved. Final identity checks narrow external-writer races but are not atomic CAS.", json!({"path":{"type":"string"},"expected_sha256":{"type":"string"},"edits":{"type":"array","minItems":1,"items":{"type":"object","properties":{"old_text":{"type":"string","minLength":1},"new_text":{"type":"string"}},"required":["old_text","new_text"],"additionalProperties":false}}}), &["path","expected_sha256","edits"]));
        tools.push(definition("write_file", "Replace a workspace file using expected_sha256 from read_file. Content and file identity are checked again immediately before replacement; an external writer racing after that check can still change it. For a NEW file pass expected_sha256=null; creation never overwrites an existing target. Existing files are limited to 16 MiB. All contents must be provided.", json!({"path":{"type":"string"},"content":{"type":"string"},"expected_sha256":{"type":["string","null"]}}), &["path","content","expected_sha256"]));
        tools.push(definition("shell", "Run a shell command in the workspace with the user's local privileges. Treat as a write operation. Default timeout is 60 seconds; choose a longer timeout up to 3600 seconds for builds or integration tests. The session's overall deadline still applies. Output is bounded; timed-out commands require reconciliation. Prefer read_file/list_files for inspection.", json!({"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":MAX_SHELL_TIMEOUT_SECONDS,"default":DEFAULT_SHELL_TIMEOUT_SECONDS}}), &["command"]));
    }
    if !single_job {
        tools.extend([
            definition("job_send", "Send a bounded assignment to another internal job. Omit job_id to create one; otherwise continue an existing job. Returns input_id; wait on that exact input ID. Every created job shares this input's budget.", json!({"job_id":{"type":"string"},"title":{"type":"string"},"message":{"type":"string"}}), &["message"]),
            definition("job_wait", "Suspend until the specified input IDs each receive a delivery, failure, or input_resolved with its explicit completed/superseded outcome. Use alone; no polling and no model slot is occupied while waiting.", json!({"input_ids":{"type":"array","items":{"type":"string"},"minItems":1}}), &["input_ids"]),
            definition("job_handoff", "Transfer the current original input and conversation focus to an idle existing job, or a new job if job_id is omitted. Only before tools have acted on this input; use alone. The target owns its response.", json!({"job_id":{"type":"string"},"title":{"type":"string"}}), &[]),
            definition("job_close", "Close an idle internal job while retaining its records. Current or busy jobs cannot be closed.", json!({"job_id":{"type":"string"}}), &["job_id"]),
            definition("job_control", "Pause another job or explicitly authorize retry of its retained work with ready. Default session resume leaves old work under ended ancestors paused; ready is an explicit Agent action and retains its original input and shared budget. A retried job receives current session instructions.", json!({"job_id":{"type":"string"},"state":{"type":"string","enum":["paused","ready"]}}), &["job_id","state"]),
        ]);
    }
    tools
}

fn definition(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> ToolDefinition {
    ToolDefinition::new(
        name.try_into().expect("static tool name"),
        description,
        json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
    )
}

pub fn is_external(name: &str) -> bool {
    matches!(
        name,
        "read_file" | "list_files" | "search_files" | "write_file" | "edit_file" | "shell"
    )
}

pub fn is_write(name: &str) -> bool {
    matches!(name, "write_file" | "edit_file" | "shell")
}
pub fn is_control(name: &str) -> bool {
    matches!(name, "job_wait" | "job_handoff" | "ask_user" | "pause_work")
}

pub fn string_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string argument: {name}"))
}

/// Resolve through existing ancestors and reject symlink escapes. Shell execution
/// is deliberately separate: it runs with local user privileges, not a sandbox.
fn workspace_path(workspace: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !path.is_absolute(),
        "path must be relative to the workspace"
    );
    ensure!(
        !path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_))),
        "parent traversal is not allowed"
    );
    let root = workspace.canonicalize()?;
    let target = root.join(path);
    let mut ancestor = target.as_path();
    while !ancestor.exists() {
        // A dangling symlink is not an absent path suitable for creation.
        ensure!(
            std::fs::symlink_metadata(ancestor).is_err(),
            "dangling symlink is not allowed"
        );
        ancestor = ancestor.parent().context("path has no existing ancestor")?;
    }
    ensure!(
        ancestor.canonicalize()?.starts_with(&root),
        "path escapes the workspace through a symlink"
    );
    Ok(target)
}

pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub struct ToolOutcome {
    pub content: Value,
    pub uncertain: bool,
}

pub async fn execute(
    workspace: &Path,
    name: &str,
    args: &Value,
    write_leases: Option<[std::sync::Arc<std::fs::File>; 2]>,
) -> ToolOutcome {
    if name == "shell" {
        return match shell(workspace, args, write_leases.as_ref()).await {
            Ok(outcome) => outcome,
            Err(error) => ToolOutcome {
                content: json!({"error":format!("{error:#}")}),
                uncertain: false,
            },
        };
    }
    if name == "edit_file" {
        return edit_file_prepared(
            workspace,
            args,
            |parent| std::fs::File::open(parent)?.sync_all(),
            |_| Ok(()),
        );
    }
    if name == "write_file" {
        return write_file(workspace, args, |parent| {
            std::fs::File::open(parent)?.sync_all()
        });
    }
    let result = file_tool(workspace, name, args);
    ToolOutcome {
        content: result.unwrap_or_else(|error| json!({"error":format!("{error:#}")})),
        uncertain: false,
    }
}

fn file_tool(workspace: &Path, name: &str, args: &Value) -> Result<Value> {
    if name == "search_files" {
        return search_files(workspace, args);
    }
    let path = workspace_path(
        workspace,
        args.get("path").and_then(Value::as_str).unwrap_or("."),
    )?;
    match name {
        "read_file" => {
            ensure!(path.is_file(), "path is not a regular file");
            ensure!(
                std::fs::metadata(&path)?.len() <= FILE_LIMIT,
                "file exceeds 16 MiB; use a bounded shell command"
            );
            let mut bytes = Vec::new();
            use std::io::Read;
            std::fs::File::open(&path)?
                .take(FILE_LIMIT + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 <= FILE_LIMIT,
                "file exceeds 16 MiB; use a bounded shell command"
            );
            let text = std::str::from_utf8(&bytes).context("file is not UTF-8")?;
            let mut start =
                (args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize).min(text.len());
            while !text.is_char_boundary(start) {
                start += 1;
            }
            let limit = (args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_READ_LIMIT as u64) as usize)
                .min(OUTPUT_LIMIT);
            let mut end = start.saturating_add(limit).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            Ok(
                json!({"text":&text[start..end],"sha256":sha256(&bytes),"bytes":bytes.len(),"next_offset":if end<bytes.len(){Some(end)}else{None}}),
            )
        }
        "list_files" => {
            let mut entries = std::fs::read_dir(path)?
                .take(1001)
                .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
                .collect::<std::io::Result<Vec<_>>>()?;
            entries.sort();
            let truncated = entries.len() > 1000;
            entries.truncate(1000);
            Ok(json!({"entries":entries,"truncated":truncated}))
        }
        _ => bail!("unknown external tool: {name}"),
    }
}

const SEARCH_EXCLUSIONS: [&str; 4] = [".git", "target", "node_modules", ".bone"];

/// Stricter than legacy read/write paths: these tools never follow any symlink,
/// including one pointing inside the workspace. Recheck before installation.
fn source_path(workspace: &Path, relative: &str) -> Result<(PathBuf, String)> {
    let path = Path::new(relative);
    ensure!(
        !path.is_absolute(),
        "path must be relative to the workspace"
    );
    let root = workspace.canonicalize()?;
    let mut target = root.clone();
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                let part = part.to_str().context("path is not UTF-8")?;
                parts.push(part);
                target.push(part);
                let metadata =
                    std::fs::symlink_metadata(&target).context("source path is missing")?;
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "symlink paths are not allowed"
                );
            }
            _ => bail!("parent traversal and absolute paths are not allowed"),
        }
    }
    Ok((
        target,
        if parts.is_empty() {
            ".".to_owned()
        } else {
            parts.join("/")
        },
    ))
}

fn source_bytes(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "source is not a regular file"
    );
    ensure!(metadata.len() <= FILE_LIMIT, "file exceeds 16 MiB");
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    ensure!(file.metadata()?.is_file(), "source is not a regular file");
    let mut bytes = Vec::new();
    file.take(FILE_LIMIT + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= FILE_LIMIT, "file exceeds 16 MiB");
    Ok(bytes)
}

// Cursor is a consistency token, not an authorization token: its checksum is
// public and a known algorithm can construct a valid position. Every decoded
// request/path/version/occurrence is still revalidated before reading results.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchCursor {
    version: u8,
    request: String,
    path: String,
    sha256: String,
    byte_offset: usize,
}
fn cursor_encode(cursor: &SearchCursor) -> Result<String> {
    let bytes = serde_json::to_vec(cursor)?;
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("{hex}.{}", sha256(&bytes)))
}
fn cursor_decode(text: &str) -> Result<SearchCursor> {
    ensure!(
        text.len() <= OUTPUT_LIMIT,
        "invalid search cursor: too long"
    );
    let (hex, checksum) = text.split_once('.').context("invalid search cursor")?;
    ensure!(
        hex.len().is_multiple_of(2),
        "invalid search cursor encoding"
    );
    let bytes = hex
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair)?;
            Ok(u8::from_str_radix(pair, 16)?)
        })
        .collect::<Result<Vec<_>>>()
        .context("invalid search cursor encoding")?;
    ensure!(sha256(&bytes) == checksum, "invalid search cursor checksum");
    let cursor: SearchCursor =
        serde_json::from_slice(&bytes).context("invalid search cursor data")?;
    ensure!(cursor.version == 1, "invalid search cursor version");
    Ok(cursor)
}

fn search_paths(
    root: &Path,
    target: &Path,
    paths: &mut Vec<PathBuf>,
    skipped: &mut usize,
    skipped_directories: &mut usize,
) -> Result<()> {
    let metadata = std::fs::symlink_metadata(target)?;
    if metadata.file_type().is_symlink() {
        *skipped += 1;
        return Ok(());
    }
    if metadata.is_dir() {
        if target != root
            && target
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| SEARCH_EXCLUSIONS.contains(&name))
        {
            *skipped_directories += 1;
            return Ok(());
        }
        for entry in std::fs::read_dir(target)? {
            search_paths(root, &entry?.path(), paths, skipped, skipped_directories)?;
        }
    } else if metadata.is_file() {
        if target.strip_prefix(root)?.to_str().is_some() {
            paths.push(target.to_owned());
        } else {
            *skipped += 1;
        }
    } else {
        *skipped += 1;
    }
    Ok(())
}

// A preview belongs to the matched line(s), not unrelated following lines.
// Reserve enough of the scalar budget for the full query when it fits; left
// context never pushes its end outside the 1024-character preview.
fn search_snippet(text: &str, offset: usize, query: &str) -> String {
    let query_end = offset + query.len();
    let left_budget = 1024_usize.saturating_sub(query.chars().count()).min(128);
    let start = text[..offset]
        .char_indices()
        .rev()
        .take(left_budget)
        .take_while(|(_, character)| *character != '\n')
        .last()
        .map_or(offset, |(start, _)| start);
    let right_limit = if query.ends_with('\n') { 0 } else { 1024 };
    text[start..query_end]
        .chars()
        .chain(
            text[query_end..]
                .chars()
                .take_while(|character| *character != '\n')
                .take(right_limit),
        )
        .take(1024)
        .collect()
}

fn search_files(workspace: &Path, args: &Value) -> Result<Value> {
    let query = string_arg(args, "query")?;
    ensure!(!query.is_empty(), "query cannot be empty");
    let limit = args
        .get("limit")
        .map(|value| value.as_u64().context("limit must be an integer"))
        .transpose()?
        .unwrap_or(50);
    ensure!(
        (1..=200).contains(&limit),
        "limit must be between 1 and 200"
    );
    let relative = args
        .get("path")
        .map(|value| value.as_str().context("path must be a string"))
        .transpose()?
        .unwrap_or(".");
    let (target, normalized) = source_path(workspace, relative)?;
    let root = workspace.canonicalize()?;
    let components: Vec<_> = normalized.split('/').collect();
    let excluded = components.iter().enumerate().any(|(index, name)| {
        SEARCH_EXCLUSIONS.contains(name) && (index + 1 < components.len() || target.is_dir())
    });
    ensure!(
        !excluded || args.get("cursor").is_none(),
        "search cursor points inside an excluded directory"
    );
    if excluded {
        return Ok(
            json!({"matches":[],"next_cursor":null,"truncated":false,"scanned_files":0,"skipped_files":0,"skipped_directories":1}),
        );
    }
    let request = sha256(&serde_json::to_vec(
        &json!({"query":query,"path":normalized,"exclusions":SEARCH_EXCLUSIONS}),
    )?);
    let cursor = args
        .get("cursor")
        .map(|value| cursor_decode(value.as_str().context("cursor must be a string")?))
        .transpose()?;
    if let Some(cursor) = &cursor {
        ensure!(
            cursor.request == request,
            "search cursor does not match query, path or exclusions"
        );
        let (reference, reference_path) = source_path(workspace, &cursor.path)
            .context("search cursor reference is missing or retyped")?;
        ensure!(
            reference_path == cursor.path
                && (reference == target || reference.starts_with(&target)),
            "search cursor is outside the requested path"
        );
        ensure!(!Path::new(&cursor.path).parent().unwrap_or(Path::new("")).components().any(|part| matches!(part,Component::Normal(name) if SEARCH_EXCLUSIONS.iter().any(|skip| name==*skip))), "search cursor points inside an excluded directory");
        let bytes =
            source_bytes(&reference).context("search cursor reference is missing or retyped")?;
        ensure!(
            sha256(&bytes) == cursor.sha256,
            "search cursor reference file changed"
        );
        ensure!(!bytes.contains(&0), "search cursor reference is binary");
        let text =
            std::str::from_utf8(&bytes).context("search cursor reference is no longer UTF-8")?;
        ensure!(
            text.match_indices(query)
                .any(|(offset, _)| offset == cursor.byte_offset),
            "invalid search cursor occurrence position"
        );
    }
    let mut paths = Vec::new();
    let mut skipped = 0;
    let mut skipped_directories = 0;
    search_paths(
        &root,
        &target,
        &mut paths,
        &mut skipped,
        &mut skipped_directories,
    )?;
    paths.sort();
    let mut scanned = 0;
    let mut matches = Vec::new();
    let mut last: Option<SearchCursor> = None;
    let mut has_next = false;
    'files: for path in paths {
        let relative = path
            .strip_prefix(&root)?
            .to_str()
            .context("file path is not UTF-8")?
            .replace(std::path::MAIN_SEPARATOR, "/");
        if cursor.as_ref().is_some_and(|cursor| relative < cursor.path) {
            continue;
        }
        // Symlink/type/size checks are repeated because the tree may have changed.
        let bytes =
            match source_path(workspace, &relative).and_then(|(path, _)| source_bytes(&path)) {
                Ok(bytes) => bytes,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) if !bytes.contains(&0) => text,
            _ => {
                skipped += 1;
                continue;
            }
        };
        scanned += 1;
        let hash = sha256(&bytes);
        ensure!(
            !cursor
                .as_ref()
                .is_some_and(|cursor| relative == cursor.path && hash != cursor.sha256),
            "search cursor reference file changed during traversal"
        );
        let mut line = 1;
        let mut line_offset = 0;
        for (offset, _) in text.match_indices(query) {
            line += text.as_bytes()[line_offset..offset]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count();
            line_offset = offset;
            if cursor
                .as_ref()
                .is_some_and(|cursor| relative == cursor.path && offset <= cursor.byte_offset)
            {
                continue;
            }
            if matches.len() >= limit as usize {
                has_next = true;
                break 'files;
            }
            let record = json!({"path":relative,"line":line,"byte_offset":offset,"snippet":search_snippet(text, offset, query),"sha256":hash});
            let next = SearchCursor {
                version: 1,
                request: request.clone(),
                path: relative.clone(),
                sha256: hash.clone(),
                byte_offset: offset,
            };
            matches.push(record);
            // Reserve an actual continuation, even for the final page, so a
            // later lookahead never makes an already accepted page too large.
            let projected = json!({"matches":matches,"next_cursor":cursor_encode(&next)?,"truncated":true,"scanned_files":usize::MAX,"skipped_files":usize::MAX,"skipped_directories":usize::MAX});
            if serde_json::to_vec(&projected)?.len() > OUTPUT_LIMIT {
                matches.pop();
                ensure!(
                    !matches.is_empty(),
                    "search match cannot fit the bounded output"
                );
                has_next = true;
                break 'files;
            }
            last = Some(next);
        }
    }
    let next = if has_next {
        Some(cursor_encode(
            last.as_ref()
                .context("search produced an empty continuation")?,
        )?)
    } else {
        None
    };
    let result = json!({"matches":matches,"next_cursor":next,"truncated":has_next,"scanned_files":scanned,"skipped_files":skipped,"skipped_directories":skipped_directories});
    ensure!(
        serde_json::to_vec(&result)?.len() <= OUTPUT_LIMIT,
        "search result exceeds output limit"
    );
    Ok(result)
}

fn edit_file_prepared(
    workspace: &Path,
    args: &Value,
    sync_directory: fn(&Path) -> std::io::Result<()>,
    before_install: impl FnOnce(&Path) -> Result<()>,
) -> ToolOutcome {
    let prepared = (|| -> Result<(Value, usize)> {
        let relative = string_arg(args, "path")?;
        let (path, _) = source_path(workspace, relative)?;
        let bytes = source_bytes(&path)?;
        let source = std::str::from_utf8(&bytes).context("source file is not UTF-8")?;
        let expected = string_arg(args, "expected_sha256")?;
        ensure!(
            sha256(&bytes) == expected,
            "file changed; expected SHA-256 does not match"
        );
        let edits = args["edits"].as_array().context("edits must be an array")?;
        ensure!(!edits.is_empty(), "edits cannot be empty");
        let mut spans = Vec::new();
        for edit in edits {
            let old = string_arg(edit, "old_text")?;
            let new = string_arg(edit, "new_text")?;
            ensure!(!old.is_empty(), "old_text cannot be empty");
            let start = source
                .find(old)
                .context("old_text is absent from the original source")?;
            // Advance one Unicode scalar rather than old.len(): overlapping
            // occurrences ("aa" in "aaa") are ambiguous too.
            let after_start = start + source[start..].chars().next().unwrap().len_utf8();
            ensure!(
                !source[after_start..].contains(old),
                "old_text occurs more than once in the original source"
            );
            spans.push((start, start + old.len(), new));
        }
        spans.sort_by_key(|span| span.0);
        ensure!(
            spans.windows(2).all(|pair| pair[0].1 <= pair[1].0),
            "original edit spans overlap"
        );
        let mut content = String::new();
        let mut offset = 0;
        for (start, end, new) in spans {
            content.push_str(&source[offset..start]);
            content.push_str(new);
            offset = end;
        }
        content.push_str(&source[offset..]);
        Ok((
            json!({"path":relative,"expected_sha256":expected,"content":content}),
            edits.len(),
        ))
    })();
    let (replacement, count) = match prepared {
        Ok(value) => value,
        Err(error) => {
            return ToolOutcome {
                content: json!({"error":format!("{error:#}")}),
                uncertain: false,
            };
        }
    };
    let mut outcome = write_file_prepared(workspace, &replacement, sync_directory, |path| {
        before_install(path)?;
        source_path(workspace, string_arg(args, "path")?)?;
        Ok(())
    });
    if outcome.content.get("error").is_none() {
        outcome.content["edits_applied"] = json!(count);
    }
    outcome
}

#[derive(Debug, PartialEq, Eq)]
struct FileVersion {
    sha256: String,
    bytes: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileVersion {
    fn metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            sha256: String::new(),
            bytes: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

/// A fixed buffer and byte ceiling bound hashing even if a file grows while read.
fn file_version(path: &Path) -> Result<Option<FileVersion>> {
    use std::io::Read;
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    ensure!(metadata.is_file(), "path is not a regular file");
    ensure!(
        metadata.len() <= FILE_LIMIT,
        "file exceeds 16 MiB; use a bounded shell command"
    );
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A target changed into a FIFO between metadata and open must not hang.
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "path is not a regular file");
    let mut version = FileVersion::metadata(&metadata);
    ensure!(
        version.bytes <= FILE_LIMIT,
        "file exceeds 16 MiB; use a bounded shell command"
    );
    let mut reader = file.take(FILE_LIMIT + 1);
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        ensure!(
            bytes <= FILE_LIMIT,
            "file exceeds 16 MiB; use a bounded shell command"
        );
        hash.update(&buffer[..count]);
    }
    ensure!(
        version == FileVersion::metadata(&reader.get_ref().metadata()?),
        "file changed while checking it; read it again before replacing it"
    );
    version.sha256 = format!("{:x}", hash.finalize());
    Ok(Some(version))
}

fn write_file(
    workspace: &Path,
    args: &Value,
    sync_directory: fn(&Path) -> std::io::Result<()>,
) -> ToolOutcome {
    write_file_prepared(workspace, args, sync_directory, |_| Ok(()))
}

fn write_file_prepared(
    workspace: &Path,
    args: &Value,
    sync_directory: fn(&Path) -> std::io::Result<()>,
    before_install: impl FnOnce(&Path) -> Result<()>,
) -> ToolOutcome {
    let mut installed = false;
    let result = (|| -> Result<Value> {
        let path = workspace_path(workspace, string_arg(args, "path")?)?;
        let content = string_arg(args, "content")?;
        let expected = args
            .get("expected_sha256")
            .context("expected_sha256 is required (null for a new file)")?;
        let original = file_version(&path)?;
        if let Some(original) = &original {
            let expected = expected
                .as_str()
                .context("existing file requires its expected SHA-256")?;
            ensure!(
                original.sha256 == expected,
                "file changed; read it again before replacing it"
            );
        } else {
            ensure!(expected.is_null(), "expected file no longer exists");
        }
        let parent = path.parent().context("file needs a parent")?;
        std::fs::create_dir_all(parent)?;
        let _ = workspace_path(workspace, string_arg(args, "path")?)?;
        let canonical_parent = parent.canonicalize()?;
        let temporary = canonical_parent.join(format!(".bone-{}.tmp", uuid::Uuid::new_v4()));
        let replacement = (|| -> Result<()> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            if let Ok(metadata) = std::fs::metadata(&path) {
                file.set_permissions(metadata.permissions())?;
            }
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            before_install(&path)?;
            let _ = workspace_path(workspace, string_arg(args, "path")?)?;
            ensure!(
                parent.canonicalize()? == canonical_parent,
                "file's parent directory changed; inspect it before replacing it"
            );
            if original.is_some() {
                ensure!(
                    file_version(&path)? == original,
                    "file changed during replacement preparation; read it again before replacing it"
                );
                // This narrows the external-writer race; it is not an atomic CAS.
                std::fs::rename(&temporary, &path)?;
                installed = true;
            } else {
                // Both names share one directory/filesystem. Unlike rename,
                // hard_link fails atomically if another writer created the target.
                std::fs::hard_link(&temporary, &path)?;
                installed = true;
                std::fs::remove_file(&temporary)?;
            }
            sync_directory(&canonical_parent)?;
            Ok(())
        })();
        if replacement.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        replacement?;
        Ok(json!({"bytes":content.len(),"sha256":sha256(content.as_bytes())}))
    })();
    match result {
        Ok(content) => ToolOutcome {
            content,
            uncertain: false,
        },
        Err(error) => ToolOutcome {
            content: if installed {
                json!({"error":format!("{error:#}"),"effect":"unknown","instruction":"The target was installed but cleanup or directory durability could not be confirmed. Inspect it before retrying."})
            } else {
                json!({"error":format!("{error:#}")})
            },
            uncertain: installed,
        },
    }
}

async fn drain(mut reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    let mut buf = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buf).await?;
        if count == 0 {
            break;
        }
        let take = count.min(OUTPUT_LIMIT.saturating_sub(out.len()));
        out.extend_from_slice(&buf[..take]);
        truncated |= take < count;
    }
    Ok((out, truncated))
}

struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        // SAFETY: the spawned process has its own process group, whose ID is its PID.
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}

fn shell_timeout_seconds(args: &Value) -> Result<u64> {
    let Some(value) = args.get("timeout_seconds") else {
        return Ok(DEFAULT_SHELL_TIMEOUT_SECONDS);
    };
    let seconds = value
        .as_u64()
        .context("timeout_seconds must be an integer")?;
    ensure!(
        (1..=MAX_SHELL_TIMEOUT_SECONDS).contains(&seconds),
        "timeout_seconds must be between 1 and {MAX_SHELL_TIMEOUT_SECONDS}"
    );
    Ok(seconds)
}

async fn shell(
    workspace: &Path,
    args: &Value,
    write_leases: Option<&[std::sync::Arc<std::fs::File>; 2]>,
) -> Result<ToolOutcome> {
    let command = string_arg(args, "command")?;
    let seconds = shell_timeout_seconds(args)?;
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        cmd.process_group(0);
        if let Some(leases) = write_leases {
            let fds = [leases[0].as_raw_fd(), leases[1].as_raw_fd()];
            // SAFETY: the lease lives through spawn. fcntl is async-signal-safe;
            // this hook runs only in the fork child and alters only its fd flags.
            // Parent flags stay CLOEXEC. Ordinary shell descendants retain the
            // physical stable AND legacy workspace leases if BONE is killed
            // before they stop. Older executables observe only the legacy lock.
            unsafe {
                cmd.pre_exec(move || {
                    for fd in fds {
                        let flags = libc::fcntl(fd, libc::F_GETFD);
                        if flags < 0
                            || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
        }
    }
    let mut child = cmd.spawn()?;
    let _group = ProcessGroup(child.id().context("shell did not start")?);
    let stdout = child.stdout.take().context("missing stdout")?;
    let stderr = child.stderr.take().context("missing stderr")?;
    let wait = async {
        let (status, out, err) = tokio::try_join!(child.wait(), drain(stdout), drain(stderr))?;
        Ok::<_, std::io::Error>((status, out, err))
    };
    match tokio::time::timeout(Duration::from_secs(seconds), wait).await {
        Ok(Ok((status, out, err))) => Ok(ToolOutcome {
            content: json!({"exit_code":status.code(),"stdout":String::from_utf8_lossy(&out.0),"stderr":String::from_utf8_lossy(&err.0),"truncated":out.1||err.1}),
            uncertain: false,
        }),
        Ok(Err(error)) => Ok(ToolOutcome {
            content: json!({"error":error.to_string(),"effect":"unknown"}),
            uncertain: true,
        }),
        Err(_) => Ok(ToolOutcome {
            content: json!({"error":"shell timed out; inspect effects before resuming writes","effect":"unknown"}),
            uncertain: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shell_timeout_accepts_long_builds_and_rejects_invalid_explicit_values() {
        assert_eq!(shell_timeout_seconds(&json!({})).unwrap(), 60);
        for seconds in [1, 60, 900, 3600] {
            assert_eq!(
                shell_timeout_seconds(&json!({"timeout_seconds":seconds})).unwrap(),
                seconds
            );
        }
        for value in [
            json!(0),
            json!(-1),
            json!(3601),
            json!(1.5),
            json!("900"),
            json!(true),
            Value::Null,
        ] {
            let error = shell_timeout_seconds(&json!({"timeout_seconds":value})).unwrap_err();
            assert!(error.to_string().contains("timeout_seconds"));
        }
    }

    #[test]
    fn shell_timeout_schema_matches_runtime_limits() {
        let tool = definitions(true, false)
            .into_iter()
            .find(|tool| tool.name.as_str() == "shell")
            .unwrap();
        let schema = &tool.parameters["properties"]["timeout_seconds"];
        assert_eq!(schema["type"], "integer");
        assert_eq!(schema["minimum"], 1);
        assert_eq!(schema["maximum"], MAX_SHELL_TIMEOUT_SECONDS);
        assert_eq!(schema["default"], DEFAULT_SHELL_TIMEOUT_SECONDS);
        assert_eq!(
            shell_timeout_seconds(&json!({"timeout_seconds":schema["maximum"]})).unwrap(),
            MAX_SHELL_TIMEOUT_SECONDS
        );
    }

    #[tokio::test]
    async fn replacements_require_matching_content() {
        let dir = tempfile::tempdir().unwrap();
        let args = json!({"path":"x","content":"one","expected_sha256":null});
        assert!(
            execute(dir.path(), "write_file", &args, None)
                .await
                .content
                .get("error")
                .is_none()
        );
        assert!(
            execute(dir.path(), "write_file", &args, None)
                .await
                .content
                .get("error")
                .is_some()
        );
        let args = json!({"path":"x","content":"two","expected_sha256":sha256(b"one")});
        assert!(
            execute(dir.path(), "write_file", &args, None)
                .await
                .content
                .get("error")
                .is_none()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("x")).unwrap(),
            "two"
        );
    }
    #[test]
    fn directory_sync_failure_preserves_unknown_outcome_after_replacement() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x"), "old").unwrap();
        let args = json!({"path":"x","content":"new","expected_sha256":sha256(b"old")});
        let out = write_file(dir.path(), &args, |_| {
            Err(std::io::Error::other("injected directory sync failure"))
        });
        assert!(out.uncertain);
        assert_eq!(out.content["effect"], "unknown");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("x")).unwrap(),
            "new"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        let rejected = write_file(dir.path(), &args, |_| Ok(()));
        assert!(!rejected.uncertain);
        assert!(rejected.content.get("error").is_some());
    }
    #[tokio::test]
    async fn sparse_oversized_files_are_rejected_before_loading_contents() {
        let dir = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(dir.path().join("huge")).unwrap();
        file.set_len(512 * 1024 * 1024).unwrap();
        let outcome = execute(dir.path(), "read_file", &json!({"path":"huge"}), None).await;
        assert!(
            outcome.content["error"]
                .as_str()
                .unwrap()
                .contains("16 MiB")
        );
        assert!(!outcome.uncertain);
    }
    #[tokio::test]
    async fn directory_listing_limits_entries_and_reports_truncation() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..1002 {
            std::fs::File::create(dir.path().join(format!("file-{index}"))).unwrap();
        }
        let outcome = execute(dir.path(), "list_files", &json!({}), None).await;
        assert_eq!(outcome.content["entries"].as_array().unwrap().len(), 1000);
        assert_eq!(outcome.content["truncated"], true);
    }
    #[test]
    fn traversal_and_symlink_escapes_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(workspace_path(dir.path(), "../escape").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/tmp", dir.path().join("outside")).unwrap();
            assert!(workspace_path(dir.path(), "outside/escaped").is_err());
        }
    }
    #[tokio::test]
    async fn shell_output_is_bounded_and_nonzero_exit_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let out = execute(
            dir.path(),
            "shell",
            &json!({"command":"printf test; exit 7"}),
            None,
        )
        .await;
        assert_eq!(out.content["stdout"], "test");
        assert_eq!(out.content["exit_code"], 7);
        assert!(!out.uncertain);
    }

    #[test]
    fn external_save_during_preparation_is_not_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.py");
        std::fs::write(&path, "original").unwrap();
        let outcome = write_file_prepared(
            directory.path(),
            &json!({"path":"source.py","content":"agent replacement","expected_sha256":sha256(b"original")}),
            |_| Ok(()),
            |path| {
                std::fs::write(path, "editor save")?;
                Ok(())
            },
        );
        assert!(!outcome.uncertain);
        assert!(
            outcome.content["error"]
                .as_str()
                .unwrap()
                .contains("changed during")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "editor save");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn external_replacement_with_identical_content_is_detected_by_identity() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.py");
        std::fs::write(&path, "original").unwrap();
        let outcome = write_file_prepared(
            directory.path(),
            &json!({"path":"source.py","content":"agent replacement","expected_sha256":sha256(b"original")}),
            |_| Ok(()),
            |path| {
                let other = path.with_extension("editor-save");
                std::fs::write(&other, "original")?;
                std::fs::rename(other, path)?;
                Ok(())
            },
        );
        assert!(!outcome.uncertain);
        assert!(
            outcome.content["error"]
                .as_str()
                .unwrap()
                .contains("changed during")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "original");
    }

    #[test]
    fn new_file_creation_does_not_replace_a_target_created_during_preparation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("new.py");
        let outcome = write_file_prepared(
            directory.path(),
            &json!({"path":"new.py","content":"agent content","expected_sha256":null}),
            |_| Ok(()),
            |path| {
                std::fs::write(path, "external new file")?;
                Ok(())
            },
        );
        assert!(!outcome.uncertain);
        assert!(outcome.content["error"].is_string());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external new file");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn new_file_directory_sync_failure_is_unknown_after_the_atomic_create() {
        let directory = tempfile::tempdir().unwrap();
        let outcome = write_file(
            directory.path(),
            &json!({"path":"new.py","content":"new content","expected_sha256":null}),
            |_| Err(std::io::Error::other("injected directory sync failure")),
        );
        assert!(outcome.uncertain);
        assert_eq!(outcome.content["effect"], "unknown");
        assert_eq!(
            std::fs::read_to_string(directory.path().join("new.py")).unwrap(),
            "new content"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn write_hash_rejects_an_oversized_existing_file_before_loading_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("growing.log");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(512 * 1024 * 1024)
            .unwrap();
        let outcome = write_file(
            directory.path(),
            &json!({"path":"growing.log","content":"replacement","expected_sha256":"previous-hash"}),
            |_| Ok(()),
        );
        assert!(!outcome.uncertain);
        assert!(
            outcome.content["error"]
                .as_str()
                .unwrap()
                .contains("16 MiB")
        );
        assert_eq!(std::fs::metadata(path).unwrap().len(), 512 * 1024 * 1024);
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess helper for the killed-parent lease test"]
    fn inherited_lease_shell_helper() {
        use fs2::FileExt;
        let directory =
            std::path::PathBuf::from(std::env::var_os("BONE_TOOL_LEASE_TEST_DIR").unwrap());
        let lease = std::sync::Arc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(directory.join("lease.lock"))
                .unwrap(),
        );
        lease.try_lock_exclusive().unwrap();
        let legacy = std::sync::Arc::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(directory.join("legacy.lock"))
                .unwrap(),
        );
        legacy.try_lock_exclusive().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(execute(
            &directory,
            "shell",
            &json!({"command":"printf ready > shell.started; sleep 2; printf finished > shell.finished","timeout_seconds":10}),
            Some([lease, legacy]),
        ));
    }

    #[cfg(unix)]
    #[test]
    fn killed_parent_does_not_release_the_frontground_shells_physical_lease() {
        use fs2::FileExt;
        use std::process::{Command, Stdio};
        let directory = tempfile::tempdir().unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.path().join("lease.lock"))
            .unwrap();
        let legacy = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(directory.path().join("legacy.lock"))
            .unwrap();
        let mut parent = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tools::tests::inherited_lease_shell_helper",
                "--ignored",
                "--nocapture",
            ])
            .env("BONE_TOOL_LEASE_TEST_DIR", directory.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        while !directory.path().join("shell.started").exists() {
            if started.elapsed() > Duration::from_secs(5) {
                let _ = parent.kill();
                let _ = parent.wait();
                panic!("shell helper did not start");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        parent.kill().unwrap();
        parent.wait().unwrap();
        // This is the exact kernel lock check used when recovering a write.
        assert_eq!(
            lock.try_lock_exclusive().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            legacy.try_lock_exclusive().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let stopped = std::time::Instant::now();
        loop {
            if lock.try_lock_exclusive().is_ok() && legacy.try_lock_exclusive().is_ok() {
                break;
            }
            assert!(
                stopped.elapsed() < Duration::from_secs(5),
                "orphan shell did not finish"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            std::fs::read_to_string(directory.path().join("shell.finished")).unwrap(),
            "finished"
        );
        FileExt::unlock(&lock).unwrap();
        FileExt::unlock(&legacy).unwrap();
    }
}

#[cfg(test)]
mod workspace_engineering_tests {
    use super::*;

    fn page(dir: &Path, query: &str, limit: usize, cursor: Option<&Value>) -> Value {
        let mut args = json!({"query":query,"limit":limit});
        if let Some(cursor) = cursor {
            args["cursor"] = cursor.clone();
        }
        search_files(dir, &args).unwrap()
    }
    fn edits(dir: &Path, source: &str, edits: Value) -> ToolOutcome {
        std::fs::write(dir.join("source"), source).unwrap();
        edit_file_prepared(
            dir,
            &json!({"path":"source","expected_sha256":sha256(source.as_bytes()),"edits":edits}),
            |_| Ok(()),
            |_| Ok(()),
        )
    }

    #[test]
    fn search_native_schema_modes_and_final_bounds() {
        let readonly = definitions(true, true);
        let search = readonly
            .iter()
            .find(|tool| tool.name.as_str() == "search_files")
            .unwrap();
        assert_eq!(search.parameters["properties"]["limit"]["default"], 50);
        assert_eq!(search.parameters["properties"]["limit"]["maximum"], 200);
        assert!(
            !readonly
                .iter()
                .any(|tool| tool.name.as_str() == "edit_file")
        );
        assert!(is_external("search_files") && !is_write("search_files"));
        assert!(is_external("edit_file") && is_write("edit_file"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "q ".repeat(220)).unwrap();
        assert_eq!(
            search_files(dir.path(), &json!({"query":"q"})).unwrap()["matches"]
                .as_array()
                .unwrap()
                .len(),
            50
        );
        assert!(
            page(dir.path(), "q", 200, None)["matches"]
                .as_array()
                .unwrap()
                .len()
                <= 200
        );
        for args in [
            json!({"query":""}),
            json!({"query":"q","limit":0}),
            json!({"query":"q","limit":201}),
            json!({"query":"q","limit":1.5}),
            json!({"query":"q","limit":"50"}),
        ] {
            assert!(search_files(dir.path(), &args).is_err());
        }
    }

    #[test]
    fn short_line_search_returns_default50_max200_pages_without_unrelated_line_repetition() {
        let dir = tempfile::tempdir().unwrap();
        let source = "needle\r\n".repeat(240);
        std::fs::write(dir.path().join("many.txt"), &source).unwrap();
        let default = search_files(dir.path(), &json!({"query":"needle"})).unwrap();
        assert_eq!(default["matches"].as_array().unwrap().len(), 50);
        let first = page(dir.path(), "needle", 200, None);
        assert_eq!(first["matches"].as_array().unwrap().len(), 200);
        assert_eq!(first["truncated"], true);
        assert!(serde_json::to_vec(&first).unwrap().len() <= OUTPUT_LIMIT);
        for record in first["matches"].as_array().unwrap() {
            assert_eq!(record["snippet"], "needle\r");
            assert_eq!(record["sha256"], sha256(source.as_bytes()));
        }
        let last = page(dir.path(), "needle", 200, Some(&first["next_cursor"]));
        assert_eq!(last["matches"].as_array().unwrap().len(), 40);
        assert_eq!(last["matches"][0]["byte_offset"], 200 * 8);
        assert_eq!(last["matches"][0]["line"], 201);
        assert_eq!(last["truncated"], false);
        assert!(last["next_cursor"].is_null());
        assert_eq!(
            search_snippet("before needle\nAFTER", 7, "needle\n"),
            "before needle\n"
        );
        let source = format!(
            "{}命中\r\n下一行{}\n不要附带",
            "左".repeat(1600),
            "右".repeat(1600)
        );
        let offset = source.find("命中").unwrap();
        let snippet = search_snippet(&source, offset, "命中\r\n下一行");
        assert!(snippet.contains("命中\r\n下一行"));
        assert_eq!(snippet.chars().count(), 1024);
        assert!(!snippet.contains("不要附带"));
    }

    #[test]
    fn unicode_crlf_paging_occurrences_and_whole_file_hash() {
        let dir = tempfile::tempdir().unwrap();
        let a = "é café café\r\n末 café\r\ntail not matched";
        std::fs::write(dir.path().join("a"), a).unwrap();
        std::fs::write(dir.path().join("z"), "café").unwrap();
        let mut found = Vec::new();
        let mut cursor = None;
        loop {
            let current = page(dir.path(), "café", 1, cursor.as_ref());
            let records = current["matches"].as_array().unwrap();
            assert_eq!(records.len(), 1);
            found.extend(records.iter().cloned());
            if current["next_cursor"].is_null() {
                assert_eq!(current["truncated"], false);
                break;
            }
            cursor = Some(current["next_cursor"].clone());
        }
        assert_eq!(
            found
                .iter()
                .map(|value| value["path"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["a", "a", "a", "z"]
        );
        assert_eq!(
            found
                .iter()
                .map(|value| value["byte_offset"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![3, 9, 20, 0]
        );
        assert_eq!(
            found
                .iter()
                .map(|value| value["line"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 1, 2, 1]
        );
        for value in &found[..3] {
            assert_eq!(value["sha256"], sha256(a.as_bytes()));
            assert!(value["snippet"].as_str().unwrap().contains("café"));
        }
        let current = page(dir.path(), "café", 1, None);
        std::fs::write(dir.path().join("a"), format!("{a} altered tail")).unwrap();
        assert!(
            search_files(
                dir.path(),
                &json!({"query":"café","cursor":current["next_cursor"]})
            )
            .unwrap_err()
            .to_string()
            .contains("changed")
        );
    }

    #[test]
    fn search_nonoverlap_and_empty_last_page() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "aaaaa").unwrap();
        let first = page(dir.path(), "aa", 1, None);
        assert_eq!(first["matches"][0]["byte_offset"], 0);
        let last = page(dir.path(), "aa", 200, Some(&first["next_cursor"]));
        assert_eq!(last["matches"][0]["byte_offset"], 2);
        assert!(last["next_cursor"].is_null());
        assert_eq!(last["truncated"], false);
        assert!(
            page(dir.path(), "missing", 1, None)["matches"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn search_output_ceiling_produces_contiguous_smaller_pages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a"),
            format!("{}{}", "X".repeat(160), "é\\\n".repeat(1500)),
        )
        .unwrap();
        let mut cursor = None;
        let mut offsets = Vec::new();
        loop {
            let current = page(dir.path(), "X", 200, cursor.as_ref());
            assert!(serde_json::to_vec(&current).unwrap().len() <= OUTPUT_LIMIT);
            assert!(current["matches"].as_array().unwrap().len() < 200);
            for value in current["matches"].as_array().unwrap() {
                assert!(value["snippet"].as_str().unwrap().chars().count() <= 1024);
                offsets.push(value["byte_offset"].as_u64().unwrap());
            }
            if current["next_cursor"].is_null() {
                break;
            }
            cursor = Some(current["next_cursor"].clone());
        }
        assert_eq!(offsets, (0..160).collect::<Vec<u64>>());
        let query = "é".repeat(1200);
        std::fs::write(dir.path().join("long-query"), &query).unwrap();
        let result = search_files(dir.path(), &json!({"query":query,"path":"long-query"})).unwrap();
        assert_eq!(
            result["matches"][0]["snippet"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            1024
        );
    }

    #[test]
    fn cursor_rejects_bad_checksum_version_position_request_removed_and_retyped_reference() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), "q q").unwrap();
        let first = page(dir.path(), "q", 1, None);
        let valid = first["next_cursor"].as_str().unwrap();
        for invalid in ["garbage".to_owned(), format!("{valid}0")] {
            assert!(search_files(dir.path(), &json!({"query":"q","cursor":invalid})).is_err());
        }
        for args in [
            json!({"query":"different","cursor":valid}),
            json!({"query":"q","path":"a","cursor":valid}),
        ] {
            assert!(search_files(dir.path(), &args).is_err());
        }
        let mut cursor = cursor_decode(valid).unwrap();
        cursor.version = 2;
        assert!(
            search_files(
                dir.path(),
                &json!({"query":"q","cursor":cursor_encode(&cursor).unwrap()})
            )
            .is_err()
        );
        cursor.version = 1;
        cursor.byte_offset = 1;
        assert!(
            search_files(
                dir.path(),
                &json!({"query":"q","cursor":cursor_encode(&cursor).unwrap()})
            )
            .is_err()
        );
        std::fs::remove_file(dir.path().join("a")).unwrap();
        assert!(search_files(dir.path(), &json!({"query":"q","cursor":valid})).is_err());
        std::fs::create_dir(dir.path().join("a")).unwrap();
        assert!(search_files(dir.path(), &json!({"query":"q","cursor":valid})).is_err());
    }

    #[test]
    fn search_skips_binary_oversize_and_exclusions_without_touching_sources() {
        let dir = tempfile::tempdir().unwrap();
        for skip in SEARCH_EXCLUSIONS {
            std::fs::create_dir(dir.path().join(skip)).unwrap();
            std::fs::write(dir.path().join(skip).join("hidden"), "q").unwrap();
        }
        std::fs::write(dir.path().join("bad-utf8"), [0xff]).unwrap();
        std::fs::write(dir.path().join("binary"), b"q\0").unwrap();
        let huge = std::fs::File::create(dir.path().join("huge")).unwrap();
        huge.set_len(FILE_LIMIT + 1).unwrap();
        std::fs::write(dir.path().join("good"), "q").unwrap();
        let result = page(dir.path(), "q", 50, None);
        assert_eq!(result["matches"].as_array().unwrap().len(), 1);
        assert_eq!(result["matches"][0]["path"], "good");
        assert_eq!(result["scanned_files"], 1);
        assert_eq!(result["skipped_files"], 3);
        assert_eq!(result["skipped_directories"], 4);
        let excluded =
            search_files(dir.path(), &json!({"query":"q","path":"target/hidden"})).unwrap();
        assert!(excluded["matches"].as_array().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn search_and_edit_reject_all_explicit_symlinks_and_bad_paths() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("source"), "q q").unwrap();
        std::fs::write(outside.path().join("file"), "q").unwrap();
        symlink(dir.path().join("source"), dir.path().join("inside-link")).unwrap();
        symlink(outside.path(), dir.path().join("dir-link")).unwrap();
        let result = page(dir.path(), "q", 50, None);
        assert_eq!(result["matches"].as_array().unwrap().len(), 2);
        assert_eq!(result["skipped_files"], 2);
        for path in ["inside-link", "dir-link/file", "../source", "/tmp/absolute"] {
            assert!(search_files(dir.path(), &json!({"path":path,"query":"q"})).is_err());
            let outcome = edit_file_prepared(
                dir.path(),
                &json!({"path":path,"expected_sha256":sha256(b"q q"),"edits":[{"old_text":"q","new_text":"x"}]}),
                |_| Ok(()),
                |_| Ok(()),
            );
            assert!(outcome.content.get("error").is_some() && !outcome.uncertain);
        }
        assert_eq!(std::fs::read(dir.path().join("source")).unwrap(), b"q q");
    }

    #[test]
    fn edit_uses_original_spans_non_cascading_and_preserves_unicode_crlf() {
        let dir = tempfile::tempdir().unwrap();
        let source = "alpha\r\nβeta\r\n末\r\n";
        let outcome = edits(
            dir.path(),
            source,
            json!([{"old_text":"alpha","new_text":"βeta"},{"old_text":"βeta","new_text":"γamma"}]),
        );
        assert!(outcome.content.get("error").is_none());
        let expected = "βeta\r\nγamma\r\n末\r\n";
        assert_eq!(
            std::fs::read(dir.path().join("source")).unwrap(),
            expected.as_bytes()
        );
        assert_eq!(outcome.content["sha256"], sha256(expected.as_bytes()));
        assert_eq!(outcome.content["bytes"], expected.len());
        assert_eq!(outcome.content["edits_applied"], 2);
    }

    #[test]
    fn edit_batch_validation_rejects_repeated_overlapping_absent_empty_and_stale_without_install() {
        let dir = tempfile::tempdir().unwrap();
        for (source, batch) in [
            ("aaa", json!([{"old_text":"aa","new_text":"x"}])),
            (
                "abc",
                json!([{"old_text":"ab","new_text":"x"},{"old_text":"bc","new_text":"y"}]),
            ),
            (
                "abc",
                json!([{"old_text":"ab","new_text":"x"},{"old_text":"ab","new_text":"y"}]),
            ),
            (
                "abc",
                json!([{"old_text":"ab","new_text":"x"},{"old_text":"missing","new_text":"y"}]),
            ),
            ("abc", json!([{"old_text":"","new_text":"x"}])),
            ("abc", json!([])),
        ] {
            let outcome = edits(dir.path(), source, batch);
            assert!(outcome.content.get("error").is_some() && !outcome.uncertain);
            assert_eq!(
                std::fs::read(dir.path().join("source")).unwrap(),
                source.as_bytes()
            );
            assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        }
        let outcome = edit_file_prepared(
            dir.path(),
            &json!({"path":"source","expected_sha256":"stale","edits":[{"old_text":"abc","new_text":"x"}]}),
            |_| Ok(()),
            |_| Ok(()),
        );
        assert!(outcome.content.get("error").is_some());
        assert_eq!(std::fs::read(dir.path().join("source")).unwrap(), b"abc");
    }

    #[cfg(unix)]
    #[test]
    fn edit_preserves_mode_and_reuses_final_identity_and_uncertain_durability() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751)).unwrap();
        let args = json!({"path":"source","expected_sha256":sha256(b"old"),"edits":[{"old_text":"old","new_text":"new"}]});
        let outcome = edit_file_prepared(dir.path(), &args, |_| Ok(()), |_| Ok(()));
        assert!(outcome.content.get("error").is_none());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o751
        );
        std::fs::write(&path, "old").unwrap();
        let outcome = edit_file_prepared(
            dir.path(),
            &args,
            |_| Ok(()),
            |path| {
                std::fs::write(path, "raced")?;
                Ok(())
            },
        );
        assert!(outcome.content.get("error").is_some() && !outcome.uncertain);
        assert_eq!(std::fs::read(&path).unwrap(), b"raced");
        std::fs::write(&path, "old").unwrap();
        let outcome = edit_file_prepared(
            dir.path(),
            &args,
            |_| Err(std::io::Error::other("durability fault")),
            |_| Ok(()),
        );
        assert!(outcome.uncertain);
        assert_eq!(outcome.content["effect"], "unknown");
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn edit_rejects_missing_directory_invalid_utf8_and_oversize_before_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("directory")).unwrap();
        std::fs::write(dir.path().join("invalid"), [0xff]).unwrap();
        std::fs::File::create(dir.path().join("huge"))
            .unwrap()
            .set_len(FILE_LIMIT + 1)
            .unwrap();
        for path in ["missing", "directory", "invalid", "huge"] {
            let outcome = edit_file_prepared(
                dir.path(),
                &json!({"path":path,"expected_sha256":"bad","edits":[{"old_text":"old","new_text":"new"}]}),
                |_| Ok(()),
                |_| Ok(()),
            );
            assert!(outcome.content.get("error").is_some() && !outcome.uncertain);
        }
        assert!(!dir.path().join("missing").exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    }
}
