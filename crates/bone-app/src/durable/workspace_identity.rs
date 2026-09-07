use std::{
    env,
    ffi::OsString,
    fmt, fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use uuid::Uuid;

use super::{RegistryError, WorkspaceError, WorkspaceRegistry};

/// A local, opaque identity for one canonical BONE workspace.
///
/// It is a random UUID allocated by [`WorkspaceRegistry`], not a reversible
/// hash of a path. It is safe to persist locally but must not be used as a
/// telemetry identifier.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(Uuid);

impl WorkspaceId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn is_nil(self) -> bool {
        self.0.is_nil()
    }

    pub fn as_uuid(self) -> Uuid {
        self.0
    }

    pub fn parse_str(value: &str) -> Result<Self, uuid::Error> {
        Uuid::parse_str(value).map(Self)
    }
}

impl Default for WorkspaceId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Debug for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("WorkspaceId").field(&self.0).finish()
    }
}

impl FromStr for WorkspaceId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse_str(value)
    }
}

/// A losslessly serializable absolute canonical path.
///
/// JSON strings cannot faithfully represent every Unix path. This type stores
/// platform-tagged OS-string units in hexadecimal, while callers always use
/// [`CanonicalPath::as_path`] rather than depending on that storage encoding.
#[derive(Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct CanonicalPath(PathBuf);

impl CanonicalPath {
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, WorkspaceError> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(WorkspaceError::NonAbsoluteCanonicalPath { path });
        }
        Ok(Self(path))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }

    pub(crate) fn storage_encoding(&self) -> String {
        encode_os_path(&self.0)
    }
}

impl fmt::Debug for CanonicalPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CanonicalPath")
            .field(&self.0)
            .finish()
    }
}

impl Serialize for CanonicalPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.storage_encoding())
    }
}

impl<'de> Deserialize<'de> for CanonicalPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let path = decode_os_path(&encoded).map_err(de::Error::custom)?;
        CanonicalPath::new(path).map_err(de::Error::custom)
    }
}

/// The immutable workspace selected by the process at launch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceContext {
    id: WorkspaceId,
    canonical_root: CanonicalPath,
    display_root: PathBuf,
}

impl WorkspaceContext {
    /// Resolve one exact launch directory and allocate/lookup its stable ID.
    ///
    /// This does not climb to a Git root and never falls back to a parent
    /// directory if canonicalization fails.
    pub fn discover(
        launch_directory: impl AsRef<Path>,
        registry: &WorkspaceRegistry,
    ) -> Result<Self, WorkspaceError> {
        let display_root = absolute_display_path(launch_directory.as_ref())?;
        let canonical_root =
            fs::canonicalize(&display_root).map_err(|source| WorkspaceError::Canonicalize {
                path: display_root.clone(),
                source,
            })?;
        let metadata =
            fs::metadata(&canonical_root).map_err(|source| WorkspaceError::Canonicalize {
                path: canonical_root.clone(),
                source,
            })?;
        if !metadata.is_dir() {
            return Err(WorkspaceError::NotDirectory {
                path: canonical_root,
            });
        }

        let canonical_root = CanonicalPath::new(canonical_root)?;
        let id = registry.resolve_or_create_canonical(&canonical_root)?;
        Ok(Self {
            id,
            canonical_root,
            display_root,
        })
    }

    /// Rebuild a context when the caller has already canonicalized and checked
    /// the workspace root. This is useful for deterministic tests and startup
    /// code that centralizes filesystem discovery.
    pub fn from_canonical(
        id: WorkspaceId,
        canonical_root: CanonicalPath,
        display_root: impl Into<PathBuf>,
    ) -> Result<Self, WorkspaceError> {
        if id.is_nil() {
            return Err(RegistryError::InvalidWorkspaceId {
                key: "context".to_owned(),
            }
            .into());
        }
        let display_root = display_root.into();
        if !display_root.is_absolute() {
            return Err(WorkspaceError::NonAbsoluteCanonicalPath { path: display_root });
        }
        Ok(Self {
            id,
            canonical_root,
            display_root,
        })
    }

    pub fn id(&self) -> WorkspaceId {
        self.id
    }

    /// Canonical path for security boundaries, tool roots, and registry keys.
    pub fn canonical_root(&self) -> &Path {
        self.canonical_root.as_path()
    }

    pub fn canonical_path(&self) -> &CanonicalPath {
        &self.canonical_root
    }

    /// Absolute user-facing launch path. It may preserve a symlink spelling.
    pub fn display_root(&self) -> &Path {
        &self.display_root
    }
}

fn absolute_display_path(path: &Path) -> Result<PathBuf, WorkspaceError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|source| WorkspaceError::Canonicalize {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn encode_os_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    format!("unix:{}", hex_encode(path.as_os_str().as_bytes()))
}

#[cfg(unix)]
fn decode_os_path(encoded: &str) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;

    let hex = encoded
        .strip_prefix("unix:")
        .ok_or_else(|| "canonical path belongs to a different platform".to_owned())?;
    Ok(PathBuf::from(OsString::from_vec(hex_decode(hex)?)))
}

#[cfg(windows)]
fn encode_os_path(path: &Path) -> String {
    use std::fmt::Write;
    use std::os::windows::ffi::OsStrExt;

    let mut encoded = String::from("windows:");
    for unit in path.as_os_str().encode_wide() {
        let _ = write!(encoded, "{unit:04x}");
    }
    encoded
}

#[cfg(windows)]
fn decode_os_path(encoded: &str) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;

    let hex = encoded
        .strip_prefix("windows:")
        .ok_or_else(|| "canonical path belongs to a different platform".to_owned())?;
    if hex.len() & 3 != 0 {
        return Err("invalid Windows canonical path encoding".to_owned());
    }
    let mut units = Vec::with_capacity(hex.len() / 4);
    for offset in (0..hex.len()).step_by(4) {
        let unit = u16::from_str_radix(&hex[offset..offset + 4], 16)
            .map_err(|_| "invalid Windows canonical path encoding".to_owned())?;
        units.push(unit);
    }
    Ok(PathBuf::from(OsString::from_wide(&units)))
}

#[cfg(not(any(unix, windows)))]
fn encode_os_path(path: &Path) -> String {
    format!("portable:{}", hex_encode(path.to_string_lossy().as_bytes()))
}

#[cfg(not(any(unix, windows)))]
fn decode_os_path(encoded: &str) -> Result<PathBuf, String> {
    let hex = encoded
        .strip_prefix("portable:")
        .ok_or_else(|| "canonical path belongs to a different platform".to_owned())?;
    let value = String::from_utf8(hex_decode(hex)?)
        .map_err(|_| "invalid portable canonical path encoding".to_owned())?;
    Ok(PathBuf::from(value))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn hex_decode(encoded: &str) -> Result<Vec<u8>, String> {
    if encoded.len() & 1 != 0 {
        return Err("invalid canonical path encoding".to_owned());
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    for offset in (0..encoded.len()).step_by(2) {
        bytes.push(
            u8::from_str_radix(&encoded[offset..offset + 2], 16)
                .map_err(|_| "invalid canonical path encoding".to_owned())?,
        );
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;
    use crate::WorkspaceRegistry;

    fn private_data() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            directory.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        directory
    }

    #[test]
    fn canonical_path_json_round_trips_losslessly() {
        let path = CanonicalPath::new(PathBuf::from("/tmp/bone workspace")).unwrap();
        let encoded = serde_json::to_string(&path).unwrap();
        let decoded: CanonicalPath = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, path);
        assert!(encoded.contains("unix:") || encoded.contains("windows:"));
    }

    #[test]
    fn discovery_uses_exact_directory_and_preserves_display_path() {
        let data = private_data();
        let workspace = tempfile::tempdir().unwrap();
        let nested = workspace.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let registry = WorkspaceRegistry::open_in(data.path()).unwrap();

        let context = WorkspaceContext::discover(&nested, &registry).unwrap();
        assert_eq!(context.display_root(), nested);
        assert_eq!(context.canonical_root(), fs::canonicalize(&nested).unwrap());
        assert_ne!(context.canonical_root(), workspace.path());
    }

    #[test]
    fn discovery_rejects_file_roots() {
        let data = private_data();
        let workspace = tempfile::tempdir().unwrap();
        let file = workspace.path().join("not-a-directory");
        fs::write(&file, "x").unwrap();
        let registry = WorkspaceRegistry::open_in(data.path()).unwrap();

        assert!(matches!(
            WorkspaceContext::discover(&file, &registry),
            Err(WorkspaceError::NotDirectory { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_spellings_resolve_to_one_workspace_id() {
        use std::os::unix::fs::symlink;

        let data = private_data();
        let workspace = tempfile::tempdir().unwrap();
        let link_parent = tempfile::tempdir().unwrap();
        let link = link_parent.path().join("workspace-link");
        symlink(workspace.path(), &link).unwrap();
        let registry = WorkspaceRegistry::open_in(data.path()).unwrap();

        let direct = WorkspaceContext::discover(workspace.path(), &registry).unwrap();
        let through_link = WorkspaceContext::discover(&link, &registry).unwrap();
        assert_eq!(direct.id(), through_link.id());
        assert_eq!(through_link.display_root(), Path::new(&link));
    }
}
