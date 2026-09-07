use std::{
    fmt,
    fs::File,
    path::{Path, PathBuf},
};

/// An application-defined name for one fail-fast OS file lease.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LeaseKey(String);

impl LeaseKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A process-lifetime, fail-fast OS lease.
///
/// It represents application ownership and is separate from SQLite's short
/// write transactions. Dropping it releases the lock.
pub struct Lease {
    path: PathBuf,
    _file: File,
}

impl Lease {
    pub(crate) fn new(path: PathBuf, file: File) -> Self {
        Self { path, _file: file }
    }

    /// Diagnostic-only sidecar path. Callers must not replace it.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Debug for Lease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Lease")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

pub(crate) fn lease_file_name(key: &LeaseKey) -> String {
    let mut encoded = String::with_capacity(key.as_str().len() * 2 + 5);
    for byte in key.as_str().bytes() {
        use std::fmt::Write;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded.push_str(".lock");
    encoded
}
