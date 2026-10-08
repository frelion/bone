//! Local UI persistence and read-only workspace inspection.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncReadExt;

const DRAFT_LIMIT: usize = 128 * 1024;
const ALL_DRAFTS_LIMIT: usize = 1024 * 1024;
const HISTORY_LIMIT: usize = 1024 * 1024;
const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
// JSON escaping can expand a byte to six ASCII bytes.
const SAVED_LIMIT: usize = 6 * (ALL_DRAFTS_LIMIT + HISTORY_LIMIT) + 32 * 1024;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedDraft {
    pub text: String,
    pub cursor: usize,
    pub selection: Option<(usize, usize)>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiSaved {
    pub draft: String,
    pub history: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<(usize, usize)>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub drafts: HashMap<String, SavedDraft>,
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
        saved
            .reply_to
            .as_ref()
            .is_none_or(|id| !id.is_empty() && id.len() <= 128),
        "invalid saved reply target"
    );
    ensure!(
        saved.history.len() <= 100
            && saved.history.iter().map(String::len).sum::<usize>() <= HISTORY_LIMIT,
        "input history exceeds 100 entries or 1 MiB"
    );
    ensure!(
        saved.drafts.len() <= 100
            && saved
                .drafts
                .iter()
                .all(|(target, draft)| { target.len() <= 128 && draft.text.len() <= DRAFT_LIMIT })
            && saved.draft.len()
                + saved
                    .drafts
                    .values()
                    .map(|draft| draft.text.len())
                    .sum::<usize>()
                <= ALL_DRAFTS_LIMIT,
        "saved drafts exceed 100 targets, 128 KiB each, or 1 MiB total"
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

/// Jump to a durable delivery without loading a long session's tool bodies.
pub fn latest_delivery(data: &Path, session: &str) -> Result<Option<bone::state::Event>> {
    use rusqlite::OptionalExtension;
    let connection = rusqlite::Connection::open_with_flags(
        data.join("sessions.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let payload: Option<String> = connection
        .query_row(
            "SELECT payload FROM events WHERE session_id = ?1
             AND json_extract(payload, '$.kind') = 'delivery'
             ORDER BY sequence DESC LIMIT 1",
            [session],
            |row| row.get(0),
        )
        .optional()?;
    payload
        .map(|text| serde_json::from_str(&text).context("invalid durable delivery record"))
        .transpose()
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
async fn read_output(
    stream: impl tokio::io::AsyncRead + Unpin,
    limit: usize,
    error: &str,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    stream
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(bytes.len() <= limit, "{error}");
    Ok(bytes)
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
    let (out, err) = tokio::try_join!(
        read_output(
            stdout,
            limit,
            "git output exceeds 2 MiB; narrow the workspace changes"
        ),
        read_output(stderr, 8192, "git error output exceeds 8 KiB"),
    )?;
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
        let paths = git_output(
            &root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
            256 * 1024,
        )
        .await?;
        let remaining = OUTPUT_LIMIT.saturating_sub(output.len());
        let inspect_root = root.clone();
        let additions =
            tokio::task::spawn_blocking(move || untracked_diff(&inspect_root, &paths, remaining))
                .await??;
        output.push_str(&additions);
        ensure!(output.len() <= OUTPUT_LIMIT, "git output exceeds 2 MiB");
        Ok(output)
    })
    .await
    .context("git inspection timed out after 10 seconds")?
}
// Inspect new files as part of the real workspace diff; do not run user diff drivers.
fn untracked_diff(root: &Path, paths: &str, limit: usize) -> Result<String> {
    let mut output = String::from("\nUntracked file contents\n");
    for relative in paths.split('\0').filter(|p| !p.is_empty()) {
        let path = root.join(relative);
        ensure!(path.starts_with(root), "invalid untracked file path");
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            continue;
        }
        let label = format!("{:?}", relative);
        let mut patch =
            format!("\ndiff --git a/{label} b/{label}\nnew file\n--- /dev/null\n+++ b/{label}\n");
        if metadata.is_symlink() {
            patch.push_str(&format!("+symlink → {}\n", fs::read_link(&path)?.display()));
        } else {
            ensure!(
                path.canonicalize()?.starts_with(root),
                "untracked file escapes workspace"
            );
            let mut bytes = Vec::new();
            fs::File::open(&path)?
                .take(256 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            let truncated = bytes.len() > 256 * 1024;
            bytes.truncate(256 * 1024);
            if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
                patch.push_str("[binary file; content not rendered]\n");
            } else {
                let text = std::str::from_utf8(&bytes)?;
                for line in text.lines() {
                    patch.push('+');
                    patch.push_str(line);
                    patch.push('\n');
                }
                if !text.is_empty() && !text.ends_with('\n') {
                    patch.push_str("\\ No newline at end of file\n");
                }
            }
            if truncated {
                patch.push_str(
                    "[file preview capped at 256 KiB; inspect the file for the complete content]\n",
                );
            }
        }
        if output.len() + patch.len() + 128 > limit {
            output.push_str("\n[remaining untracked content omitted: 2 MiB inspection limit]\n");
            break;
        }
        output.push_str(&patch);
    }
    if output.len() > limit {
        return Ok(String::new());
    }
    Ok(output)
}
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn text(value: &Value) -> String {
    project_text(value, "\n", true)
}
pub(super) fn native_text(value: &Value) -> String {
    project_text(value, "\n\n", false)
}
fn project_text(value: &Value, separator: &str, include_json: bool) -> String {
    match value {
        Value::Array(items) => items
            .iter()
            .map(|item| project_text(item, separator, include_json))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(separator),
        Value::Object(item) => match item.get("type").and_then(Value::as_str) {
            Some("text") => item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .into(),
            Some("reasoning" | "encrypted" | "toolcall") => String::new(),
            Some("json") if include_json => {
                item.get("value").map(Value::to_string).unwrap_or_default()
            }
            _ => item
                .get("content")
                .map(|content| project_text(content, separator, include_json))
                .unwrap_or_default(),
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
    let mut responses = HashMap::new();
    let mut events = Vec::new();
    let mut inputs = BTreeSet::new();
    let mut cursor = None;
    let mut source_bytes = 0usize;
    let mut html = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>BONE {}</title><style>body{{max-width:960px;margin:40px auto;padding:0 24px;font:16px system-ui;background:#101218;color:#e3e6ed}}article{{padding:16px 0;border-bottom:1px solid #303644}}pre{{white-space:pre-wrap;overflow-wrap:anywhere}}small{{color:#9ba6b9}}</style><body><h1>BONE session {}</h1>",
        escape(session),
        escape(session)
    );
    loop {
        let page = bone::history_page(data, session, cursor.as_deref(), 20)?;
        cursor = page.next_cursor;
        for mut event in page.events {
            let data = std::mem::take(&mut event.data);
            let body = match event.kind.as_str() {
                "input" if data["source"] == "user" => {
                    inputs.insert(event.id.clone());
                    text(&data["message"])
                }
                "model_message" => text(&data["response"]["choice"]),
                "delivery" => data["response_event"].as_str().unwrap_or_default().into(),
                "question" => data["question"].as_str().unwrap_or_default().into(),
                "failure" => data["error"].as_str().unwrap_or_default().into(),
                "input_paused" => data["text"].as_str().unwrap_or_default().into(),
                "tool_started" => format!(
                    "{} ({})",
                    data["tool_name"].as_str().unwrap_or("tool"),
                    data["effect"].as_str().unwrap_or("unknown")
                ),
                "tool_result" | "tool_reconciled" => preview(text(&data["message"])),
                _ => continue,
            };
            // Project readable content directly, retaining references until every
            // record in the session has been indexed. A transaction can append a
            // delivery before its referenced input or response.
            source_bytes =
                source_bytes.saturating_add(body.len() + serde_json::to_vec(&event)?.len());
            ensure!(
                source_bytes <= 32 * 1024 * 1024,
                "session export exceeds 32 MiB; original events remain in SQLite"
            );
            if event.kind == "model_message" {
                responses.insert(event.id, body);
                continue;
            }
            events.push((event, body));
        }
        if !page.has_more {
            break;
        }
        ensure!(cursor.is_some(), "history cursor did not advance");
    }
    for (event, mut body) in events {
        if matches!(event.kind.as_str(), "delivery" | "failure" | "input_paused")
            && !event
                .reply_to
                .as_ref()
                .is_some_and(|id| inputs.contains(id))
        {
            continue;
        }
        if event.kind == "delivery" {
            body = responses.get(&body).cloned().unwrap_or_default();
        }
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
#[path = "../../tests/unit/tui_services.rs"]
mod tests;
