//! App-owned API-key credentials for LLM provider profiles.
//!
//! Profile definitions live in the App's durable settings, but keys do not:
//! this module stores each key in the platform credential store under its stable
//! profile ID. On macOS that is Keychain Services.

use keyring::{Entry, Error as KeyringError};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::config::Profile;

const SERVICE: &str = "bone-api-key";

/// A textual API key intentionally lacking `Debug` and `Display`.
///
/// Keep it in memory only long enough to construct an LLM endpoint. Its
/// `as_str` accessor exists for that hand-off; it must not be used in logs,
/// errors, or durable records.
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a user-supplied API key without changing it.
    pub fn new(value: String) -> Result<Self, ApiKeyCredentialError> {
        if value.trim().is_empty() {
            return Err(ApiKeyCredentialError::InvalidApiKey);
        }
        Ok(Self(value))
    }

    /// Borrows the key for an immediate request-client construction.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ApiKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

/// Redacted failures while accessing one provider profile's API key.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ApiKeyCredentialError {
    #[error("API key is not configured for this provider profile")]
    MissingApiKey,
    #[error("API key is invalid")]
    InvalidApiKey,
    #[error("API-key credential storage is unavailable")]
    Unavailable,
}

/// A concrete handle to the API-key slot for one stable provider profile.
///
/// The fixed service plus a profile/endpoint-bound account prevents a saved
/// key from being redirected when another store happens to reuse the same
/// profile ID for a different endpoint. The implementation uses the native
/// platform store:
/// Keychain Services on macOS, Windows Credential Manager on Windows, and
/// Secret Service on supported Unix desktops.
pub struct ApiKeyCredentials {
    entry: Entry,
}

impl ApiKeyCredentials {
    /// Opens the credential slot associated with a stable App profile ID.
    pub fn for_profile(profile: &Profile) -> Result<Self, ApiKeyCredentialError> {
        profile
            .validate()
            .map_err(|_| ApiKeyCredentialError::Unavailable)?;
        let entry = Entry::new(SERVICE, &credential_account(profile)?)
            .map_err(|_| ApiKeyCredentialError::Unavailable)?;
        Ok(Self { entry })
    }

    /// Replaces the API key stored for this profile.
    pub fn save(&self, api_key: &ApiKey) -> Result<(), ApiKeyCredentialError> {
        self.entry
            .set_password(api_key.as_str())
            .map_err(|_| ApiKeyCredentialError::Unavailable)
    }

    /// Reads the API key stored for this profile.
    pub fn read(&self) -> Result<ApiKey, ApiKeyCredentialError> {
        let api_key = self.entry.get_password().map_err(map_read_error)?;
        ApiKey::new(api_key)
    }

    /// Removes the API key, treating an already-empty slot as cleared.
    pub fn clear(&self) -> Result<(), ApiKeyCredentialError> {
        match self.entry.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
            Err(_) => Err(ApiKeyCredentialError::Unavailable),
        }
    }
}

fn map_read_error(error: KeyringError) -> ApiKeyCredentialError {
    match error {
        KeyringError::NoEntry => ApiKeyCredentialError::MissingApiKey,
        _ => ApiKeyCredentialError::Unavailable,
    }
}

fn credential_account(profile: &Profile) -> Result<String, ApiKeyCredentialError> {
    let endpoint =
        serde_json::to_vec(&profile.endpoint).map_err(|_| ApiKeyCredentialError::Unavailable)?;
    let mut hasher = Sha256::new();
    hasher.update(b"bone-api-key-slot.v1");
    hasher.update(endpoint);
    let digest = hasher.finalize();
    let fingerprint = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("{}:{fingerprint}", profile.id))
}

#[cfg(test)]
mod tests {
    use std::sync::Once;

    use bone_llm::EndpointConfig;

    use super::{ApiKey, ApiKeyCredentialError, ApiKeyCredentials, Profile};

    fn profile(id: &str, endpoint: EndpointConfig) -> Profile {
        Profile::new(crate::config::ProfileId::new(id).unwrap(), id, endpoint).unwrap()
    }

    fn install_mock_keyring() {
        static INSTALLED: Once = Once::new();
        INSTALLED.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
    }

    #[test]
    fn saves_reads_and_clears_a_profile_key_without_a_system_keychain() {
        install_mock_keyring();
        let profile = profile(
            "read-clear",
            EndpointConfig::OpenAiResponses { base_url: None },
        );
        let credentials = ApiKeyCredentials::for_profile(&profile).unwrap();

        assert!(matches!(
            credentials.read(),
            Err(ApiKeyCredentialError::MissingApiKey)
        ));

        let api_key = ApiKey::new("test-api-key".to_owned()).unwrap();
        credentials.save(&api_key).unwrap();
        let loaded = credentials.read().unwrap();
        assert_eq!(loaded.as_str(), "test-api-key");

        credentials.clear().unwrap();
        credentials.clear().unwrap();
        assert!(matches!(
            credentials.read(),
            Err(ApiKeyCredentialError::MissingApiKey)
        ));
    }

    #[test]
    fn keeps_profile_slots_separate() {
        install_mock_keyring();
        let openai = ApiKeyCredentials::for_profile(&profile(
            "separate-openai",
            EndpointConfig::OpenAiResponses { base_url: None },
        ))
        .unwrap();
        let anthropic = ApiKeyCredentials::for_profile(&profile(
            "separate-anthropic",
            EndpointConfig::AnthropicMessages { base_url: None },
        ))
        .unwrap();
        openai
            .save(&ApiKey::new("openai-key".to_owned()).unwrap())
            .unwrap();

        assert!(matches!(
            anthropic.read(),
            Err(ApiKeyCredentialError::MissingApiKey)
        ));
    }

    #[test]
    fn rejects_an_empty_api_key() {
        assert!(matches!(
            ApiKey::new(String::new()),
            Err(ApiKeyCredentialError::InvalidApiKey)
        ));
    }

    #[test]
    fn same_profile_name_on_a_different_endpoint_cannot_read_the_key() {
        install_mock_keyring();
        let official = profile("shared", EndpointConfig::OpenAiResponses { base_url: None });
        let gateway = profile(
            "shared",
            EndpointConfig::OpenAiResponses {
                base_url: Some("https://gateway.example/v1".into()),
            },
        );
        ApiKeyCredentials::for_profile(&official)
            .unwrap()
            .save(&ApiKey::new("official-key".to_owned()).unwrap())
            .unwrap();
        assert!(matches!(
            ApiKeyCredentials::for_profile(&gateway).unwrap().read(),
            Err(ApiKeyCredentialError::MissingApiKey)
        ));
    }
}
