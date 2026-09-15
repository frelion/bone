//! App-owned, file-backed API-key credentials for LLM provider profiles.

use std::{
    collections::HashSet,
    fs,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    config::Profile,
    safe_file::{self, SafeFileError},
};

const SCHEMA_VERSION: u32 = 1;

/// A textual API key intentionally lacking `Debug`, `Display`, and serde traits.
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: String) -> Result<Self, ApiKeyCredentialError> {
        if value.trim().is_empty() {
            return Err(ApiKeyCredentialError::InvalidApiKey);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ApiKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ApiKeyCredentialError {
    #[error("API key is not configured for this provider profile")]
    MissingApiKey,
    #[error("API key is invalid")]
    InvalidApiKey,
    #[error("API-key credential file is unavailable or unsafe")]
    Unavailable,
    #[error("saved API key belongs to a different provider endpoint")]
    EndpointMismatch,
}

pub struct ApiKeyCredentials {
    path: PathBuf,
    lock_path: PathBuf,
    profile_id: String,
    endpoint_fingerprint: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    schema_version: u32,
    #[serde(default)]
    api_keys: Vec<CredentialRecord>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRecord {
    profile_id: String,
    endpoint_fingerprint: String,
    api_key: String,
}

impl ApiKeyCredentials {
    pub fn for_profile(bone_home: &Path, profile: &Profile) -> Result<Self, ApiKeyCredentialError> {
        profile
            .validate()
            .map_err(|_| ApiKeyCredentialError::Unavailable)?;
        if !bone_home.is_absolute() {
            return Err(ApiKeyCredentialError::Unavailable);
        }
        safe(safe_file::ensure_private_directory(bone_home))?;
        Ok(Self {
            path: bone_home.join("credentials.toml"),
            lock_path: bone_home.join("credentials.lock"),
            profile_id: profile.id.to_string(),
            endpoint_fingerprint: endpoint_fingerprint(profile)?,
        })
    }

    pub fn save(&self, api_key: &ApiKey) -> Result<(), ApiKeyCredentialError> {
        let _lock = self.lock()?;
        let mut file = self.load()?;
        match file
            .api_keys
            .iter_mut()
            .find(|record| record.profile_id == self.profile_id)
        {
            Some(record) => {
                record.endpoint_fingerprint = self.endpoint_fingerprint.clone();
                record.api_key = api_key.as_str().to_owned();
            }
            None => file.api_keys.push(CredentialRecord {
                profile_id: self.profile_id.clone(),
                endpoint_fingerprint: self.endpoint_fingerprint.clone(),
                api_key: api_key.as_str().to_owned(),
            }),
        }
        self.store(&file)
    }

    pub fn read(&self) -> Result<ApiKey, ApiKeyCredentialError> {
        let _lock = self.lock()?;
        let file = self.load()?;
        let record = file
            .api_keys
            .iter()
            .find(|record| record.profile_id == self.profile_id)
            .ok_or(ApiKeyCredentialError::MissingApiKey)?;
        if record.endpoint_fingerprint != self.endpoint_fingerprint {
            return Err(ApiKeyCredentialError::EndpointMismatch);
        }
        ApiKey::new(record.api_key.clone())
    }

    pub fn clear(&self) -> Result<(), ApiKeyCredentialError> {
        let _lock = self.lock()?;
        let mut file = self.load()?;
        file.api_keys
            .retain(|record| record.profile_id != self.profile_id);
        if self.path.exists() {
            self.store(&file)?;
        }
        Ok(())
    }

    fn lock(&self) -> Result<std::fs::File, ApiKeyCredentialError> {
        safe(safe_file::lock_private_file(&self.lock_path))
    }

    fn load(&self) -> Result<CredentialFile, ApiKeyCredentialError> {
        match fs::symlink_metadata(&self.path) {
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {
                return Ok(CredentialFile {
                    schema_version: SCHEMA_VERSION,
                    api_keys: Vec::new(),
                });
            }
            Err(_) => return Err(ApiKeyCredentialError::Unavailable),
        }
        let mut source = safe(safe_file::open_existing_private_file(&self.path))?
            .ok_or(ApiKeyCredentialError::Unavailable)?;
        let mut bytes = Vec::new();
        source
            .read_to_end(&mut bytes)
            .map_err(|_| ApiKeyCredentialError::Unavailable)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| ApiKeyCredentialError::Unavailable)?;
        let file: CredentialFile =
            toml::from_str(text).map_err(|_| ApiKeyCredentialError::Unavailable)?;
        if file.schema_version != SCHEMA_VERSION {
            return Err(ApiKeyCredentialError::Unavailable);
        }
        let mut profiles = HashSet::new();
        for record in &file.api_keys {
            crate::ProfileId::new(record.profile_id.clone())
                .map_err(|_| ApiKeyCredentialError::Unavailable)?;
            if !profiles.insert(record.profile_id.as_str())
                || record.endpoint_fingerprint.len() != 64
                || !record
                    .endpoint_fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                || ApiKey::new(record.api_key.clone()).is_err()
            {
                return Err(ApiKeyCredentialError::Unavailable);
            }
        }
        Ok(file)
    }

    fn store(&self, file: &CredentialFile) -> Result<(), ApiKeyCredentialError> {
        let text = toml::to_string_pretty(file).map_err(|_| ApiKeyCredentialError::Unavailable)?;
        let parent = self
            .path
            .parent()
            .ok_or(ApiKeyCredentialError::Unavailable)?;
        safe(safe_file::ensure_private_directory(parent))?;
        safe(safe_file::atomic_write_private(&self.path, text.as_bytes()))
    }
}

fn endpoint_fingerprint(profile: &Profile) -> Result<String, ApiKeyCredentialError> {
    let mut endpoint = profile.endpoint.protocol().as_str().to_owned();
    if let Some(base_url) = profile.endpoint.base_url() {
        let mut url = url::Url::parse(base_url).map_err(|_| ApiKeyCredentialError::Unavailable)?;
        let normalized_path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&normalized_path);
        endpoint.push('|');
        endpoint.push_str(url.as_str().trim_end_matches('/'));
    } else {
        endpoint.push_str("|official");
    }
    let mut hasher = Sha256::new();
    hasher.update(b"bone-api-key-slot-v1");
    hasher.update(endpoint.as_bytes());
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn safe<T>(result: Result<T, SafeFileError>) -> Result<T, ApiKeyCredentialError> {
    result.map_err(|_| ApiKeyCredentialError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProfileId;
    use bone_adapters::llm::EndpointConfig;

    fn profile(endpoint: EndpointConfig) -> Profile {
        Profile::new(ProfileId::new("test").unwrap(), "Test", endpoint).unwrap()
    }

    #[test]
    fn saves_reads_clears_and_binds_endpoint() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".bone");
        let official = profile(EndpointConfig::OpenAiResponses { base_url: None });
        let saved = ApiKeyCredentials::for_profile(&home, &official).unwrap();
        saved.save(&ApiKey::new("secret".into()).unwrap()).unwrap();
        assert_eq!(saved.read().unwrap().as_str(), "secret");
        let redirected = profile(EndpointConfig::OpenAiResponses {
            base_url: Some("https://example.test/v1".into()),
        });
        assert!(matches!(
            ApiKeyCredentials::for_profile(&home, &redirected)
                .unwrap()
                .read(),
            Err(ApiKeyCredentialError::EndpointMismatch)
        ));
        saved.clear().unwrap();
        assert!(matches!(
            saved.read(),
            Err(ApiKeyCredentialError::MissingApiKey)
        ));
    }

    #[test]
    fn endpoint_fingerprint_normalizes_scheme_authority_and_trailing_slash() {
        let first = profile(EndpointConfig::OpenAiResponses {
            base_url: Some("https://EXAMPLE.test:443/a/../v1/".into()),
        });
        let second = profile(EndpointConfig::OpenAiResponses {
            base_url: Some("HTTPS://example.test/v1".into()),
        });
        assert_eq!(endpoint_fingerprint(&first), endpoint_fingerprint(&second));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symbolic_links_and_group_readable_credential_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".bone");
        fs::create_dir_all(&home).unwrap();
        let target = temp.path().join("target.toml");
        fs::write(&target, "schema_version = 1\n").unwrap();
        symlink(&target, home.join("credentials.toml")).unwrap();
        let credentials = ApiKeyCredentials::for_profile(
            &home,
            &profile(EndpointConfig::OpenAiResponses { base_url: None }),
        )
        .unwrap();
        assert!(matches!(
            credentials.read(),
            Err(ApiKeyCredentialError::Unavailable)
        ));

        fs::remove_file(home.join("credentials.toml")).unwrap();
        fs::write(home.join("credentials.toml"), "schema_version = 1\n").unwrap();
        fs::set_permissions(
            home.join("credentials.toml"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        assert!(matches!(
            credentials.read(),
            Err(ApiKeyCredentialError::Unavailable)
        ));
    }

    #[test]
    fn rejects_duplicate_records_without_exposing_their_secret() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(".bone");
        let profile = profile(EndpointConfig::OpenAiResponses { base_url: None });
        let credentials = ApiKeyCredentials::for_profile(&home, &profile).unwrap();
        let fingerprint = endpoint_fingerprint(&profile).unwrap();
        let contents = format!(
            "schema_version = 1\n\n[[api_keys]]\nprofile_id = \"test\"\nendpoint_fingerprint = \"{fingerprint}\"\napi_key = \"do-not-print\"\n\n[[api_keys]]\nprofile_id = \"test\"\nendpoint_fingerprint = \"{fingerprint}\"\napi_key = \"also-secret\"\n"
        );
        fs::write(&credentials.path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&credentials.path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let error = credentials.read().err().unwrap();
        assert_eq!(error, ApiKeyCredentialError::Unavailable);
        let diagnostic = format!("{error:?} {error}");
        assert!(!diagnostic.contains("do-not-print"));
        assert!(!diagnostic.contains("also-secret"));
    }

    #[test]
    fn concurrent_profile_writes_merge_under_the_file_lock() {
        let temp = tempfile::tempdir().unwrap();
        let home = std::sync::Arc::new(temp.path().join(".bone"));
        let threads = (0..8)
            .map(|index| {
                let home = std::sync::Arc::clone(&home);
                std::thread::spawn(move || {
                    let profile = Profile::new(
                        ProfileId::new(format!("test-{index}")).unwrap(),
                        format!("Test {index}"),
                        EndpointConfig::OpenAiResponses { base_url: None },
                    )
                    .unwrap();
                    let credentials = ApiKeyCredentials::for_profile(&home, &profile).unwrap();
                    credentials
                        .save(&ApiKey::new(format!("secret-{index}")).unwrap())
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        for index in 0..8 {
            let profile = Profile::new(
                ProfileId::new(format!("test-{index}")).unwrap(),
                format!("Test {index}"),
                EndpointConfig::OpenAiResponses { base_url: None },
            )
            .unwrap();
            assert_eq!(
                ApiKeyCredentials::for_profile(&home, &profile)
                    .unwrap()
                    .read()
                    .unwrap()
                    .as_str(),
                format!("secret-{index}")
            );
        }
    }
}
