//! Local UI persistence and read-only workspace inspection.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncReadExt;

const DRAFT_LIMIT: usize = 128 * 1024;
const HISTORY_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
// JSON escaping can expand a byte to six ASCII bytes.
const SAVED_LIMIT: usize = 6 * (DRAFT_LIMIT + HISTORY_LIMIT) + 4096;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct UiSaved {
    pub draft: String,
    pub history: Vec<String>,
}
fn session_path(data: &Path, session: &str) -> Result<PathBuf> {
    ensure!(
        !session.is_empty()
            && session.len() <= 128
            && session
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "invalid session ID for UI persistence"
    );
    Ok(data.join("tui").join(format!("{session}.json")))
}
fn validate(saved: &UiSaved) -> Result<()> {
    ensure!(saved.draft.len() <= DRAFT_LIMIT, "draft exceeds 128 KiB");
    ensure!(
        saved.history.len() <= 100
            && saved.history.iter().map(String::len).sum::<usize>() <= HISTORY_LIMIT,
        "input history exceeds 100 entries or 1 MiB"
    );
    Ok(())
}
pub fn load(data: &Path, session: &str) -> Result<UiSaved> {
    let path = session_path(data, session)?;
    let file = match fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(UiSaved::default()),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take((SAVED_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= SAVED_LIMIT,
        "saved UI state is too large; ignoring corrupt state"
    );
    let saved: UiSaved = serde_json::from_slice(&bytes)
        .context("saved UI state is corrupt; ignoring saved draft/history")?;
    validate(&saved).context("saved UI state is invalid; ignoring saved draft/history")?;
    Ok(saved)
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing destination directory")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}
pub fn save(data: &Path, session: &str, saved: &UiSaved) -> Result<()> {
    validate(saved)?;
    atomic_write(&session_path(data, session)?, &serde_json::to_vec(saved)?)
}

pub fn files(workspace: &Path) -> Result<Vec<String>> {
    let root = workspace
        .canonicalize()
        .context("resolve workspace for file completion")?;
    ensure!(root.is_dir(), "workspace is not a directory");
    let mut stack = vec![root.clone()];
    let mut files = Vec::new();
    let mut inspected = 0usize;
    while let Some(dir) = stack.pop() {
        let mut entries = fs::read_dir(&dir)?
            .take(50_001usize.saturating_sub(inspected))
            .collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            inspected += 1;
            if inspected > 50_000 || files.len() >= 5_000 {
                files.sort();
                return Ok(files);
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.')
                || matches!(
                    name.as_ref(),
                    "target" | "node_modules" | "generated" | "dist" | "build" | "vendor"
                )
            {
                continue;
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            let path = entry.path();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                files.push(path.strip_prefix(&root)?.to_string_lossy().into_owned());
            }
        }
    }
    files.sort();
    Ok(files)
}
async fn git_output(workspace: &Path, args: &[&str], limit: usize) -> Result<String> {
    let mut command = tokio::process::Command::new("git");
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .args(args)
        .current_dir(workspace)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().context("start read-only git inspection")?;
    let stdout = child.stdout.take().context("missing git stdout")?;
    let stderr = child.stderr.take().context("missing git stderr")?;
    let read = |stream: tokio::process::ChildStdout| async move {
        let mut bytes = Vec::new();
        stream
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        ensure!(
            bytes.len() <= limit,
            "git output exceeds 2 MiB; narrow the workspace changes"
        );
        Ok::<_, anyhow::Error>(bytes)
    };
    let out = read(stdout);
    let err = async move {
        let mut bytes = Vec::new();
        stderr.take(8193).read_to_end(&mut bytes).await?;
        ensure!(bytes.len() <= 8192, "git error output exceeds 8 KiB");
        Ok::<_, anyhow::Error>(bytes)
    };
    let (out, err) = tokio::try_join!(out, err)?;
    let status = child.wait().await?;
    ensure!(
        status.success(),
        "git inspection failed: {}",
        String::from_utf8_lossy(&err)
    );
    Ok(String::from_utf8_lossy(&out).into_owned())
}
pub async fn git_diff(workspace: &Path) -> Result<String> {
    let root = workspace.canonicalize()?;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut output = String::from("Working tree\n");
        output.push_str(
            &git_output(
                &root,
                &["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--"],
                OUTPUT_LIMIT,
            )
            .await?,
        );
        output.push_str("\nStaged\n");
        output.push_str(
            &git_output(
                &root,
                &[
                    "diff",
                    "--cached",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    "--",
                ],
                OUTPUT_LIMIT.saturating_sub(output.len()),
            )
            .await?,
        );
        output.push_str("\nStatus (includes untracked paths)\n");
        output.push_str(
            &git_output(
                &root,
                &["status", "--short", "--untracked-files=normal"],
                OUTPUT_LIMIT.saturating_sub(output.len()),
            )
            .await?,
        );
        ensure!(output.len() <= OUTPUT_LIMIT, "git output exceeds 2 MiB");
        Ok(output)
    })
    .await
    .context("git inspection timed out after 10 seconds")?
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn text(value: &Value) -> String {
    match value {
        Value::Array(items) => items
            .iter()
            .map(text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(item) => match item.get("type").and_then(Value::as_str) {
            Some("text") => item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            Some("reasoning" | "encrypted" | "toolcall") => String::new(),
            Some("json") => item.get("value").map(Value::to_string).unwrap_or_default(),
            _ => item.get("content").map(text).unwrap_or_default(),
        },
        _ => String::new(),
    }
}
fn preview(mut text: String) -> String {
    const LIMIT: usize = 128 * 1024;
    if text.len() > LIMIT {
        let mut end = LIMIT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(
            "\n[Tool result preview capped at 128 KiB; complete original remains in SQLite.]",
        );
    }
    text
}
pub fn export(data: &Path, session: &str) -> Result<PathBuf> {
    session_path(data, session)?;
    let mut events = Vec::new();
    let mut cursor = None;
    let mut source_bytes = 0usize;
    loop {
        let page = bone::history_page(data, session, cursor.as_deref(), 20)?;
        cursor = page.next_cursor;
        for mut event in page.events {
            // Keep only readable, allowlisted fields while resolving delivery references.
            // Large tool originals and opaque transport blocks stay in SQLite.
            let data = &event.data;
            event.data = match event.kind.as_str() {
                "input" => {
                    serde_json::json!({"source":data["source"], "message":[{"type":"text", "text":text(&data["message"])}]})
                }
                "model_message" => {
                    serde_json::json!({"response":{"choice":[{"type":"text", "text":text(&data["response"]["choice"])}]}})
                }
                "delivery" => serde_json::json!({"response_event":data["response_event"]}),
                "question" => serde_json::json!({"question":data["question"]}),
                "failure" => serde_json::json!({"error":data["error"]}),
                "input_paused" => serde_json::json!({"text":data["text"]}),
                "tool_started" => {
                    serde_json::json!({"tool_name":data["tool_name"], "effect":data["effect"]})
                }
                "tool_result" | "tool_reconciled" => {
                    serde_json::json!({"message":[{"type":"text", "text":preview(text(&data["message"]))}]})
                }
                _ => continue,
            };
            source_bytes = source_bytes.saturating_add(serde_json::to_vec(&event)?.len());
            ensure!(
                source_bytes <= 32 * 1024 * 1024,
                "session export exceeds 32 MiB; original events remain in SQLite"
            );
            events.push(event);
        }
        if !page.has_more {
            break;
        }
        ensure!(cursor.is_some(), "history cursor did not advance");
    }
    let by_id: HashMap<_, _> = events.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut html = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>BONE {}</title><style>body{{max-width:960px;margin:40px auto;padding:0 24px;font:16px system-ui;background:#101218;color:#e3e6ed}}article{{padding:16px 0;border-bottom:1px solid #303644}}pre{{white-space:pre-wrap;overflow-wrap:anywhere}}small{{color:#9ba6b9}}</style><body><h1>BONE session {}</h1>",
        escape(session),
        escape(session)
    );
    for event in &events {
        let user = event.kind == "input" && event.data["source"] == "user";
        let public = event
            .reply_to
            .as_deref()
            .and_then(|id| by_id.get(id))
            .is_some_and(|e| e.data["source"] == "user");
        let body = match event.kind.as_str() {
            "input" if user => text(&event.data["message"]),
            "question" => event.data["question"].as_str().unwrap_or_default().into(),
            "delivery" if public => event.data["response_event"]
                .as_str()
                .and_then(|id| by_id.get(id))
                .map(|e| text(&e.data["response"]["choice"]))
                .unwrap_or_default(),
            "failure" if public => event.data["error"].as_str().unwrap_or_default().into(),
            "input_paused" if public => event.data["text"].as_str().unwrap_or_default().into(),
            "tool_started" => format!(
                "{} ({})",
                event.data["tool_name"].as_str().unwrap_or("tool"),
                event.data["effect"].as_str().unwrap_or("unknown")
            ),
            "tool_result" | "tool_reconciled" => preview(text(&event.data["message"])),
            _ => continue,
        };
        html.push_str(&format!("<article><h2>{}</h2><small>event {} · job {} · call {} · root input {} · timestamp {}</small><pre>{}</pre></article>", escape(&event.kind), escape(&event.id), escape(event.job_id.as_deref().unwrap_or("—")), escape(event.call_id.as_deref().unwrap_or("—")), escape(event.root_input.as_deref().unwrap_or("—")), escape(&event.timestamp), escape(&body)));
        ensure!(
            html.len() <= 32 * 1024 * 1024,
            "HTML export exceeds 32 MiB; original events remain in SQLite"
        );
    }
    ensure!(
        html.len() + 14 <= 32 * 1024 * 1024,
        "HTML export exceeds 32 MiB; original events remain in SQLite"
    );
    html.push_str("</body></html>");
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let path = data
        .join("tui/exports")
        .join(format!("{session}-{now}-{}.html", uuid::Uuid::new_v4()));
    atomic_write(&path, html.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drafts_roundtrip_and_reject_corruption_and_paths() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path(), "session").unwrap().draft, "");
        let saved = UiSaved {
            draft: "中文 draft".into(),
            history: vec!["one".into()],
        };
        save(dir.path(), "session", &saved).unwrap();
        assert_eq!(load(dir.path(), "session").unwrap().history, saved.history);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(session_path(dir.path(), "session").unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        fs::write(session_path(dir.path(), "session").unwrap(), "broken").unwrap();
        assert!(load(dir.path(), "session").is_err());
        assert!(save(dir.path(), "../escape", &saved).is_err());
        assert!(
            save(
                dir.path(),
                "session",
                &UiSaved {
                    draft: "a".repeat(DRAFT_LIMIT + 1),
                    history: vec![]
                }
            )
            .is_err()
        );
    }
    #[test]
    fn html_and_opaque_content_are_safe() {
        assert_eq!(
            escape("</script><>&\"'"),
            "&lt;/script&gt;&lt;&gt;&amp;&quot;&#39;"
        );
        assert_eq!(
            text(
                &serde_json::json!([{"type":"reasoning","text":"secret"},{"type":"text","text":"visible"}])
            ),
            "visible"
        );
    }
    #[tokio::test]
    async fn git_inspection_reports_untracked_paths_and_non_repositories() {
        let dir = tempfile::tempdir().unwrap();
        assert!(git_diff(dir.path()).await.is_err());
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(dir.path().join("untracked.txt"), "unchanged by inspection").unwrap();
        let output = git_diff(dir.path()).await.unwrap();
        assert!(output.contains("?? untracked.txt"));
        assert_eq!(
            fs::read_to_string(dir.path().join("untracked.txt")).unwrap(),
            "unchanged by inspection"
        );
    }
    #[tokio::test]
    async fn export_reads_durable_conversation_and_escapes_script_markup() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let profile = bone::config::Profile::from_model("openai:gpt-4o-mini").unwrap();
        let mut engine = bone::runtime::Engine::open(
            &data,
            dir.path(),
            None,
            profile,
            "test".into(),
            Default::default(),
        )
        .unwrap();
        engine
            .post("hello </script><script>alert(1)</script>", None)
            .unwrap();
        let path = export(&data, &engine.state().id).unwrap();
        let html = fs::read_to_string(path).unwrap();
        assert!(html.contains("hello &lt;/script&gt;&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("root input"));
        assert!(preview("中".repeat(50_000)).ends_with("complete original remains in SQLite.]"));
    }
    #[test]
    fn file_index_ignores_generated_hidden_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), "").unwrap();
        fs::create_dir(dir.path().join("target")).unwrap();
        fs::write(dir.path().join("target/generated.rs"), "").unwrap();
        fs::write(dir.path().join(".hidden"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path(), dir.path().join("loop")).unwrap();
        assert_eq!(files(dir.path()).unwrap(), vec!["main.rs"]);
    }
}
