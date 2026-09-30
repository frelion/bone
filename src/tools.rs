use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rig_core::completion::ToolDefinition;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const OUTPUT_LIMIT: usize = 32 * 1024;

pub fn definitions(single_job: bool, read_only: bool) -> Vec<ToolDefinition> {
    let mut tools = vec![
        definition(
            "read_file",
            "Read a workspace UTF-8 file. Returns its SHA-256 for safe replacement; offset and limit are bytes.",
            json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1}}),
            &["path"],
        ),
        definition(
            "list_files",
            "List files in a workspace directory (no recursive traversal).",
            json!({"path":{"type":"string"}}),
            &[],
        ),
        definition(
            "job_inspect",
            "Inspect internal jobs and their pending input IDs; optionally read one job's recent records. Jobs are contexts of this same agent.",
            json!({"job_id":{"type":"string"}}),
            &[],
        ),
        definition(
            "ask_user",
            "Ask the user for missing information and suspend this job. Use alone in a tool-call batch.",
            json!({"question":{"type":"string"}}),
            &["question"],
        ),
        definition(
            "pause_work",
            "Pause all work in this session when the user asks to stop. Use alone.",
            json!({"reason":{"type":"string"}}),
            &["reason"],
        ),
    ];
    if !read_only {
        tools.push(definition("write_file", "Replace a workspace file only if expected_sha256 matches the hash returned by read_file. For a NEW file pass expected_sha256=null. All contents must be provided.", json!({"path":{"type":"string"},"content":{"type":"string"},"expected_sha256":{"type":["string","null"]}}), &["path","content","expected_sha256"]));
        tools.push(definition("shell", "Run a shell command in the workspace with the user's local privileges. Treat as a write operation. Output is bounded; timed-out commands require reconciliation. Prefer read_file/list_files for inspection.", json!({"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":300}}), &["command"]));
    }
    if !single_job {
        tools.extend([
            definition("job_send", "Send a bounded assignment to another internal job. Omit job_id to create one; otherwise continue an existing job. Returns input_id; wait on that exact input ID. Every created job shares this input's budget.", json!({"job_id":{"type":"string"},"title":{"type":"string"},"message":{"type":"string"}}), &["message"]),
            definition("job_wait", "Suspend until the specified input IDs each receive a delivery or failure. Use alone; no polling and no model slot is occupied while waiting.", json!({"input_ids":{"type":"array","items":{"type":"string"},"minItems":1}}), &["input_ids"]),
            definition("job_handoff", "Transfer the current original input and conversation focus to an idle existing job, or a new job if job_id is omitted. Only before tools have acted on this input; use alone. The target owns its response.", json!({"job_id":{"type":"string"},"title":{"type":"string"}}), &[]),
            definition("job_close", "Close an idle internal job while retaining its records. Current or busy jobs cannot be closed.", json!({"job_id":{"type":"string"}}), &["job_id"]),
            definition("job_control", "Pause or resume another job after a changed user instruction. A resumed job receives current session instructions.", json!({"job_id":{"type":"string"},"state":{"type":"string","enum":["paused","ready"]}}), &["job_id","state"]),
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
    matches!(name, "read_file" | "list_files" | "write_file" | "shell")
}

pub fn is_write(name: &str) -> bool {
    matches!(name, "write_file" | "shell")
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

pub async fn execute(workspace: &Path, name: &str, args: &Value) -> ToolOutcome {
    if name == "shell" {
        return match shell(workspace, args).await {
            Ok(outcome) => outcome,
            Err(error) => ToolOutcome {
                content: json!({"error":format!("{error:#}")}),
                uncertain: false,
            },
        };
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
    let path = workspace_path(
        workspace,
        args.get("path").and_then(Value::as_str).unwrap_or("."),
    )?;
    match name {
        "read_file" => {
            ensure!(path.is_file(), "path is not a regular file");
            ensure!(
                std::fs::metadata(&path)?.len() <= 16 * 1024 * 1024,
                "file exceeds 16 MiB; use a bounded shell command"
            );
            let mut bytes = Vec::new();
            use std::io::Read;
            std::fs::File::open(&path)?
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 16 * 1024 * 1024,
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
                .unwrap_or(OUTPUT_LIMIT as u64) as usize)
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

fn write_file(
    workspace: &Path,
    args: &Value,
    sync_directory: fn(&Path) -> std::io::Result<()>,
) -> ToolOutcome {
    let mut renamed = false;
    let result = (|| -> Result<Value> {
        let path = workspace_path(workspace, string_arg(args, "path")?)?;
        let content = string_arg(args, "content")?;
        let expected = args
            .get("expected_sha256")
            .context("expected_sha256 is required (null for a new file)")?;
        if path.exists() {
            ensure!(path.is_file(), "path is not a regular file");
            let expected = expected
                .as_str()
                .context("existing file requires its expected SHA-256")?;
            ensure!(
                sha256(&std::fs::read(&path)?) == expected,
                "file changed; read it again before replacing it"
            );
        } else {
            ensure!(expected.is_null(), "expected file no longer exists");
        }
        let parent = path.parent().context("file needs a parent")?;
        std::fs::create_dir_all(parent)?;
        let _ = workspace_path(workspace, string_arg(args, "path")?)?;
        let temporary = parent.join(format!(".bone-{}.tmp", uuid::Uuid::new_v4()));
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
            std::fs::rename(&temporary, &path)?;
            renamed = true;
            sync_directory(parent)?;
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
            content: if renamed {
                json!({"error":format!("{error:#}"),"effect":"unknown","instruction":"The file was replaced but directory durability could not be confirmed. Inspect it before retrying."})
            } else {
                json!({"error":format!("{error:#}")})
            },
            uncertain: renamed,
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

async fn shell(workspace: &Path, args: &Value) -> Result<ToolOutcome> {
    let command = string_arg(args, "command")?;
    let seconds = args
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(60)
        .clamp(1, 300);
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
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
    #[tokio::test]
    async fn replacements_require_matching_content() {
        let dir = tempfile::tempdir().unwrap();
        let args = json!({"path":"x","content":"one","expected_sha256":null});
        assert!(
            execute(dir.path(), "write_file", &args)
                .await
                .content
                .get("error")
                .is_none()
        );
        assert!(
            execute(dir.path(), "write_file", &args)
                .await
                .content
                .get("error")
                .is_some()
        );
        let args = json!({"path":"x","content":"two","expected_sha256":sha256(b"one")});
        assert!(
            execute(dir.path(), "write_file", &args)
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
        let outcome = execute(dir.path(), "read_file", &json!({"path":"huge"})).await;
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
        let outcome = execute(dir.path(), "list_files", &json!({})).await;
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
        )
        .await;
        assert_eq!(out.content["stdout"], "test");
        assert_eq!(out.content["exit_code"], 7);
        assert!(!out.uncertain);
    }
}
