//! Shared ChatGPT authentication types and target-specific dispatch.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub use crate::providers::internal::auth::{DeviceCodeHandler, DeviceCodePrompt};

#[cfg(not(target_family = "wasm"))]
mod native;
#[cfg(target_family = "wasm")]
mod wasm;

#[cfg(not(target_family = "wasm"))]
use native as platform;
#[cfg(target_family = "wasm")]
use wasm as platform;

#[derive(Clone)]
pub enum AuthSource {
    AccessToken {
        access_token: String,
        account_id: Option<String>,
    },
    OAuth,
    #[cfg(test)]
    RejectionTest {
        initial: AuthContext,
        refreshed: AuthContext,
        control: Arc<RejectionTestControl>,
    },
}

impl fmt::Debug for AuthSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccessToken { .. } => f.write_str("AccessToken(<redacted>)"),
            Self::OAuth => f.write_str("OAuth"),
            #[cfg(test)]
            Self::RejectionTest { .. } => f.write_str("RejectionTest(<redacted>)"),
        }
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct RejectionTestControl {
    refreshes: std::sync::atomic::AtomicUsize,
    invalidated_tokens: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl RejectionTestControl {
    pub(crate) fn refreshes(&self) -> usize {
        self.refreshes.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub(crate) fn invalidated_tokens(&self) -> Vec<String> {
        match self.invalidated_tokens.lock() {
            Ok(tokens) => tokens.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

#[derive(Clone)]
pub struct Authenticator {
    source: AuthSource,
    platform: platform::PlatformAuthenticator,
    state_lock: Arc<Mutex<()>>,
}

impl fmt::Debug for Authenticator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Authenticator")
            .field("source", &self.source)
            .field("platform", &self.platform)
            .finish()
    }
}

pub use crate::providers::internal::auth::AuthError;

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub access_token: String,
    pub account_id: Option<String>,
}

impl Authenticator {
    pub fn new(
        source: AuthSource,
        auth_file: Option<PathBuf>,
        device_code_handler: DeviceCodeHandler,
        allow_device_flow: bool,
    ) -> Self {
        Self {
            source,
            platform: platform::PlatformAuthenticator::new(
                auth_file,
                device_code_handler,
                allow_device_flow,
            ),
            state_lock: Arc::new(Mutex::new(())),
        }
    }

    #[cfg(test)]
    pub(crate) fn for_rejection_test(
        initial: AuthContext,
        refreshed: AuthContext,
    ) -> (Self, Arc<RejectionTestControl>) {
        let control = Arc::new(RejectionTestControl::default());
        let source = AuthSource::RejectionTest {
            initial,
            refreshed,
            control: Arc::clone(&control),
        };
        (
            Self::new(source, None, DeviceCodeHandler::default(), false),
            control,
        )
    }

    pub async fn auth_context(&self) -> Result<AuthContext, AuthError> {
        match &self.source {
            AuthSource::AccessToken {
                access_token,
                account_id,
            } => Ok(AuthContext {
                access_token: access_token.clone(),
                account_id: account_id.clone(),
            }),
            AuthSource::OAuth => {
                let _guard = self.state_lock.lock().await;
                self.platform.auth_context_oauth().await
            }
            #[cfg(test)]
            AuthSource::RejectionTest { initial, .. } => Ok(initial.clone()),
        }
    }

    /// Refresh OAuth credentials after the backend rejected `access_token`.
    ///
    /// The expected token makes concurrent rejection handling single-flight:
    /// if another request already replaced it, this call reuses that newer
    /// context instead of refreshing a second time. Static access-token clients
    /// cannot refresh and return the same stable reconnect-required error.
    pub(crate) async fn refresh_after_rejection(
        &self,
        access_token: &str,
    ) -> Result<AuthContext, AuthError> {
        match &self.source {
            AuthSource::AccessToken { .. } => Err(reconnect_required()),
            AuthSource::OAuth => {
                let _guard = self.state_lock.lock().await;
                self.platform
                    .refresh_after_rejection_oauth(access_token)
                    .await
            }
            #[cfg(test)]
            AuthSource::RejectionTest {
                refreshed, control, ..
            } => {
                control
                    .refreshes
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(refreshed.clone())
            }
        }
    }

    /// Remove an OAuth record only if it still contains the rejected token.
    ///
    /// The generation comparison prevents a slower rejected request from
    /// deleting credentials that another request refreshed while it waited for
    /// `state_lock`. Static access-token clients have no cache to invalidate.
    pub(crate) async fn invalidate_after_rejection(
        &self,
        access_token: &str,
    ) -> Result<(), AuthError> {
        match &self.source {
            AuthSource::AccessToken { .. } => Ok(()),
            AuthSource::OAuth => {
                let _guard = self.state_lock.lock().await;
                self.platform.invalidate_after_rejection_oauth(access_token)
            }
            #[cfg(test)]
            AuthSource::RejectionTest { control, .. } => {
                match control.invalidated_tokens.lock() {
                    Ok(mut tokens) => tokens.push(access_token.to_owned()),
                    Err(poisoned) => poisoned.into_inner().push(access_token.to_owned()),
                }
                Ok(())
            }
        }
    }
}

pub(crate) fn reconnect_required() -> AuthError {
    AuthError::Message(
        "ChatGPT authentication was rejected; reconnect the subscription and try again".into(),
    )
}
