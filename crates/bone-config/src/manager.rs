use std::{
    any::TypeId,
    collections::BTreeMap,
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::ConfigError;

const MAX_CONFIG_BYTES: usize = 1024 * 1024;

/// A typed, independently owned section of the shared configuration file.
///
/// Implementations should use `#[serde(deny_unknown_fields)]` on object-like
/// values so misspelled settings fail locally instead of being ignored.
pub trait ConfigSection: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Stable dot-separated key used in the shared file and by the config tool.
    const KEY: &'static str;

    /// Short human- and model-facing purpose of this section.
    fn description() -> &'static str;

    /// JSON Schema shown to humans and models that edit this section.
    fn schema() -> Value;

    /// Domain validation that runs after deserialization.
    ///
    /// The returned message is intended for a human-facing client and must
    /// describe the rule without copying values from the configuration.
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

type ValidateValue = dyn Fn(&Value) -> Result<(), ConfigError> + Send + Sync;

#[derive(Clone)]
struct RegisteredSection {
    info: ConfigSectionInfo,
    type_id: TypeId,
    validate: Arc<ValidateValue>,
}

/// Public metadata for one registered configuration section.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ConfigSectionInfo {
    pub key: String,
    pub description: String,
    pub schema: Value,
}

/// Opaque content revision used for compare-and-swap writes.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct ConfigRevision([u8; 32]);

impl ConfigRevision {
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

impl fmt::Display for ConfigRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ConfigRevision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ConfigRevision")
            .field(&self.to_string())
            .finish()
    }
}

impl FromStr for ConfigRevision {
    type Err = ConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ConfigError::InvalidRevision);
        }

        let mut revision = [0_u8; 32];
        for (index, slot) in revision.iter_mut().enumerate() {
            let offset = index * 2;
            *slot = u8::from_str_radix(&value[offset..offset + 2], 16)
                .map_err(|_| ConfigError::InvalidRevision)?;
        }
        Ok(Self(revision))
    }
}

/// Result of an idempotent section mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigChange {
    pub revision: ConfigRevision,
    pub changed: bool,
}

/// An immutable, validated view of all configured sections.
#[derive(Clone)]
pub struct ConfigSnapshot {
    values: Arc<BTreeMap<String, Value>>,
    revision: ConfigRevision,
    sections: Arc<BTreeMap<String, RegisteredSection>>,
}

impl fmt::Debug for ConfigSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigSnapshot")
            .field("revision", &self.revision)
            .field("configured_sections", &self.values.len())
            .finish()
    }
}

impl ConfigSnapshot {
    pub fn revision(&self) -> &ConfigRevision {
        &self.revision
    }

    pub fn contains(&self, section: &str) -> bool {
        self.values.contains_key(section)
    }

    pub fn value(&self, section: &str) -> Option<&Value> {
        self.values.get(section)
    }

    pub fn get<T>(&self) -> Result<Option<T>, ConfigError>
    where
        T: ConfigSection,
    {
        check_registered_type::<T>(&self.sections)?;
        self.value(T::KEY)
            .map(|value| decode_section::<T>(value))
            .transpose()
    }
}

/// Builder used to register every section before constructing the shared store.
#[derive(Default)]
pub struct ConfigManagerBuilder {
    sections: BTreeMap<String, RegisteredSection>,
}

impl ConfigManagerBuilder {
    pub fn register<T>(mut self) -> Result<Self, ConfigError>
    where
        T: ConfigSection,
    {
        validate_section_key(T::KEY)?;
        if self.sections.contains_key(T::KEY) {
            return Err(ConfigError::DuplicateSection(T::KEY.to_owned()));
        }

        let schema = T::schema();
        if !schema.is_object() {
            return Err(ConfigError::InvalidSection {
                section: T::KEY.to_owned(),
                message: "schema must be a JSON object".to_owned(),
            });
        }

        let registered = RegisteredSection {
            info: ConfigSectionInfo {
                key: T::KEY.to_owned(),
                description: T::description().to_owned(),
                schema,
            },
            type_id: TypeId::of::<T>(),
            validate: Arc::new(|value| decode_section::<T>(value).map(|_| ())),
        };
        self.sections.insert(T::KEY.to_owned(), registered);
        Ok(self)
    }

    /// Build a configuration manager at an explicit absolute path.
    ///
    /// The parent directory is created when missing. The configuration file
    /// itself is created only by the first successful mutation.
    pub fn build(self, path: impl AsRef<Path>) -> Result<ConfigManager, ConfigError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(ConfigError::RelativePath);
        }
        let parent = path.parent().ok_or(ConfigError::MissingParent)?;
        ensure_parent(parent)?;

        let manager = ConfigManager {
            path: Arc::new(path.to_path_buf()),
            lock_path: Arc::new(lock_path(path)),
            sections: Arc::new(self.sections),
        };
        manager.snapshot()?;
        Ok(manager)
    }
}

/// Thread-safe typed configuration service shared by UI, CLI, and tools.
#[derive(Clone)]
pub struct ConfigManager {
    path: Arc<PathBuf>,
    lock_path: Arc<PathBuf>,
    sections: Arc<BTreeMap<String, RegisteredSection>>,
}

impl fmt::Debug for ConfigManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConfigManager")
            .field("path", &self.path)
            .field("registered_sections", &self.sections.len())
            .finish()
    }
}

impl ConfigManager {
    pub fn builder() -> ConfigManagerBuilder {
        ConfigManagerBuilder::default()
    }

    pub fn sections(&self) -> Vec<ConfigSectionInfo> {
        self.sections
            .values()
            .map(|section| section.info.clone())
            .collect()
    }

    pub fn section(&self, key: &str) -> Option<ConfigSectionInfo> {
        self.sections.get(key).map(|section| section.info.clone())
    }

    pub fn schema(&self, key: &str) -> Result<Value, ConfigError> {
        self.section(key)
            .map(|section| section.schema)
            .ok_or_else(|| ConfigError::UnknownSection(key.to_owned()))
    }

    pub fn snapshot(&self) -> Result<ConfigSnapshot, ConfigError> {
        let _lock = StoreLock::acquire(&self.lock_path)?;
        self.read_snapshot()
    }

    pub fn set<T>(
        &self,
        value: &T,
        expected_revision: &ConfigRevision,
    ) -> Result<ConfigChange, ConfigError>
    where
        T: ConfigSection,
    {
        check_registered_type::<T>(&self.sections)?;
        let value = serde_json::to_value(value)?;
        self.set_value(T::KEY, value, expected_revision)
    }

    pub fn remove<T>(&self, expected_revision: &ConfigRevision) -> Result<ConfigChange, ConfigError>
    where
        T: ConfigSection,
    {
        check_registered_type::<T>(&self.sections)?;
        self.remove_value(T::KEY, expected_revision)
    }

    /// Validate one dynamic value without changing persistent state.
    pub fn validate_value(&self, section: &str, value: &Value) -> Result<(), ConfigError> {
        let registered = self
            .sections
            .get(section)
            .ok_or_else(|| ConfigError::UnknownSection(section.to_owned()))?;
        (registered.validate)(value)
    }

    pub fn set_value(
        &self,
        section: &str,
        value: Value,
        expected_revision: &ConfigRevision,
    ) -> Result<ConfigChange, ConfigError> {
        self.validate_value(section, &value)?;

        let _lock = StoreLock::acquire(&self.lock_path)?;
        let current = self.read_snapshot()?;
        if current.revision != *expected_revision {
            return Err(ConfigError::RevisionConflict);
        }
        if current.value(section) == Some(&value) {
            return Ok(ConfigChange {
                revision: current.revision,
                changed: false,
            });
        }

        let mut values = (*current.values).clone();
        values.insert(section.to_owned(), value);
        let revision = self.write_values(&values)?;
        Ok(ConfigChange {
            revision,
            changed: true,
        })
    }

    pub fn remove_value(
        &self,
        section: &str,
        expected_revision: &ConfigRevision,
    ) -> Result<ConfigChange, ConfigError> {
        if !self.sections.contains_key(section) {
            return Err(ConfigError::UnknownSection(section.to_owned()));
        }

        let _lock = StoreLock::acquire(&self.lock_path)?;
        let current = self.read_snapshot()?;
        if current.revision != *expected_revision {
            return Err(ConfigError::RevisionConflict);
        }

        let mut values = (*current.values).clone();
        if values.remove(section).is_none() {
            return Ok(ConfigChange {
                revision: current.revision,
                changed: false,
            });
        }
        let revision = self.write_values(&values)?;
        Ok(ConfigChange {
            revision,
            changed: true,
        })
    }

    fn read_snapshot(&self) -> Result<ConfigSnapshot, ConfigError> {
        let (values, bytes) = read_values(&self.path)?;
        self.validate_values(&values)?;
        Ok(ConfigSnapshot {
            values: Arc::new(values),
            revision: ConfigRevision::from_bytes(&bytes),
            sections: Arc::clone(&self.sections),
        })
    }

    fn validate_values(&self, values: &BTreeMap<String, Value>) -> Result<(), ConfigError> {
        for (key, value) in values {
            let registered = self
                .sections
                .get(key)
                .ok_or_else(|| ConfigError::UnknownSection(key.clone()))?;
            (registered.validate)(value)?;
        }
        Ok(())
    }

    fn write_values(
        &self,
        values: &BTreeMap<String, Value>,
    ) -> Result<ConfigRevision, ConfigError> {
        self.validate_values(values)?;
        let mut bytes = serde_json::to_vec_pretty(values)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(ConfigError::DocumentTooLarge {
                maximum_bytes: MAX_CONFIG_BYTES,
            });
        }

        validate_existing_file(&self.path)?;
        let parent = self.path.parent().ok_or(ConfigError::MissingParent)?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".bone-config-")
            .tempfile_in(parent)
            .map_err(|source| ConfigError::io("create temporary file", parent, source))?;
        set_config_file_permissions(temporary.as_file(), &self.path)?;
        temporary
            .write_all(&bytes)
            .map_err(|source| ConfigError::io("write temporary file", temporary.path(), source))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|source| ConfigError::io("sync temporary file", temporary.path(), source))?;
        temporary.persist(&*self.path).map_err(|error| {
            ConfigError::io("replace configuration file", &*self.path, error.error)
        })?;
        sync_directory(parent)?;
        Ok(ConfigRevision::from_bytes(&bytes))
    }
}

fn decode_section<T>(value: &Value) -> Result<T, ConfigError>
where
    T: ConfigSection,
{
    let decoded: T =
        serde_json::from_value(value.clone()).map_err(|_| ConfigError::InvalidSection {
            section: T::KEY.to_owned(),
            message: "value does not match the registered type".to_owned(),
        })?;
    decoded
        .validate()
        .map_err(|message| ConfigError::InvalidSection {
            section: T::KEY.to_owned(),
            message,
        })?;
    Ok(decoded)
}

fn check_registered_type<T>(
    sections: &BTreeMap<String, RegisteredSection>,
) -> Result<(), ConfigError>
where
    T: ConfigSection,
{
    match sections.get(T::KEY) {
        Some(section) if section.type_id == TypeId::of::<T>() => Ok(()),
        Some(_) => Err(ConfigError::SectionTypeMismatch(T::KEY.to_owned())),
        None => Err(ConfigError::UnknownSection(T::KEY.to_owned())),
    }
}

fn validate_section_key(key: &str) -> Result<(), ConfigError> {
    let valid = !key.is_empty()
        && key.len() <= 128
        && key.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                })
        });
    if valid {
        Ok(())
    } else {
        Err(ConfigError::InvalidSectionKey(key.to_owned()))
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

fn ensure_parent(path: &Path) -> Result<(), ConfigError> {
    if !path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(path).map_err(|source| {
                ConfigError::io("create configuration directory", path, source)
            })?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(path)
            .map_err(|source| ConfigError::io("create configuration directory", path, source))?;
    }

    let metadata = fs::symlink_metadata(path)
        .map_err(|source| ConfigError::io("inspect configuration directory", path, source))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(ConfigError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "parent must be a real directory".to_owned(),
        });
    }
    Ok(())
}

fn read_values(path: &Path) -> Result<(BTreeMap<String, Value>, Vec<u8>), ConfigError> {
    let mut file = match open_existing_file(path)? {
        Some(file) => file,
        None => return Ok((BTreeMap::new(), b"{}".to_vec())),
    };
    let metadata = file
        .metadata()
        .map_err(|source| ConfigError::io("inspect configuration file", path, source))?;
    if metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(ConfigError::DocumentTooLarge {
            maximum_bytes: MAX_CONFIG_BYTES,
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| ConfigError::io("read configuration file", path, source))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::DocumentTooLarge {
            maximum_bytes: MAX_CONFIG_BYTES,
        });
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| ConfigError::InvalidDocument)?;
    let object = value.as_object().ok_or(ConfigError::InvalidDocument)?;
    Ok((
        object
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
        bytes,
    ))
}

fn open_existing_file(path: &Path) -> Result<Option<File>, ConfigError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(ConfigError::UnsafeStorage {
                path: path.to_path_buf(),
                reason: "configuration must be a regular file".to_owned(),
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConfigError::io("inspect configuration file", path, source));
        }
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ConfigError::io("open configuration file", path, source)),
    };
    validate_open_file(&file, path)?;
    Ok(Some(file))
}

fn validate_existing_file(path: &Path) -> Result<(), ConfigError> {
    let _ = open_existing_file(path)?;
    Ok(())
}

fn validate_open_file(file: &File, path: &Path) -> Result<(), ConfigError> {
    let metadata = file
        .metadata()
        .map_err(|source| ConfigError::io("inspect configuration file", path, source))?;
    if !metadata.file_type().is_file() {
        return Err(ConfigError::UnsafeStorage {
            path: path.to_path_buf(),
            reason: "configuration must be a regular file".to_owned(),
        });
    }
    Ok(())
}

struct StoreLock {
    file: File,
}

impl StoreLock {
    fn acquire(path: &Path) -> Result<Self, ConfigError> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options
            .open(path)
            .map_err(|source| ConfigError::io("open configuration lock", path, source))?;
        validate_open_file(&file, path)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(ConfigError::Busy);
            }
            Err(source) => {
                return Err(ConfigError::io("lock configuration", path, source));
            }
        }
        Ok(Self { file })
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(unix)]
fn set_config_file_permissions(file: &File, target: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mode = match fs::symlink_metadata(target) {
        Ok(metadata) => metadata.mode() & 0o777,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0o600,
        Err(source) => {
            return Err(ConfigError::io(
                "inspect configuration permissions",
                target,
                source,
            ));
        }
    };
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|source| ConfigError::io("set configuration permissions", target, source))
}

#[cfg(not(unix))]
fn set_config_file_permissions(_: &File, _: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), ConfigError> {
    let directory = File::open(path)
        .map_err(|source| ConfigError::io("open configuration directory", path, source))?;
    directory
        .sync_all()
        .map_err(|source| ConfigError::io("sync configuration directory", path, source))
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) -> Result<(), ConfigError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc, thread};

    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    struct ExampleConfig {
        enabled: bool,
        limit: usize,
    }

    impl ConfigSection for ExampleConfig {
        const KEY: &'static str = "tools.example";

        fn description() -> &'static str {
            "Example settings"
        }

        fn schema() -> Value {
            json!({
                "type": "object",
                "properties": {
                    "enabled": { "type": "boolean" },
                    "limit": { "type": "integer", "minimum": 1 }
                },
                "required": ["enabled", "limit"],
                "additionalProperties": false
            })
        }

        fn validate(&self) -> Result<(), String> {
            if self.limit == 0 {
                Err("limit must be positive".to_owned())
            } else {
                Ok(())
            }
        }
    }

    #[derive(Deserialize, Serialize)]
    struct ConflictingConfig {
        enabled: bool,
    }

    impl ConfigSection for ConflictingConfig {
        const KEY: &'static str = ExampleConfig::KEY;

        fn description() -> &'static str {
            "Conflicting settings"
        }

        fn schema() -> Value {
            json!({"type": "object"})
        }
    }

    #[derive(Deserialize, Serialize)]
    struct UnregisteredConfig;

    impl ConfigSection for UnregisteredConfig {
        const KEY: &'static str = "tools.unregistered";

        fn description() -> &'static str {
            "Unregistered settings"
        }

        fn schema() -> Value {
            json!({"type": "null"})
        }
    }

    fn manager() -> (tempfile::TempDir, ConfigManager) {
        let directory = tempfile::tempdir().unwrap();
        let manager = ConfigManager::builder()
            .register::<ExampleConfig>()
            .unwrap()
            .build(directory.path().join("config.json"))
            .unwrap();
        (directory, manager)
    }

    #[test]
    fn registers_lists_and_reads_typed_sections() {
        let (_directory, manager) = manager();
        assert_eq!(manager.sections()[0].key, ExampleConfig::KEY);
        assert_eq!(
            manager.schema(ExampleConfig::KEY).unwrap()["type"],
            "object"
        );

        let empty = manager.snapshot().unwrap();
        assert_eq!(empty.get::<ExampleConfig>().unwrap(), None);
        let change = manager
            .set(
                &ExampleConfig {
                    enabled: true,
                    limit: 3,
                },
                empty.revision(),
            )
            .unwrap();
        assert!(change.changed);

        let snapshot = manager.snapshot().unwrap();
        assert_eq!(snapshot.revision(), &change.revision);
        assert_eq!(
            snapshot.get::<ExampleConfig>().unwrap(),
            Some(ExampleConfig {
                enabled: true,
                limit: 3
            })
        );
    }

    #[test]
    fn dynamic_mutations_validate_and_are_idempotent() {
        let (_directory, manager) = manager();
        let initial = manager.snapshot().unwrap();
        let change = manager
            .set_value(
                ExampleConfig::KEY,
                json!({"enabled": true, "limit": 4}),
                initial.revision(),
            )
            .unwrap();
        let unchanged = manager
            .set_value(
                ExampleConfig::KEY,
                json!({"enabled": true, "limit": 4}),
                &change.revision,
            )
            .unwrap();
        assert!(!unchanged.changed);
        assert_eq!(unchanged.revision, change.revision);

        let invalid = manager.set_value(
            ExampleConfig::KEY,
            json!({"enabled": true, "limit": 0}),
            &change.revision,
        );
        assert!(matches!(invalid, Err(ConfigError::InvalidSection { .. })));
        assert_eq!(manager.snapshot().unwrap().revision(), &change.revision);

        let removed = manager
            .remove_value(ExampleConfig::KEY, &change.revision)
            .unwrap();
        assert!(removed.changed);
        assert!(!manager.snapshot().unwrap().contains(ExampleConfig::KEY));
    }

    #[test]
    fn stale_writers_cannot_replace_newer_configuration() {
        let (_directory, manager) = manager();
        let initial = manager.snapshot().unwrap();
        let first = manager
            .set_value(
                ExampleConfig::KEY,
                json!({"enabled": true, "limit": 1}),
                initial.revision(),
            )
            .unwrap();
        assert_ne!(first.revision, *initial.revision());

        let stale = manager.set_value(
            ExampleConfig::KEY,
            json!({"enabled": false, "limit": 2}),
            initial.revision(),
        );
        assert!(matches!(stale, Err(ConfigError::RevisionConflict)));
    }

    #[test]
    fn concurrent_writers_have_exactly_one_winner() {
        let (_directory, manager) = manager();
        let manager = Arc::new(manager);
        let revision = manager.snapshot().unwrap().revision().clone();
        let handles = [1, 2].map(|limit| {
            let manager = Arc::clone(&manager);
            let revision = revision.clone();
            thread::spawn(move || {
                manager.set_value(
                    ExampleConfig::KEY,
                    json!({"enabled": true, "limit": limit}),
                    &revision,
                )
            })
        });
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        assert!(results.iter().any(|result| {
            matches!(
                result,
                Err(ConfigError::RevisionConflict | ConfigError::Busy)
            )
        }));
    }

    #[test]
    fn rejects_unknown_sections_and_unknown_fields_on_build() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, br#"{"unknown":{}}"#).unwrap();
        let error = ConfigManager::builder()
            .register::<ExampleConfig>()
            .unwrap()
            .build(&path)
            .unwrap_err();
        assert!(matches!(error, ConfigError::UnknownSection(section) if section == "unknown"));

        fs::write(
            &path,
            br#"{"tools.example":{"enabled":true,"limit":1,"typo":true}}"#,
        )
        .unwrap();
        let error = ConfigManager::builder()
            .register::<ExampleConfig>()
            .unwrap()
            .build(&path)
            .unwrap_err();
        assert!(matches!(error, ConfigError::InvalidSection { .. }));
    }

    #[test]
    fn revision_parser_is_strict() {
        let revision = ConfigRevision::from_bytes(b"example");
        assert_eq!(
            revision.to_string().parse::<ConfigRevision>().unwrap(),
            revision
        );
        assert!(
            revision
                .to_string()
                .to_uppercase()
                .parse::<ConfigRevision>()
                .is_err()
        );
        assert!("0".repeat(63).parse::<ConfigRevision>().is_err());
        assert!("g".repeat(64).parse::<ConfigRevision>().is_err());
    }

    #[test]
    fn typed_access_requires_the_registered_rust_type() {
        let (_directory, manager) = manager();
        let snapshot = manager.snapshot().unwrap();
        assert!(matches!(
            snapshot.get::<ConflictingConfig>(),
            Err(ConfigError::SectionTypeMismatch(section)) if section == ExampleConfig::KEY
        ));
        assert!(matches!(
            snapshot.get::<UnregisteredConfig>(),
            Err(ConfigError::UnknownSection(section)) if section == UnregisteredConfig::KEY
        ));
        assert!(matches!(
            manager.set(&ConflictingConfig { enabled: true }, snapshot.revision()),
            Err(ConfigError::SectionTypeMismatch(_))
        ));
    }

    #[test]
    fn snapshot_debug_does_not_print_values() {
        let (_directory, manager) = manager();
        let initial = manager.snapshot().unwrap();
        manager
            .set_value(
                ExampleConfig::KEY,
                json!({"enabled": true, "limit": 17}),
                initial.revision(),
            )
            .unwrap();
        let debug = format!("{:?}", manager.snapshot().unwrap());
        assert!(debug.contains("configured_sections"));
        assert!(!debug.contains("enabled"));
    }

    #[test]
    fn a_held_store_lock_returns_busy() {
        let (_directory, manager) = manager();
        let _lock = StoreLock::acquire(&manager.lock_path).unwrap();
        assert!(matches!(manager.snapshot(), Err(ConfigError::Busy)));
    }

    #[test]
    fn requires_absolute_paths_and_regular_non_symlink_files() {
        let relative = ConfigManager::builder()
            .register::<ExampleConfig>()
            .unwrap()
            .build("config.json")
            .unwrap_err();
        assert!(matches!(relative, ConfigError::RelativePath));

        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            let directory = tempfile::tempdir().unwrap();
            let readable = directory.path().join("readable.json");
            fs::write(&readable, b"{}").unwrap();
            fs::set_permissions(&readable, fs::Permissions::from_mode(0o644)).unwrap();
            let manager = ConfigManager::builder()
                .register::<ExampleConfig>()
                .unwrap()
                .build(&readable)
                .unwrap();
            let revision = manager.snapshot().unwrap().revision().clone();
            manager
                .set_value(
                    ExampleConfig::KEY,
                    json!({"enabled": true, "limit": 1}),
                    &revision,
                )
                .unwrap();
            assert_eq!(
                fs::metadata(&readable).unwrap().permissions().mode() & 0o777,
                0o644
            );

            let target = directory.path().join("target.json");
            fs::write(&target, b"{}").unwrap();
            fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            let link = directory.path().join("config.json");
            symlink(&target, &link).unwrap();
            assert!(
                ConfigManager::builder()
                    .register::<ExampleConfig>()
                    .unwrap()
                    .build(link)
                    .is_err()
            );
        }
    }
}
