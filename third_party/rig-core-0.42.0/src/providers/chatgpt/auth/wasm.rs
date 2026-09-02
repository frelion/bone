//! WASM ChatGPT auth implementation.

use super::{AuthContext, AuthError, DeviceCodeHandler};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub(super) struct PlatformAuthenticator;

impl PlatformAuthenticator {
    pub(super) fn new(
        _auth_file: Option<PathBuf>,
        _device_code_handler: DeviceCodeHandler,
        _allow_device_flow: bool,
    ) -> Self {
        Self
    }

    pub(super) async fn auth_context_oauth(&self) -> Result<AuthContext, AuthError> {
        Err(AuthError::Message(
            "ChatGPT OAuth is not supported on wasm targets".into(),
        ))
    }

    pub(super) async fn refresh_after_rejection_oauth(
        &self,
        _access_token: &str,
    ) -> Result<AuthContext, AuthError> {
        Err(AuthError::Message(
            "ChatGPT OAuth is not supported on wasm targets".into(),
        ))
    }

    pub(super) fn invalidate_after_rejection_oauth(
        &self,
        _access_token: &str,
    ) -> Result<(), AuthError> {
        Err(AuthError::Message(
            "ChatGPT OAuth is not supported on wasm targets".into(),
        ))
    }
}
