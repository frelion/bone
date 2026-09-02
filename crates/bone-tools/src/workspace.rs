use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use crate::ToolError;

#[derive(Clone, Debug)]
pub(crate) struct Workspace {
    root: Arc<PathBuf>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResolvedPath {
    pub(crate) absolute: PathBuf,
    pub(crate) display: String,
}

impl Workspace {
    pub(crate) fn new(root: impl AsRef<Path>) -> Result<Self, ToolError> {
        let requested = root.as_ref();
        let canonical = std::fs::canonicalize(requested).map_err(|error| {
            ToolError::io_display(
                "canonicalize workspace",
                requested.display().to_string(),
                error,
            )
        })?;
        let metadata = std::fs::metadata(&canonical).map_err(|error| {
            ToolError::io_display("inspect workspace", requested.display().to_string(), error)
        })?;
        if !metadata.is_dir() {
            return Err(ToolError::InvalidArgs(format!(
                "workspace is not a directory: {}",
                requested.display()
            )));
        }
        Ok(Self {
            root: Arc::new(canonical),
        })
    }

    pub(crate) fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub(crate) async fn resolve_existing(&self, raw: &str) -> Result<ResolvedPath, ToolError> {
        if raw.trim().is_empty() {
            return Err(ToolError::InvalidArgs("path must not be empty".to_owned()));
        }
        reject_nul(raw)?;
        let requested = Path::new(raw);
        let candidate = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            self.root.join(requested)
        };
        let canonical = tokio::fs::canonicalize(&candidate)
            .await
            .map_err(|error| ToolError::io_display("resolve path", raw, error))?;
        self.require_inside(&canonical, raw)?;
        Ok(ResolvedPath {
            display: self.display(&canonical),
            absolute: canonical,
        })
    }

    pub(crate) async fn resolve_patch_path(&self, raw: &str) -> Result<ResolvedPath, ToolError> {
        let relative = normalize_relative(raw)?;
        let candidate = self.root.join(&relative);

        let mut inspected = self.root().to_path_buf();
        for component in relative.components() {
            inspected.push(component.as_os_str());
            match tokio::fs::symlink_metadata(&inspected).await {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(ToolError::PermissionDenied {
                        path: raw.to_owned(),
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => {
                    return Err(ToolError::io_display("inspect patch path", raw, error));
                }
            }
        }

        let mut ancestor = candidate.as_path();
        loop {
            match tokio::fs::symlink_metadata(ancestor).await {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    ancestor = ancestor
                        .parent()
                        .ok_or_else(|| ToolError::OutsideWorkspace {
                            path: raw.to_owned(),
                        })?;
                }
                Err(error) => {
                    return Err(ToolError::io_display("inspect patch ancestor", raw, error));
                }
            }
        }
        let canonical_ancestor = tokio::fs::canonicalize(ancestor)
            .await
            .map_err(|error| ToolError::io_display("resolve patch path", raw, error))?;
        self.require_inside(&canonical_ancestor, raw)?;

        Ok(ResolvedPath {
            absolute: candidate,
            display: path_to_slashes(&relative),
        })
    }

    pub(crate) fn display(&self, path: &Path) -> String {
        path.strip_prefix(self.root())
            .map(|relative| {
                let display = path_to_slashes(relative);
                if display.is_empty() {
                    ".".to_owned()
                } else {
                    display
                }
            })
            .unwrap_or_else(|_| path.display().to_string())
    }

    fn require_inside(&self, canonical: &Path, raw: &str) -> Result<(), ToolError> {
        if canonical.starts_with(self.root()) {
            Ok(())
        } else {
            Err(ToolError::OutsideWorkspace {
                path: raw.to_owned(),
            })
        }
    }
}

fn normalize_relative(raw: &str) -> Result<PathBuf, ToolError> {
    if raw.trim().is_empty() {
        return Err(ToolError::InvalidArgs(
            "patch path must not be empty".to_owned(),
        ));
    }
    reject_nul(raw)?;
    let path = Path::new(raw);
    if path.is_absolute() || looks_like_windows_absolute(raw) {
        return Err(ToolError::OutsideWorkspace {
            path: raw.to_owned(),
        });
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                return Err(ToolError::OutsideWorkspace {
                    path: raw.to_owned(),
                });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(ToolError::OutsideWorkspace {
                    path: raw.to_owned(),
                });
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(ToolError::InvalidArgs(
            "patch path must name a file".to_owned(),
        ));
    }
    Ok(normalized)
}

fn reject_nul(raw: &str) -> Result<(), ToolError> {
    if raw.contains('\0') {
        Err(ToolError::InvalidArgs(
            "path must not contain a NUL character".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn looks_like_windows_absolute(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    raw.starts_with("\\\\")
        || matches!(bytes, [drive, b':', b'\\' | b'/', ..] if drive.is_ascii_alphabetic())
}

pub(crate) fn path_to_slashes(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_paths_reject_parent_and_absolute_components() {
        assert!(normalize_relative("src/../lib.rs").is_err());
        assert!(normalize_relative("../secret").is_err());
        assert!(normalize_relative("/tmp/secret").is_err());
        assert!(normalize_relative("C:\\tmp\\secret").is_err());
        assert!(normalize_relative("src/\0secret").is_err());
    }
}
