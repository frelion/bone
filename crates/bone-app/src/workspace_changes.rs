use std::{
    ffi::OsStr,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Component, Path},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{
    Error, GitFileState, Result, WorkspaceBaseline, WorkspaceChangeCursor, WorkspaceChangePage,
    WorkspaceChangedFile, WorkspaceFileCursor, WorkspaceFileMedia, WorkspaceFilePage,
    WorkspaceFileSource,
};

const MAX_CHANGE_PAGE: usize = 512;
const MAX_FILE_BYTES: usize = 1024 * 1024;
const GIT_DEADLINE: Duration = Duration::from_secs(5);

struct BoundedBody {
    media: WorkspaceFileMedia,
    text: Option<String>,
    bytes_read: u64,
    total_bytes: Option<u64>,
    offset: u64,
    identity: String,
    has_more: bool,
}

pub(crate) fn changes(
    root: &Path,
    cursor: Option<WorkspaceChangeCursor>,
    limit: usize,
) -> Result<WorkspaceChangePage> {
    if !(1..=MAX_CHANGE_PAGE).contains(&limit) {
        return Err(Error::InvalidState(format!(
            "workspace change page limit must be between 1 and {MAX_CHANGE_PAGE}"
        )));
    }
    let root_identity = workspace_root_identity(root)?;
    let baseline = baseline(root)?;
    if baseline == WorkspaceBaseline::NotGit {
        return Ok(WorkspaceChangePage {
            baseline,
            files: Vec::new(),
            next_cursor: None,
        });
    }
    if let Some(cursor) = &cursor {
        validate_relative_path(&cursor.after)?;
        if cursor.baseline != baseline || cursor.root_identity != root_identity {
            return Err(Error::InvalidState(
                "workspace change continuation belongs to another workspace or baseline".into(),
            ));
        }
    }
    let after = cursor.as_ref().map(|cursor| cursor.after.as_str());
    let mut files = status_page(root, after, limit + 1, None)?;
    let next_cursor = if files.len() > limit {
        files.truncate(limit);
        files.last().map(|file| WorkspaceChangeCursor {
            after: file.path.clone(),
            baseline: baseline.clone(),
            root_identity,
        })
    } else {
        None
    };
    Ok(WorkspaceChangePage {
        baseline,
        files,
        next_cursor,
    })
}

pub(crate) fn file_page(
    root: &Path,
    path: &str,
    source: WorkspaceFileSource,
    cursor: Option<WorkspaceFileCursor>,
    max_bytes: usize,
) -> Result<WorkspaceFilePage> {
    if max_bytes == 0 || max_bytes > MAX_FILE_BYTES {
        return Err(Error::InvalidState(format!(
            "workspace file byte limit must be between 1 and {MAX_FILE_BYTES}"
        )));
    }
    validate_relative_path(path)?;
    let baseline = baseline(root)?;
    if baseline == WorkspaceBaseline::NotGit {
        return Err(Error::InvalidState(
            "workspace is not a Git repository".into(),
        ));
    }
    let changed = status_page(root, None, 2, Some(path))?;
    if !changed.iter().any(|entry| entry.path == path) {
        return Err(Error::InvalidState(
            "path is not a current workspace change".into(),
        ));
    }

    let offset = if let Some(cursor) = &cursor {
        if cursor.baseline != baseline || cursor.path != path || cursor.source != source {
            return Err(Error::InvalidState(
                "workspace file continuation belongs to another baseline, path, or source".into(),
            ));
        }
        cursor.offset
    } else {
        0
    };

    let body = match source {
        WorkspaceFileSource::WorkingTree => read_working_tree(root, path, offset, max_bytes)?,
        WorkspaceFileSource::DiffAgainstHead => {
            let WorkspaceBaseline::Git { head: Some(head) } = &baseline else {
                return Err(Error::InvalidState(
                    "a diff against HEAD is unavailable before the first commit".into(),
                ));
            };
            read_diff(root, path, head, offset, max_bytes)?
        }
    };
    if let Some(cursor) = &cursor
        && cursor.identity != body.identity
    {
        return Err(Error::InvalidState(
            "workspace file changed while it was being paged".into(),
        ));
    }
    let next_offset = body.offset.saturating_add(body.bytes_read);
    let next_cursor = body.has_more.then(|| WorkspaceFileCursor {
        baseline: baseline.clone(),
        path: path.to_owned(),
        source,
        offset: next_offset,
        identity: body.identity.clone(),
    });
    Ok(WorkspaceFilePage {
        baseline,
        path: path.to_owned(),
        source,
        media: body.media,
        text: body.text,
        offset: body.offset,
        bytes_read: body.bytes_read,
        total_bytes: body.total_bytes,
        next_cursor,
    })
}

fn baseline(root: &Path) -> Result<WorkspaceBaseline> {
    open_workspace_root(root)
        .map_err(|error| Error::InvalidState(format!("open workspace root: {error}")))?;
    let expected = root
        .canonicalize()
        .map_err(|error| Error::InvalidState(format!("resolve workspace root: {error}")))?;
    let top = git_output(
        root,
        ["rev-parse", "--path-format=absolute", "--show-toplevel"],
        4096,
    )?;
    if !top.success {
        return Ok(WorkspaceBaseline::NotGit);
    }
    let top = std::str::from_utf8(trim_ascii(&top.bytes))
        .map_err(|_| Error::InvalidState("Git returned a non-UTF-8 repository root".into()))?;
    let actual = Path::new(top)
        .canonicalize()
        .map_err(|error| Error::InvalidState(format!("resolve Git repository root: {error}")))?;
    if actual != expected {
        return Err(Error::InvalidState(
            "Git repository root does not match the opened workspace".into(),
        ));
    }
    let inside = git_output(root, ["rev-parse", "--is-inside-work-tree"], 64)?;
    if !inside.success || trim_ascii(&inside.bytes) != b"true" {
        return Ok(WorkspaceBaseline::NotGit);
    }
    let head = git_output(root, ["rev-parse", "--verify", "HEAD"], 256)?;
    let head = if head.success {
        Some(
            std::str::from_utf8(trim_ascii(&head.bytes))
                .map_err(|_| Error::InvalidState("Git returned a non-UTF-8 HEAD".into()))?
                .to_owned(),
        )
    } else {
        None
    };
    Ok(WorkspaceBaseline::Git { head })
}

fn status_page(
    root: &Path,
    after: Option<&str>,
    take: usize,
    pathspec: Option<&str>,
) -> Result<Vec<WorkspaceChangedFile>> {
    let mut command = git_command(root);
    command.args([
        "-c",
        "status.relativePaths=true",
        "-c",
        "core.quotepath=false",
        "-c",
        "core.fsmonitor=false",
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--",
    ]);
    let literal_pathspec = pathspec.map(|path| format!(":(literal){path}"));
    command.arg(literal_pathspec.as_deref().unwrap_or("."));
    command.stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| Error::InvalidState(format!("start Git status: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::InvalidState("Git status stdout was unavailable".into()))?;
    let after = after.map(str::to_owned);
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut result = Vec::with_capacity(take);
        let mut record = Vec::new();
        let parsed = loop {
            record.clear();
            let read = match reader.read_until(0, &mut record) {
                Ok(read) => read,
                Err(error) => {
                    break Err(Error::InvalidState(format!("read Git status: {error}")));
                }
            };
            if read == 0 {
                break Ok((result, false));
            }
            if record.last() == Some(&0) {
                record.pop();
            }
            if record.len() < 4 || record[2] != b' ' {
                break Err(Error::InvalidState(
                    "Git returned malformed status data".into(),
                ));
            }
            let index_byte = record[0];
            let worktree_byte = record[1];
            let path = match safe_git_path(&record[3..]) {
                Ok(path) => path,
                Err(error) => break Err(error),
            };
            if matches!(index_byte, b'R' | b'C') {
                let mut original = Vec::new();
                if let Err(error) = reader.read_until(0, &mut original) {
                    break Err(Error::InvalidState(format!(
                        "read Git rename status: {error}"
                    )));
                }
            }
            if after.as_deref().is_none_or(|after| path.as_str() > after) {
                result.push(WorkspaceChangedFile {
                    path,
                    tracked: index_byte != b'?' && worktree_byte != b'?',
                    index: file_state(index_byte),
                    worktree: file_state(worktree_byte),
                });
                if result.len() == take {
                    break Ok((result, true));
                }
            }
        };
        let _ = tx.send(parsed);
    });
    let started = Instant::now();
    let (result, reached_limit) = match rx.recv_timeout(GIT_DEADLINE) {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => {
            kill_and_wait(&mut child);
            finish_killed_reader(reader);
            return Err(error);
        }
        Err(_) => {
            kill_and_wait(&mut child);
            finish_killed_reader(reader);
            return Err(Error::InvalidState(
                "Git status exceeded its deadline".into(),
            ));
        }
    };
    if reached_limit {
        kill_and_wait(&mut child);
    } else {
        let status = wait_until_deadline(&mut child, started, "Git status")?;
        if !status.success() {
            return Err(Error::InvalidState("Git status failed".into()));
        }
    }
    let _ = reader.join();
    Ok(result)
}

fn file_state(value: u8) -> GitFileState {
    match value {
        b' ' => GitFileState::Unchanged,
        b'?' => GitFileState::Untracked,
        b'M' => GitFileState::Modified,
        b'A' => GitFileState::Added,
        b'D' => GitFileState::Deleted,
        b'R' => GitFileState::Renamed,
        b'C' => GitFileState::Copied,
        b'T' => GitFileState::TypeChanged,
        b'U' => GitFileState::Unmerged,
        _ => GitFileState::Unknown,
    }
}

fn read_working_tree(
    root: &Path,
    path: &str,
    offset: u64,
    max_bytes: usize,
) -> Result<BoundedBody> {
    let target = root.join(path);
    let metadata = std::fs::symlink_metadata(&target);
    let metadata = match metadata {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BoundedBody {
                media: WorkspaceFileMedia::Missing,
                text: None,
                bytes_read: 0,
                total_bytes: None,
                offset: 0,
                identity: "missing".into(),
                has_more: false,
            });
        }
        Err(error) => {
            return Err(Error::InvalidState(format!(
                "inspect workspace file {path}: {error}"
            )));
        }
    };
    if metadata.file_type().is_symlink() {
        let link = std::fs::read_link(&target)
            .map_err(|error| Error::InvalidState(format!("read workspace symlink: {error}")))?;
        let value = link.to_string_lossy().into_owned();
        let start = usize::try_from(offset)
            .map_err(|_| Error::InvalidState("workspace file offset is out of range".into()))?;
        if start > value.len() || !value.is_char_boundary(start) {
            return Err(Error::InvalidState(
                "workspace file offset is not a valid UTF-8 page boundary".into(),
            ));
        }
        let end = utf8_page_end(&value, start, max_bytes);
        return Ok(BoundedBody {
            media: WorkspaceFileMedia::Text,
            text: Some(value[start..end].to_owned()),
            bytes_read: (end - start) as u64,
            total_bytes: Some(value.len() as u64),
            offset,
            identity: hex_digest(value.as_bytes()),
            has_more: end < value.len(),
        });
    }
    let mut file = open_workspace_file(root, Path::new(path))
        .map_err(|error| Error::InvalidState(format!("open workspace file: {error}")))?;
    let opened_metadata = file
        .metadata()
        .map_err(|error| Error::InvalidState(format!("inspect opened workspace file: {error}")))?;
    let canonical = target
        .canonicalize()
        .map_err(|error| Error::InvalidState(format!("resolve workspace file: {error}")))?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| Error::InvalidState(format!("resolve workspace root: {error}")))?;
    if !canonical.starts_with(&canonical_root) || !opened_metadata.is_file() {
        return Err(Error::InvalidState(
            "workspace file does not resolve to a regular file inside the workspace".into(),
        ));
    }
    let identity = metadata_identity(&opened_metadata)?;
    let total = opened_metadata.len();
    if offset > total {
        return Err(Error::InvalidState(
            "workspace file offset is out of range".into(),
        ));
    }
    let mut probe = Vec::with_capacity(8 * 1024 + 4);
    file.by_ref()
        .take(8 * 1024 + 4)
        .read_to_end(&mut probe)
        .map_err(|error| Error::InvalidState(format!("probe workspace file: {error}")))?;
    if probe.contains(&0) || has_invalid_utf8(&probe) {
        return Ok(BoundedBody {
            media: WorkspaceFileMedia::Binary,
            text: None,
            bytes_read: 0,
            total_bytes: Some(total),
            offset,
            identity,
            has_more: false,
        });
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| Error::InvalidState(format!("seek workspace file: {error}")))?;
    let mut bytes = Vec::with_capacity(max_bytes.saturating_add(4));
    file.by_ref()
        .take(max_bytes as u64 + 4)
        .read_to_end(&mut bytes)
        .map_err(|error| Error::InvalidState(format!("read workspace file: {error}")))?;
    let end = bounded_utf8_end(&bytes, max_bytes)?;
    if end == 0 && offset < total {
        return Err(Error::InvalidState(
            "workspace file page budget cannot contain the next UTF-8 character".into(),
        ));
    }
    bytes.truncate(end);
    let bytes_read = bytes.len() as u64;
    let final_identity = metadata_identity(&file.metadata().map_err(|error| {
        Error::InvalidState(format!("reinspect opened workspace file: {error}"))
    })?)?;
    if final_identity != identity {
        return Err(Error::InvalidState(
            "workspace file changed while it was being read".into(),
        ));
    }
    Ok(BoundedBody {
        media: WorkspaceFileMedia::Text,
        text: Some(String::from_utf8(bytes).expect("UTF-8 checked above")),
        bytes_read,
        total_bytes: Some(total),
        offset,
        identity,
        has_more: offset.saturating_add(bytes_read) < total,
    })
}

fn read_diff(
    root: &Path,
    path: &str,
    head: &str,
    offset: u64,
    max_bytes: usize,
) -> Result<BoundedBody> {
    let pathspec = format!(":(literal){path}");
    let binary = git_output(
        root,
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "-z",
            head,
            "--",
            pathspec.as_str(),
        ],
        4096,
    )?;
    if !binary.success {
        return Err(Error::InvalidState(
            "Git could not inspect the file diff".into(),
        ));
    }
    if binary.bytes.starts_with(b"-\t-\t") {
        return Ok(BoundedBody {
            media: WorkspaceFileMedia::Binary,
            text: None,
            bytes_read: 0,
            total_bytes: None,
            offset,
            identity: diff_identity(root, path, head)?,
            has_more: false,
        });
    }
    let identity = diff_identity(root, path, head)?;
    let diff = git_output_slice(
        root,
        [
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            head,
            "--",
            pathspec.as_str(),
        ],
        offset,
        max_bytes.saturating_add(4),
    )?;
    if !diff.success {
        return Err(Error::InvalidState(
            "Git could not read the file diff".into(),
        ));
    }
    let available = diff.bytes.as_slice();
    let end = bounded_utf8_end(available, max_bytes)?;
    if end == 0 && (diff.truncated || !diff.bytes.is_empty()) {
        return Err(Error::InvalidState(
            "workspace diff page budget cannot contain the next UTF-8 character".into(),
        ));
    }
    let bytes = &available[..end];
    let bytes_read = bytes.len() as u64;
    Ok(BoundedBody {
        media: WorkspaceFileMedia::Text,
        text: Some(String::from_utf8(bytes.to_vec()).expect("UTF-8 checked above")),
        bytes_read,
        total_bytes: None,
        offset,
        identity,
        has_more: diff.truncated || end < diff.bytes.len(),
    })
}

struct CommandOutput {
    success: bool,
    bytes: Vec<u8>,
    truncated: bool,
}

fn git_output<I, S>(root: &Path, args: I, limit: usize) -> Result<CommandOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git_output_slice(root, args, 0, limit)
}

fn git_output_slice<I, S>(root: &Path, args: I, skip: u64, limit: usize) -> Result<CommandOutput>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = git_command(root);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| Error::InvalidState(format!("start Git: {error}")))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::InvalidState("Git stdout was unavailable".into()))?;
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
        let result = (|| {
            let skipped = std::io::copy(&mut stdout.by_ref().take(skip), &mut std::io::sink())?;
            if skipped == skip {
                stdout
                    .by_ref()
                    .take(limit as u64 + 1)
                    .read_to_end(&mut bytes)?;
            }
            Ok::<_, std::io::Error>(bytes)
        })();
        let _ = tx.send(result);
    });
    let started = Instant::now();
    let mut bytes = match rx.recv_timeout(GIT_DEADLINE) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => {
            kill_and_wait(&mut child);
            finish_killed_reader(reader);
            return Err(Error::InvalidState(format!("read Git output: {error}")));
        }
        Err(_) => {
            kill_and_wait(&mut child);
            finish_killed_reader(reader);
            return Err(Error::InvalidState(
                "Git command exceeded its deadline".into(),
            ));
        }
    };
    let truncated = bytes.len() > limit;
    if truncated {
        bytes.truncate(limit);
        kill_and_wait(&mut child);
        finish_killed_reader(reader);
        return Ok(CommandOutput {
            success: true,
            bytes,
            truncated: true,
        });
    }
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| Error::InvalidState(format!("wait for Git: {error}")))?
        {
            break status;
        }
        if started.elapsed() >= GIT_DEADLINE {
            kill_and_wait(&mut child);
            finish_killed_reader(reader);
            return Err(Error::InvalidState(
                "Git command exceeded its deadline".into(),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    };
    let _ = reader.join();
    Ok(CommandOutput {
        success: status.success(),
        bytes,
        truncated: false,
    })
}

fn git_command(root: &Path) -> Command {
    #[cfg(test)]
    let program = std::env::var_os("BONE_APP_TEST_GIT").unwrap_or_else(|| "git".into());
    #[cfg(not(test))]
    let program = OsStr::new("git");
    let mut command = Command::new(program);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.arg("-C").arg(root);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_COMMON_DIR",
        "GIT_NAMESPACE",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_EXEC_PATH",
        "GIT_PREFIX",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    ] {
        command.env_remove(name);
    }
    command.env("LC_ALL", "C");
    command.env("GIT_OPTIONAL_LOCKS", "0");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env(
        "GIT_CONFIG_GLOBAL",
        if cfg!(windows) { "NUL" } else { "/dev/null" },
    );
    command
}

fn kill_and_wait(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn finish_killed_reader<T>(reader: thread::JoinHandle<T>) {
    let _ = reader.join();
}

#[cfg(not(unix))]
fn finish_killed_reader<T>(reader: thread::JoinHandle<T>) {
    drop(reader);
}

fn wait_until_deadline(
    child: &mut std::process::Child,
    started: Instant,
    label: &str,
) -> Result<std::process::ExitStatus> {
    wait_with_deadline(child, started, GIT_DEADLINE, label)
}

fn wait_with_deadline(
    child: &mut std::process::Child,
    started: Instant,
    deadline: Duration,
    label: &str,
) -> Result<std::process::ExitStatus> {
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| Error::InvalidState(format!("wait for {label}: {error}")))?
        {
            return Ok(status);
        }
        if started.elapsed() >= deadline {
            kill_and_wait(child);
            return Err(Error::InvalidState(format!(
                "{label} exceeded its deadline"
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn bounded_utf8_end(bytes: &[u8], max_bytes: usize) -> Result<usize> {
    let candidate = bytes.len().min(max_bytes);
    match std::str::from_utf8(&bytes[..candidate]) {
        Ok(_) => Ok(candidate),
        Err(error) if error.error_len().is_none() => Ok(error.valid_up_to()),
        Err(_) => Err(Error::InvalidState(
            "workspace text contains invalid UTF-8".into(),
        )),
    }
}

fn has_invalid_utf8(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_err_and(|error| error.error_len().is_some())
}

fn utf8_page_end(value: &str, start: usize, max_bytes: usize) -> usize {
    let mut end = start.saturating_add(max_bytes).min(value.len());
    while end > start && !value.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn diff_identity(root: &Path, path: &str, head: &str) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(head.as_bytes());
    digest.update([0]);
    digest.update(path.as_bytes());
    if let Ok(metadata) = std::fs::symlink_metadata(root.join(path)) {
        digest.update(metadata_identity(&metadata)?.as_bytes());
    } else {
        digest.update(b"missing");
    }
    let index = git_output(root, ["rev-parse", "--git-path", "index"], 4096)?;
    if index.success
        && let Ok(index_path) = std::str::from_utf8(trim_ascii(&index.bytes))
        && let Ok(metadata) = std::fs::metadata(if Path::new(index_path).is_absolute() {
            Path::new(index_path).to_owned()
        } else {
            root.join(index_path)
        })
    {
        digest.update(metadata_identity(&metadata)?.as_bytes());
    }
    Ok(hex_digest(&digest.finalize()))
}

#[cfg(unix)]
fn metadata_identity(metadata: &std::fs::Metadata) -> Result<String> {
    use std::os::unix::fs::MetadataExt;

    Ok(format!(
        "{}:{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    ))
}

#[cfg(not(unix))]
fn metadata_identity(metadata: &std::fs::Metadata) -> Result<String> {
    use std::time::UNIX_EPOCH;

    let modified = metadata
        .modified()
        .map_err(|error| Error::InvalidState(format!("read file modification time: {error}")))?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::InvalidState("file modification time predates the epoch".into()))?;
    Ok(format!(
        "{}:{}:{}",
        metadata.len(),
        modified.as_secs(),
        modified.subsec_nanos()
    ))
}

#[cfg(unix)]
fn open_workspace_file(root: &Path, relative: &Path) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};

    let mut directory = open_workspace_root(root)?;
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace path is not relative and normalized",
            ));
        };
        let last = components.peek().is_none();
        let flags = OFlags::RDONLY
            | OFlags::CLOEXEC
            | OFlags::NOFOLLOW
            | if last {
                OFlags::empty()
            } else {
                OFlags::DIRECTORY
            };
        let opened =
            openat(&directory, name, flags, Mode::empty()).map_err(std::io::Error::from)?;
        if last {
            return Ok(File::from(opened));
        }
        directory = File::from(opened);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        "workspace path is empty",
    ))
}

#[cfg(unix)]
fn open_workspace_root(root: &Path) -> std::io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};

    open(
        root,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(std::io::Error::from)
}

#[cfg(unix)]
fn workspace_root_identity(root: &Path) -> Result<String> {
    use std::os::unix::fs::MetadataExt;

    let metadata = open_workspace_root(root)
        .and_then(|directory| directory.metadata())
        .map_err(|error| Error::InvalidState(format!("open workspace root: {error}")))?;
    Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn open_workspace_file(root: &Path, relative: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .open(root.join(relative))
}

#[cfg(not(unix))]
fn open_workspace_root(root: &Path) -> std::io::Result<File> {
    File::open(root)
}

#[cfg(not(unix))]
fn workspace_root_identity(root: &Path) -> Result<String> {
    root.canonicalize()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| Error::InvalidState(format!("resolve workspace root: {error}")))
}

fn safe_git_path(bytes: &[u8]) -> Result<String> {
    let path = std::str::from_utf8(bytes)
        .map_err(|_| Error::InvalidState("Git path is not valid UTF-8".into()))?;
    validate_relative_path(path)?;
    Ok(path.to_owned())
}

fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty() || path.chars().any(char::is_control) || path.contains('\\') {
        return Err(Error::InvalidState("unsafe workspace-relative path".into()));
    }
    if Path::new(path)
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(Error::InvalidState("unsafe workspace-relative path".into()));
    }
    Ok(())
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, process::Command};

    use super::*;

    fn run_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env("LC_ALL", "C")
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> tempfile::TempDir {
        let temporary = tempfile::tempdir().expect("temporary repository");
        run_git(temporary.path(), &["init", "--quiet"]);
        run_git(
            temporary.path(),
            &["config", "user.email", "bone@example.invalid"],
        );
        run_git(temporary.path(), &["config", "user.name", "BONE test"]);
        std::fs::write(temporary.path().join("tracked.txt"), "old\n")
            .expect("write tracked fixture");
        run_git(temporary.path(), &["add", "tracked.txt"]);
        run_git(temporary.path(), &["commit", "--quiet", "-m", "baseline"]);
        temporary
    }

    #[test]
    fn utf8_split_at_page_and_probe_boundaries_remains_text() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let mut value = "a".repeat(8 * 1024 - 1);
        value.push('😀');
        value.push_str("tail");
        std::fs::write(temporary.path().join("utf8.txt"), &value).expect("write UTF-8 fixture");

        let first = read_working_tree(temporary.path(), "utf8.txt", 0, 8 * 1024)
            .expect("read first text page");
        assert_eq!(first.media, WorkspaceFileMedia::Text);
        assert_eq!(first.bytes_read, (8 * 1024 - 1) as u64);
        assert!(first.has_more);
        let second = read_working_tree(temporary.path(), "utf8.txt", first.bytes_read, 16)
            .expect("read second text page");
        assert_eq!(second.text.as_deref(), Some("😀tail"));
        assert!(!second.has_more);
    }

    #[test]
    fn file_continuation_is_bound_and_rejects_changed_content() {
        let repository = repository();
        std::fs::write(
            repository.path().join("tracked.txt"),
            "first page\nsecond page\n",
        )
        .expect("modify fixture");
        let first = file_page(
            repository.path(),
            "tracked.txt",
            WorkspaceFileSource::WorkingTree,
            None,
            8,
        )
        .expect("first page");
        let cursor = first.next_cursor.expect("continuation");
        assert_eq!(first.text.as_deref(), Some("first pa"));

        std::fs::write(
            repository.path().join("tracked.txt"),
            "changed!\nsecond page\n",
        )
        .expect("change fixture between pages");
        assert!(
            file_page(
                repository.path(),
                "tracked.txt",
                WorkspaceFileSource::WorkingTree,
                Some(cursor),
                8,
            )
            .unwrap_err()
            .to_string()
            .contains("changed")
        );
    }

    #[test]
    fn diff_pages_reassemble_and_cursor_rejects_another_source() {
        let repository = repository();
        std::fs::write(
            repository.path().join("tracked.txt"),
            "new line one\nnew line two\nnew line three\n",
        )
        .expect("modify fixture");
        let mut cursor = None;
        let mut combined = String::new();
        loop {
            let page = file_page(
                repository.path(),
                "tracked.txt",
                WorkspaceFileSource::DiffAgainstHead,
                cursor,
                17,
            )
            .expect("read diff page");
            combined.push_str(page.text.as_deref().expect("text diff"));
            let Some(next) = page.next_cursor else { break };
            if combined.is_empty() {
                unreachable!();
            }
            cursor = Some(next);
        }
        assert!(combined.contains("+new line three"));

        let first = file_page(
            repository.path(),
            "tracked.txt",
            WorkspaceFileSource::DiffAgainstHead,
            None,
            17,
        )
        .expect("fresh diff page");
        assert!(
            file_page(
                repository.path(),
                "tracked.txt",
                WorkspaceFileSource::WorkingTree,
                first.next_cursor,
                17,
            )
            .is_err()
        );
    }

    #[test]
    fn canonical_git_toplevel_must_equal_the_opened_workspace() {
        let repository = repository();
        let nested = repository.path().join("nested");
        std::fs::create_dir(&nested).expect("nested workspace");
        assert!(changes(&nested, None, 16).is_err());
    }

    #[test]
    fn change_cursor_rejects_another_workspace_and_changed_head() {
        let first = repository();
        std::fs::write(first.path().join("tracked.txt"), "changed\n").expect("modify fixture");
        std::fs::write(first.path().join("untracked.txt"), "new\n").expect("new fixture");
        let cursor = changes(first.path(), None, 1)
            .expect("first change page")
            .next_cursor
            .expect("continuation");

        let second = repository();
        std::fs::write(second.path().join("tracked.txt"), "other\n").expect("modify other");
        assert!(changes(second.path(), Some(cursor.clone()), 1).is_err());

        run_git(first.path(), &["add", "tracked.txt"]);
        run_git(first.path(), &["commit", "--quiet", "-m", "new baseline"]);
        assert!(changes(first.path(), Some(cursor), 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn replaced_workspace_root_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let repository = repository();
        let original = repository.path().to_owned();
        let moved = original.with_extension("moved");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::rename(&original, &moved).expect("move registered root");
        symlink(outside.path(), &original).expect("replace root with symlink");
        assert!(changes(&original, None, 16).is_err());
        std::fs::remove_file(&original).expect("remove replacement symlink");
        std::fs::rename(&moved, &original).expect("restore tempdir root");
    }

    #[cfg(unix)]
    #[test]
    fn parent_symlink_cannot_escape_the_workspace_reader() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::write(outside.path().join("secret"), "must not be read").expect("outside fixture");
        symlink(outside.path(), workspace.path().join("escape")).expect("parent symlink");
        assert!(read_working_tree(workspace.path(), "escape/secret", 0, 64).is_err());
    }

    #[test]
    fn git_command_clears_repository_redirection_and_global_config() {
        let command = git_command(Path::new("."));
        let env = command.get_envs().collect::<Vec<_>>();
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            assert!(
                env.iter()
                    .any(|(key, value)| { *key == OsStr::new(name) && value.is_none() })
            );
        }
        assert!(env.iter().any(|(key, value)| {
            *key == OsStr::new("GIT_CONFIG_GLOBAL")
                && value.is_some_and(|value| {
                    value == OsStr::new(if cfg!(windows) { "NUL" } else { "/dev/null" })
                })
        }));
    }

    #[test]
    fn inherited_git_redirection_cannot_escape_the_workspace() {
        const CHILD_ROOT: &str = "BONE_APP_HOSTILE_GIT_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let page = changes(Path::new(&root), None, 16).expect("query intended repository");
            assert!(page.files.iter().any(|file| file.path == "tracked.txt"));
            assert!(!page.files.iter().any(|file| file.path == "hostile.txt"));
            return;
        }

        let intended = repository();
        std::fs::write(intended.path().join("tracked.txt"), "intended change\n")
            .expect("modify intended repository");
        let hostile = repository();
        std::fs::write(hostile.path().join("hostile.txt"), "hostile\n")
            .expect("write hostile fixture");

        let output = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                "workspace_changes::tests::inherited_git_redirection_cannot_escape_the_workspace",
                "--nocapture",
            ])
            .env(CHILD_ROOT, intended.path())
            .env("GIT_DIR", hostile.path().join(".git"))
            .env("GIT_WORK_TREE", hostile.path())
            .env("GIT_INDEX_FILE", hostile.path().join(".git/index"))
            .output()
            .expect("run hostile environment child");
        assert!(
            output.status.success(),
            "hostile environment child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn deadline_kills_and_reaps_a_stuck_child() {
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .spawn()
            .expect("spawn stuck fixture");
        let error = wait_with_deadline(
            &mut child,
            Instant::now(),
            Duration::from_millis(20),
            "fixture",
        )
        .unwrap_err();
        assert!(error.to_string().contains("deadline"));
        assert!(child.try_wait().expect("child was reaped").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn git_deadline_kills_descendants_inheriting_stdout() {
        use std::os::unix::fs::PermissionsExt;

        const CHILD_GIT: &str = "BONE_APP_FAKE_GIT";
        if std::env::var_os(CHILD_GIT).is_some() {
            let started = Instant::now();
            assert!(git_output(Path::new("."), ["status"], 64).is_err());
            assert!(
                started.elapsed() < Duration::from_secs(6),
                "Git timeout must include descendant stdout cleanup"
            );
            return;
        }

        let temporary = tempfile::tempdir().expect("fake git directory");
        let git = temporary.path().join("git");
        std::fs::write(&git, "#!/bin/sh\n(sleep 30) &\nexit 0\n").expect("write fake git");
        let mut permissions = std::fs::metadata(&git).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&git, permissions).expect("make fake git executable");
        let output = Command::new(std::env::current_exe().expect("current test executable"))
            .args([
                "--exact",
                "workspace_changes::tests::git_deadline_kills_descendants_inheriting_stdout",
                "--nocapture",
            ])
            .env(CHILD_GIT, "1")
            .env("BONE_APP_TEST_GIT", &git)
            .output()
            .expect("run isolated fake Git test");
        assert!(
            output.status.success(),
            "fake Git child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
