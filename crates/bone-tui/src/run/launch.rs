use std::{env, ffi::OsString, path::PathBuf};

use super::RunError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LaunchOptions {
    pub(super) data_dir: Option<PathBuf>,
    pub(super) workspace: PathBuf,
}

impl LaunchOptions {
    pub(super) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, RunError> {
        let mut data_dir = None;
        let mut workspace = env::current_dir()?;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--data-dir") => {
                    data_dir =
                        Some(PathBuf::from(args.next().ok_or_else(|| {
                            RunError::Usage("--data-dir 需要一个路径".into())
                        })?));
                }
                Some("--workspace") => {
                    workspace = PathBuf::from(
                        args.next()
                            .ok_or_else(|| RunError::Usage("--workspace 需要一个路径".into()))?,
                    );
                }
                Some(value) => return Err(RunError::Usage(format!("未知参数：{value}"))),
                None => return Err(RunError::Usage("参数必须是有效的 UTF-8".into())),
            }
        }
        Ok(Self {
            data_dir,
            workspace,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_launch_paths_are_kept_at_the_process_boundary() {
        let parsed = LaunchOptions::parse([
            OsString::from("--data-dir"),
            OsString::from("/tmp/bone-data"),
            OsString::from("--workspace"),
            OsString::from("/tmp/project"),
        ])
        .unwrap();
        assert_eq!(parsed.data_dir, Some(PathBuf::from("/tmp/bone-data")));
        assert_eq!(parsed.workspace, PathBuf::from("/tmp/project"));
    }
}
