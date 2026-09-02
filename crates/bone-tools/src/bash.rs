use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fmt, io,
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use rig_core::tool::{PortableTool, ToolExecutionError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    task::JoinHandle,
};

use crate::{ToolEnvironment, ToolError};

const SHELL: &str = "bash";
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const READ_CHUNK_BYTES: usize = 8 * 1024;
const SAFE_INHERITED_ENVIRONMENT: &[&str] = &[
    "PATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_COLLATE",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "TERM",
    "COLORTERM",
    "NO_COLOR",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SYSTEMROOT",
    "WINDIR",
    "PATHEXT",
];

/// Arguments accepted by [`BashTool`].
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BashArgs {
    /// Exact Bash source passed to `bash -c`.
    pub command: String,
    /// Working directory inside the workspace; defaults to the workspace root.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Command deadline in seconds.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// Structured result of one non-interactive Bash command.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct BashOutput {
    pub stdout: String,
    pub stderr: String,
    /// `None` when the process was terminated by a signal.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// Whether either stream exceeded its retained byte budget or could not be fully drained.
    pub truncated: bool,
}

/// Execute a bounded, non-interactive Bash command in the workspace.
///
/// This tool does not implement authorization or an OS sandbox. The workspace
/// boundary constrains the requested initial working directory only; a caller
/// must apply any command approval and sandbox policy before invoking it. The
/// default child environment is cleared and rebuilt from a small fixed
/// allowlist; use [`BashTool::with_process_environment`] when the runtime needs
/// an explicit replacement environment.
/// Unix builds clean up the spawned process group. Other platforms guarantee
/// cleanup of only the direct child until the host supplies an equivalent
/// process-tree primitive such as a Windows Job Object.
#[derive(Clone)]
pub struct BashTool {
    environment: ToolEnvironment,
    process_environment: Arc<BTreeMap<OsString, OsString>>,
}

impl BashTool {
    /// Create a Bash tool with a sanitized child environment.
    ///
    /// The child environment is cleared before a small usability allowlist is
    /// copied from the host. Credentials, proxy settings, `HOME`, `BASH_ENV`,
    /// and SSH-agent variables are not inherited by default.
    pub fn new(environment: ToolEnvironment) -> Self {
        Self {
            environment,
            process_environment: Arc::new(sanitized_process_environment()),
        }
    }

    /// Create a Bash tool with an explicit replacement child environment.
    ///
    /// No host variables are added implicitly. The runtime should include
    /// `PATH` and any other values its commands require. Environment values are
    /// deliberately omitted from this tool's [`Debug`](std::fmt::Debug) output.
    pub fn with_process_environment<I, K, V>(
        environment: ToolEnvironment,
        variables: I,
    ) -> Result<Self, ToolError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<OsString>,
        V: Into<OsString>,
    {
        let mut process_environment = BTreeMap::new();
        for (name, value) in variables {
            let name = name.into();
            let value = value.into();
            validate_environment_variable(&name, &value)?;

            #[cfg(windows)]
            if process_environment
                .keys()
                .any(|existing: &OsString| os_eq_ignore_ascii_case(existing, &name))
            {
                return Err(ToolError::InvalidArgs(format!(
                    "duplicate process environment variable: {}",
                    name.to_string_lossy()
                )));
            }

            #[cfg(not(windows))]
            if process_environment.contains_key(&name) {
                return Err(ToolError::InvalidArgs(format!(
                    "duplicate process environment variable: {}",
                    name.to_string_lossy()
                )));
            }

            process_environment.insert(name, value);
        }
        Ok(Self {
            environment,
            process_environment: Arc::new(process_environment),
        })
    }

    async fn resolve_cwd(&self, requested: Option<&str>) -> Result<PathBuf, ToolError> {
        let path = match requested {
            Some(path) => {
                self.environment
                    .workspace
                    .resolve_existing(path)
                    .await?
                    .absolute
            }
            None => self.environment.workspace.root().to_path_buf(),
        };

        let metadata = tokio::fs::metadata(&path).await.map_err(|error| {
            ToolError::io_display(
                "inspect working directory",
                self.environment.workspace.display(&path),
                error,
            )
        })?;
        if !metadata.is_dir() {
            return Err(ToolError::InvalidArgs(format!(
                "working directory is not a directory: {}",
                self.environment.workspace.display(&path)
            )));
        }
        Ok(path)
    }

    fn timeout(&self, requested_secs: Option<u64>) -> Result<Duration, ToolError> {
        let Some(seconds) = requested_secs else {
            return Ok(self.environment.limits.default_bash_timeout);
        };
        if seconds == 0 {
            return Err(ToolError::InvalidArgs(
                "timeout_secs must be greater than zero".to_owned(),
            ));
        }
        let timeout = Duration::from_secs(seconds);
        if timeout > self.environment.limits.max_bash_timeout {
            return Err(ToolError::InvalidArgs(format!(
                "timeout_secs must not exceed {}",
                display_duration(self.environment.limits.max_bash_timeout)
            )));
        }
        Ok(timeout)
    }

    async fn execute(
        &self,
        command_source: String,
        cwd: &Path,
        deadline: Duration,
    ) -> Result<BashOutput, ToolError> {
        let mut command = Command::new(SHELL);
        command
            .arg("-c")
            .arg(command_source)
            .current_dir(cwd)
            .env_clear()
            .envs(self.process_environment.iter())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // A fresh process group lets timeout and cancellation terminate Bash
        // together with every descendant that has not deliberately escaped it.
        #[cfg(unix)]
        command.process_group(0);

        let child = command.spawn().map_err(|source| ToolError::Spawn {
            shell: SHELL.to_owned(),
            source,
        })?;
        let mut child = ChildGuard::new(child);

        let stdout = child
            .child_mut()
            .stdout
            .take()
            .ok_or_else(|| ToolError::Task("Bash stdout pipe was unavailable".to_owned()))?;
        let stderr = child
            .child_mut()
            .stderr
            .take()
            .ok_or_else(|| ToolError::Task("Bash stderr pipe was unavailable".to_owned()))?;
        let stdout = CaptureTask::spawn(stdout, self.environment.limits.max_output_bytes);
        let stderr = CaptureTask::spawn(stderr, self.environment.limits.max_output_bytes);

        let (status, timed_out) = child.wait_with_timeout(deadline).await?;

        // `bash -c 'server &'` may let the group leader exit while descendants
        // remain. A one-shot tool never transfers ownership of such processes,
        // so remove any remaining group members before releasing the guard.
        #[cfg(unix)]
        child
            .start_kill_tree()
            .map_err(|error| process_error("clean up Bash process group", error))?;
        child.disarm();

        let (stdout, stderr) = tokio::join!(stdout.finish("stdout"), stderr.finish("stderr"));
        let stdout = stdout?;
        let stderr = stderr?;
        let (stdout_text, stdout_truncated) =
            stdout.into_text(self.environment.limits.max_output_bytes);
        let (stderr_text, stderr_truncated) =
            stderr.into_text(self.environment.limits.max_output_bytes);

        Ok(BashOutput {
            stdout: stdout_text,
            stderr: stderr_text,
            exit_code: status.code(),
            timed_out,
            truncated: stdout_truncated || stderr_truncated,
        })
    }
}

impl fmt::Debug for BashTool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BashTool")
            .field("environment", &self.environment)
            .field(
                "process_environment_variable_count",
                &self.process_environment.len(),
            )
            .finish()
    }
}

impl PortableTool for BashTool {
    const NAME: &'static str = "bash";
    type Args = BashArgs;
    type Output = BashOutput;
    type Error = ToolError;

    fn description(&self) -> String {
        format!(
            "Execute one non-interactive Bash command in the workspace with a sanitized child environment. Returns bounded stdout and stderr, exit code, timeout state, and truncation state. Commands are limited to {} UTF-8 bytes, each output stream to {} bytes, and at most {}. Unix timeout or cancellation kills the process group; on non-Unix only the direct Bash child is guaranteed to terminate and descendants may survive.",
            self.environment.limits.max_bash_command_bytes,
            self.environment.limits.max_output_bytes,
            display_duration(self.environment.limits.max_bash_timeout)
        )
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "minLength": 1,
                    "description": format!("Exact Bash source to execute; at most {} UTF-8 bytes", self.environment.limits.max_bash_command_bytes)
                },
                "cwd": {
                    "type": "string",
                    "description": "Working directory relative to the workspace, or an absolute directory inside it"
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": self.environment.limits.max_bash_timeout.as_secs(),
                    "description": "Execution timeout in seconds"
                }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn map_error(&self, error: Self::Error) -> ToolExecutionError {
        error.into_execution_error()
    }

    async fn call(&self, arguments: Self::Args) -> Result<Self::Output, Self::Error> {
        if arguments.command.trim().is_empty() {
            return Err(ToolError::InvalidArgs(
                "command must not be empty".to_owned(),
            ));
        }
        if arguments.command.contains('\0') {
            return Err(ToolError::InvalidArgs(
                "command must not contain a NUL character".to_owned(),
            ));
        }
        if arguments.command.len() > self.environment.limits.max_bash_command_bytes {
            return Err(ToolError::InvalidArgs(format!(
                "command exceeds the {} byte limit",
                self.environment.limits.max_bash_command_bytes
            )));
        }

        let timeout = self.timeout(arguments.timeout_secs)?;
        let cwd = self.resolve_cwd(arguments.cwd.as_deref()).await?;
        self.execute(arguments.command, &cwd, timeout).await
    }
}

fn sanitized_process_environment() -> BTreeMap<OsString, OsString> {
    SAFE_INHERITED_ENVIRONMENT
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
        .collect()
}

fn validate_environment_variable(name: &OsStr, value: &OsStr) -> Result<(), ToolError> {
    let name_bytes = name.as_encoded_bytes();
    if name_bytes.is_empty() {
        return Err(ToolError::InvalidArgs(
            "process environment variable name must not be empty".to_owned(),
        ));
    }
    if name_bytes.contains(&b'=') {
        return Err(ToolError::InvalidArgs(format!(
            "process environment variable name must not contain `=`: {}",
            name.to_string_lossy()
        )));
    }
    if name_bytes.contains(&0) {
        return Err(ToolError::InvalidArgs(
            "process environment variable name must not contain a NUL character".to_owned(),
        ));
    }
    if value.as_encoded_bytes().contains(&0) {
        return Err(ToolError::InvalidArgs(format!(
            "process environment variable value must not contain a NUL character: {}",
            name.to_string_lossy()
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn os_eq_ignore_ascii_case(left: &OsStr, right: &OsStr) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn display_duration(duration: Duration) -> String {
    if duration.subsec_nanos() == 0 {
        format!("{} seconds", duration.as_secs())
    } else {
        format!("{} seconds", duration.as_secs_f64())
    }
}

fn process_error(operation: &'static str, error: io::Error) -> ToolError {
    ToolError::Task(format!("{operation}: {error}"))
}

struct ChildGuard {
    child: Option<Child>,
    #[cfg(unix)]
    process_group: Option<libc::pid_t>,
    armed: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        #[cfg(unix)]
        let process_group = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok());

        Self {
            child: Some(child),
            #[cfg(unix)]
            process_group,
            armed: true,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child
            .as_mut()
            .expect("child is retained until the guard is dropped")
    }

    fn start_kill_tree(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }

        #[cfg(unix)]
        let group_result = kill_process_group(self.process_group);

        // Always attempt the direct child too. This is the fallback on Windows
        // and also lets Tokio track that its child is expected to exit.
        let child_result = match self.child_mut().start_kill() {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
            Err(error) => Err(error),
        };

        #[cfg(unix)]
        group_result?;
        child_result
    }

    async fn wait_with_timeout(
        &mut self,
        deadline: Duration,
    ) -> Result<(ExitStatus, bool), ToolError> {
        match tokio::time::timeout(deadline, self.child_mut().wait()).await {
            Ok(status) => status
                .map(|status| (status, false))
                .map_err(|error| process_error("wait for Bash", error)),
            Err(_) => {
                // Kill first, but always attempt to reap even when signaling
                // reports an error so the direct child cannot become a zombie.
                let kill_result = self.start_kill_tree();
                let wait_result = self.child_mut().wait().await;
                kill_result.map_err(|error| process_error("terminate timed-out Bash", error))?;
                let status =
                    wait_result.map_err(|error| process_error("reap timed-out Bash", error))?;
                Ok((status, true))
            }
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        #[cfg(unix)]
        let _ = kill_process_group(self.process_group);

        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = child.start_kill();

        // Drop cannot await. When cancellation happens under Tokio, transfer
        // the killed child to a tiny reaper task; otherwise Tokio's own
        // kill-on-drop fallback still applies.
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            std::mem::drop(runtime.spawn(async move {
                let _ = child.wait().await;
            }));
        }
    }
}

#[cfg(unix)]
fn kill_process_group(process_group: Option<libc::pid_t>) -> io::Result<()> {
    let Some(process_group) = process_group else {
        return Ok(());
    };

    // SAFETY: `process_group` is the positive PID returned for a child spawned
    // with `process_group(0)`. Negating it addresses that process group only.
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[derive(Debug)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CapturedOutput {
    fn incomplete() -> Self {
        Self {
            bytes: Vec::new(),
            truncated: true,
        }
    }

    fn into_text(self, max_bytes: usize) -> (String, bool) {
        let mut text = String::from_utf8_lossy(&self.bytes).into_owned();
        let utf8_truncated = truncate_utf8(&mut text, max_bytes);
        (text, self.truncated || utf8_truncated)
    }
}

struct CaptureTask {
    handle: Option<JoinHandle<io::Result<CapturedOutput>>>,
}

impl CaptureTask {
    fn spawn<R>(reader: R, max_bytes: usize) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        Self {
            handle: Some(tokio::spawn(read_capped(reader, max_bytes))),
        }
    }

    async fn finish(mut self, stream: &'static str) -> Result<CapturedOutput, ToolError> {
        let mut handle = self
            .handle
            .take()
            .expect("capture task is consumed exactly once");
        match tokio::time::timeout(OUTPUT_DRAIN_TIMEOUT, &mut handle).await {
            Ok(Ok(Ok(output))) => Ok(output),
            Ok(Ok(Err(error))) => Err(process_error("read Bash output", error)),
            Ok(Err(error)) => Err(ToolError::Task(format!(
                "Bash {stream} capture task failed: {error}"
            ))),
            Err(_) => {
                handle.abort();
                let _ = handle.await;
                Ok(CapturedOutput::incomplete())
            }
        }
    }
}

impl Drop for CaptureTask {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

async fn read_capped<R>(mut reader: R, max_bytes: usize) -> io::Result<CapturedOutput>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(max_bytes.min(READ_CHUNK_BYTES));
    let mut truncated = false;
    let mut chunk = [0_u8; READ_CHUNK_BYTES];

    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = max_bytes.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&chunk[..retained]);
        truncated |= retained < read;

        // Continue draining after the cap so the child never blocks on a full pipe.
    }

    Ok(CapturedOutput { bytes, truncated })
}

fn truncate_utf8(value: &mut String, max_bytes: usize) -> bool {
    if value.len() <= max_bytes {
        return false;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    true
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use rig_core::tool::PortableTool;

    use super::*;
    use crate::ToolLimits;

    fn tool_with_timeout(root: &Path, timeout: Duration) -> BashTool {
        let limits = ToolLimits {
            default_bash_timeout: timeout,
            max_bash_timeout: Duration::from_secs(2),
            ..ToolLimits::default()
        };
        ToolEnvironment::with_limits(root, limits).unwrap().bash()
    }

    #[tokio::test]
    async fn captures_stdout_and_stderr() {
        let temp = tempfile::tempdir().unwrap();
        let tool = ToolEnvironment::new(temp.path()).unwrap().bash();

        let output = tool
            .call(BashArgs {
                command: "printf 'hello'; printf 'warning' >&2".to_owned(),
                cwd: None,
                timeout_secs: Some(2),
            })
            .await
            .unwrap();

        assert_eq!(output.stdout, "hello");
        assert_eq!(output.stderr, "warning");
        assert_eq!(output.exit_code, Some(0));
        assert!(!output.timed_out);
        assert!(!output.truncated);
    }

    #[tokio::test]
    async fn default_environment_excludes_sensitive_host_variables() {
        let temp = tempfile::tempdir().unwrap();
        let tool = ToolEnvironment::new(temp.path()).unwrap().bash();

        let output = tool
            .call(BashArgs {
                command:
                    "printf '%s|%s|%s' \"${HOME+set}\" \"${BASH_ENV+set}\" \"${SSH_AUTH_SOCK+set}\""
                        .to_owned(),
                cwd: None,
                timeout_secs: Some(2),
            })
            .await
            .unwrap();

        assert_eq!(output.stdout, "||");
    }

    #[tokio::test]
    async fn explicit_environment_is_a_complete_replacement_and_debug_is_redacted() {
        let temp = tempfile::tempdir().unwrap();
        let environment = ToolEnvironment::new(temp.path()).unwrap();
        let tool = BashTool::with_process_environment(
            environment,
            [("BONE_VISIBLE", "configured-secret-value")],
        )
        .unwrap();

        let debug = format!("{tool:?}");
        assert!(!debug.contains("configured-secret-value"));
        assert!(debug.contains("process_environment_variable_count"));

        let output = tool
            .call(BashArgs {
                command: "printf '%s|%s' \"$BONE_VISIBLE\" \"${HOME+set}\"".to_owned(),
                cwd: None,
                timeout_secs: Some(2),
            })
            .await
            .unwrap();
        assert_eq!(output.stdout, "configured-secret-value|");
    }

    #[test]
    fn sanitized_environment_contains_only_the_fixed_allowlist() {
        let environment = sanitized_process_environment();
        assert!(environment.keys().all(|name| {
            SAFE_INHERITED_ENVIRONMENT
                .iter()
                .any(|allowed| name == OsStr::new(allowed))
        }));
        assert!(!environment.contains_key(OsStr::new("HOME")));
        assert!(!environment.contains_key(OsStr::new("BASH_ENV")));
    }

    #[test]
    fn explicit_environment_rejects_invalid_or_duplicate_entries() {
        let temp = tempfile::tempdir().unwrap();
        let environment = ToolEnvironment::new(temp.path()).unwrap();

        assert!(matches!(
            BashTool::with_process_environment(environment.clone(), [("", "value")]),
            Err(ToolError::InvalidArgs(_))
        ));
        assert!(matches!(
            BashTool::with_process_environment(environment.clone(), [("BAD=NAME", "value")]),
            Err(ToolError::InvalidArgs(_))
        ));
        assert!(matches!(
            BashTool::with_process_environment(environment.clone(), [("NAME", "bad\0value")]),
            Err(ToolError::InvalidArgs(_))
        ));
        assert!(matches!(
            BashTool::with_process_environment(
                environment,
                [("DUPLICATE", "one"), ("DUPLICATE", "two")],
            ),
            Err(ToolError::InvalidArgs(_))
        ));
    }

    #[tokio::test]
    async fn preserves_non_zero_exit_as_structured_output() {
        let temp = tempfile::tempdir().unwrap();
        let tool = ToolEnvironment::new(temp.path()).unwrap().bash();

        let output = tool
            .call(BashArgs {
                command: "printf 'bad input' >&2; exit 7".to_owned(),
                cwd: None,
                timeout_secs: Some(2),
            })
            .await
            .unwrap();

        assert_eq!(output.stderr, "bad input");
        assert_eq!(output.exit_code, Some(7));
        assert!(!output.timed_out);
    }

    #[tokio::test]
    async fn rejects_nul_in_command_as_invalid_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let tool = ToolEnvironment::new(temp.path()).unwrap().bash();

        let result = tool
            .call(BashArgs {
                command: "printf '\0'".to_owned(),
                cwd: None,
                timeout_secs: Some(2),
            })
            .await;

        assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
    }

    #[tokio::test]
    async fn times_out_and_returns_promptly() {
        let temp = tempfile::tempdir().unwrap();
        let tool = tool_with_timeout(temp.path(), Duration::from_millis(50));
        let started = Instant::now();

        let output = tool
            .call(BashArgs {
                command: "printf 'started'; sleep 30".to_owned(),
                cwd: None,
                timeout_secs: None,
            })
            .await
            .unwrap();

        assert!(output.timed_out);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(output.stdout, "started");
        #[cfg(unix)]
        assert_eq!(output.exit_code, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timing_out_kills_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let tool = tool_with_timeout(temp.path(), Duration::from_millis(200));

        let output = tool
            .call(BashArgs {
                command: "sleep 30 & echo $! > child.pid; wait".to_owned(),
                cwd: None,
                timeout_secs: None,
            })
            .await
            .unwrap();

        assert!(output.timed_out);
        let descendant = wait_for_pid(&temp.path().join("child.pid")).await;
        assert!(wait_until_gone(descendant).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dropping_the_call_kills_descendants() {
        let temp = tempfile::tempdir().unwrap();
        let tool = tool_with_timeout(temp.path(), Duration::from_secs(2));
        let command = "sleep 30 & echo $! > child.pid; wait".to_owned();
        let task = tokio::spawn(async move {
            tool.call(BashArgs {
                command,
                cwd: None,
                timeout_secs: None,
            })
            .await
        });

        let pid_file = temp.path().join("child.pid");
        let descendant = wait_for_pid(&pid_file).await;
        task.abort();
        let _ = task.await;

        assert!(wait_until_gone(descendant).await);
    }

    #[cfg(unix)]
    async fn wait_for_pid(path: &Path) -> libc::pid_t {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(contents) = tokio::fs::read_to_string(path).await
                && let Ok(pid) = contents.trim().parse()
            {
                return pid;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "Bash did not record its descendant PID"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[cfg(unix)]
    async fn wait_until_gone(pid: libc::pid_t) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: signal zero only tests whether this test-owned PID exists.
            let result = unsafe { libc::kill(pid, 0) };
            if result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
