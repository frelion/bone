//! Device-flow OAuth helpers shared by the native (non-wasm) ChatGPT and
//! Copilot authenticators: on-disk JSON record caching, token expiry checks,
//! and the device-code prompt fallback.

use super::auth::AuthError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

/// Invokes the provider's device-code callback when one is registered,
/// otherwise prints `fallback_message` to stdout.
pub(crate) fn emit_device_code_prompt<P>(
    callback: Option<&Arc<dyn Fn(P) + Send + Sync>>,
    prompt: P,
    fallback_message: &str,
) {
    if let Some(callback) = callback {
        callback(prompt);
    } else {
        println!("{fallback_message}");
    }
}

pub(crate) fn ensure_parent_dir(path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

/// Returns true when the token is expired (or has no expiry), treating the
/// token as expired `skew_seconds` before its actual `expires_at`.
pub(crate) fn token_expired(expires_at: Option<i64>, skew_seconds: i64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default();

    match expires_at {
        Some(exp) => now >= exp - skew_seconds,
        None => true,
    }
}

/// Reads a JSON record from `path`, returning `T::default()` when no path is
/// configured or the file does not exist.
pub(crate) fn read_json_record<T: Default + DeserializeOwned>(
    path: Option<&Path>,
) -> Result<T, AuthError> {
    let Some(path) = path else {
        return Ok(T::default());
    };

    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(err) => Err(err.into()),
    }
}

/// Writes a JSON record to `path`, a no-op when no path is configured.
///
/// Writes a private sibling file and atomically replaces the destination.
/// Unix also syncs the directory entry after replacement.
pub(crate) fn write_json_record<T: Serialize>(
    path: Option<&Path>,
    record: &T,
) -> Result<(), AuthError> {
    let Some(path) = path else {
        return Ok(());
    };

    ensure_parent_dir(path)?;
    let bytes = serde_json::to_vec_pretty(record)?;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| AuthError::from(error.error))?;
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Removes a cached JSON record. A missing path or file is already the desired
/// state and succeeds.
pub(crate) fn remove_json_record(path: Option<&Path>) -> Result<(), AuthError> {
    let Some(path) = path else {
        return Ok(());
    };

    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{read_json_record, write_json_record};
    use serde::{Deserialize, Serialize};
    use std::os::unix::fs::PermissionsExt;

    #[derive(Debug, Default, Deserialize, PartialEq, Serialize)]
    struct Record {
        value: String,
    }

    #[test]
    fn atomic_json_write_replaces_complete_private_record() {
        let directory = std::env::temp_dir().join(format!(
            "rig-device-auth-atomic-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir(&directory).expect("temporary directory");
        let path = directory.join("auth.json");

        write_json_record(
            Some(&path),
            &Record {
                value: "first".into(),
            },
        )
        .expect("first atomic write");
        write_json_record(
            Some(&path),
            &Record {
                value: "second".into(),
            },
        )
        .expect("replacement atomic write");

        assert_eq!(
            read_json_record::<Record>(Some(&path)).expect("complete record"),
            Record {
                value: "second".into()
            }
        );
        assert_eq!(
            std::fs::metadata(&path)
                .expect("credential metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::read_dir(&directory)
                .expect("temporary directory entries")
                .count(),
            1,
            "successful replacement must not leave a sibling temporary file"
        );

        std::fs::remove_dir_all(directory).expect("clean temporary directory");
    }
}
