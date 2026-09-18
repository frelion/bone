//! Versioned user and workspace configuration files.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ConfigChange, ConfigScope, Profile, ProfileId, RuntimeOverrides, WorkspaceId, WorkspaceInfo,
    config::RuntimeSettings,
    safe_file::{self, SafeFileError},
    storage::StoreError,
};

const SCHEMA_VERSION: u32 = 1;

#[derive(Clone)]
pub(crate) struct FileConfigs {
    bone_home: PathBuf,
    inner: Arc<Mutex<State>>,
}

struct State {
    user: Loaded<UserConfig>,
    projects: HashMap<WorkspaceId, ProjectState>,
}

struct Loaded<T> {
    value: T,
    digest: Option<String>,
}

struct ProjectState {
    root: PathBuf,
    loaded: Loaded<ProjectConfig>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserConfig {
    schema_version: u32,
    #[serde(default)]
    settings: RuntimeSettings,
    #[serde(default = "default_profiles")]
    profiles: Vec<Profile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectConfig {
    schema_version: u32,
    #[serde(default)]
    overrides: RuntimeOverrides,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectConfigStatus {
    pub path: PathBuf,
    pub exists: bool,
    pub overrides: RuntimeOverrides,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            settings: RuntimeSettings::default(),
            profiles: default_profiles(),
        }
    }
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            overrides: RuntimeOverrides::default(),
        }
    }
}

fn default_profiles() -> Vec<Profile> {
    Vec::new()
}

impl FileConfigs {
    pub(crate) fn open(bone_home: PathBuf) -> Result<Self, StoreError> {
        if !bone_home.is_absolute() {
            return Err(StoreError::RelativeRoot { path: bone_home });
        }
        safe(
            safe_file::ensure_private_directory(&bone_home),
            "prepare BONE home",
            &bone_home,
        )?;
        let user = load_user(&bone_home)?;
        Ok(Self {
            bone_home,
            inner: Arc::new(Mutex::new(State {
                user,
                projects: HashMap::new(),
            })),
        })
    }

    pub(crate) fn load_workspace(
        &self,
        workspace: &WorkspaceInfo,
    ) -> Result<ProjectConfigStatus, StoreError> {
        let path = checked_project_path(&workspace.root, false)?;
        let loaded = load_project(&path)?;
        let status = ProjectConfigStatus {
            path,
            exists: loaded.digest.is_some(),
            overrides: loaded.value.overrides.clone(),
        };
        self.inner
            .lock()
            .expect("file config mutex poisoned")
            .projects
            .insert(
                workspace.id,
                ProjectState {
                    root: workspace.root.clone(),
                    loaded,
                },
            );
        Ok(status)
    }

    pub(crate) fn reload_user(&self) -> Result<(), StoreError> {
        let loaded = load_user(&self.bone_home)?;
        self.inner.lock().expect("file config mutex poisoned").user = loaded;
        Ok(())
    }

    pub(crate) fn reload_all(
        &self,
        workspace: &WorkspaceInfo,
    ) -> Result<ProjectConfigStatus, StoreError> {
        // Parse and validate both files before publishing either snapshot.
        let user = load_user(&self.bone_home)?;
        let path = checked_project_path(&workspace.root, false)?;
        let loaded = load_project(&path)?;
        let status = ProjectConfigStatus {
            path,
            exists: loaded.digest.is_some(),
            overrides: loaded.value.overrides.clone(),
        };
        let mut state = self.inner.lock().expect("file config mutex poisoned");
        state.user = user;
        state.projects.insert(
            workspace.id,
            ProjectState {
                root: workspace.root.clone(),
                loaded,
            },
        );
        Ok(status)
    }

    pub(crate) fn user_settings(&self) -> RuntimeSettings {
        self.inner
            .lock()
            .expect("file config mutex poisoned")
            .user
            .value
            .settings
            .clone()
    }

    pub(crate) fn profiles(&self) -> Vec<Profile> {
        self.inner
            .lock()
            .expect("file config mutex poisoned")
            .user
            .value
            .profiles
            .clone()
    }

    pub(crate) fn overrides(&self, workspace: WorkspaceId) -> RuntimeOverrides {
        self.inner
            .lock()
            .expect("file config mutex poisoned")
            .projects
            .get(&workspace)
            .map(|project| project.loaded.value.overrides.clone())
            .unwrap_or_default()
    }

    pub(crate) fn project_status(&self, workspace: WorkspaceId) -> Option<ProjectConfigStatus> {
        self.inner
            .lock()
            .expect("file config mutex poisoned")
            .projects
            .get(&workspace)
            .map(|project| ProjectConfigStatus {
                path: project_path(&project.root),
                exists: project.loaded.digest.is_some(),
                overrides: project.loaded.value.overrides.clone(),
            })
    }

    pub(crate) fn lock_project(&self, workspace: WorkspaceId) -> Result<File, StoreError> {
        let root = self
            .inner
            .lock()
            .expect("file config mutex poisoned")
            .projects
            .get(&workspace)
            .map(|project| project.root.clone())
            .ok_or(StoreError::Corrupt {
                message: "workspace configuration was not loaded",
            })?;
        let config_path = checked_project_path(&root, true)?;
        #[cfg(unix)]
        return safe(
            safe_file::lock_directory(config_path.parent().expect("project config has parent")),
            "lock project configuration",
            &config_path,
        );

        #[cfg(not(unix))]
        {
            let lock_dir = self.bone_home.join("locks/project-config");
            safe(
                safe_file::ensure_private_directory(&lock_dir),
                "prepare project configuration locks",
                &lock_dir,
            )?;
            let lock_path = lock_dir.join(format!(
                "{}.lock",
                digest(root.to_string_lossy().as_bytes())
            ));
            safe(
                safe_file::lock_private_file(&lock_path),
                "lock project configuration",
                &lock_path,
            )
        }
    }

    pub(crate) fn update(
        &self,
        scope: ConfigScope,
        change: ConfigChange,
    ) -> Result<RuntimeOverrides, StoreError> {
        match scope {
            ConfigScope::User => {
                let path = self.bone_home.join("config.toml");
                let lock_path = self.bone_home.join("config.lock");
                let _lock = safe(
                    safe_file::lock_private_file(&lock_path),
                    "lock user configuration",
                    &lock_path,
                )?;
                let mut state = self.inner.lock().expect("file config mutex poisoned");
                ensure_unchanged(&path, state.user.digest.as_deref())?;
                let mut next = state.user.value.clone();
                apply_settings_change(&mut next.settings, change);
                validate_user(&next)?;
                let digest = write_private_toml(&path, &next)?;
                state.user = Loaded {
                    value: next,
                    digest: Some(digest),
                };
                Ok(settings_as_overrides(&state.user.value.settings))
            }
            ConfigScope::Workspace(_) => unreachable!("use update_workspace for project files"),
            ConfigScope::Session(_) => unreachable!("session configuration remains in SQLite"),
        }
    }

    pub(crate) fn update_workspace(
        &self,
        workspace: WorkspaceId,
        change: ConfigChange,
    ) -> Result<RuntimeOverrides, StoreError> {
        let _lock = self.lock_project(workspace)?;
        let mut state = self.inner.lock().expect("file config mutex poisoned");
        let project = state
            .projects
            .get_mut(&workspace)
            .ok_or(StoreError::Corrupt {
                message: "workspace configuration was not loaded",
            })?;
        let path = checked_project_path(&project.root, true)?;
        ensure_unchanged(&path, project.loaded.digest.as_deref())?;
        let mut next = project.loaded.value.clone();
        apply_override_change(&mut next.overrides, change);
        validate_project(&next)?;
        let digest = write_project_toml(&path, &next)?;
        let values = next.overrides.clone();
        project.loaded = Loaded {
            value: next,
            digest: Some(digest.clone()),
        };
        Ok(values)
    }

    pub(crate) fn save_profile(&self, profile: Profile) -> Result<(), StoreError> {
        let path = self.bone_home.join("config.toml");
        let lock_path = self.bone_home.join("config.lock");
        let _lock = safe(
            safe_file::lock_private_file(&lock_path),
            "lock user configuration",
            &lock_path,
        )?;
        let mut state = self.inner.lock().expect("file config mutex poisoned");
        ensure_unchanged(&path, state.user.digest.as_deref())?;
        let mut next = state.user.value.clone();
        match next.profiles.iter_mut().find(|item| item.id == profile.id) {
            Some(current) => *current = profile,
            None => next.profiles.push(profile),
        }
        validate_user(&next)?;
        let digest = write_private_toml(&path, &next)?;
        state.user = Loaded {
            value: next,
            digest: Some(digest),
        };
        Ok(())
    }

    /// Remove one saved profile and rewrite the user configuration. Removing
    /// an id that is not saved is a no-op that leaves the file untouched, so a
    /// repeated delete never fails and never creates a configuration file.
    pub(crate) fn delete_profile(&self, profile: &ProfileId) -> Result<(), StoreError> {
        let path = self.bone_home.join("config.toml");
        let lock_path = self.bone_home.join("config.lock");
        let _lock = safe(
            safe_file::lock_private_file(&lock_path),
            "lock user configuration",
            &lock_path,
        )?;
        let mut state = self.inner.lock().expect("file config mutex poisoned");
        ensure_unchanged(&path, state.user.digest.as_deref())?;
        if !state
            .user
            .value
            .profiles
            .iter()
            .any(|item| item.id == *profile)
        {
            return Ok(());
        }
        let mut next = state.user.value.clone();
        next.profiles.retain(|item| item.id != *profile);
        validate_user(&next)?;
        let digest = write_private_toml(&path, &next)?;
        state.user = Loaded {
            value: next,
            digest: Some(digest),
        };
        Ok(())
    }
}

fn validate_user(config: &UserConfig) -> Result<(), StoreError> {
    validate_version(config.schema_version)?;
    let mut ids = std::collections::HashSet::new();
    for profile in &config.profiles {
        profile.validate().map_err(|error| StoreError::ConfigFile {
            message: format!("profiles.{}: {error}", profile.id),
        })?;
        if !ids.insert(profile.id.clone()) {
            return Err(StoreError::ConfigFile {
                message: format!("duplicate profile `{}`", profile.id),
            });
        }
    }
    if let Some(selection) = &config.settings.worker {
        selection
            .validate()
            .map_err(|error| config_field_error("settings.worker", error))?;
    }
    if let Some(selection) = &config.settings.coordinator {
        selection
            .validate()
            .map_err(|error| config_field_error("settings.coordinator", error))?;
    }
    crate::config::validate_agent_limits(&config.settings.limits)
        .map_err(|error| config_field_error("settings.limits", error))?;
    crate::config::validate_tool_limits(&config.settings.tools)
        .map_err(|error| config_field_error("settings.tools", error))?;
    Ok(())
}

fn load_user(bone_home: &Path) -> Result<Loaded<UserConfig>, StoreError> {
    let path = bone_home.join("config.toml");
    validate_private_config_if_present(&path)?;
    let loaded = load_optional::<UserConfig>(&path)?.unwrap_or_else(|| Loaded {
        value: UserConfig::default(),
        digest: None,
    });
    validate_user(&loaded.value)?;
    Ok(loaded)
}

fn load_project(path: &Path) -> Result<Loaded<ProjectConfig>, StoreError> {
    validate_regular_config_if_present(path)?;
    let loaded = load_optional::<ProjectConfig>(path)?.unwrap_or_else(|| Loaded {
        value: ProjectConfig::default(),
        digest: None,
    });
    validate_project(&loaded.value)?;
    Ok(loaded)
}

fn validate_project(config: &ProjectConfig) -> Result<(), StoreError> {
    validate_version(config.schema_version)?;
    if let Some(selection) = &config.overrides.worker {
        selection
            .validate()
            .map_err(|error| config_field_error("overrides.worker", error))?;
    }
    if let Some(selection) = &config.overrides.coordinator {
        selection
            .validate()
            .map_err(|error| config_field_error("overrides.coordinator", error))?;
    }
    if let Some(limits) = &config.overrides.limits {
        crate::config::validate_agent_limits(limits)
            .map_err(|error| config_field_error("overrides.limits", error))?;
    }
    if let Some(tools) = &config.overrides.tools {
        crate::config::validate_tool_limits(tools)
            .map_err(|error| config_field_error("overrides.tools", error))?;
    }
    Ok(())
}

fn config_field_error(field: &str, error: crate::ConfigError) -> StoreError {
    StoreError::ConfigFile {
        message: format!("{field}: {error}"),
    }
}

fn validate_version(version: u32) -> Result<(), StoreError> {
    if version == SCHEMA_VERSION {
        Ok(())
    } else {
        Err(StoreError::ConfigVersion {
            expected: SCHEMA_VERSION,
            actual: version,
        })
    }
}

fn load_optional<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<Loaded<T>>, StoreError> {
    let Some(bytes) = safe(
        safe_file::read_regular_file(path),
        "read configuration file",
        path,
    )?
    else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| StoreError::ConfigFile {
        message: format!("{} is not UTF-8", path.display()),
    })?;
    let value = toml::from_str(text).map_err(|error| StoreError::ConfigFile {
        message: format!("{}: {error}", path.display()),
    })?;
    Ok(Some(Loaded {
        value,
        digest: Some(digest(&bytes)),
    }))
}

fn ensure_unchanged(path: &Path, expected: Option<&str>) -> Result<(), StoreError> {
    let actual = safe(
        safe_file::read_regular_file(path),
        "inspect configuration file",
        path,
    )?
    .map(|bytes| digest(&bytes));
    if actual.as_deref() == expected {
        Ok(())
    } else {
        Err(StoreError::ConfigConflict {
            path: path.to_path_buf(),
        })
    }
}

fn digest(bytes: &[u8]) -> String {
    let value = Sha256::digest(bytes);
    value.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_private_toml<T: Serialize>(path: &Path, value: &T) -> Result<String, StoreError> {
    let text = serialize_toml(value)?;
    safe(
        safe_file::atomic_write_private(path, text.as_bytes()),
        "write user configuration",
        path,
    )?;
    Ok(digest(text.as_bytes()))
}

fn write_project_toml<T: Serialize>(path: &Path, value: &T) -> Result<String, StoreError> {
    let text = serialize_toml(value)?;
    safe(
        safe_file::atomic_write_regular(path, text.as_bytes()),
        "write project configuration",
        path,
    )?;
    Ok(digest(text.as_bytes()))
}

fn serialize_toml<T: Serialize>(value: &T) -> Result<String, StoreError> {
    toml::to_string_pretty(value).map_err(|error| StoreError::ConfigFile {
        message: error.to_string(),
    })
}

fn validate_private_config_if_present(path: &Path) -> Result<(), StoreError> {
    validate_regular_config_if_present(path)
}

fn validate_regular_config_if_present(path: &Path) -> Result<(), StoreError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(StoreError::io("inspect configuration", path, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(StoreError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "must be a regular file, not a symbolic link",
        });
    }
    Ok(())
}

fn project_path(root: &Path) -> PathBuf {
    root.join(".bone/config.toml")
}

fn checked_project_path(root: &Path, create: bool) -> Result<PathBuf, StoreError> {
    let root = fs::canonicalize(root)
        .map_err(|error| StoreError::io("resolve workspace root", root, error))?;
    let directory = root.join(".bone");
    match fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(StoreError::UnsafeStorage {
                path: directory,
                reason: "project .bone must be a real directory",
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound && create => {
            match fs::create_dir(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(StoreError::io(
                        "create project configuration directory",
                        &directory,
                        error,
                    ));
                }
            }
            let metadata = fs::symlink_metadata(&directory).map_err(|error| {
                StoreError::io("inspect project configuration directory", &directory, error)
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(StoreError::UnsafeStorage {
                    path: directory,
                    reason: "project .bone must be a real directory",
                });
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(directory.join("config.toml"));
        }
        Err(error) => {
            return Err(StoreError::io(
                "inspect project configuration directory",
                &directory,
                error,
            ));
        }
    }
    let resolved = fs::canonicalize(&directory).map_err(|error| {
        StoreError::io("resolve project configuration directory", &directory, error)
    })?;
    if resolved.parent() != Some(root.as_path()) {
        return Err(StoreError::UnsafeStorage {
            path: directory,
            reason: "project .bone escapes the workspace root",
        });
    }
    Ok(resolved.join("config.toml"))
}

fn safe<T>(
    result: Result<T, SafeFileError>,
    operation: &'static str,
    path: &Path,
) -> Result<T, StoreError> {
    result.map_err(|error| match error {
        SafeFileError::Io(error) => StoreError::io(operation, path, error),
        SafeFileError::Unsafe(reason) => StoreError::UnsafeStorage {
            path: path.to_path_buf(),
            reason,
        },
    })
}

fn settings_as_overrides(settings: &RuntimeSettings) -> RuntimeOverrides {
    RuntimeOverrides {
        worker: settings.worker.clone(),
        coordinator: settings.coordinator.clone(),
        limits: Some(settings.limits.clone()),
        tools: Some(settings.tools.clone()),
    }
}

fn apply_settings_change(settings: &mut RuntimeSettings, change: ConfigChange) {
    match change {
        ConfigChange::Model(value) => {
            settings.worker = value.clone();
            settings.coordinator = value;
        }
        ConfigChange::Worker(value) => settings.worker = value,
        ConfigChange::Coordinator(value) => settings.coordinator = value,
        ConfigChange::Limits(value) => settings.limits = value.unwrap_or_default(),
        ConfigChange::Tools(value) => settings.tools = value.unwrap_or_default(),
    }
}

fn apply_override_change(overrides: &mut RuntimeOverrides, change: ConfigChange) {
    match change {
        ConfigChange::Model(value) => {
            overrides.worker = value.clone();
            overrides.coordinator = value;
        }
        ConfigChange::Worker(value) => overrides.worker = value,
        ConfigChange::Coordinator(value) => overrides.coordinator = value,
        ConfigChange::Limits(value) => overrides.limits = value,
        ConfigChange::Tools(value) => overrides.tools = value,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{ConfigChange, ConfigScope, EndpointConfig, ProfileId, ToolLimits};

    #[test]
    fn user_file_detects_external_edits_instead_of_overwriting_them() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        let configs = FileConfigs::open(home.clone()).unwrap();
        configs
            .update(
                ConfigScope::User,
                ConfigChange::Tools(Some(ToolLimits::default())),
            )
            .unwrap();
        fs::write(home.join("config.toml"), "schema_version = 1\n").unwrap();
        assert!(matches!(
            configs.update(ConfigScope::User, ConfigChange::Tools(None)),
            Err(StoreError::ConfigConflict { .. })
        ));
    }

    #[test]
    fn separate_app_instances_cannot_overwrite_a_stale_user_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        let first = FileConfigs::open(home.clone()).unwrap();
        let second = FileConfigs::open(home).unwrap();
        first
            .update(
                ConfigScope::User,
                ConfigChange::Tools(Some(ToolLimits::default())),
            )
            .unwrap();
        assert!(matches!(
            second.update(ConfigScope::User, ConfigChange::Tools(None)),
            Err(StoreError::ConfigConflict { .. })
        ));
    }

    #[test]
    fn new_user_config_has_no_implicit_profiles() {
        let temporary = tempfile::tempdir().unwrap();
        let configs = FileConfigs::open(temporary.path().join(".bone")).unwrap();

        assert!(configs.profiles().is_empty());
    }

    #[test]
    fn profile_models_are_persisted_with_the_profile() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        let configs = FileConfigs::open(home.clone()).unwrap();
        let mut profile = Profile::new(
            ProfileId::new("local").unwrap(),
            "Local",
            EndpointConfig::OpenAiResponses {
                base_url: Some("http://127.0.0.1:8080/v1".into()),
            },
        )
        .unwrap();
        profile.add_model("qwen").unwrap();
        profile.add_model("deepseek").unwrap();
        configs.save_profile(profile.clone()).unwrap();

        let reloaded = FileConfigs::open(home).unwrap();
        assert_eq!(
            reloaded
                .profiles()
                .into_iter()
                .find(|item| item.id == profile.id),
            Some(profile)
        );
    }

    #[test]
    fn delete_profile_rewrites_the_file_and_keeps_the_remaining_order() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        let configs = FileConfigs::open(home.clone()).unwrap();
        let profiles = [
            Profile::new(
                ProfileId::new("first").unwrap(),
                "First",
                EndpointConfig::OpenAiResponses { base_url: None },
            )
            .unwrap(),
            Profile::new(
                ProfileId::new("second").unwrap(),
                "Second",
                EndpointConfig::OpenAiResponses { base_url: None },
            )
            .unwrap(),
            Profile::chatgpt(),
        ];
        for profile in profiles.clone() {
            configs.save_profile(profile).unwrap();
        }
        let path = home.join("config.toml");
        let before = fs::read_to_string(&path).unwrap();

        configs.delete_profile(&profiles[1].id).unwrap();

        let expected = vec![profiles[0].clone(), profiles[2].clone()];
        assert_eq!(configs.profiles(), expected);
        let after = fs::read_to_string(&path).unwrap();
        assert_ne!(after, before);
        assert!(!after.contains("Second"));
        assert!(after.contains("First"));
        // A fresh reader observes the removal too: it is durable, not a view
        // over in-memory state.
        assert_eq!(FileConfigs::open(home).unwrap().profiles(), expected);
    }

    #[test]
    fn delete_profile_is_idempotent_for_an_unsaved_id() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        let configs = FileConfigs::open(home.clone()).unwrap();
        let saved = Profile::new(
            ProfileId::new("saved").unwrap(),
            "Saved",
            EndpointConfig::OpenAiResponses { base_url: None },
        )
        .unwrap();
        configs.save_profile(saved.clone()).unwrap();
        let path = home.join("config.toml");
        let before = fs::read(&path).unwrap();

        let missing = ProfileId::new("missing").unwrap();
        configs.delete_profile(&missing).unwrap();
        configs.delete_profile(&missing).unwrap();

        assert_eq!(configs.profiles(), vec![saved]);
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn deleting_an_unsaved_profile_does_not_create_the_config_file() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        let configs = FileConfigs::open(home.clone()).unwrap();

        configs
            .delete_profile(&ProfileId::new("missing").unwrap())
            .unwrap();

        assert!(!home.join("config.toml").exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_project_bone_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        let root = temporary.path().join("workspace");
        let outside = temporary.path().join("outside");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("config.toml"), "schema_version = 1\n").unwrap();
        symlink(&outside, root.join(".bone")).unwrap();
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            root,
        };
        let configs = FileConfigs::open(home).unwrap();
        assert!(matches!(
            configs.load_workspace(&workspace),
            Err(StoreError::UnsafeStorage { .. })
        ));
    }

    #[test]
    fn project_file_is_loaded_immediately() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        let root = temporary.path().join("workspace");
        fs::create_dir_all(root.join(".bone")).unwrap();
        fs::write(
            root.join(".bone/config.toml"),
            "schema_version = 1\n[overrides]\n",
        )
        .unwrap();
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            root,
        };
        let configs = FileConfigs::open(home).unwrap();
        let status = configs.load_workspace(&workspace).unwrap();
        assert!(status.exists);
        assert_eq!(configs.overrides(workspace.id), RuntimeOverrides::default());
    }

    #[test]
    fn project_write_updates_the_loaded_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        let root = temporary.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            root,
        };
        let configs = FileConfigs::open(home).unwrap();
        configs.load_workspace(&workspace).unwrap();
        let expected = RuntimeOverrides {
            tools: Some(ToolLimits::default()),
            ..RuntimeOverrides::default()
        };
        let updated = configs
            .update_workspace(
                workspace.id,
                ConfigChange::Tools(Some(ToolLimits::default())),
            )
            .unwrap();
        assert_eq!(updated, expected);
        assert_eq!(configs.overrides(workspace.id), expected);
    }

    #[test]
    fn project_write_rejects_external_changes() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        let root = temporary.path().join("workspace");
        fs::create_dir_all(root.join(".bone")).unwrap();
        let path = root.join(".bone/config.toml");
        fs::write(&path, "schema_version = 1\n[overrides]\n").unwrap();
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            root,
        };
        let configs = FileConfigs::open(home).unwrap();
        configs.load_workspace(&workspace).unwrap();
        fs::write(
            &path,
            "schema_version = 1\n[overrides.worker]\nprofile = \"chatgpt\"\nmodel = \"changed\"\n",
        )
        .unwrap();
        assert!(matches!(
            configs.update_workspace(workspace.id, ConfigChange::Tools(None)),
            Err(StoreError::ConfigConflict { .. })
        ));
    }

    #[test]
    fn rejects_unknown_and_future_schema_fields() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join("config.toml"),
            "schema_version = 2\nunknown = true\n",
        )
        .unwrap();
        assert!(FileConfigs::open(home).is_err());
    }

    #[test]
    fn combined_reload_publishes_neither_file_when_one_is_invalid() {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("home");
        let root = temporary.path().join("workspace");
        fs::create_dir_all(root.join(".bone")).unwrap();
        let configs = FileConfigs::open(home.clone()).unwrap();
        configs
            .update(
                ConfigScope::User,
                ConfigChange::Tools(Some(ToolLimits::default())),
            )
            .unwrap();
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            root,
        };
        configs.load_workspace(&workspace).unwrap();

        let mut edited = UserConfig::default();
        edited.settings.tools.max_bash_timeout = std::time::Duration::from_secs(300);
        fs::write(home.join("config.toml"), toml::to_string(&edited).unwrap()).unwrap();
        fs::write(
            workspace.root.join(".bone/config.toml"),
            "schema_version = 999\n",
        )
        .unwrap();
        assert!(configs.reload_all(&workspace).is_err());
        assert_eq!(configs.user_settings().tools, ToolLimits::default());
    }

    #[cfg(unix)]
    #[test]
    fn failed_project_write_does_not_change_the_loaded_snapshot() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join(".bone");
        let root = temporary.path().join("workspace");
        let project_dir = root.join(".bone");
        fs::create_dir_all(&project_dir).unwrap();
        let workspace = WorkspaceInfo {
            id: WorkspaceId::new(),
            root,
        };
        let configs = FileConfigs::open(home).unwrap();
        configs.load_workspace(&workspace).unwrap();
        fs::set_permissions(&project_dir, fs::Permissions::from_mode(0o500)).unwrap();
        let result = configs.update_workspace(
            workspace.id,
            ConfigChange::Tools(Some(ToolLimits::default())),
        );
        fs::set_permissions(&project_dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(result.is_err());
        assert_eq!(configs.overrides(workspace.id), RuntimeOverrides::default());
    }
}
