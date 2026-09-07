use std::{
    fmt, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{
    ProviderAuthError, StoreError,
    security::{
        ensure_private_directory, existing_private_file, sync_directory, try_acquire_private_lock,
        validate_existing_private_directory, write_new_private_file,
    },
    sqlite::StoreInner,
};

/// A provider OAuth cache owned by an external integration rather than BONE's
/// SQLite schema.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderId {
    ChatGptSubscription,
}

impl ProviderId {
    fn directory_name(self) -> &'static str {
        match self {
            Self::ChatGptSubscription => "chatgpt-subscription",
        }
    }
}

/// Capability for provider-managed OAuth files beneath this store's explicit
/// private config root.
#[derive(Clone)]
pub struct ProviderAuthStore {
    inner: Arc<StoreInner>,
}

impl ProviderAuthStore {
    pub(crate) fn new(inner: Arc<StoreInner>) -> Self {
        Self { inner }
    }

    /// Acquire a process-lifetime exclusive lease for a provider cache.
    ///
    /// The provider owns `auth.json` content. BONE only verifies the file's
    /// safety, initializes a missing cache to `{}`, and holds `auth.lock`
    /// until every endpoint/model that relies on it is dropped.
    pub fn acquire(&self, provider: ProviderId) -> Result<ProviderAuthLease, ProviderAuthError> {
        self.acquire_inner(provider).map_err(Into::into)
    }

    /// Delete a provider's local OAuth cache without revoking the upstream
    /// account. This is a no-op when no cache exists and never creates
    /// directory artifacts in that case.
    pub fn clear(&self, provider: ProviderId) -> Result<(), ProviderAuthError> {
        self.clear_inner(provider).map_err(Into::into)
    }

    fn acquire_inner(&self, provider: ProviderId) -> Result<ProviderAuthLease, StoreError> {
        let paths = self.provider_paths_create(provider)?;
        let lock = try_acquire_private_lock(&paths.lock_file)?;

        match existing_private_file(&paths.auth_file)? {
            Some(file) => drop(file),
            None => {
                if write_new_private_file(&paths.auth_file, b"{}")? {
                    sync_directory(&paths.service_directory)?;
                } else {
                    // Another process may have created it between our first
                    // check and creation attempt. The held lock means it is
                    // not another well-behaved BONE process; still validate
                    // before exposing a path to Rig.
                    drop(
                        existing_private_file(&paths.auth_file)?.ok_or(StoreError::Corrupt {
                            message: "provider auth file disappeared during initialization",
                        })?,
                    );
                }
            }
        }

        Ok(ProviderAuthLease {
            inner: Arc::new(ProviderAuthLeaseInner {
                provider,
                auth_file: paths.auth_file,
                _lock: lock,
            }),
        })
    }

    fn clear_inner(&self, provider: ProviderId) -> Result<(), StoreError> {
        let Some(paths) = self.provider_paths_existing(provider)? else {
            return Ok(());
        };

        // Do not create a lock sidecar just to log out from a never-created
        // auth file. It is both a cleaner user experience and avoids hidden
        // filesystem writes for an idempotent no-op.
        if existing_private_file(&paths.auth_file)?.is_none() {
            return Ok(());
        }
        let _lock = try_acquire_private_lock(&paths.lock_file)?;
        let Some(file) = existing_private_file(&paths.auth_file)? else {
            return Ok(());
        };
        drop(file);
        fs::remove_file(&paths.auth_file).map_err(|error| {
            StoreError::io("remove provider auth file", &paths.auth_file, error)
        })?;
        sync_directory(&paths.service_directory)
    }

    fn provider_paths_create(&self, provider: ProviderId) -> Result<ProviderPaths, StoreError> {
        let config_root = ensure_private_directory(self.inner.roots().config_root())?;
        let providers = ensure_private_directory(&config_root.join("providers"))?;
        let service_directory =
            ensure_private_directory(&providers.join(provider.directory_name()))?;
        Ok(ProviderPaths::new(service_directory))
    }

    fn provider_paths_existing(
        &self,
        provider: ProviderId,
    ) -> Result<Option<ProviderPaths>, StoreError> {
        let config_root = match fs::symlink_metadata(self.inner.roots().config_root()) {
            Ok(_) => validate_existing_private_directory(self.inner.roots().config_root())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StoreError::io(
                    "inspect provider config root",
                    self.inner.roots().config_root(),
                    error,
                ));
            }
        };
        let providers_path = config_root.join("providers");
        let providers = match fs::symlink_metadata(&providers_path) {
            Ok(_) => validate_existing_private_directory(&providers_path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(StoreError::io(
                    "inspect provider directory",
                    &providers_path,
                    error,
                ));
            }
        };
        let service_path = providers.join(provider.directory_name());
        match fs::symlink_metadata(&service_path) {
            Ok(_) => Ok(Some(ProviderPaths::new(
                validate_existing_private_directory(&service_path)?,
            ))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StoreError::io(
                "inspect provider service directory",
                &service_path,
                error,
            )),
        }
    }
}

impl fmt::Debug for ProviderAuthStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAuthStore")
            .finish_non_exhaustive()
    }
}

struct ProviderPaths {
    service_directory: PathBuf,
    auth_file: PathBuf,
    lock_file: PathBuf,
}

impl ProviderPaths {
    fn new(service_directory: PathBuf) -> Self {
        Self {
            auth_file: service_directory.join("auth.json"),
            lock_file: service_directory.join("auth.lock"),
            service_directory,
        }
    }
}

struct ProviderAuthLeaseInner {
    provider: ProviderId,
    auth_file: PathBuf,
    _lock: fs::File,
}

/// A cloneable, long-lived lease over one provider's private OAuth cache.
///
/// It deliberately exposes only the verified cache file path required by Rig;
/// it never parses, serializes, logs, or otherwise exposes credential bytes.
#[derive(Clone)]
pub struct ProviderAuthLease {
    inner: Arc<ProviderAuthLeaseInner>,
}

impl ProviderAuthLease {
    pub fn provider(&self) -> ProviderId {
        self.inner.provider
    }

    pub fn auth_file(&self) -> &Path {
        &self.inner.auth_file
    }
}

impl fmt::Debug for ProviderAuthLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderAuthLease")
            .field("provider", &self.inner.provider)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{BoneStore, StoreRoots};

    fn store() -> (tempfile::TempDir, BoneStore) {
        let temporary = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        fs::set_permissions(
            temporary.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        )
        .unwrap();
        let store = BoneStore::open_at(
            StoreRoots::new(
                temporary.path().join("data"),
                temporary.path().join("config"),
            )
            .unwrap(),
        )
        .unwrap();
        (temporary, store)
    }

    #[test]
    fn provider_cache_is_private_and_exclusive() {
        let (_temporary, store) = store();
        let auth = store.provider_auth();
        let lease = auth.acquire(ProviderId::ChatGptSubscription).unwrap();
        assert_eq!(fs::read_to_string(lease.auth_file()).unwrap(), "{}");
        assert!(matches!(
            auth.acquire(ProviderId::ChatGptSubscription),
            Err(ProviderAuthError::Busy)
        ));
        assert_eq!(
            auth.clear(ProviderId::ChatGptSubscription),
            Err(ProviderAuthError::Busy)
        );
        drop(lease);
        auth.clear(ProviderId::ChatGptSubscription).unwrap();
    }

    #[test]
    fn clearing_a_missing_cache_creates_no_provider_artifacts() {
        let (_temporary, store) = store();
        store
            .provider_auth()
            .clear(ProviderId::ChatGptSubscription)
            .unwrap();
        assert!(!store.roots().config_root().exists());
    }

    #[test]
    fn provider_lease_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<ProviderAuthLease>();
    }
}
