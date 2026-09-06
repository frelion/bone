use std::{env, ffi::OsString, path::PathBuf};

use crate::ConfigError;

/// Resolve the shared configuration path without reading or creating files.
///
/// `BONE_CONFIG` selects an explicit path. Otherwise, use
/// `$XDG_CONFIG_HOME/bone/config.json`, falling back to
/// `$HOME/.config/bone/config.json` when `XDG_CONFIG_HOME` is unset or empty.
/// The selected path must be absolute.
pub fn default_path() -> Result<PathBuf, ConfigError> {
    resolve_path(
        env::var_os("BONE_CONFIG"),
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
    )
}

fn resolve_path(
    explicit_path: Option<OsString>,
    config_directory: Option<OsString>,
    home_directory: Option<OsString>,
) -> Result<PathBuf, ConfigError> {
    let path = match explicit_path {
        Some(path) => PathBuf::from(path),
        None => config_directory
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                home_directory
                    .filter(|value| !value.is_empty())
                    .map(|path| PathBuf::from(path).join(".config"))
            })
            .ok_or(ConfigError::MissingDefaultPath)?
            .join("bone")
            .join("config.json"),
    };
    if !path.is_absolute() {
        return Err(ConfigError::RelativePath);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_path_takes_precedence_over_default_directories() {
        let directory = tempfile::tempdir().unwrap();
        let explicit = directory.path().join("custom.json");
        assert_eq!(
            resolve_path(
                Some(explicit.clone().into_os_string()),
                Some("relative-xdg".into()),
                Some("relative-home".into()),
            )
            .unwrap(),
            explicit
        );
    }

    #[test]
    fn uses_xdg_directory_then_home_when_xdg_is_unset_or_empty() {
        let directory = tempfile::tempdir().unwrap();
        let config_directory = directory.path().join("xdg");
        let home_directory = directory.path().join("user");
        assert_eq!(
            resolve_path(
                None,
                Some(config_directory.clone().into_os_string()),
                Some(home_directory.clone().into_os_string()),
            )
            .unwrap(),
            config_directory.join("bone/config.json")
        );
        for config_directory in [None, Some(OsString::new())] {
            assert_eq!(
                resolve_path(
                    None,
                    config_directory,
                    Some(home_directory.clone().into_os_string()),
                )
                .unwrap(),
                home_directory.join(".config/bone/config.json")
            );
        }
    }

    #[test]
    fn rejects_empty_or_relative_selected_paths() {
        for explicit in ["", "config.json"] {
            assert!(matches!(
                resolve_path(Some(explicit.into()), None, None),
                Err(ConfigError::RelativePath)
            ));
        }
        assert!(matches!(
            resolve_path(None, Some("relative".into()), None),
            Err(ConfigError::RelativePath)
        ));
        assert!(matches!(
            resolve_path(None, None, Some("relative".into())),
            Err(ConfigError::RelativePath)
        ));
    }

    #[test]
    fn reports_missing_default_directory() {
        assert!(matches!(
            resolve_path(None, None, None),
            Err(ConfigError::MissingDefaultPath)
        ));
        assert!(matches!(
            resolve_path(None, Some(OsString::new()), Some(OsString::new())),
            Err(ConfigError::MissingDefaultPath)
        ));
    }
}
