//! Native ChatGPT OAuth and token cache implementation.

use super::{AuthContext, AuthError, DeviceCodeHandler, DeviceCodePrompt, reconnect_required};
use crate::providers::internal::device_auth::{
    emit_device_code_prompt, read_json_record, remove_json_record, token_expired, write_json_record,
};
use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

const CHATGPT_AUTH_BASE: &str = "https://auth.openai.com";
const CHATGPT_DEVICE_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const CHATGPT_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const CHATGPT_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CHATGPT_DEVICE_VERIFY_URL: &str = "https://auth.openai.com/codex/device";
const CHATGPT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const TOKEN_EXPIRY_SKEW_SECONDS: i64 = 60;
const DEVICE_CODE_TIMEOUT_SECONDS: i64 = 15 * 60;
const DEVICE_CODE_POLL_SLEEP_SECONDS: u64 = 5;

#[derive(Debug, Clone)]
pub(super) struct PlatformAuthenticator {
    auth_file: Option<PathBuf>,
    device_code_handler: DeviceCodeHandler,
    allow_device_flow: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct AuthRecord {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_at: Option<i64>,
    account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(default, deserialize_with = "deserialize_optional_u64")]
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DeviceTokenResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, Deserialize)]
struct OAuthTokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OAuthErrorResponse {
    error: Option<String>,
}

enum RefreshTokensError {
    Reauthenticate,
    Auth(AuthError),
}

impl PlatformAuthenticator {
    pub(super) fn new(
        auth_file: Option<PathBuf>,
        device_code_handler: DeviceCodeHandler,
        allow_device_flow: bool,
    ) -> Self {
        Self {
            auth_file,
            device_code_handler,
            allow_device_flow,
        }
    }

    pub(super) async fn auth_context_oauth(&self) -> Result<AuthContext, AuthError> {
        let mut record: AuthRecord = match read_json_record(self.auth_file.as_deref()) {
            Ok(record) => record,
            // A partial previous write must not permanently brick explicit
            // connection. Only an interactive authenticator may discard it;
            // request-time authenticators remain non-interactive and require a
            // deliberate reconnect.
            Err(AuthError::Json(_)) => {
                remove_json_record(self.auth_file.as_deref())?;
                if self.allow_device_flow {
                    AuthRecord::default()
                } else {
                    return Err(reconnect_required());
                }
            }
            Err(error) => return Err(error),
        };

        if let Some(access_token) = record.access_token.clone()
            && !token_expired(record.expires_at, TOKEN_EXPIRY_SKEW_SECONDS)
        {
            let account_id = record
                .account_id
                .clone()
                .or_else(|| extract_account_id(record.id_token.as_deref()))
                .or_else(|| extract_account_id(Some(&access_token)));
            if account_id != record.account_id {
                record.account_id = account_id.clone();
                write_json_record(self.auth_file.as_deref(), &record)?;
            }
            return Ok(AuthContext {
                access_token,
                account_id,
            });
        }

        if let Some(refresh_token) = record.refresh_token.clone() {
            match self.refresh_tokens(&refresh_token).await {
                Ok(refreshed) => {
                    write_json_record(self.auth_file.as_deref(), &refreshed)?;
                    return Ok(AuthContext {
                        access_token: refreshed.access_token.unwrap_or_default(),
                        account_id: refreshed.account_id,
                    });
                }
                Err(RefreshTokensError::Reauthenticate) => {
                    remove_json_record(self.auth_file.as_deref())?;
                }
                Err(RefreshTokensError::Auth(err)) => return Err(err),
            }
        }

        if !self.allow_device_flow {
            return Err(AuthError::Message(
                "ChatGPT sign-in required. Reconnect ChatGPT in Settings before using this provider."
                    .into(),
            ));
        }

        let fresh = self.login_device_flow().await?;
        write_json_record(self.auth_file.as_deref(), &fresh)?;
        Ok(AuthContext {
            access_token: fresh.access_token.unwrap_or_default(),
            account_id: fresh.account_id,
        })
    }

    pub(super) async fn refresh_after_rejection_oauth(
        &self,
        rejected_access_token: &str,
    ) -> Result<AuthContext, AuthError> {
        let record: AuthRecord = match read_json_record(self.auth_file.as_deref()) {
            Ok(record) => record,
            Err(AuthError::Json(_)) => {
                remove_json_record(self.auth_file.as_deref())?;
                return Err(reconnect_required());
            }
            Err(error) => return Err(error),
        };

        // Another rejected request may have refreshed while this caller waited
        // for Authenticator::state_lock. Reuse that generation instead of
        // creating a refresh storm.
        if let Some(access_token) = record.access_token.clone()
            && access_token != rejected_access_token
            && !token_expired(record.expires_at, TOKEN_EXPIRY_SKEW_SECONDS)
        {
            return Ok(AuthContext {
                access_token,
                account_id: record
                    .account_id
                    .or_else(|| extract_account_id(record.id_token.as_deref())),
            });
        }

        let Some(refresh_token) = record.refresh_token.as_deref() else {
            remove_json_record(self.auth_file.as_deref())?;
            return Err(reconnect_required());
        };

        match self.refresh_tokens(refresh_token).await {
            Ok(refreshed) => {
                write_json_record(self.auth_file.as_deref(), &refreshed)?;
                Ok(AuthContext {
                    access_token: refreshed.access_token.unwrap_or_default(),
                    account_id: refreshed.account_id,
                })
            }
            Err(RefreshTokensError::Reauthenticate) => {
                remove_json_record(self.auth_file.as_deref())?;
                Err(reconnect_required())
            }
            Err(RefreshTokensError::Auth(_)) => Err(reconnect_required()),
        }
    }

    pub(super) fn invalidate_after_rejection_oauth(
        &self,
        rejected_access_token: &str,
    ) -> Result<(), AuthError> {
        let record: AuthRecord = match read_json_record(self.auth_file.as_deref()) {
            Ok(record) => record,
            Err(AuthError::Json(_)) => {
                return remove_json_record(self.auth_file.as_deref());
            }
            Err(error) => return Err(error),
        };

        if record.access_token.as_deref() == Some(rejected_access_token) {
            remove_json_record(self.auth_file.as_deref())?;
        }
        Ok(())
    }

    async fn login_device_flow(&self) -> Result<AuthRecord, AuthError> {
        let client = reqwest::Client::new();
        let device = client
            .post(CHATGPT_DEVICE_CODE_URL)
            .json(&serde_json::json!({ "client_id": CHATGPT_CLIENT_ID }))
            .send()
            .await?
            .error_for_status()?
            .json::<DeviceCodeResponse>()
            .await?;

        emit_device_code_prompt(
            self.device_code_handler.0.as_ref(),
            DeviceCodePrompt {
                verification_uri: CHATGPT_DEVICE_VERIFY_URL.to_string(),
                user_code: device.user_code.clone(),
            },
            &format!(
                "Sign in with ChatGPT:\n1) Visit {CHATGPT_DEVICE_VERIFY_URL}\n2) Enter code: {}\nDo not share this device code.",
                device.user_code
            ),
        );

        let interval = device.interval.unwrap_or(DEVICE_CODE_POLL_SLEEP_SECONDS);
        let start = std::time::Instant::now();
        let code = loop {
            if start.elapsed().as_secs() as i64 >= DEVICE_CODE_TIMEOUT_SECONDS {
                return Err(AuthError::Message(
                    "Timed out waiting for ChatGPT device authorization".into(),
                ));
            }

            let response = client
                .post(CHATGPT_DEVICE_TOKEN_URL)
                .json(&serde_json::json!({
                    "device_auth_id": device.device_auth_id,
                    "user_code": device.user_code,
                }))
                .send()
                .await?;

            if response.status().is_success() {
                let token_response = response.json::<DeviceTokenResponse>().await?;
                break token_response;
            }

            let status = response.status();
            if status.as_u16() == 403 || status.as_u16() == 404 {
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                continue;
            }

            let text = response.text().await.unwrap_or_default();
            return Err(AuthError::Message(format!(
                "ChatGPT device authorization failed: {status} {text}"
            )));
        };

        let redirect_uri = format!("{CHATGPT_AUTH_BASE}/deviceauth/callback");
        let form = [
            ("grant_type", "authorization_code"),
            ("code", code.authorization_code.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", CHATGPT_CLIENT_ID),
            ("code_verifier", code.code_verifier.as_str()),
        ];
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();

        let tokens = client
            .post(CHATGPT_OAUTH_TOKEN_URL)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await?
            .error_for_status()?
            .json::<OAuthTokenResponse>()
            .await?;

        Ok(build_auth_record(tokens, None))
    }

    async fn refresh_tokens(&self, refresh_token: &str) -> Result<AuthRecord, RefreshTokensError> {
        let client = reqwest::Client::new();
        let form = [
            ("client_id", CHATGPT_CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("scope", "openid profile email"),
        ];

        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();

        let response = client
            .post(CHATGPT_OAUTH_TOKEN_URL)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .map_err(AuthError::from)
            .map_err(RefreshTokensError::Auth)?;

        let status = response.status();
        if status.is_success() {
            let tokens = response
                .json::<OAuthTokenResponse>()
                .await
                .map_err(AuthError::from)
                .map_err(RefreshTokensError::Auth)?;
            return Ok(build_auth_record(tokens, Some(refresh_token.to_owned())));
        }

        let body = response.text().await.unwrap_or_default();
        let oauth_error = serde_json::from_str::<OAuthErrorResponse>(&body).ok();
        if should_reauthenticate_after_refresh(
            status,
            oauth_error
                .as_ref()
                .and_then(|error| error.error.as_deref()),
        ) {
            return Err(RefreshTokensError::Reauthenticate);
        }

        Err(RefreshTokensError::Auth(AuthError::Message(
            format_refresh_error(status, oauth_error.as_ref(), &body),
        )))
    }
}

fn build_auth_record(
    tokens: OAuthTokenResponse,
    previous_refresh_token: Option<String>,
) -> AuthRecord {
    let access_token = Some(tokens.access_token);
    let id_token = tokens.id_token;
    AuthRecord {
        expires_at: access_token
            .as_deref()
            .and_then(extract_expiration_timestamp),
        account_id: extract_account_id(id_token.as_deref()).or_else(|| {
            access_token
                .as_deref()
                .and_then(|token| extract_account_id(Some(token)))
        }),
        access_token,
        refresh_token: tokens.refresh_token.or(previous_refresh_token),
        id_token,
    }
}

fn extract_expiration_timestamp(token: &str) -> Option<i64> {
    decode_jwt_claims(token)
        .get("exp")
        .and_then(|value| value.as_i64().or_else(|| value.as_u64().map(|v| v as i64)))
}

fn extract_account_id(token: Option<&str>) -> Option<String> {
    let claims = decode_jwt_claims(token?);
    claims
        .get("https://api.openai.com/auth")
        .and_then(|value| value.as_object())
        .and_then(|map| map.get("chatgpt_account_id"))
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
}

fn decode_jwt_claims(token: &str) -> serde_json::Value {
    let payload = token.split('.').nth(1).unwrap_or_default();
    let decoded = BASE64_URL_SAFE_NO_PAD.decode(payload.as_bytes());
    decoded
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or(serde_json::Value::Null)
}

fn should_reauthenticate_after_refresh(
    status: reqwest::StatusCode,
    error_code: Option<&str>,
) -> bool {
    matches!(
        status,
        reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNAUTHORIZED
    ) && matches!(error_code, Some("invalid_grant"))
}

fn format_refresh_error(
    status: reqwest::StatusCode,
    oauth_error: Option<&OAuthErrorResponse>,
    _body: &str,
) -> String {
    let error_code = oauth_error.and_then(|error| error.error.as_deref());

    if let Some(error_code) = error_code {
        return format!("ChatGPT token refresh failed: {status} {error_code}");
    }

    format!("ChatGPT token refresh failed: {status}")
}

fn deserialize_optional_u64<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum U64OrString {
        U64(u64),
        String(String),
    }

    let value = Option::<U64OrString>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(U64OrString::U64(value)) => Ok(Some(value)),
        Some(U64OrString::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else {
                value
                    .parse::<u64>()
                    .map(Some)
                    .map_err(serde::de::Error::custom)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceCodeHandler, DeviceCodeResponse, OAuthErrorResponse, OAuthTokenResponse,
        PlatformAuthenticator, build_auth_record, format_refresh_error,
        should_reauthenticate_after_refresh,
    };
    use reqwest::StatusCode;

    fn unexpired_access_token(marker: &str) -> String {
        use base64::Engine;
        use base64::prelude::BASE64_URL_SAFE_NO_PAD;

        let expires_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_secs()
            + 3600;
        let claims = serde_json::json!({ "exp": expires_at, "marker": marker });
        format!(
            "header.{}.signature",
            BASE64_URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims"))
        )
    }

    #[test]
    fn device_code_response_accepts_numeric_interval() {
        let response: DeviceCodeResponse = serde_json::from_str(
            r#"{
                "device_auth_id": "deviceauth_123",
                "user_code": "ABCD-EFGH",
                "interval": 5
            }"#,
        )
        .expect("device code response");

        assert_eq!(response.interval, Some(5));
    }

    #[test]
    fn device_code_response_accepts_string_interval() {
        let response: DeviceCodeResponse = serde_json::from_str(
            r#"{
                "device_auth_id": "deviceauth_123",
                "user_code": "ABCD-EFGH",
                "interval": "5"
            }"#,
        )
        .expect("device code response");

        assert_eq!(response.interval, Some(5));
    }

    #[test]
    fn refresh_reauth_only_on_invalid_grant() {
        assert!(should_reauthenticate_after_refresh(
            StatusCode::BAD_REQUEST,
            Some("invalid_grant")
        ));
        assert!(should_reauthenticate_after_refresh(
            StatusCode::UNAUTHORIZED,
            Some("invalid_grant")
        ));
        assert!(!should_reauthenticate_after_refresh(
            StatusCode::BAD_GATEWAY,
            Some("invalid_grant")
        ));
        assert!(!should_reauthenticate_after_refresh(
            StatusCode::BAD_REQUEST,
            Some("invalid_request")
        ));
        assert!(!should_reauthenticate_after_refresh(
            StatusCode::UNAUTHORIZED,
            None
        ));
    }

    #[tokio::test]
    async fn noninteractive_oauth_requires_sign_in_instead_of_device_flow() {
        let auth = PlatformAuthenticator::new(None, DeviceCodeHandler::default(), false);
        let err = auth
            .auth_context_oauth()
            .await
            .expect_err("missing cached auth should not start device flow")
            .to_string();

        assert!(err.contains("ChatGPT sign-in required"), "{err}");
    }

    #[tokio::test]
    async fn corrupt_cache_is_invalidated_without_leaking_its_contents() {
        let directory = std::env::temp_dir().join(format!(
            "rig-chatgpt-corrupt-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir(&directory).expect("temporary directory");
        let path = directory.join("auth.json");
        std::fs::write(&path, br#"{"access_token":"sentinel-secret""#)
            .expect("corrupt credential fixture");

        let auth =
            PlatformAuthenticator::new(Some(path.clone()), DeviceCodeHandler::default(), false);
        let error = auth
            .auth_context_oauth()
            .await
            .expect_err("corrupt noninteractive cache must require reconnect")
            .to_string();

        assert!(error.contains("reconnect"), "{error}");
        assert!(!error.contains("sentinel-secret"), "{error}");
        assert!(!path.exists(), "corrupt cache must be invalidated");
        std::fs::remove_dir(directory).expect("clean temporary directory");
    }

    #[tokio::test]
    async fn rejection_reuses_a_newer_cached_token_without_refreshing_again() {
        let directory = std::env::temp_dir().join(format!(
            "rig-chatgpt-generation-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir(&directory).expect("temporary directory");
        let path = directory.join("auth.json");
        let newer = unexpired_access_token("newer");
        let record = super::AuthRecord {
            access_token: Some(newer.clone()),
            expires_at: super::extract_expiration_timestamp(&newer),
            ..Default::default()
        };
        crate::providers::internal::device_auth::write_json_record(Some(&path), &record)
            .expect("credential fixture");

        let auth =
            PlatformAuthenticator::new(Some(path.clone()), DeviceCodeHandler::default(), false);
        let context = auth
            .refresh_after_rejection_oauth("older-rejected-token")
            .await
            .expect("newer generation should be reused without network refresh");

        assert_eq!(context.access_token, newer);
        std::fs::remove_file(path).expect("remove credential fixture");
        std::fs::remove_dir(directory).expect("clean temporary directory");
    }

    #[tokio::test]
    async fn rejection_without_refresh_token_invalidates_cache() {
        let directory = std::env::temp_dir().join(format!(
            "rig-chatgpt-rejected-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir(&directory).expect("temporary directory");
        let path = directory.join("auth.json");
        let rejected = unexpired_access_token("rejected");
        let record = super::AuthRecord {
            access_token: Some(rejected.clone()),
            expires_at: super::extract_expiration_timestamp(&rejected),
            ..Default::default()
        };
        crate::providers::internal::device_auth::write_json_record(Some(&path), &record)
            .expect("credential fixture");

        let auth =
            PlatformAuthenticator::new(Some(path.clone()), DeviceCodeHandler::default(), false);
        let error = auth
            .refresh_after_rejection_oauth(&rejected)
            .await
            .expect_err("a rejected token without refresh credentials must reconnect")
            .to_string();

        assert!(error.contains("reconnect"), "{error}");
        assert!(!path.exists(), "rejected cache must be invalidated");
        std::fs::remove_dir(directory).expect("clean temporary directory");
    }

    #[test]
    fn invalidation_only_removes_the_rejected_token_generation() {
        let directory = std::env::temp_dir().join(format!(
            "rig-chatgpt-invalidate-generation-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        std::fs::create_dir(&directory).expect("temporary directory");
        let path = directory.join("auth.json");
        let current = unexpired_access_token("current");
        let record = super::AuthRecord {
            access_token: Some(current.clone()),
            expires_at: super::extract_expiration_timestamp(&current),
            ..Default::default()
        };
        crate::providers::internal::device_auth::write_json_record(Some(&path), &record)
            .expect("credential fixture");

        let auth =
            PlatformAuthenticator::new(Some(path.clone()), DeviceCodeHandler::default(), false);
        auth.invalidate_after_rejection_oauth("stale-token")
            .expect("a stale rejection must not remove newer credentials");
        assert!(path.exists(), "newer credential generation must survive");

        auth.invalidate_after_rejection_oauth(&current)
            .expect("the currently rejected generation should be invalidated");
        assert!(
            !path.exists(),
            "rejected credential generation must be removed"
        );
        std::fs::remove_dir(directory).expect("clean temporary directory");
    }

    #[test]
    fn refresh_error_never_includes_oauth_description_or_body() {
        let oauth_error = OAuthErrorResponse {
            error: Some("temporarily_unavailable".into()),
        };

        assert_eq!(
            format_refresh_error(
                StatusCode::BAD_GATEWAY,
                Some(&oauth_error),
                "sentinel-secret-body"
            ),
            "ChatGPT token refresh failed: 502 Bad Gateway temporarily_unavailable"
        );
    }

    #[test]
    fn build_auth_record_preserves_existing_refresh_token_when_refresh_omits_one() {
        let record = build_auth_record(
            OAuthTokenResponse {
                access_token: "access-token".into(),
                refresh_token: None,
                id_token: None,
            },
            Some("cached-refresh-token".into()),
        );

        assert_eq!(
            record.refresh_token.as_deref(),
            Some("cached-refresh-token")
        );
    }
}
