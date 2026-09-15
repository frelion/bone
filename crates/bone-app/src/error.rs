use crate::{ConfigProblem, CredentialProblem, ProfileId, RuntimeId, SessionId};

/// Failures a frontend can act on without knowing App internals.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the app is shutting down")]
    Closed,
    #[error("workspace not found")]
    WorkspaceNotFound,
    #[error("session not found")]
    SessionNotFound,
    #[error("session {0} is already open in another process")]
    SessionBusy(SessionId),
    #[error("request ID was reused with different input")]
    RequestConflict,
    #[error("runtime {0} is no longer current")]
    StaleRuntime(RuntimeId),
    #[error("the workspace write is still executing")]
    WriteInProgress,
    #[error("invalid session state: {0}")]
    InvalidState(String),
    #[error("runtime configuration is incomplete: {0:?}")]
    Configuration(ConfigProblem),
    #[error("profile {0} needs login")]
    LoginRequired(ProfileId),
    #[error("{0}")]
    Credential(CredentialProblem),
    #[error("configuration schema version {actual} is unsupported; expected {expected}")]
    ConfigVersion { expected: u32, actual: u32 },
    #[error("configuration file is invalid: {0}")]
    ConfigFile(String),
    #[error("configuration file changed outside BONE: {0}")]
    ConfigConflict(std::path::PathBuf),
    #[error("project configuration is not trusted: {0}")]
    ProjectConfigUntrusted(std::path::PathBuf),
    #[error("profile {0} is still in use")]
    ProfileBusy(ProfileId),
    #[error(
        "credentials for profile {profile} were saved, but running sessions did not reload: {message}"
    )]
    CredentialsSaved { profile: ProfileId, message: String },
    #[error("provider: {0}")]
    Provider(String),
    #[error("storage: {0}")]
    Storage(String),
    #[error("tools: {0}")]
    Tools(String),
    #[error("agent: {0}")]
    Agent(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<crate::storage::StoreError> for Error {
    fn from(error: crate::storage::StoreError) -> Self {
        match error {
            crate::storage::StoreError::ConfigVersion { expected, actual } => {
                Self::ConfigVersion { expected, actual }
            }
            crate::storage::StoreError::ConfigFile { message } => Self::ConfigFile(message),
            crate::storage::StoreError::ConfigConflict { path } => Self::ConfigConflict(path),
            crate::storage::StoreError::ProjectConfigUntrusted { path } => {
                Self::ProjectConfigUntrusted(path)
            }
            error => Self::Storage(error.to_string()),
        }
    }
}
