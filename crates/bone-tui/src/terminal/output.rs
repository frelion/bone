use std::io::{self, Write};

use base64::{Engine, engine::general_purpose::STANDARD};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PointerShape {
    Default,
    Text,
    Pointer,
    ResizeHorizontal,
}

impl PointerShape {
    pub(super) fn write(self, output: &mut impl Write) -> io::Result<()> {
        let name = match self {
            Self::Default => "default",
            Self::Text => "text",
            Self::Pointer => "pointer",
            Self::ResizeHorizontal => "ew-resize",
        };
        // Unsupported terminals may ignore this progressive enhancement. Never
        // query it: OSC replies are not input events understood by Crossterm.
        write!(output, "\x1b]22;{name}\x1b\\")?;
        output.flush()
    }
}

pub(super) fn reset_pointer(output: &mut impl Write) -> io::Result<()> {
    output.write_all(b"\x1b]22;\x1b\\")?;
    output.flush()
}

pub(crate) fn copy_text(text: &str) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    if std::env::var_os("SSH_CONNECTION").is_none()
        && std::env::var_os("SSH_CLIENT").is_none()
        && std::env::var_os("SSH_TTY").is_none()
    {
        return copy_local_macos(text);
    }
    copy_osc52(&mut io::stdout().lock(), text)
}

fn copy_osc52(output: &mut impl Write, text: &str) -> io::Result<()> {
    // Base64 preserves UTF-8 and prevents embedded controls from escaping OSC.
    // This writes to the client terminal, including over SSH; it never queries
    // the clipboard. Multiplexers and terminal policy may block the request.
    write!(output, "\x1b]52;c;{}\x1b\\", STANDARD.encode(text))?;
    output.flush()
}

#[cfg(target_os = "macos")]
fn copy_local_macos(text: &str) -> io::Result<()> {
    use std::process::{Command, Stdio};

    // Terminal.app need not support OSC 52. Local macOS uses its system tool;
    // SSH must instead address the remote client's clipboard through OSC 52.
    let mut child = Command::new("/usr/bin/pbcopy")
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let written = child
        .stdin
        .take()
        .expect("piped pbcopy stdin")
        .write_all(text.as_bytes());
    // Reap the child even when writing fails, and close stdin before waiting.
    let status = child.wait();
    written?;
    let status = status?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("pbcopy exited with {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_preserves_unicode_and_embedded_terminal_controls() {
        let text = "中文 👩‍💻\n\x1b]52;c;other\x07";
        let mut output = Vec::new();
        copy_osc52(&mut output, text).unwrap();
        let payload = output.strip_prefix(b"\x1b]52;c;").unwrap();
        let payload = payload.strip_suffix(b"\x1b\\").unwrap();
        assert_eq!(STANDARD.decode(payload).unwrap(), text.as_bytes());
        assert!(!payload.contains(&0x1b));
        assert!(!payload.contains(&0x07));
    }
}
