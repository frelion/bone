//! Constructors and protocol-specific helpers for supported LLM wire APIs.

use crate::ConfigError;

pub mod anthropic_messages;
pub mod openai_chat_completions;
pub mod openai_responses;

/// The wire contract used for requests, responses, and streaming events.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    /// OpenAI's `/responses` API, distinct from Chat Completions.
    OpenAiResponses,
    /// OpenAI's `/chat/completions` API, distinct from Responses.
    OpenAiChatCompletions,
    /// Anthropic's `/v1/messages` API.
    AnthropicMessages,
}

impl Protocol {
    /// A stable, human-readable protocol identifier for logs and telemetry.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAiResponses => "openai-responses",
            Self::OpenAiChatCompletions => "openai-chat-completions",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

pub(crate) fn validate_base_url(base_url: &str) -> Result<(), ConfigError> {
    if base_url.trim().is_empty() {
        return Err(ConfigError::EmptyBaseUrl);
    }

    let uri = base_url
        .parse::<rig_core::http_client::Uri>()
        .map_err(|_| ConfigError::InvalidBaseUrl)?;
    let is_http = matches!(uri.scheme_str(), Some("http" | "https"));
    let has_safe_authority = uri
        .authority()
        .is_some_and(|authority| !authority.as_str().contains('@'));
    if !is_http || !has_safe_authority || uri.query().is_some() {
        return Err(ConfigError::InvalidBaseUrl);
    }

    Ok(())
}

/// Build the transport used by endpoints that attach credentials to requests.
///
/// Redirects are deliberately rejected. A redirect changes the authority that
/// received the locally validated request, and a provider-specific credential
/// header must never follow it to a different origin.
pub(crate) fn no_redirect_http_client() -> Result<rig_core::http_client::ReqwestClient, ConfigError>
{
    rig_core::http_client::ReqwestClient::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ConfigError::HttpClientInitialization)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    #[test]
    fn exposes_stable_protocol_names() {
        assert_eq!(Protocol::OpenAiResponses.as_str(), "openai-responses");
        assert_eq!(
            Protocol::OpenAiChatCompletions.as_str(),
            "openai-chat-completions"
        );
        assert_eq!(Protocol::AnthropicMessages.as_str(), "anthropic-messages");
    }

    #[test]
    fn accepts_only_absolute_http_base_urls_without_credentials_or_queries() {
        assert_eq!(validate_base_url("  "), Err(ConfigError::EmptyBaseUrl));
        assert_eq!(
            validate_base_url("gateway.example/v1"),
            Err(ConfigError::InvalidBaseUrl)
        );
        assert_eq!(
            validate_base_url("ftp://gateway.example/v1"),
            Err(ConfigError::InvalidBaseUrl)
        );
        assert_eq!(
            validate_base_url("https://gateway.example/v1?tenant=one"),
            Err(ConfigError::InvalidBaseUrl)
        );
        let credentialed_url = "https://user:secret@gateway.example/v1";
        let error = validate_base_url(credentialed_url).unwrap_err();
        assert_eq!(error, ConfigError::InvalidBaseUrl);
        assert!(!error.to_string().contains("secret"));
        assert_eq!(validate_base_url("https://gateway.example/v1"), Ok(()));
    }

    #[tokio::test]
    async fn credential_transport_does_not_follow_redirects() {
        let target = TcpListener::bind("127.0.0.1:0").unwrap();
        target.set_nonblocking(true).unwrap();
        let target_address = target.local_addr().unwrap();
        let (target_seen_sender, target_seen_receiver) = mpsc::channel();
        let target_server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match target.accept() {
                    Ok(_) => {
                        target_seen_sender.send(true).unwrap();
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            target_seen_sender.send(false).unwrap();
                            return;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("target server failed: {error}"),
                }
            }
        });

        let redirect = TcpListener::bind("127.0.0.1:0").unwrap();
        let redirect_address = redirect.local_addr().unwrap();
        let redirect_server = thread::spawn(move || {
            let (mut stream, _) = redirect.accept().unwrap();
            let mut request = [0; 1024];
            assert!(stream.read(&mut request).unwrap() > 0);
            write!(
                stream,
                "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_address}/credential\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            stream.flush().unwrap();
        });

        let response = no_redirect_http_client()
            .unwrap()
            .get(format!("http://{redirect_address}/request"))
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), http::StatusCode::TEMPORARY_REDIRECT);
        assert!(
            !target_seen_receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
        );
        redirect_server.join().unwrap();
        target_server.join().unwrap();
    }
}
