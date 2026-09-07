use std::{
    fmt,
    fs::File,
    path::{Path, PathBuf},
};

use fs2::FileExt;

/// A process-lifetime, fail-fast OS writer lease.
///
/// It represents runtime ownership, not ordinary SQLite write serialization.
/// Dropping it releases the native lock; a crashed process releases it when
/// the operating system closes its file descriptors.
#[must_use = "dropping a Lease immediately releases writer ownership"]
pub struct Lease {
    path: PathBuf,
    file: File,
}

impl Lease {
    pub(crate) fn new(path: PathBuf, file: File) -> Self {
        Self { path, file }
    }

    /// Diagnostic-only sidecar path. Callers must never delete or replace it.
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

impl Drop for Lease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}
